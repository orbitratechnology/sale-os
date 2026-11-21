//! E00-T04 acceptance: demo20's contents, determinism, and reset.

use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_devtools::{Profile, STAFF, database_exists, dump, ean13, reset, seed};
use pos_domain::staff::Pin;
use pos_local_db::{DbOptions, LocalDb};
use tempfile::TempDir;

async fn scalar(dir: &std::path::Path, sql: &str) -> i64 {
    let db = LocalDb::open(&DbOptions::in_dir(dir)).await.unwrap();
    let n: i64 = sqlx::query_scalar(sql).fetch_one(db.readers()).await.unwrap();
    db.close().await;
    n
}

#[tokio::test]
async fn demo20_has_twenty_products_with_barcodes_two_rates_and_a_variant_product() {
    let dir = TempDir::new().unwrap();
    let s = seed(dir.path(), Profile::Demo20, 1).await.unwrap();
    assert_eq!((s.products, s.variants, s.barcodes), (20, 23, 23));
    let p = dir.path();
    assert_eq!(
        scalar(p, "SELECT count(*) FROM products WHERE status = 'active'").await,
        20
    );
    assert_eq!(
        scalar(
            p,
            "SELECT count(*) FROM product_variants v WHERE NOT EXISTS
             (SELECT 1 FROM product_barcodes b WHERE b.variant_id = v.variant_id AND b.status = 'active')"
        )
        .await,
        0,
        "every variant has a barcode"
    );
    assert_eq!(scalar(p, "SELECT count(*) FROM tax_rates").await, 2);
    assert_eq!(
        scalar(p, "SELECT count(*) FROM tax_rates WHERE rate_ppm = 180000").await,
        1
    );
    assert_eq!(
        scalar(p, "SELECT count(*) FROM tax_rates WHERE rate_ppm = 0").await,
        1
    );
    // One product sold in sizes and colours.
    assert_eq!(
        scalar(
            p,
            "SELECT count(*) FROM (SELECT product_id FROM product_variants GROUP BY product_id HAVING count(*) > 1)"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            p,
            "SELECT count(*) FROM product_variants
             WHERE json_extract(options_json, '$.Size') IS NOT NULL AND json_extract(options_json, '$.Colour') IS NOT NULL"
        )
        .await,
        4
    );
    // Written through the services: each product has its audit row and outbox event.
    assert_eq!(
        scalar(
            p,
            "SELECT count(*) FROM audit_events WHERE action = 'catalog.product_created'"
        )
        .await,
        20
    );
    assert_eq!(
        scalar(
            p,
            "SELECT count(*) FROM outbox_events WHERE event_type = 'product.upserted'"
        )
        .await,
        20
    );
    // The printed PINs work with the app's own hasher.
    let db = LocalDb::open(&DbOptions::in_dir(p)).await.unwrap();
    for (name, _, pin) in STAFF {
        let phc: String = sqlx::query_scalar("SELECT pin_hash FROM staff_users WHERE display_name = ?")
            .bind(name)
            .fetch_one(db.readers())
            .await
            .unwrap();
        assert!(
            Argon2PinHasher::new()
                .verify(&Pin::parse(pin).unwrap(), &phc)
                .unwrap(),
            "{name}"
        );
    }
    db.close().await;
}

#[tokio::test]
async fn the_same_seed_gives_the_same_ids_and_data_and_another_seed_does_not() {
    let (a, b, c) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    seed(a.path(), Profile::Demo20, 7).await.unwrap();
    seed(b.path(), Profile::Demo20, 7).await.unwrap();
    seed(c.path(), Profile::Demo20, 8).await.unwrap();
    let (da, db, dc) = (
        dump(a.path()).await.unwrap(),
        dump(b.path()).await.unwrap(),
        dump(c.path()).await.unwrap(),
    );
    assert!(da.len() > 100, "{} rows", da.len());
    if let Some((x, y)) = da.iter().zip(&db).find(|(x, y)| x != y) {
        panic!("seed 7 twice differs:\n{x}\n{y}");
    }
    assert_eq!(da.len(), db.len());
    assert_ne!(da, dc, "another seed gives other IDs");
}

#[tokio::test]
async fn seeding_refuses_an_existing_database_and_reset_empties_it() {
    let dir = TempDir::new().unwrap();
    seed(dir.path(), Profile::Demo20, 1).await.unwrap();
    let e = seed(dir.path(), Profile::Demo20, 1).await.unwrap_err();
    assert!(e.to_string().contains("reset it first"), "{e}");
    reset(dir.path()).await.unwrap();
    assert!(database_exists(dir.path()));
    // Migrated and empty: the app will open the setup wizard.
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM products").await, 0);
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM businesses").await, 0);
    assert!(scalar(dir.path(), "SELECT count(*) FROM _sqlx_migrations").await >= 22);
}

#[test]
fn generated_barcodes_are_valid_ean13() {
    for n in [1, 42, 999_999_999] {
        let code = ean13(479, n);
        assert_eq!(code.len(), 13);
        assert!(
            pos_domain::catalog::normalize_barcode(&code, None).is_ok(),
            "{code}"
        );
    }
}
