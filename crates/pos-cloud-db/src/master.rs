//! Master-data storage (E17-T06, ADR-011): the current version and snapshot of each
//! aggregate, the writes that move it to the next version, and the owners of barcodes and
//! SKUs. The applier checks versions under [`lock`], so edits of one business's master data
//! apply one at a time across all its registers and the cloud.

use crate::tenant::TenantTx;
use chrono::{DateTime, NaiveDate, Utc};
use pos_sync::events::{
    BusinessUpdatedV1, CategoryUpsertedV1, LocationUpsertedV1, ProductUpsertedV1, RegisterUpsertedV1,
    SettingUpdatedV1, StaffUpsertedV1, TaxCategoryUpsertedV1, TaxRateUpsertedV1,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// The master aggregates and where they live: (table, id column).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Business,
    Location,
    Register,
    Setting,
    Staff,
    TaxRate,
    TaxCategory,
    Category,
    Product,
}

impl Kind {
    fn table(self) -> (&'static str, &'static str) {
        match self {
            Kind::Business => ("businesses", "business_id"),
            Kind::Location => ("locations", "location_id"),
            Kind::Register => ("registers", "register_id"),
            Kind::Setting => ("business_settings", "key"),
            Kind::Staff => ("staff_users", "user_id"),
            Kind::TaxRate => ("tax_rates", "tax_rate_id"),
            Kind::TaxCategory => ("tax_categories", "tax_category_id"),
            Kind::Category => ("categories", "category_id"),
            Kind::Product => ("products", "product_id"),
        }
    }
}

fn biz(tx: &TenantTx<'_>) -> Uuid {
    *tx.business_id().as_uuid()
}

fn tag<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn status(archived: bool) -> &'static str {
    if archived { "archived" } else { "active" }
}

fn version(v: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(v).map_err(|_| sqlx::Error::Protocol("version out of range".into()))
}

fn at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_else(Utc::now)
}

fn day(s: &Option<String>) -> Option<NaiveDate> {
    s.as_deref()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
}

/// Serializes master-data edits of the transaction's business until it ends.
pub async fn lock(tx: &mut TenantTx<'_>) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('master:' || $1::text, 0))")
        .bind(b)
        .execute(tx.conn())
        .await
        .map(|_| ())
}

/// The aggregate's current version and snapshot (`None` when the cloud has never seen it).
/// A row the link flow created has version 0 and no snapshot.
pub async fn current(
    tx: &mut TenantTx<'_>,
    kind: Kind,
    id: &str,
) -> Result<Option<(i64, Value)>, sqlx::Error> {
    let b = biz(tx);
    let (table, column) = kind.table();
    let row: Option<(i64, Option<Value>)> = sqlx::query_as(&format!(
        "SELECT version, snapshot FROM {table} WHERE business_id = $1 AND {column}::text = $2"
    ))
    .bind(b)
    .bind(id)
    .fetch_optional(tx.conn())
    .await?;
    Ok(row.map(|(v, s)| (v, s.unwrap_or(Value::Null))))
}

/// The active product (other than `except`) that holds a barcode, if any.
pub async fn barcode_owner(
    tx: &mut TenantTx<'_>,
    value: &str,
    except: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let b = biz(tx);
    sqlx::query_scalar(
        "SELECT product_id FROM product_barcodes
          WHERE business_id = $1 AND value_normalized = $2 AND status = 'active' AND product_id <> $3",
    )
    .bind(b)
    .bind(value)
    .bind(except)
    .fetch_optional(tx.conn())
    .await
}

/// The product (other than `except`) with an active variant using this SKU, if any.
pub async fn sku_owner(tx: &mut TenantTx<'_>, sku: &str, except: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    let b = biz(tx);
    sqlx::query_scalar(
        "SELECT product_id FROM product_variants
          WHERE business_id = $1 AND lower(sku) = lower($2) AND status = 'active' AND product_id <> $3",
    )
    .bind(b)
    .bind(sku)
    .bind(except)
    .fetch_optional(tx.conn())
    .await
}

pub async fn update_business(
    tx: &mut TenantTx<'_>,
    p: &BusinessUpdatedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "UPDATE businesses SET name = $2, country_code = $3, timezone = $4, currency_code = $5, currency_exponent = $6,
                prices_include_tax = $7, profile = $8, snapshot = $8, version = $9, updated_at = $10
          WHERE business_id = $1",
    )
    .bind(b)
    .bind(&p.name)
    .bind(&p.country_code)
    .bind(&p.timezone)
    .bind(&p.currency.code)
    .bind(i16::from(p.currency.exponent))
    .bind(p.prices_include_tax)
    .bind(snapshot)
    .bind(version(p.version)?)
    .bind(at(p.updated_at))
    .execute(tx.conn())
    .await
    .map(|_| ())
}

pub async fn upsert_location(
    tx: &mut TenantTx<'_>,
    p: &LocationUpsertedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO locations (business_id, location_id, name, address, status, version, updated_at, snapshot)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (business_id, location_id) DO UPDATE SET name = EXCLUDED.name, address = EXCLUDED.address,
           status = EXCLUDED.status, version = EXCLUDED.version, updated_at = EXCLUDED.updated_at,
           snapshot = EXCLUDED.snapshot",
    )
    .bind(b)
    .bind(p.location_id)
    .bind(&p.name)
    .bind(&p.address)
    .bind(status(p.archived))
    .bind(version(p.version)?)
    .bind(at(p.updated_at))
    .bind(snapshot)
    .execute(tx.conn())
    .await
    .map(|_| ())
}

pub async fn update_register(
    tx: &mut TenantTx<'_>,
    p: &RegisterUpsertedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "UPDATE registers SET location_id = $3, name = $4, code = $5, receipt_prefix = $6, status = $7, version = $8,
                updated_at = $9, snapshot = $10
          WHERE business_id = $1 AND register_id = $2",
    )
    .bind(b)
    .bind(p.register_id)
    .bind(p.location_id)
    .bind(&p.name)
    .bind(&p.code)
    .bind(&p.receipt_prefix)
    .bind(if p.retired { "retired" } else { "active" })
    .bind(version(p.version)?)
    .bind(at(p.updated_at))
    .bind(snapshot)
    .execute(tx.conn())
    .await
    .map(|_| ())
}

pub async fn upsert_setting(
    tx: &mut TenantTx<'_>,
    p: &SettingUpdatedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO business_settings (business_id, key, value, version, snapshot, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (business_id, key) DO UPDATE SET value = EXCLUDED.value, version = EXCLUDED.version,
           snapshot = EXCLUDED.snapshot, updated_at = EXCLUDED.updated_at",
    )
    .bind(b)
    .bind(&p.key)
    .bind(&p.value)
    .bind(version(p.version)?)
    .bind(snapshot)
    .bind(at(p.updated_at))
    .execute(tx.conn())
    .await
    .map(|_| ())
}

pub async fn upsert_staff(
    tx: &mut TenantTx<'_>,
    p: &StaffUpsertedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO staff_users (business_id, user_id, display_name, role, pin_hash, status, version, snapshot, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (business_id, user_id) DO UPDATE SET display_name = EXCLUDED.display_name, role = EXCLUDED.role,
           pin_hash = EXCLUDED.pin_hash, status = EXCLUDED.status, version = EXCLUDED.version,
           snapshot = EXCLUDED.snapshot, updated_at = EXCLUDED.updated_at",
    )
    .bind(b)
    .bind(p.user_id)
    .bind(&p.display_name)
    .bind(tag(&p.role))
    .bind(&p.pin_hash)
    .bind(if p.active { "active" } else { "inactive" })
    .bind(version(p.version)?)
    .bind(snapshot)
    .bind(at(p.updated_at))
    .execute(tx.conn())
    .await
    .map(|_| ())
}

pub async fn upsert_tax_rate(
    tx: &mut TenantTx<'_>,
    p: &TaxRateUpsertedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO tax_rates (business_id, tax_rate_id, name, rate_ppm, effective_from, effective_to, status, version,
                                snapshot, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (business_id, tax_rate_id) DO UPDATE SET name = EXCLUDED.name, rate_ppm = EXCLUDED.rate_ppm,
           effective_from = EXCLUDED.effective_from, effective_to = EXCLUDED.effective_to, status = EXCLUDED.status,
           version = EXCLUDED.version, snapshot = EXCLUDED.snapshot, updated_at = EXCLUDED.updated_at",
    )
    .bind(b)
    .bind(p.tax_rate_id)
    .bind(&p.name)
    .bind(i32::try_from(p.rate).map_err(|_| sqlx::Error::Protocol("rate out of range".into()))?)
    .bind(day(&p.effective_from))
    .bind(day(&p.effective_to))
    .bind(status(p.archived))
    .bind(version(p.version)?)
    .bind(snapshot)
    .bind(at(p.updated_at))
    .execute(tx.conn())
    .await
    .map(|_| ())
}

pub async fn upsert_tax_category(
    tx: &mut TenantTx<'_>,
    p: &TaxCategoryUpsertedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO tax_categories (business_id, tax_category_id, name, is_default, rate_ids, status, version, snapshot,
                                     updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (business_id, tax_category_id) DO UPDATE SET name = EXCLUDED.name, is_default = EXCLUDED.is_default,
           rate_ids = EXCLUDED.rate_ids, status = EXCLUDED.status, version = EXCLUDED.version,
           snapshot = EXCLUDED.snapshot, updated_at = EXCLUDED.updated_at",
    )
    .bind(b)
    .bind(p.tax_category_id)
    .bind(&p.name)
    .bind(p.is_default)
    .bind(&p.rate_ids)
    .bind(status(p.archived))
    .bind(version(p.version)?)
    .bind(snapshot)
    .bind(at(p.updated_at))
    .execute(tx.conn())
    .await
    .map(|_| ())
}

pub async fn upsert_category(
    tx: &mut TenantTx<'_>,
    p: &CategoryUpsertedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO categories (business_id, category_id, parent_id, name, sort_order, status, version, snapshot, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (business_id, category_id) DO UPDATE SET parent_id = EXCLUDED.parent_id, name = EXCLUDED.name,
           sort_order = EXCLUDED.sort_order, status = EXCLUDED.status, version = EXCLUDED.version,
           snapshot = EXCLUDED.snapshot, updated_at = EXCLUDED.updated_at",
    )
    .bind(b)
    .bind(p.category_id)
    .bind(p.parent_id)
    .bind(&p.name)
    .bind(p.sort_order)
    .bind(status(p.archived))
    .bind(version(p.version)?)
    .bind(snapshot)
    .bind(at(p.updated_at))
    .execute(tx.conn())
    .await
    .map(|_| ())
}

/// The whole product: its row, its variants (upserted by id) and its barcodes (replaced).
pub async fn upsert_product(
    tx: &mut TenantTx<'_>,
    p: &ProductUpsertedV1,
    snapshot: &Value,
) -> Result<(), sqlx::Error> {
    let b = biz(tx);
    sqlx::query(
        "INSERT INTO products (business_id, product_id, name, description, category_id, product_type, unit_code,
                               tax_category_id, quick_select, status, version, snapshot, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now())
         ON CONFLICT (business_id, product_id) DO UPDATE SET name = EXCLUDED.name, description = EXCLUDED.description,
           category_id = EXCLUDED.category_id, product_type = EXCLUDED.product_type, unit_code = EXCLUDED.unit_code,
           tax_category_id = EXCLUDED.tax_category_id, quick_select = EXCLUDED.quick_select, status = EXCLUDED.status,
           version = EXCLUDED.version, snapshot = EXCLUDED.snapshot, updated_at = now()",
    )
    .bind(b)
    .bind(p.id)
    .bind(&p.name)
    .bind(&p.description)
    .bind(p.category_id)
    .bind(tag(&p.product_type))
    .bind(tag(&p.unit))
    .bind(p.tax_category_id)
    .bind(p.quick_select)
    .bind(status(p.archived))
    .bind(version(p.version)?)
    .bind(snapshot)
    .execute(tx.conn())
    .await?;
    // Archive first, so a SKU or barcode moving between this product's variants never
    // collides with itself.
    sqlx::query("UPDATE product_variants SET status = 'archived' WHERE business_id = $1 AND product_id = $2")
        .bind(b)
        .bind(p.id)
        .execute(tx.conn())
        .await?;
    sqlx::query("DELETE FROM product_barcodes WHERE business_id = $1 AND product_id = $2")
        .bind(b)
        .bind(p.id)
        .execute(tx.conn())
        .await?;
    for v in &p.variants {
        let live = !(p.archived || v.archived);
        sqlx::query(
            "INSERT INTO product_variants (business_id, variant_id, product_id, is_default, sku, options, price_minor,
                                           cost_minor, track_stock, low_stock_threshold_milli, scan_qty_milli, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (business_id, variant_id) DO UPDATE SET is_default = EXCLUDED.is_default, sku = EXCLUDED.sku,
               options = EXCLUDED.options, price_minor = EXCLUDED.price_minor, cost_minor = EXCLUDED.cost_minor,
               track_stock = EXCLUDED.track_stock, low_stock_threshold_milli = EXCLUDED.low_stock_threshold_milli,
               scan_qty_milli = EXCLUDED.scan_qty_milli, status = EXCLUDED.status",
        )
        .bind(b)
        .bind(v.id)
        .bind(p.id)
        .bind(v.is_default)
        .bind(&v.sku)
        .bind(serde_json::to_value(&v.options).map_err(|e| sqlx::Error::Encode(Box::new(e)))?)
        .bind(v.price)
        .bind(v.cost)
        .bind(v.track_stock)
        .bind(v.low_stock_threshold)
        .bind(v.scan_qty)
        .bind(status(!live))
        .execute(tx.conn())
        .await?;
        for c in &v.barcodes {
            sqlx::query(
                "INSERT INTO product_barcodes (business_id, barcode_id, variant_id, product_id, value_normalized,
                                               value_raw, symbology, qualifier, pack_qty_milli, status)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(b)
            .bind(c.id)
            .bind(v.id)
            .bind(p.id)
            .bind(&c.value)
            .bind(&c.raw)
            .bind(tag(&c.symbology))
            .bind(tag(&c.qualifier))
            .bind(c.pack_qty)
            .bind(status(!live))
            .execute(tx.conn())
            .await?;
        }
    }
    Ok(())
}
