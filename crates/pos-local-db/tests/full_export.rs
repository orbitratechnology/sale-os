//! E19-T04: the full data export. Every table is in it with every row, as the manifest says;
//! PIN hashes are not; the ZIP is whole; and `scripts/read-export.py` reads it back.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_domain::Money;
use pos_domain::cart::AddVia;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_local_db::full_export;
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

/// The entries of a ZIP whose local headers carry their sizes (as this export writes them).
fn unzip(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]) as usize;
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
    let mut out = BTreeMap::new();
    let mut i = 0;
    while u32_at(i) == 0x0403_4b50 {
        let (crc, compressed, size) = (u32_at(i + 14), u32_at(i + 18) as usize, u32_at(i + 22) as usize);
        let (name_len, extra_len) = (u16_at(i + 26), u16_at(i + 28));
        let name = String::from_utf8(bytes[i + 30..i + 30 + name_len].to_vec()).unwrap();
        let start = i + 30 + name_len + extra_len;
        let mut data = Vec::new();
        flate2::read::DeflateDecoder::new(&bytes[start..start + compressed])
            .read_to_end(&mut data)
            .unwrap();
        assert_eq!(data.len(), size, "{name}: size");
        assert_eq!(crc32fast::hash(&data), crc, "{name}: crc");
        out.insert(name, data);
        i = start + compressed;
    }
    assert_eq!(
        u32_at(i),
        0x0201_4b50,
        "the central directory follows the entries"
    );
    out
}

#[tokio::test]
async fn every_table_every_row_and_no_pin_hashes() {
    let shop = common::shop().await;
    let a = Arc::new(shop.store.clone());
    let tea = CatalogService {
        store: a.clone(),
        catalog: a.clone(),
    }
    .create(
        &shop.owner,
        ProductInput {
            // A name a spreadsheet would run, with a comma and quotes.
            name: "=Tea, \"English\"".into(),
            description: None,
            category_id: None,
            product_type: ProductType::Stock,
            unit: Unit::Ea,
            tax_category_id: shop.tax_category,
            quick_select: false,
            variants: vec![VariantInput::simple(Money::from_minor(250))],
        },
    )
    .await
    .unwrap()
    .variants[0]
        .id;
    let carts = Arc::new(CartService::new(a.clone(), a.clone(), a.clone()));
    for _ in 0..3 {
        carts
            .add_variant(&shop.owner, tea, None, AddVia::Search)
            .await
            .unwrap();
        let view = carts.current(&shop.owner).await.unwrap();
        CheckoutService {
            carts: carts.clone(),
            store: a.clone(),
            setup: a.clone(),
            stock: a.clone(),
            receipts: a.clone(),
        }
        .complete_sale(
            &shop.owner,
            CompleteSale {
                cart_id: view.cart.id,
                revision: view.cart.revision,
                tenders: vec![TenderInput {
                    kind: TenderKind::Cash,
                    amount: view.priced.total,
                    external_reference: None,
                }],
            },
        )
        .await
        .unwrap();
    }

    let db = shop.store.db();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("export.zip");
    let manifest = full_export::write(db, &path, "0.1.0-test", 1_800_000_000_000)
        .await
        .unwrap();
    assert!(!path.with_extension("zip.partial").exists());

    // The manifest names every table there is, with its rows.
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
           AND name NOT LIKE '_sqlx%' AND name NOT LIKE 'product_search%' ORDER BY name",
    )
    .fetch_all(db.readers())
    .await
    .unwrap();
    let exported: Vec<String> = manifest.tables.iter().map(|t| t.name.clone()).collect();
    assert_eq!(exported, names);
    assert!(manifest.excluded_tables.iter().any(|t| t == "product_search"));
    for t in &manifest.tables {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM \"{}\"", t.name))
            .fetch_one(db.readers())
            .await
            .unwrap();
        assert_eq!(t.rows, u64::try_from(n).unwrap(), "{}", t.name);
    }
    let sales = manifest.tables.iter().find(|t| t.name == "sales").unwrap();
    assert_eq!(sales.rows, 3);
    let staff = manifest.tables.iter().find(|t| t.name == "staff_users").unwrap();
    assert!(staff.columns.iter().all(|c| c.name != "pin_hash"));
    assert_eq!(manifest.excluded_columns, ["staff_users.pin_hash"]);

    // The ZIP is whole; no PIN hash is anywhere in it.
    let files = unzip(&std::fs::read(&path).unwrap());
    assert_eq!(files.len(), manifest.tables.len() + 1);
    let pin_hashes: Vec<String> = sqlx::query_scalar("SELECT pin_hash FROM staff_users")
        .fetch_all(db.readers())
        .await
        .unwrap();
    assert!(!pin_hashes.is_empty());
    for (name, data) in &files {
        let text = String::from_utf8(data.clone()).unwrap();
        for h in &pin_hashes {
            assert!(!text.contains(h.as_str()), "{name} holds a PIN hash");
        }
        assert!(!text.contains("$argon2"), "{name} holds a PHC string");
    }
    let products = String::from_utf8(files["tables/products.csv"].clone()).unwrap();
    assert!(
        products.contains("\"'=Tea, \"\"English\"\"\""),
        "quoted and defused: {products}"
    );

    // The documented reader agrees (where Python is installed).
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/read-export.py");
    let python = ["python3", "python"].into_iter().find(|p| {
        std::process::Command::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    });
    let Some(python) = python else {
        eprintln!("skipping the reader check: no Python");
        return;
    };
    let check = std::process::Command::new(python)
        .args(["-I", script, "check"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = dir.path().join("back.db");
    let load = std::process::Command::new(python)
        .args(["-I", script, "sqlite"])
        .arg(&path)
        .arg(&out)
        .output()
        .unwrap();
    assert!(load.status.success(), "{}", String::from_utf8_lossy(&load.stderr));
    // Read back, values are as they were: NULLs, numbers, text with the guard removed.
    use sqlx::{ConnectOptions, Connection};
    let mut back = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&out)
        .connect()
        .await
        .unwrap();
    let name: String = sqlx::query_scalar("SELECT name FROM products")
        .fetch_one(&mut back)
        .await
        .unwrap();
    assert_eq!(name, "=Tea, \"English\"");
    let original: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT receipt_number, total_minor, approver_user_id FROM sales ORDER BY receipt_seq",
    )
    .fetch_all(db.readers())
    .await
    .unwrap();
    let again: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT receipt_number, total_minor, approver_user_id FROM sales ORDER BY receipt_seq",
    )
    .fetch_all(&mut back)
    .await
    .unwrap();
    assert_eq!(again, original);
    back.close().await.unwrap();
}
