//! E06-T01: the catalogue schema enforces its invariants at the database level too.

use pos_local_db::{DbOptions, LocalDb};
use sqlx::SqlitePool;
use tempfile::TempDir;

async fn db() -> (TempDir, LocalDb) {
    let dir = TempDir::new().unwrap();
    let db = LocalDb::open(&DbOptions::in_dir(dir.path())).await.unwrap();
    let w = db.writer();
    for sql in [
        "INSERT INTO businesses (business_id, name, country_code, timezone, currency_code, currency_exponent,
           prices_include_tax, created_at, updated_at) VALUES ('b', 'Shop', 'GB', 'Europe/London', 'GBP', 2, 1, 0, 0)",
        "INSERT INTO tax_categories (tax_category_id, business_id, name, is_default, created_at, updated_at)
           VALUES ('tc', 'b', 'Standard', 1, 0, 0)",
        "INSERT INTO products (product_id, business_id, name, product_type, unit_code, tax_category_id, created_at, updated_at)
           VALUES ('p1', 'b', 'Cola', 'stock', 'ea', 'tc', 0, 0), ('p2', 'b', 'Crisps', 'stock', 'ea', 'tc', 0, 0)",
    ] {
        sqlx::query(sql).execute(w).await.unwrap();
    }
    (dir, db)
}

async fn variant(
    w: &SqlitePool,
    id: &str,
    product: &str,
    sku: Option<&str>,
    default: bool,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO product_variants (variant_id, business_id, product_id, is_default, sku, price_minor, track_stock,
           status, created_at, updated_at) VALUES (?, 'b', ?, ?, ?, 100, 1, ?, 0, 0)",
    )
    .bind(id)
    .bind(product)
    .bind(default)
    .bind(sku)
    .bind(status)
    .execute(w)
    .await
    .map(|_| ())
}

async fn barcode(
    w: &SqlitePool,
    id: &str,
    variant: &str,
    value: &str,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO product_barcodes (barcode_id, business_id, variant_id, value_normalized, value_raw, status, created_at)
         VALUES (?, 'b', ?, ?, ?, ?, 0)",
    )
    .bind(id)
    .bind(variant)
    .bind(value)
    .bind(value)
    .bind(status)
    .execute(w)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn units_are_seeded_with_fraction_rules() {
    let (_d, db) = db().await;
    let rows: Vec<(String, bool)> =
        sqlx::query_as("SELECT unit_code, allows_fraction FROM units ORDER BY unit_code")
            .fetch_all(db.readers())
            .await
            .unwrap();
    assert!(rows.contains(&("ea".into(), false)) && rows.contains(&("kg".into(), true)));
    assert_eq!(rows.len(), 9);
}

#[tokio::test]
async fn barcodes_are_unique_among_active_rows_only() {
    let (_d, db) = db().await;
    let w = db.writer();
    variant(w, "v1", "p1", Some("COLA-1"), true, "active")
        .await
        .unwrap();
    variant(w, "v2", "p2", Some("CRISP-1"), true, "active")
        .await
        .unwrap();
    barcode(w, "b1", "v1", "5012345678900", "active").await.unwrap();
    // CAT-004: the same value on another active variant is rejected...
    assert!(barcode(w, "b2", "v2", "5012345678900", "active").await.is_err());
    // ...but several aliases on one variant are fine (CAT-003),
    barcode(w, "b3", "v1", "0012345678905", "active").await.unwrap();
    // and an archived owner's barcode can be reused.
    sqlx::query("UPDATE product_barcodes SET status = 'archived' WHERE barcode_id = 'b1'")
        .execute(w)
        .await
        .unwrap();
    barcode(w, "b4", "v2", "5012345678900", "active").await.unwrap();
}

#[tokio::test]
async fn skus_are_unique_case_insensitively_among_active_variants() {
    let (_d, db) = db().await;
    let w = db.writer();
    variant(w, "v1", "p1", Some("COLA-1"), true, "active")
        .await
        .unwrap();
    assert!(
        variant(w, "v2", "p2", Some("cola-1"), true, "active")
            .await
            .is_err()
    );
    variant(w, "v3", "p2", Some("cola-1"), true, "archived")
        .await
        .unwrap();
    // Variants without SKUs never collide.
    variant(w, "v4", "p2", None, false, "active").await.unwrap();
    variant(w, "v5", "p2", None, false, "active").await.unwrap();
}

#[tokio::test]
async fn one_default_variant_per_product() {
    let (_d, db) = db().await;
    let w = db.writer();
    variant(w, "v1", "p1", None, true, "active").await.unwrap();
    assert!(variant(w, "v2", "p1", None, true, "active").await.is_err());
    variant(w, "v3", "p1", None, false, "active").await.unwrap();
}

#[tokio::test]
async fn one_default_tax_category_and_valid_rates() {
    let (_d, db) = db().await;
    let w = db.writer();
    let second_default = sqlx::query(
        "INSERT INTO tax_categories (tax_category_id, business_id, name, is_default, created_at, updated_at)
         VALUES ('tc2', 'b', 'Reduced', 1, 0, 0)",
    )
    .execute(w)
    .await;
    assert!(second_default.is_err());
    let bad_rate = sqlx::query(
        "INSERT INTO tax_rates (tax_rate_id, business_id, name, rate_ppm, created_at, updated_at)
         VALUES ('r', 'b', 'Too much', 1000001, 0, 0)",
    )
    .execute(w)
    .await;
    assert!(bad_rate.is_err());
}

#[tokio::test]
async fn product_type_and_unit_are_constrained() {
    let (_d, db) = db().await;
    let w = db.writer();
    let bad_type = sqlx::query(
        "INSERT INTO products (product_id, business_id, name, product_type, unit_code, tax_category_id, created_at, updated_at)
         VALUES ('p3', 'b', 'X', 'gadget', 'ea', 'tc', 0, 0)",
    )
    .execute(w)
    .await;
    assert!(bad_type.is_err());
    let bad_unit = sqlx::query(
        "INSERT INTO products (product_id, business_id, name, product_type, unit_code, tax_category_id, created_at, updated_at)
         VALUES ('p4', 'b', 'X', 'stock', 'furlong', 'tc', 0, 0)",
    )
    .execute(w)
    .await;
    assert!(bad_unit.is_err(), "unit_code must reference units");
}

#[tokio::test]
async fn fts_index_supports_prefix_and_diacritic_insensitive_search() {
    let (_d, db) = db().await;
    let w = db.writer();
    sqlx::query("INSERT INTO product_search (variant_id, name, label, sku, barcodes, category) VALUES ('v1', 'Café Latte 330ml', '', 'LAT-1', '5012345678900', 'Drinks')")
        .execute(w)
        .await
        .unwrap();
    for q in ["cafe", "caf*", "latte", "lat*", "drinks"] {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM product_search WHERE product_search MATCH ?")
            .bind(q)
            .fetch_one(db.readers())
            .await
            .unwrap();
        assert_eq!(n, 1, "{q}");
    }
}
