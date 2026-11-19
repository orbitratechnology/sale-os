//! The scan pipeline (E07-T02, BAR-001, BAR-004..007, PRD §7.2).
//!
//! Every barcode delivery, from the wedge scanner or typed by hand, goes through
//! [`CartService::scan`]:
//!
//! 1. Each delivery carries a `scanEventId` minted by the UI. It is recorded in
//!    `scan_events_seen` **in the same transaction** as the cart change, so a delivery
//!    repeated by the IPC layer counts once ([`ScanOutcome::Duplicate`]). Scans are never
//!    de-duplicated by value or time: ten scans of the same code are ten items (BAR-007).
//! 2. The code is normalised, then looked up as a barcode, then as a SKU.
//! 3. A match adds the scan quantity (1, or the pack quantity) and merges with the last line
//!    of the same variant unless that line has a discount or override (BAR-005).
//! 4. An unknown code changes nothing ([`ScanOutcome::NotFound`], BAR-006).
//! 5. A scale label (BAR-009, `scanner.label_rules`) is looked up by its base code (the value
//!    as zeros) and becomes a line of its own: the label's weight, or exactly its price.

use crate::WriteSet;
use crate::cart::{CartService, CartView, Loaded, cart_err};
use crate::context::{AppError, CommandContext};
use crate::write::WriteError;
use pos_domain::cart::{AddVia, CartChange};
use pos_domain::{ErrorCode, Permission, ScanEventId};

/// Where the code came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanSource {
    /// A barcode scanner (keyboard wedge, serial, or the virtual scanner).
    Scanner,
    /// Typed into the code field and submitted (BAR-001).
    Manual,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScanOutcome {
    Added {
        view: CartView,
        change: CartChange,
    },
    /// This delivery was already processed. The cart is unchanged.
    Duplicate {
        view: CartView,
    },
    /// No barcode or SKU matches. The cart is unchanged; the UI offers search or create.
    NotFound {
        code: String,
    },
}

/// Longest code accepted. GS1-128 tops out at 48 characters; this leaves room for SKUs.
const MAX_CODE_LEN: usize = 64;

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
        if code.is_empty() || code.chars().count() > MAX_CODE_LEN || code.chars().any(char::is_control) {
            return Err(AppError::Validation(
                "the code is empty, too long or contains control characters".into(),
                ErrorCode::ScanInvalid,
            ));
        }
        let guard = self.exclusive().await;
        let rules = self.label_rules().await?;
        let label = if rules.is_empty() {
            None
        } else {
            pos_domain::catalog::normalize_barcode(code, None)
                .ok()
                .and_then(|n| pos_domain::catalog::variable_measure::parse_label(&n.value, &rules))
        };
        let lookup = label.as_ref().map_or(code, |l| l.base_code.as_str());
        let Some(hit) = self.catalog.lookup_code(lookup).await? else {
            // Still recorded, so a repeated delivery of an unknown code is not reported twice.
            let set = WriteSet::builder(ctx.write_context())
                .record_scan(scan_event_id)
                .build()?;
            return match self.store().apply(set).await {
                Ok(_) => Ok(ScanOutcome::NotFound {
                    code: code.to_owned(),
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
        let Loaded { mut cart, base } = self.load(ctx).await?;
        let change = match &label {
            None => cart.add(sellable, hit.scan_qty, via, ctx.ids.as_ref()),
            Some(l) => {
                let (qty, label) = label_line(&sellable, l.value)?;
                cart.add_label(sellable, qty, label, via, ctx.ids.as_ref())
            }
        }
        .map_err(cart_err)?;
        match self.save(ctx, cart, base, None, Some(scan_event_id)).await {
            Ok(view) => Ok(ScanOutcome::Added { view, change }),
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

/// The quantity and line label a scale label gives `item` (BAR-009). A weight label needs an
/// item sold by weight; a price label on an item sold by weight records the weight the price
/// buys (for stock), and on any other item one unit.
fn label_line(
    item: &pos_domain::cart::SellableVariant,
    value: pos_domain::catalog::variable_measure::LabelValue,
) -> Result<(pos_domain::Quantity, pos_domain::cart::CartLabel), AppError> {
    use pos_domain::Quantity;
    use pos_domain::cart::CartLabel;
    use pos_domain::catalog::Unit;
    use pos_domain::catalog::variable_measure::LabelValue;
    let refuse = |msg: &str| AppError::Validation(msg.into(), ErrorCode::ScanInvalid);
    match value {
        LabelValue::Weight(grams) => {
            let qty = match item.unit {
                Unit::Kg => grams,
                Unit::G => Quantity::from_milli(grams.milli().saturating_mul(1000)),
                _ => return Err(refuse("a weight label on an item that is not sold by weight")),
            };
            if !qty.is_positive() {
                return Err(refuse("the label's weight is zero"));
            }
            Ok((qty, CartLabel::Weight))
        }
        LabelValue::Price(price) => {
            let per_unit = item.price.minor();
            let qty = match item.unit {
                Unit::Kg | Unit::G if per_unit > 0 => {
                    // What the price buys at the shelf price, to the nearest 0.001 unit.
                    let milli =
                        (i128::from(price.minor()) * 1000 + i128::from(per_unit) / 2) / i128::from(per_unit);
                    Quantity::from_milli(i64::try_from(milli).unwrap_or(i64::MAX).max(1))
                }
                _ => Quantity::from_milli(1000),
            };
            Ok((qty, CartLabel::Price { price }))
        }
    }
}
