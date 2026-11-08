//! E06-T07: the products CSV import against a real database.

mod common;

use pos_application::catalog::CatalogService;
use pos_application::context::AppError;
use pos_application::export::{Dataset, ExportService};
use pos_application::import::{CHUNK, ImportService, OnDuplicate};
use pos_application::queries::CatalogQueries;
use pos_application::records::ImportRowStatus;
use pos_application::write::{AppliedWriteSet, LocalStore, WriteError, WriteSet};
use pos_domain::catalog::import::{Column, ErrorKind, Mapping};
use pos_domain::catalog::{BarcodeInput, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::{ErrorCode, HasErrorCode, Money, Role};
use pos_local_db::SqliteStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn importer(store: Arc<dyn LocalStore>, s: &SqliteStore) -> ImportService {
    ImportService {
        store,
        imports: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
        config: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
    }
}

fn service(shop: &common::Shop) -> ImportService {
    importer(Arc::new(shop.store.clone()), &shop.store)
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/catalogue")
            .join(name),
    )
    .unwrap()
}

async fn count(shop: &common::Shop, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap()
}

async fn active_products(shop: &common::Shop) -> i64 {
    count(shop, "SELECT count(*) FROM products WHERE status = 'active'").await
}

#[tokio::test]
async fn at01_demo20_creates_twenty_products() {
    let shop = common::shop().await;
    let imp = service(&shop);
    let owner = &shop.owner;
    let preview = imp
        .start(owner, "demo20.csv", &fixture("demo20.csv"))
        .await
        .unwrap();
    assert_eq!(preview.total_rows, 22);
    assert_eq!(
        preview.mapping.len(),
        10,
        "every column of the file is recognised"
    );
    // Nothing in the catalogue yet.
    assert_eq!(active_products(&shop).await, 0);
    let summary = imp
        .validate(owner, preview.job_id, preview.mapping.clone(), OnDuplicate::Error)
        .await
        .unwrap();
    assert_eq!(summary.problems, vec![]);
    assert_eq!((summary.valid_rows, summary.products_to_create), (22, 20));
    let done = imp.commit(owner, preview.job_id).await.unwrap();
    assert_eq!(
        (done.products_created, done.rows_committed, done.chunks),
        (20, 22, 1)
    );
    assert_eq!(active_products(&shop).await, 20);
    // The T-shirt has three variants; the bananas are sold by weight with a 2.5 kg alert.
    let shirt: String = sqlx::query_scalar("SELECT product_id FROM products WHERE name = 'T-shirt'")
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap();
    let shirt = shop.store.product(shirt.parse().unwrap()).await.unwrap().unwrap();
    assert_eq!(
        shirt.variants.iter().map(|v| v.label()).collect::<Vec<_>>(),
        vec!["S", "M", "L"]
    );
    let (unit, low): (String, Option<i64>) = sqlx::query_as(
        "SELECT p.unit_code, v.low_stock_threshold_milli FROM products p JOIN product_variants v USING (product_id)
         WHERE p.name = 'Bananas'",
    )
    .fetch_one(shop.store.db().readers())
    .await
    .unwrap();
    assert_eq!((unit.as_str(), low), ("kg", Some(2500)));
    // Every product has its outbox event; one audit row for the one chunk.
    assert_eq!(
        count(
            &shop,
            "SELECT count(*) FROM outbox_events WHERE event_type = 'product.upserted'"
        )
        .await,
        20
    );
    assert_eq!(
        count(
            &shop,
            "SELECT count(*) FROM audit_events WHERE action = 'catalog.import_committed'"
        )
        .await,
        1
    );
    // A committed import cannot be committed or validated again.
    assert!(imp.commit(owner, preview.job_id).await.is_err());
    assert!(
        imp.validate(owner, preview.job_id, preview.mapping, OnDuplicate::Skip)
            .await
            .is_err()
    );
}

async fn crisps(shop: &common::Shop) -> pos_domain::catalog::Product {
    let catalog = CatalogService {
        store: Arc::new(shop.store.clone()),
        catalog: Arc::new(shop.store.clone()),
    };
    let mut v = VariantInput::simple(Money::from_minor(85));
    v.sku = Some("CRISPS".into());
    v.barcodes = vec![BarcodeInput::alias("5000159407236")];
    let mut extra = VariantInput::simple(Money::from_minor(150));
    extra.sku = Some("CRISPS-BIG".into());
    extra.options.insert("Size".into(), "Big".into());
    v.options.insert("Size".into(), "Small".into());
    catalog
        .create(
            &shop.owner,
            ProductInput {
                name: "Crisps".into(),
                description: Some("Kept".into()),
                category_id: None,
                product_type: ProductType::Stock,
                unit: Unit::Ea,
                tax_category_id: shop.tax_category,
                quick_select: true,
                variants: vec![v, extra],
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn every_problem_of_prd_16_3_is_reported_on_its_row() {
    let shop = common::shop().await;
    crisps(&shop).await;
    let imp = service(&shop);
    let owner = &shop.owner;
    let p = imp
        .start(owner, "errors.csv", &fixture("errors.csv"))
        .await
        .unwrap();
    let s = imp
        .validate(owner, p.job_id, p.mapping, OnDuplicate::Error)
        .await
        .unwrap();
    let at = |line: u32| {
        s.problems.iter().find(|x| x.line == line).map(|x| {
            (
                x.status,
                x.errors.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
            )
        })
    };
    assert_eq!(s.valid_rows, 1, "{:#?}", s.problems);
    assert_eq!(at(2), None, "the first row with a barcode is fine");
    assert_eq!(
        at(3),
        Some((
            ImportRowStatus::DuplicateInFile,
            vec![ErrorKind::DuplicateBarcodeInFile {
                value: "4006381333931".into(),
                line: 2
            }]
        ))
    );
    assert_eq!(
        at(4),
        Some((
            ImportRowStatus::DuplicateExisting,
            vec![ErrorKind::ProductExists {
                product: "Crisps".into()
            }]
        ))
    );
    assert_eq!(at(5), Some((ImportRowStatus::Error, vec![ErrorKind::Required])));
    assert_eq!(
        at(6),
        Some((
            ImportRowStatus::Error,
            vec![ErrorKind::UnknownTaxCategory {
                value: "Luxury".into()
            }]
        ))
    );
    assert_eq!(
        at(7),
        Some((
            ImportRowStatus::Error,
            vec![ErrorKind::ColumnCount {
                expected: 7,
                found: 4
            }]
        ))
    );
    assert_eq!(at(8), Some((ImportRowStatus::Error, vec![ErrorKind::Negative])));
    assert_eq!(
        at(9),
        Some((
            ImportRowStatus::Error,
            vec![ErrorKind::FractionNotAllowed { unit: "ea".into() }]
        ))
    );
    assert_eq!(
        at(10),
        Some((ImportRowStatus::Error, vec![ErrorKind::UnterminatedQuote]))
    );
    // The missing price names its column.
    let missing = &s.problems.iter().find(|x| x.line == 5).unwrap().errors[0];
    assert_eq!(missing.column, Some(Column::Price));
    assert_eq!(missing.message(), "price: required");

    // The report: the file's columns, then why; formulas defused.
    let (name, bytes) = imp.error_report(owner, p.job_id).await.unwrap();
    assert_eq!(name, "errors-errors.csv");
    let text = String::from_utf8(bytes).unwrap();
    let lines: Vec<&str> = text
        .trim_start_matches('\u{feff}')
        .split("\r\n")
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines[0],
        "row,name,sku,barcode,price,unit,tax_category,scan_quantity,error"
    );
    assert_eq!(lines.len(), 1 + 8);
    assert!(
        lines.contains(&"5,No Price,NOPRICE,,,ea,Standard,,price: required"),
        "{lines:#?}"
    );
    // A negative number is a value, not a formula: it is written as it was.
    assert!(
        lines.contains(&"8,Minus,MINUS,,-2.00,ea,Standard,,price: cannot be negative"),
        "{lines:#?}"
    );
    assert!(
        lines.contains(
            &"3,Copy Cola,GOOD2,4006381333931,1.20,ea,Standard,,barcode: 4006381333931 is also on row 2"
        ),
        "{lines:#?}"
    );
    assert!(
        lines.contains(
            &"6,Odd Tax,ODDTAX,,1.00,ea,Luxury,,\"tax category: there is none called \"\"Luxury\"\"\""
        ),
        "{lines:#?}"
    );

    // Commit imports only the good row; cancel is refused once committed.
    let done = imp.commit(owner, p.job_id).await.unwrap();
    assert_eq!(done.products_created, 1);
    assert_eq!(active_products(&shop).await, 2);
    assert!(imp.cancel(owner, p.job_id).await.is_err());
}

fn mapping_of(pairs: &[(Column, usize)]) -> Mapping {
    pairs.iter().copied().collect()
}

#[tokio::test]
async fn duplicate_policies_skip_update_or_report() {
    let shop = common::shop().await;
    let existing = crisps(&shop).await;
    let imp = service(&shop);
    let owner = &shop.owner;
    // Same product by name: a new price for "Small" (matched by SKU) and a new "Large" row.
    let file = b"name,variant,sku,price,barcode\ncrisps,Small,CRISPS,0.90,\nCrisps,Large,CRISPS-L,1.75,5012345678900\n";
    let mapping = mapping_of(&[
        (Column::Name, 0),
        (Column::Variant, 1),
        (Column::Sku, 2),
        (Column::Price, 3),
        (Column::Barcode, 4),
    ]);
    for policy in [OnDuplicate::Skip, OnDuplicate::Error] {
        let p = imp.start(owner, "update.csv", file).await.unwrap();
        let s = imp
            .validate(owner, p.job_id, mapping.clone(), policy)
            .await
            .unwrap();
        assert_eq!(s.valid_rows, 0);
        let expected = if policy == OnDuplicate::Skip {
            (2, 0)
        } else {
            (0, 2)
        };
        assert_eq!((s.skipped_rows, s.duplicate_rows), expected);
        imp.commit(owner, p.job_id).await.unwrap();
        assert_eq!(
            shop.store.product(existing.id).await.unwrap().unwrap().version,
            existing.version
        );
    }
    let p = imp.start(owner, "update.csv", file).await.unwrap();
    let s = imp
        .validate(owner, p.job_id, mapping, OnDuplicate::Update)
        .await
        .unwrap();
    assert_eq!(
        (s.valid_rows, s.products_to_update, s.products_to_create),
        (2, 1, 0),
        "{:#?}",
        s.problems
    );
    let done = imp.commit(owner, p.job_id).await.unwrap();
    assert_eq!(done.products_updated, 1);
    let after = shop.store.product(existing.id).await.unwrap().unwrap();
    assert_eq!(after.version, existing.version + 1);
    let price = |sku: &str| {
        after
            .active_variants()
            .find(|v| v.sku.as_deref() == Some(sku))
            .map(|v| v.price.minor())
    };
    // Small repriced (its barcode kept), Large added, Big (not in the file) kept as it was.
    assert_eq!(
        (price("CRISPS"), price("CRISPS-L"), price("CRISPS-BIG")),
        (Some(90), Some(175), Some(150))
    );
    let small = after
        .active_variants()
        .find(|v| v.sku.as_deref() == Some("CRISPS"))
        .unwrap();
    assert_eq!(small.barcodes.len(), 1);
    // What the file did not have is untouched.
    assert_eq!(
        (after.description.as_deref(), after.quick_select),
        (Some("Kept"), true)
    );
    assert_eq!(active_products(&shop).await, 1);
}

/// Fails the `n`-th apply part-way through its transaction, as a crash would.
struct CrashOnApply {
    inner: SqliteStore,
    crash_on: usize,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LocalStore for CrashOnApply {
    async fn apply(&self, set: WriteSet) -> Result<AppliedWriteSet, WriteError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.crash_on {
            return self
                .inner
                .clone()
                .with_fault_before_statement(200)
                .apply(set)
                .await;
        }
        self.inner.apply(set).await
    }
}

fn many(n: usize) -> Vec<u8> {
    let mut s = String::from("name,sku,price,barcode\n");
    for i in 0..n {
        s.push_str(&format!(
            "Item {i:05},SKU{i:05},{}.{:02},2{i:011}\n",
            1 + i % 50,
            i % 100
        ));
    }
    s.into_bytes()
}

#[tokio::test]
async fn a_crash_mid_commit_keeps_whole_chunks_and_resumes_without_duplicates() {
    let shop = common::shop().await;
    let owner = &shop.owner;
    let n = 2 * CHUNK + 200;
    let normal = service(&shop);
    let p = normal.start(owner, "many.csv", &many(n)).await.unwrap();
    let s = normal
        .validate(owner, p.job_id, p.mapping, OnDuplicate::Error)
        .await
        .unwrap();
    assert_eq!(s.valid_rows as usize, n, "{:#?}", s.problems.first());
    // Applies during commit: 1 = job to committing, 2 = chunk 1, 3 = chunk 2 (crashes).
    let crashing = importer(
        Arc::new(CrashOnApply {
            inner: shop.store.clone(),
            crash_on: 3,
            calls: AtomicUsize::new(0),
        }),
        &shop.store,
    );
    let e = crashing.commit(owner, p.job_id).await.unwrap_err();
    assert!(matches!(e, AppError::Write(_)), "{e:?}");
    assert_eq!(
        active_products(&shop).await,
        CHUNK as i64,
        "only the first chunk, whole"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM import_jobs")
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(status, "committing");
    // Committing again finishes the rest.
    let done = normal.commit(owner, p.job_id).await.unwrap();
    assert_eq!((done.products_created as usize, done.chunks), (n - CHUNK, 2));
    assert_eq!(active_products(&shop).await, n as i64);
    assert_eq!(
        count(&shop, "SELECT count(DISTINCT name) FROM products").await,
        n as i64
    );
    assert_eq!(
        count(
            &shop,
            "SELECT count(*) FROM audit_events WHERE action = 'catalog.import_committed'"
        )
        .await,
        3
    );
    // Every row knows the product it became.
    let unmapped = count(
        &shop,
        "SELECT count(*) FROM import_rows WHERE status != 'committed' OR json_extract(parsed_json, '$.productId') IS NULL",
    )
    .await;
    assert_eq!(unmapped, 0);
}

#[tokio::test]
async fn five_thousand_products_import_in_under_a_minute() {
    let shop = common::shop().await;
    let imp = service(&shop);
    let owner = &shop.owner;
    let started = std::time::Instant::now();
    let p = imp.start(owner, "5k.csv", &many(5_000)).await.unwrap();
    let s = imp
        .validate(owner, p.job_id, p.mapping, OnDuplicate::Error)
        .await
        .unwrap();
    assert_eq!(s.valid_rows, 5_000);
    let done = imp.commit(owner, p.job_id).await.unwrap();
    let took = started.elapsed();
    eprintln!("5,000 products: start + validate + commit in {took:?}");
    assert_eq!((done.products_created, done.chunks), (5_000, 10));
    assert!(took < std::time::Duration::from_secs(60), "{took:?}");
}

#[tokio::test]
async fn the_products_export_imports_back() {
    let a = common::shop().await;
    crisps(&a).await;
    let exports = ExportService {
        store: Arc::new(a.store.clone()),
        exports: Arc::new(a.store.clone()),
        reports: Arc::new(a.store.clone()),
    };
    let csv = exports.build(&a.owner, Dataset::Products, None).await.unwrap();
    let b = common::shop().await;
    let imp = service(&b);
    let p = imp.start(&b.owner, &csv.file_name, &csv.bytes).await.unwrap();
    let s = imp
        .validate(&b.owner, p.job_id, p.mapping, OnDuplicate::Error)
        .await
        .unwrap();
    assert_eq!(s.problems, vec![]);
    imp.commit(&b.owner, p.job_id).await.unwrap();
    let rows = |shop: &common::Shop| {
        let store = shop.store.clone();
        async move {
            let r: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(
                "SELECT p.name, v.label, v.sku, v.price_minor FROM products p JOIN product_variants v USING (product_id)
                 ORDER BY v.sku",
            )
            .fetch_all(store.db().readers())
            .await
            .unwrap();
            r
        }
    };
    assert_eq!(rows(&a).await, rows(&b).await);
    let code: String = sqlx::query_scalar("SELECT value_normalized FROM product_barcodes")
        .fetch_one(b.store.db().readers())
        .await
        .unwrap();
    assert_eq!(code, "5000159407236");
}

#[tokio::test]
async fn only_roles_that_import_may_and_cancel_changes_nothing() {
    let shop = common::shop().await;
    let imp = service(&shop);
    let e = imp
        .start(&shop.as_role(Role::Cashier), "demo20.csv", &fixture("demo20.csv"))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::PermissionDenied);
    let p = imp
        .start(&shop.owner, "demo20.csv", &fixture("demo20.csv"))
        .await
        .unwrap();
    imp.validate(&shop.owner, p.job_id, p.mapping.clone(), OnDuplicate::Error)
        .await
        .unwrap();
    imp.cancel(&shop.owner, p.job_id).await.unwrap();
    assert!(imp.commit(&shop.owner, p.job_id).await.is_err());
    assert_eq!(active_products(&shop).await, 0);
    // A mapping without the price column is refused before anything is checked.
    let p = imp
        .start(&shop.owner, "demo20.csv", &fixture("demo20.csv"))
        .await
        .unwrap();
    let mut m = p.mapping.clone();
    m.remove(&Column::Price);
    let e = imp
        .validate(&shop.owner, p.job_id, m, OnDuplicate::Error)
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    // Not a CSV at all.
    assert!(imp.start(&shop.owner, "empty.csv", b"").await.is_err());
}
