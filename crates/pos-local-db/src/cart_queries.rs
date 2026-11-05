//! Cart reads (E09-T01): the open cart and sellable variants priced for a business date.

use crate::SqliteStore;
use pos_application::queries::{CartQueries, PricingSettings, QueryError, StoredCart, StoredCartLine};
use pos_domain::cart::{AddVia, SellableVariant};
use pos_domain::catalog::Unit;
use pos_domain::pricing::{TaxMode, TaxRate, resolve_rates};
use pos_domain::{BusinessDate, Currency, CurrencyCode, Money, Quantity, RatePpm, RegisterId, VariantId};

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

#[async_trait::async_trait]
impl CartQueries for SqliteStore {
    async fn open_cart(&self, register: RegisterId) -> Result<Option<StoredCart>, QueryError> {
        let r = self.db().readers();
        let head: Option<(String, i64, String, Option<String>)> = sqlx::query_as(
            "SELECT cart_id, revision, opened_by, order_discount_json FROM carts WHERE register_id = ? AND status = 'open'",
        )
        .bind(register.to_string())
        .fetch_optional(r)
        .await
        .map_err(q)?;
        let Some((cart_id, revision, opened_by, order_discount)) = head else {
            return Ok(None);
        };
        type LineRow = (
            String,
            String,
            i64,
            Option<i64>,
            Option<String>,
            String,
            Option<String>,
        );
        let rows: Vec<LineRow> = sqlx::query_as(
            "SELECT cart_line_id, variant_id, quantity_milli, unit_price_override_minor, line_discount_json, added_via,
                    last_scan_event_id
             FROM cart_lines WHERE cart_id = ? ORDER BY position",
        )
        .bind(&cart_id)
        .fetch_all(r)
        .await
        .map_err(q)?;
        let lines = rows
            .into_iter()
            .map(|(id, variant, qty, over, disc, via, scan)| {
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
                })
            })
            .collect::<Result<Vec<_>, QueryError>>()?;
        Ok(Some(StoredCart {
            cart_id: cart_id.parse().map_err(q)?,
            revision: u64::try_from(revision).map_err(q)?,
            opened_by: opened_by.parse().map_err(q)?,
            order_discount: order_discount
                .map(|d| serde_json::from_str(&d))
                .transpose()
                .map_err(q)?,
            lines,
        }))
    }

    async fn sellable(
        &self,
        variant: VariantId,
        date: BusinessDate,
    ) -> Result<Option<SellableVariant>, QueryError> {
        let r = self.db().readers();
        type Row = (String, String, String, Option<String>, String, i64, bool, String);
        let row: Option<Row> = sqlx::query_as(
            "SELECT p.product_id, p.name, v.label, v.sku, p.unit_code, v.price_minor, v.track_stock, p.tax_category_id
             FROM product_variants v JOIN products p USING (product_id) WHERE v.variant_id = ?",
        )
        .bind(variant.to_string())
        .fetch_optional(r)
        .await
        .map_err(q)?;
        let Some((product, name, label, sku, unit, price, track, tax_category)) = row else {
            return Ok(None);
        };
        let rates: Vec<(String, String, i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT r.tax_rate_id, r.name, r.rate_ppm, r.effective_from, r.effective_to
             FROM tax_category_rates c JOIN tax_rates r USING (tax_rate_id)
             WHERE c.tax_category_id = ? AND r.status = 'active' ORDER BY r.rate_ppm DESC",
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
        }))
    }

    async fn pricing_settings(&self) -> Result<PricingSettings, QueryError> {
        let (inclusive, code, exponent, timezone): (bool, String, i64, String) = sqlx::query_as(
            "SELECT prices_include_tax, currency_code, currency_exponent, timezone FROM businesses LIMIT 1",
        )
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?
        .ok_or_else(|| QueryError("no business set up yet".into()))?;
        Ok(PricingSettings {
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
