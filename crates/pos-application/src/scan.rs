//! The scan pipeline (E07-T02, BAR-001, BAR-004..007, PRD §7.2).
//!
//! Every barcode delivery, from the wedge scanner or typed by hand, goes through
//! [`CartService::scan`]:
//!
//! 1. Each delivery carries a `scanEventId` minted by the UI. It is recorded in
//!    `scan_events_seen` **in the same transaction** as the cart change, so a delivery
//!    repeated by the IPC layer counts once ([`ScanOutcome::Duplicate`]). Scans are never
//!    de-duplicated by value or time: ten scans of the same code are ten items (BAR-007).
//! 2. The code is decoded (`pos_domain::catalog::decode_scan`, E21-T01) into lookup
//!    candidates, most specific first: the code as delivered, then what it encodes (AIM
//!    identifier stripped, the GTIN inside a GS1 string or Digital Link, UPC-E ↔ UPC-A, the
//!    EAN without its add-on, ISBN-10 → 978, Code 39/Codabar start/stop, the item code of a
//!    scale label, a keyboard-layout remap). The first candidate a product carries wins; then
//!    the raw code is tried as a SKU.
//! 3. A match adds the scan quantity (1, or the pack quantity) and merges with the last line
//!    of the same variant unless that line has a discount or override (BAR-005). A scale or
//!    GS1 label with a weight, count or price sets the line instead (E21-T02,
//!    `pos_domain::catalog::label`). A label price is the shop's own, so it needs no manager
//!    approval, but it is audited as a price override with `source: barcode_label`.
//! 4. An unknown code changes nothing ([`ScanOutcome::NotFound`], BAR-006). When the decoder
//!    noticed a likely set-up problem (scanner on the wrong keyboard layout, a failed check
//!    digit), the outcome carries a [`ScanHint`] for the cashier.

use crate::WriteSet;
use crate::audit;
use crate::cart::{Audited, CartService, CartView, Loaded, cart_err};
use crate::context::{AppError, CommandContext};
use crate::queries::CatalogHit;
use crate::write::WriteError;
use pos_domain::cart::{AddVia, CartChange};
use pos_domain::catalog::{
    CandidateSource, DecodedScan, LabelMeasure, MeasureKind, ScanDecodeConfig, ScanNote, decode_scan,
    label_line, variable_measure_rules,
};
use pos_domain::{ErrorCode, HasErrorCode, Permission, ScanEventId};

/// Where the code came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanSource {
    /// A barcode scanner (keyboard wedge, serial, or the virtual scanner).
    Scanner,
    /// Typed into the code field and submitted (BAR-001).
    Manual,
}

/// A set-up problem the decoder noticed, worth telling the cashier about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanHint {
    /// The scanner types as if on another keyboard layout (e.g. `azerty_fr`).
    KeyboardLayout(&'static str),
    /// A retail barcode's check digit is wrong: a mis-scan or a mistyped code.
    CheckDigitFailed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScanOutcome {
    Added {
        view: CartView,
        change: CartChange,
        hint: Option<ScanHint>,
    },
    /// This delivery was already processed. The cart is unchanged.
    Duplicate { view: CartView },
    /// No barcode or SKU matches. The cart is unchanged; the UI offers search or create.
    NotFound { code: String, hint: Option<ScanHint> },
}

/// Longest scan accepted: 2D codes (GS1 DataMatrix, Digital Link URLs) are long.
const MAX_CODE_LEN: usize = pos_domain::catalog::MAX_SCAN_INPUT_LEN;

/// ASCII GS, the GS1 FNC1 separator: the only control character a scan may carry.
const GS: char = '\u{1d}';

fn hint(decoded: Option<&DecodedScan>) -> Option<ScanHint> {
    let notes = &decoded?.notes;
    notes
        .iter()
        .find_map(|n| match n {
            ScanNote::KeyboardLayoutMismatch { layout } => Some(ScanHint::KeyboardLayout(layout)),
            _ => None,
        })
        .or_else(|| {
            notes
                .iter()
                .any(|n| matches!(n, ScanNote::CheckDigitFailed))
                .then_some(ScanHint::CheckDigitFailed)
        })
}

/// The measure a matched label carries: a scale label's value when its item code matched,
/// or the GS1 weight/count/price when the GTIN matched.
fn label_measure(decoded: &DecodedScan, via: &CandidateSource) -> Option<LabelMeasure> {
    match via {
        CandidateSource::VariableMeasurePlu | CandidateSource::VariableMeasureItem => {
            let m = decoded.measure.as_ref()?;
            let value = u64::from(m.value);
            Some(match m.kind {
                MeasureKind::Price => LabelMeasure::Price {
                    value,
                    decimals: m.decimals,
                },
                MeasureKind::Weight => LabelMeasure::WeightKg {
                    value,
                    decimals: m.decimals,
                },
                MeasureKind::Count => LabelMeasure::Count(value),
            })
        }
        CandidateSource::Gs1Gtin | CandidateSource::Exact | CandidateSource::AimStripped => {
            let g = decoded.gs1.as_ref()?;
            if let Some(p) = &g.price
                && p.currency.is_none()
                && let Ok(value) = p.amount.parse::<u64>()
            {
                return Some(LabelMeasure::Price {
                    value,
                    decimals: p.decimals,
                });
            }
            if let Some(w) = &g.net_weight_kg {
                return Some(LabelMeasure::WeightKg {
                    value: u64::from(w.value),
                    decimals: w.decimals,
                });
            }
            g.count.map(|c| LabelMeasure::Count(u64::from(c)))
        }
        _ => None,
    }
}

impl CartService {
    pub async fn scan(
        &self,
        ctx: &CommandContext,
        raw: &str,
        scan_event_id: ScanEventId,
        source: ScanSource,
    ) -> Result<ScanOutcome, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        let code = raw.trim();
        if code.is_empty()
            || code.chars().count() > MAX_CODE_LEN
            || code.chars().any(|c| c.is_control() && c != GS)
        {
            return Err(AppError::Validation(
                "the code is empty, too long or contains control characters".into(),
                ErrorCode::ScanInvalid,
            ));
        }
        let rules = variable_measure_rules(&self.carts.scan_rules().await?).unwrap_or_default();
        let decoded = decode_scan(
            code,
            None,
            &ScanDecodeConfig {
                variable_measure: rules,
            },
        );
        let guard = self.exclusive().await;
        let mut found: Option<(CatalogHit, CandidateSource)> = None;
        if let Ok(d) = &decoded {
            let values: Vec<String> = d.candidates.iter().map(|c| c.value.clone()).collect();
            if let Some((i, h)) = self.catalog.lookup_barcodes(&values).await? {
                found = Some((h, d.candidates[i].via));
            }
        }
        if found.is_none() && !code.contains(GS) {
            found = self
                .catalog
                .lookup_code(code)
                .await?
                .map(|h| (h, CandidateSource::Exact));
        }
        let hint = hint(decoded.as_ref().ok());
        let Some((hit, matched)) = found else {
            if let Err(e) = &decoded {
                // Nothing usable and no SKU either: a malformed scan.
                return Err(AppError::Validation(e.to_string(), e.error_code()));
            }
            // Still recorded, so a repeated delivery of an unknown code is not reported twice.
            let set = WriteSet::builder(ctx.write_context())
                .record_scan(scan_event_id)
                .build()?;
            return match self.store().apply(set).await {
                Ok(_) => Ok(ScanOutcome::NotFound {
                    code: code.replace(GS, " "),
                    hint,
                }),
                Err(WriteError::ScanAlreadySeen) => {
                    drop(guard);
                    Ok(ScanOutcome::Duplicate {
                        view: self.current(ctx).await?,
                    })
                }
                Err(e) => Err(e.into()),
            };
        };
        let sellable = self.sellable(ctx, hit.variant_id).await?;
        let via = match source {
            ScanSource::Scanner => AddVia::Scan {
                event_id: scan_event_id,
            },
            ScanSource::Manual => AddVia::ManualCode,
        };
        let label = match decoded.as_ref().ok().and_then(|d| label_measure(d, &matched)) {
            None => None,
            Some(m) => {
                let settings = self.carts.pricing_settings().await?;
                Some(
                    label_line(m, sellable.unit, sellable.price, settings.currency.exponent)
                        .map_err(|e| AppError::Validation(e.to_string(), e.error_code()))?,
                )
            }
        };
        let Loaded { mut cart, base, .. } = self.load(ctx).await?;
        let (change, audited) = match label {
            None => (
                cart.add(sellable, hit.scan_qty, via, ctx.ids.as_ref())
                    .map_err(cart_err)?,
                None,
            ),
            Some(l) => {
                // A label line stands alone: never merged into another line of the item.
                let catalogue_price = sellable.price;
                let change = cart
                    .add_separate(sellable, l.quantity, via, ctx.ids.as_ref())
                    .map_err(cart_err)?;
                let audited = match (l.price_override, &change) {
                    (Some(price), CartChange::LineAdded(line)) => {
                        cart.override_price(*line, price).map_err(cart_err)?;
                        Some(Audited::new(
                            audit::CART_PRICE_OVERRIDDEN,
                            serde_json::json!({
                                "line": line,
                                "from": catalogue_price.minor().to_string(),
                                "to": price.minor().to_string(),
                                "source": "barcode_label",
                            }),
                            None,
                        ))
                    }
                    _ => None,
                };
                (change, audited)
            }
        };
        match self.save(ctx, cart, base, audited, Some(scan_event_id)).await {
            Ok(view) => Ok(ScanOutcome::Added { view, change, hint }),
            Err(AppError::Write(WriteError::ScanAlreadySeen)) => {
                drop(guard);
                Ok(ScanOutcome::Duplicate {
                    view: self.current(ctx).await?,
                })
            }
            Err(e) => Err(e),
        }
    }
}
