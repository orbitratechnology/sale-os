//! Cart reads (E09-T01): the open cart, parked carts (POS-012) and sellable variants priced
//! for a business date.

use crate::SqliteStore;
use pos_application::queries::{
    CartQueries, ParkedCart, PricingSettings, QueryError, StoredCart, StoredCartLine,
};
use pos_domain::cart::{AddVia, SellableVariant};
use pos_domain::catalog::Unit;
use pos_domain::pricing::{TaxMode, TaxRate, resolve_rates};
use pos_domain::{
    BusinessDate, Currency, CurrencyCode, Money, Quantity, RatePpm, RegisterId, Timestamp, VariantId,
};

/// Used when the setting row is missing (setup always writes it).
const DEFAULT_DISCOUNT_LIMIT_PPM: u32 = 100_000;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

#[async_trait::async_trait]
impl CartQueries for SqliteStore {
    async fn open_cart(&self, register: RegisterId) -> Result<Option<StoredCart>, QueryError> {
        let head: Option<(String, i64, String, Option<String>)> = sqlx::query_as(
            "SELECT cart_id, revision, opened_by, order_discount_json FROM carts WHERE register_id = ? AND status = 'open'",
        )
        .bind(register.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        match head {
            Some(h) => Ok(Some(stored_cart(self, h).await?)),
            None => Ok(None),
        }
    }

    async fn parked_carts(&self, register: RegisterId) -> Result<Vec<ParkedCart>, QueryError> {
        type Row = (
            String,
            i64,
            String,
            Option<String>,
            Option<String>,
            i64,
            String,
            String,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT c.cart_id, c.revision, c.opened_by, c.order_discount_json, c.parked_label, c.parked_at,
                    c.parked_by, coalesce(u.display_name, '')
             FROM carts c LEFT JOIN staff_users u ON u.user_id = c.parked_by
             WHERE c.register_id = ? AND c.status = 'parked' ORDER BY c.parked_at, c.cart_id",
        )
        .bind(register.to_string())
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, revision, opened_by, discount, label, parked_at, parked_by, name) in rows {
            out.push(ParkedCart {
                cart: stored_cart(self, (id, revision, opened_by, discount)).await?,
                label,
                parked_at: Timestamp::from_millis(parked_at),
                parked_by: parked_by.parse().map_err(q)?,
                parked_by_name: name,
            });
        }
        Ok(out)
    }

    async fn sellable(
        &self,
        variant: VariantId,
        date: BusinessDate,
    ) -> Result<Option<SellableVariant>, QueryError> {
        let r = self.db().readers();
        type Row = (
            String,
            String,
            String,
            Option<String>,
            String,
            i64,
            bool,
            String,
            String,
            Option<i64>,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT p.product_id, p.name, v.label, v.sku, p.unit_code, v.price_minor, v.track_stock, p.tax_category_id,
               p.product_type, v.cost_minor
             FROM product_variants v JOIN products p USING (product_id) WHERE v.variant_id = ?",
        )
        .bind(variant.to_string())
        .fetch_optional(r)
        .await
        .map_err(q)?;
        let Some((product, name, label, sku, unit, price, track, tax_category, product_type, cost)) = row
        else {
            return Ok(None);
        };
        let rates: Vec<(String, String, i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT r.tax_rate_id, r.name, r.rate_ppm, r.effective_from, r.effective_to
             FROM tax_category_rates c JOIN tax_rates r USING (tax_rate_id)
             WHERE c.tax_category_id = ? AND r.status = 'active'
               AND NOT EXISTS (SELECT 1 FROM business_settings
                               WHERE key = 'tax.registered' AND trim(value_json) = 'false')
             ORDER BY r.rate_ppm DESC",
        )
        .bind(&tax_category)
        .fetch_all(r)
        .await
        .map_err(q)?;
        let rates = rates
            .into_iter()
            .map(|(id, name, ppm, from, to)| {
                Ok(TaxRate {
                    id: id.parse().map_err(q)?,
                    name,
                    rate: RatePpm::new(u32::try_from(ppm).map_err(q)?).map_err(q)?,
                    effective_from: from.map(|d| d.parse()).transpose().map_err(q)?,
                    effective_to: to.map(|d| d.parse()).transpose().map_err(q)?,
                })
            })
            .collect::<Result<Vec<_>, QueryError>>()?;
        Ok(Some(SellableVariant {
            variant_id: variant,
            product_id: product.parse().map_err(q)?,
            name,
            label,
            sku,
            unit: Unit::from_code(&unit).ok_or_else(|| QueryError(format!("unknown unit {unit:?}")))?,
            price: Money::from_minor(price),
            taxes: resolve_rates(&rates, date),
            track_stock: track,
            product_type: pos_domain::catalog::ProductType::from_code(&product_type)
                .ok_or_else(|| QueryError(format!("unknown product type {product_type:?}")))?,
            cost: cost.map(Money::from_minor),
        }))
    }

    async fn scan_rules(&self) -> Result<Vec<String>, QueryError> {
        let v: Option<String> = sqlx::query_scalar(
            "SELECT value_json FROM business_settings WHERE key = 'scanner.variable_measure_rules' LIMIT 1",
        )
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        Ok(v.and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_default())
    }

    async fn pricing_settings(&self) -> Result<PricingSettings, QueryError> {
        let (inclusive, code, exponent, timezone): (bool, String, i64, String) = sqlx::query_as(
            "SELECT prices_include_tax, currency_code, currency_exponent, timezone FROM businesses LIMIT 1",
        )
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?
        .ok_or_else(|| QueryError("no business set up yet".into()))?;
        let limit: Option<String> = sqlx::query_scalar(
            "SELECT value_json FROM business_settings WHERE key = 'discount.cashier_max_ppm' LIMIT 1",
        )
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        let discount_limit_ppm = match limit {
            Some(v) => serde_json::from_str::<u32>(&v)
                .ok()
                .filter(|p| *p <= 1_000_000)
                .ok_or_else(|| QueryError(format!("discount.cashier_max_ppm is not a rate: {v}")))?,
            None => DEFAULT_DISCOUNT_LIMIT_PPM,
        };
        Ok(PricingSettings {
            discount_limit_ppm,
            tax_mode: if inclusive {
                TaxMode::Inclusive
            } else {
                TaxMode::Exclusive
            },
            currency: Currency::new(
                CurrencyCode::new(&code).map_err(q)?,
                u8::try_from(exponent).map_err(q)?,
            )
            .map_err(q)?,
            timezone,
        })
    }
}

/// A cart's lines, given its header row (`cart_id, revision, opened_by, order_discount_json`).
async fn stored_cart(
    store: &SqliteStore,
    (cart_id, revision, opened_by, order_discount): (String, i64, String, Option<String>),
) -> Result<StoredCart, QueryError> {
    let r = store.db().readers();
    type LineRow = (
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    );
    let rows: Vec<LineRow> = sqlx::query_as(
            "SELECT cart_line_id, variant_id, quantity_milli, unit_price_override_minor, line_discount_json, added_via,
                    last_scan_event_id, label_kind, label_price_minor, price_seen_minor
             FROM cart_lines WHERE cart_id = ? ORDER BY position",
        )
        .bind(&cart_id)
        .fetch_all(r)
        .await
        .map_err(q)?;
    let lines = rows
        .into_iter()
        .map(
            |(id, variant, qty, over, disc, via, scan, label_kind, label_price, seen)| {
                let added_via = match (via.as_str(), scan) {
                    ("scan", Some(e)) => AddVia::Scan {
                        event_id: e.parse().map_err(q)?,
                    },
                    ("quick_select", _) => AddVia::QuickSelect,
                    ("manual_code", _) => AddVia::ManualCode,
                    _ => AddVia::Search,
                };
                Ok(StoredCartLine {
                    line_id: id.parse().map_err(q)?,
                    variant_id: variant.parse().map_err(q)?,
                    quantity: Quantity::from_milli(qty),
                    price_override: over.map(Money::from_minor),
                    line_discount: disc.map(|d| serde_json::from_str(&d)).transpose().map_err(q)?,
                    added_via,
                    label: match (label_kind.as_deref(), label_price) {
                        (Some("weight"), _) => Some(pos_domain::cart::CartLabel::Weight),
                        (Some("price"), Some(p)) => Some(pos_domain::cart::CartLabel::Price {
                            price: Money::from_minor(p),
                        }),
                        _ => None,
                    },
                    price_seen: seen.map(Money::from_minor),
                })
            },
        )
        .collect::<Result<Vec<_>, QueryError>>()?;
    Ok(StoredCart {
        cart_id: cart_id.parse().map_err(q)?,
        revision: u64::try_from(revision).map_err(q)?,
        opened_by: opened_by.parse().map_err(q)?,
        order_discount: order_discount
            .map(|d| serde_json::from_str(&d))
            .transpose()
            .map_err(q)?,
        lines,
    })
}
