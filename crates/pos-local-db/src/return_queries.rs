//! Reads for returns (E12).

use crate::SqliteStore;
use pos_application::queries::{QueryError, ReturnQueries};
use pos_domain::catalog::Unit;
use pos_domain::returns::{ReturnableLine, ReturnableSale};
use pos_domain::sale::SaleMode;
use pos_domain::{Currency, CurrencyCode, Money, Quantity, SaleId};

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

type LineRow = (
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<i64>,
    bool,
    i64,
);

#[async_trait::async_trait]
impl ReturnQueries for SqliteStore {
    async fn returnable_sale(&self, sale: SaleId) -> Result<Option<ReturnableSale>, QueryError> {
        let r = self.db().readers();
        let head: Option<(String, String, String, i64)> = sqlx::query_as(
            "SELECT receipt_number, mode, currency_code, currency_exponent FROM sales WHERE sale_id = ?",
        )
        .bind(sale.to_string())
        .fetch_optional(r)
        .await
        .map_err(q)?;
        let Some((receipt_number, mode, currency, exponent)) = head else {
            return Ok(None);
        };
        // A line moves stock on return only if it moved stock when sold.
        let rows: Vec<LineRow> = sqlx::query_as(
            "SELECT l.sale_line_id, l.line_no, l.product_id, l.variant_id, l.name_snapshot, l.variant_label_snapshot,
               l.unit_code, l.quantity_milli, l.total_minor, l.tax_minor, l.unit_cost_minor,
               EXISTS (SELECT 1 FROM stock_movements m WHERE m.source_type = 'sale' AND m.source_line_id = l.sale_line_id),
               coalesce((SELECT sum(rl.quantity_milli) FROM return_lines rl WHERE rl.original_sale_line_id = l.sale_line_id), 0)
             FROM sale_lines l WHERE l.sale_id = ? ORDER BY l.line_no",
        )
        .bind(sale.to_string())
        .fetch_all(r)
        .await
        .map_err(q)?;
        let lines = rows
            .into_iter()
            .map(
                |(id, no, product, variant, name, label, unit, qty, total, tax, cost, tracked, returned)| {
                    Ok(ReturnableLine {
                        sale_line_id: id.parse().map_err(q)?,
                        line_no: u32::try_from(no).map_err(q)?,
                        product_id: product.parse().map_err(q)?,
                        variant_id: variant.parse().map_err(q)?,
                        name,
                        variant_label: label,
                        unit: Unit::from_code(&unit)
                            .ok_or_else(|| QueryError(format!("unknown unit {unit:?}")))?,
                        track_stock: tracked,
                        quantity: Quantity::from_milli(qty),
                        total: Money::from_minor(total),
                        tax: Money::from_minor(tax),
                        unit_cost: cost.map(Money::from_minor),
                        returned: Quantity::from_milli(returned),
                    })
                },
            )
            .collect::<Result<Vec<_>, QueryError>>()?;
        Ok(Some(ReturnableSale {
            sale_id: sale,
            receipt_number,
            mode: if mode == "live" {
                SaleMode::Live
            } else {
                SaleMode::Sandbox
            },
            currency: Currency::new(
                CurrencyCode::new(&currency).map_err(q)?,
                u8::try_from(exponent).map_err(q)?,
            )
            .map_err(q)?,
            lines,
        }))
    }

    async fn sale_by_receipt(&self, receipt_number: &str) -> Result<Option<SaleId>, QueryError> {
        let id: Option<String> =
            sqlx::query_scalar("SELECT sale_id FROM sales WHERE receipt_number = ? COLLATE NOCASE")
                .bind(receipt_number.trim())
                .fetch_optional(self.db().readers())
                .await
                .map_err(q)?;
        id.map(|i| i.parse().map_err(q)).transpose()
    }

    async fn cashier_returns_need_approval(&self) -> Result<bool, QueryError> {
        let v: Option<String> = sqlx::query_scalar(
            "SELECT value_json FROM business_settings WHERE key = 'returns.cashier_requires_approval' LIMIT 1",
        )
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        match v {
            None => Ok(true),
            Some(v) => serde_json::from_str::<bool>(&v).map_err(|_| {
                QueryError(format!(
                    "returns.cashier_requires_approval is not true/false: {v}"
                ))
            }),
        }
    }
}
