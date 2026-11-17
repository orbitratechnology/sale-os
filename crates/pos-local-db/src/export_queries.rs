//! Rows for the CSV exports (E14-T04). Record exports include practice (sandbox) rows with a
//! `mode` column, so an export is a complete copy; the report exports are live only.

use crate::SqliteStore;
use pos_application::export::{Cell, Dataset, ExportQueries, Table};
use pos_application::queries::QueryError;
use pos_domain::Timestamp;
use pos_domain::time::BusinessDate;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

/// How each selected column becomes a cell.
#[derive(Copy, Clone)]
enum Kind {
    Text,
    OptText,
    Money,
    OptMoney,
    Qty,
    OptQty,
    Int,
    Bool,
    /// Milliseconds since the epoch, written as local time in the row's time zone column.
    LocalTime(usize),
}

fn cells(r: &SqliteRow, kinds: &[Kind]) -> Result<Vec<Cell>, QueryError> {
    kinds
        .iter()
        .enumerate()
        .map(|(i, k)| {
            Ok(match k {
                Kind::Text => Cell::Text(r.try_get(i).map_err(q)?),
                Kind::OptText => Cell::from(r.try_get::<Option<String>, _>(i).map_err(q)?),
                Kind::Money => Cell::Money(r.try_get(i).map_err(q)?),
                Kind::OptMoney => r
                    .try_get::<Option<i64>, _>(i)
                    .map_err(q)?
                    .map_or(Cell::Empty, Cell::Money),
                Kind::Qty => Cell::Qty(r.try_get(i).map_err(q)?),
                Kind::OptQty => r
                    .try_get::<Option<i64>, _>(i)
                    .map_err(q)?
                    .map_or(Cell::Empty, Cell::Qty),
                Kind::Int => Cell::Int(r.try_get(i).map_err(q)?),
                Kind::Bool => Cell::Text(
                    if r.try_get::<bool, _>(i).map_err(q)? {
                        "yes"
                    } else {
                        "no"
                    }
                    .into(),
                ),
                Kind::LocalTime(zone) => {
                    let at: i64 = r.try_get(i).map_err(q)?;
                    let zone: String = r.try_get(*zone).map_err(q)?;
                    Cell::Text(
                        pos_domain::time::local_date_time(Timestamp::from_millis(at), &zone)
                            .unwrap_or_default(),
                    )
                }
            })
        })
        .collect()
}

#[async_trait::async_trait]
impl ExportQueries for SqliteStore {
    async fn export_table(
        &self,
        dataset: Dataset,
        range: Option<(BusinessDate, BusinessDate)>,
    ) -> Result<Table, QueryError> {
        use Kind::*;
        let (from, to) = range.map_or((String::new(), String::new()), |(f, t)| {
            (f.to_string(), t.to_string())
        });
        // (header, kinds, sql) — the last selected column may be a helper (time zone) not exported.
        let (header, kinds, sql): (Vec<&'static str>, Vec<Kind>, &str) = match dataset {
            Dataset::Transactions => (
                vec![
                    "receipt_number", "business_date", "local_time", "mode", "cashier", "line_no", "name", "variant", "sku",
                    "barcode", "unit", "quantity", "unit_price", "price_overridden", "gross", "line_discount",
                    "order_discount_share", "tax", "line_total",
                ],
                vec![
                    Text, Text, LocalTime(19), Text, Text, Int, Text, Text, OptText, OptText, Text, Qty, Money, Bool, Money,
                    Money, Money, Money, Money,
                ],
                "SELECT s.receipt_number, s.business_date, s.occurred_at, s.mode, coalesce(u.display_name, ''), l.line_no,
                   l.name_snapshot, l.variant_label_snapshot, l.sku_snapshot, l.barcode_snapshot, l.unit_code, l.quantity_milli,
                   l.unit_price_minor, l.price_overridden, l.gross_minor, l.line_discount_minor, l.order_discount_alloc_minor,
                   l.tax_minor, l.total_minor, s.timezone
                 FROM sales s JOIN sale_lines l ON l.sale_id = s.sale_id LEFT JOIN staff_users u ON u.user_id = s.cashier_user_id
                 WHERE s.business_date BETWEEN ?1 AND ?2 ORDER BY s.business_date, s.receipt_seq, l.line_no",
            ),
            Dataset::Tenders => (
                vec![
                    "receipt_number", "business_date", "mode", "seq", "tender", "amount_applied", "received", "change",
                    "status", "confirmed_by", "reference",
                ],
                vec![Text, Text, Text, Int, Text, Money, OptMoney, Money, Text, Text, OptText],
                "SELECT s.receipt_number, s.business_date, s.mode, t.seq, t.tender_type, t.amount_minor, t.received_minor,
                   t.change_minor, t.status, t.confirmation_source, t.external_reference
                 FROM sales s JOIN tenders t ON t.sale_id = s.sale_id
                 WHERE s.business_date BETWEEN ?1 AND ?2 ORDER BY s.business_date, s.receipt_seq, t.seq",
            ),
            Dataset::Returns => (
                vec![
                    "return_number", "business_date", "local_time", "mode", "original_receipt", "reason", "name", "variant",
                    "quantity", "disposition", "refund", "tax", "refunded_by",
                ],
                vec![Text, Text, LocalTime(13), Text, Text, Text, Text, Text, Qty, Text, Money, Money, Text],
                "SELECT r.return_number, r.business_date, r.occurred_at, s.mode, s.receipt_number, r.reason_code,
                   l.name_snapshot, l.variant_label_snapshot, rl.quantity_milli, rl.disposition, rl.refund_minor, rl.tax_minor,
                   coalesce((SELECT group_concat(tender_type, '+') FROM (SELECT f.tender_type FROM refunds f WHERE f.return_id = r.return_id ORDER BY f.seq)), ''),
                   s.timezone
                 FROM returns r JOIN return_lines rl ON rl.return_id = r.return_id
                 JOIN sale_lines l ON l.sale_line_id = rl.original_sale_line_id JOIN sales s ON s.sale_id = r.original_sale_id
                 WHERE r.business_date BETWEEN ?1 AND ?2 ORDER BY r.business_date, r.return_seq, rl.rowid",
            ),
            Dataset::Products => (
                vec![
                    "name", "variant", "sku", "barcode", "price", "cost", "unit", "tax_category", "track_stock",
                    "low_stock_threshold", "scan_quantity", "status",
                ],
                vec![Text, Text, OptText, OptText, Money, OptMoney, Text, Text, Bool, OptQty, Qty, Text],
                "SELECT p.name, v.label, v.sku,
                   (SELECT b.value_raw FROM product_barcodes b WHERE b.variant_id = v.variant_id
                    ORDER BY b.qualifier = 'primary' DESC, b.rowid LIMIT 1),
                   v.price_minor, v.cost_minor, p.unit_code, c.name, v.track_stock, v.low_stock_threshold_milli,
                   v.scan_qty_milli, CASE WHEN p.status = 'active' AND v.status = 'active' THEN 'active' ELSE 'archived' END
                 FROM product_variants v JOIN products p ON p.product_id = v.product_id
                 JOIN tax_categories c ON c.tax_category_id = p.tax_category_id
                 WHERE ?1 = ?2 ORDER BY p.name COLLATE NOCASE, v.label",
            ),
            Dataset::Stock => (
                vec!["name", "variant", "sku", "unit", "on_hand", "damaged", "low_stock_threshold"],
                vec![Text, Text, OptText, Text, Qty, Qty, OptQty],
                "SELECT p.name, v.label, v.sku, p.unit_code,
                   coalesce((SELECT sum(on_hand_milli) FROM stock_balances b WHERE b.variant_id = v.variant_id AND b.bucket = 'sellable'), 0),
                   coalesce((SELECT sum(on_hand_milli) FROM stock_balances b WHERE b.variant_id = v.variant_id AND b.bucket = 'damaged'), 0),
                   v.low_stock_threshold_milli
                 FROM product_variants v JOIN products p ON p.product_id = v.product_id
                 WHERE v.track_stock = 1 AND v.status = 'active' AND p.status = 'active' AND ?1 = ?2
                 ORDER BY p.name COLLATE NOCASE, v.label",
            ),
            Dataset::SalesSummary | Dataset::TenderMix | Dataset::TopProducts => {
                return Err(QueryError("report exports are built from the report queries".into()));
            }
        };
        let rows = sqlx::query(sql)
            .bind(&from)
            .bind(&to)
            .fetch_all(self.db().readers())
            .await
            .map_err(q)?;
        Ok(Table {
            header,
            rows: rows.iter().map(|r| cells(r, &kinds)).collect::<Result<_, _>>()?,
        })
    }
}
