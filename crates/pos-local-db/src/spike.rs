//! STAGE 0 SPIKE (E00-T06): throwaway `spike_*` tables and queries for the vertical slice.
//! The tables are created at runtime (`ensure_spike_tables`), **not** by a migration, so
//! deleting the spike leaves no permanent schema behind. Delete this module with
//! `pos_application::spike` (E09-T08).

use crate::{LocalDb, StoreError};
use pos_application::spike::{SpikeCartLine, SpikeProduct};
use pos_domain::{BusinessId, LocationId, Money, ProductId, Quantity, RegisterId};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS spike_products (
  product_id TEXT PRIMARY KEY, name TEXT NOT NULL, barcode TEXT NOT NULL UNIQUE, price_minor INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS spike_cart_lines (
  line_id TEXT PRIMARY KEY, product_id TEXT NOT NULL REFERENCES spike_products(product_id),
  qty_milli INTEGER NOT NULL CHECK (qty_milli > 0), added_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS spike_sales (
  sale_id TEXT PRIMARY KEY, receipt_seq INTEGER NOT NULL, receipt_number TEXT NOT NULL UNIQUE,
  total_minor INTEGER NOT NULL, occurred_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS spike_sale_lines (
  line_id TEXT PRIMARY KEY, sale_id TEXT NOT NULL REFERENCES spike_sales(sale_id), product_id TEXT NOT NULL,
  name TEXT NOT NULL, barcode TEXT NOT NULL, qty_milli INTEGER NOT NULL, unit_price_minor INTEGER NOT NULL,
  total_minor INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS spike_tenders (
  tender_id TEXT PRIMARY KEY, sale_id TEXT NOT NULL REFERENCES spike_sales(sale_id), kind TEXT NOT NULL,
  amount_minor INTEGER NOT NULL, received_minor INTEGER NOT NULL, change_minor INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS spike_stock_movements (
  movement_id TEXT PRIMARY KEY, product_id TEXT NOT NULL, sale_id TEXT NOT NULL REFERENCES spike_sales(sale_id),
  delta_milli INTEGER NOT NULL, occurred_at INTEGER NOT NULL
) STRICT;
";

pub const SPIKE_TABLES: &[&str] = &[
    "spike_products",
    "spike_cart_lines",
    "spike_sales",
    "spike_sale_lines",
    "spike_tenders",
    "spike_stock_movements",
];

pub async fn ensure_spike_tables(db: &LocalDb) -> Result<(), StoreError> {
    sqlx::raw_sql(SCHEMA)
        .execute(db.writer())
        .await
        .map(|_| ())
        .map_err(StoreError::from_sqlx)
}

/// The workspace this database belongs to, if one has been created.
pub async fn workspace(db: &LocalDb) -> Result<Option<(BusinessId, LocationId, RegisterId)>, StoreError> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT business_id, location_id, register_id FROM registers ORDER BY created_at LIMIT 1",
    )
    .fetch_optional(db.readers())
    .await
    .map_err(StoreError::from_sqlx)?;
    row.map(|(b, l, r)| {
        let parse_err = |e: pos_domain::ids::IdParseError| StoreError::Corrupt(e.to_string());
        Ok((
            b.parse().map_err(parse_err)?,
            l.parse().map_err(parse_err)?,
            r.parse().map_err(parse_err)?,
        ))
    })
    .transpose()
}

fn product_from(row: (String, String, String, i64)) -> Result<SpikeProduct, StoreError> {
    Ok(SpikeProduct {
        product_id: row
            .0
            .parse::<ProductId>()
            .map_err(|e| StoreError::Corrupt(e.to_string()))?,
        name: row.1,
        barcode: row.2,
        price: Money::from_minor(row.3),
    })
}

pub async fn product_by_barcode(db: &LocalDb, barcode: &str) -> Result<Option<SpikeProduct>, StoreError> {
    let row: Option<(String, String, String, i64)> =
        sqlx::query_as("SELECT product_id, name, barcode, price_minor FROM spike_products WHERE barcode = ?")
            .bind(barcode)
            .fetch_optional(db.readers())
            .await
            .map_err(StoreError::from_sqlx)?;
    row.map(product_from).transpose()
}

pub async fn cart(db: &LocalDb) -> Result<Vec<SpikeCartLine>, StoreError> {
    let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT p.product_id, p.name, p.barcode, p.price_minor, c.qty_milli
         FROM spike_cart_lines c JOIN spike_products p USING (product_id) ORDER BY c.added_at, c.line_id",
    )
    .fetch_all(db.readers())
    .await
    .map_err(StoreError::from_sqlx)?;
    rows.into_iter()
        .map(|(id, name, barcode, price, qty)| {
            Ok(SpikeCartLine {
                product: product_from((id, name, barcode, price))?,
                quantity: Quantity::from_milli(qty),
            })
        })
        .collect()
}

pub async fn receipt_number(db: &LocalDb, sale_id: &str) -> Result<Option<String>, StoreError> {
    sqlx::query_scalar("SELECT receipt_number FROM spike_sales WHERE sale_id = ?")
        .bind(sale_id)
        .fetch_optional(db.readers())
        .await
        .map_err(StoreError::from_sqlx)
}
