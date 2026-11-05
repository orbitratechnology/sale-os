//! E06-T02 / E06-T04 acceptance: product commands and lookups against a real database.

use pos_application::LocalStore;
use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_application::catalog::CatalogService;
use pos_application::context::{Actor, AppError, CommandContext};
use pos_application::queries::{CatalogQueries, MatchedBy};
use pos_application::setup::{CreateWorkspace, NewTaxRate, SetupService};
use pos_domain::catalog::{
    BarcodeInput, BarcodeQualifier, CatalogError, ProductInput, ProductType, Unit, VariantInput,
};
use pos_domain::staff::Pin;
use pos_domain::{
    Clock, ErrorCode, HasErrorCode, IdGenerator, Money, Quantity, RatePpm, Role, SessionId, TaxCategoryId,
    Timestamp, UserId, UuidV7Generator,
};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_testkit::FixedClock;
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;

struct Env {
    _dir: TempDir,
    store: SqliteStore,
    ctx: CommandContext,
    catalog: CatalogService,
    tax: TaxCategoryId,
}

async fn env() -> Env {
    let dir = TempDir::new().unwrap();
    let store = SqliteStore::new(LocalDb::open(&DbOptions::in_dir(dir.path())).await.unwrap());
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(Timestamp::from_millis(1_767_225_600_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidV7Generator);
    let setup = SetupService {
        store: Arc::new(store.clone()),
        setup: Arc::new(store.clone()),
        hasher,
        clock: clock.clone(),
        ids: ids.clone(),
    };
    let ws = setup
        .create_workspace(CreateWorkspace {
            business_name: "Shop".into(),
            legal_name: None,
            address: serde_json::json!({}),
            phone: None,
            email: None,
            country_code: "GB".into(),
            timezone: "Europe/London".into(),
            currency_code: "GBP".into(),
            prices_include_tax: true,
            location_name: "Main".into(),
            register_name: "Till".into(),
            owner_name: "Olu".into(),
            owner_pin: Pin::parse("1234").unwrap(),
            tax_rates: vec![NewTaxRate {
                name: "VAT 20%".into(),
                rate: RatePpm::new(200_000).unwrap(),
            }],
        })
        .await
        .unwrap();
    let tax: String = sqlx::query_scalar("SELECT tax_category_id FROM tax_categories")
        .fetch_one(store.db().readers())
        .await
        .unwrap();
    let ctx = CommandContext {
        register: ws.register,
        actor: Some(Actor {
            user_id: ws.owner_user_id,
            role: Role::Owner,
            session_id: SessionId::generate(&UuidV7Generator),
        }),
        clock,
        ids,
    };
    Env {
        catalog: CatalogService {
            store: Arc::new(store.clone()),
            catalog: Arc::new(store.clone()),
        },
        _dir: dir,
        store,
        ctx,
        tax: tax.parse().unwrap(),
    }
}

fn product(e: &Env, name: &str, variants: Vec<VariantInput>) -> ProductInput {
    ProductInput {
        name: name.into(),
        description: None,
        category_id: None,
        product_type: ProductType::Stock,
        unit: Unit::Ea,
        tax_category_id: e.tax,
        quick_select: false,
        variants,
    }
}

fn variant(price: i64, sku: Option<&str>, codes: &[&str]) -> VariantInput {
    let mut v = VariantInput::simple(Money::from_minor(price));
    v.sku = sku.map(Into::into);
    v.barcodes = codes.iter().map(|c| BarcodeInput::alias(c)).collect();
    v
}

#[tokio::test]
async fn create_then_find_by_barcode_sku_and_name() {
    let e = env().await;
    let p = e
        .catalog
        .create(
            &e.ctx,
            product(
                &e,
                "Café Latte 330ml",
                vec![variant(249, Some("LAT-330"), &["012345678905"])],
            ),
        )
        .await
        .unwrap();

    // Barcode, however it arrives (BAR-004, E06-T03).
    for raw in ["012345678905", "0012345678905", " 012345678905\r"] {
        let hit = e.store.lookup_code(raw).await.unwrap().expect(raw);
        assert_eq!(
            (hit.product_id, hit.matched_by, hit.price.minor()),
            (p.id, MatchedBy::Barcode, 249)
        );
    }
    // SKU, case-insensitive (AT-02).
    assert_eq!(
        e.store.lookup_code("lat-330").await.unwrap().unwrap().matched_by,
        MatchedBy::Sku
    );
    assert!(e.store.lookup_code("NOPE").await.unwrap().is_none());
    // Name search: words, prefixes, diacritics, SKU prefix, barcode (FTS row written in the same txn).
    for q in ["latte", "caf", "cafe latte", "lat-3", "0012345678905", "330"] {
        let hits = e.store.search(q, 10).await.unwrap();
        assert_eq!(hits.len(), 1, "{q:?}");
        assert_eq!(hits[0].name, "Café Latte 330ml");
    }
    assert!(e.store.search("tea", 10).await.unwrap().is_empty());
    assert!(
        e.store.search("\"; DROP TABLE products; --", 10).await.is_ok(),
        "hostile input is harmless"
    );

    // Synced as one aggregate event and audited.
    let payload: String =
        sqlx::query_scalar("SELECT payload_json FROM outbox_events WHERE event_type = 'product.upserted'")
            .fetch_one(e.store.db().readers())
            .await
            .unwrap();
    let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(v["variants"][0]["barcodes"][0]["value"], "0012345678905");
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'catalog.product_created'")
            .fetch_one(e.store.db().readers())
            .await
            .unwrap();
    assert_eq!(audits, 1);
}

#[tokio::test]
async fn barcodes_and_skus_belong_to_one_active_product() {
    let e = env().await;
    let a = e
        .catalog
        .create(
            &e.ctx,
            product(&e, "Cola", vec![variant(120, Some("COLA"), &["4006381333931"])]),
        )
        .await
        .unwrap();
    let err = e
        .catalog
        .create(
            &e.ctx,
            product(&e, "Fake cola", vec![variant(99, None, &["4006381333931"])]),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        AppError::Catalog(CatalogError::BarcodeTaken {
            value: "4006381333931".into(),
            owner: a.id
        })
    );
    assert_eq!(err.error_code(), ErrorCode::CatalogDuplicateBarcode);
    let err = e
        .catalog
        .create(&e.ctx, product(&e, "Other", vec![variant(99, Some("cola"), &[])]))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::CatalogDuplicateSku);
    // Once the owner is archived, the barcode can be reused, and the archived product disappears from lookups.
    e.catalog.archive(&e.ctx, a.id, 1).await.unwrap();
    assert!(e.store.lookup_code("4006381333931").await.unwrap().is_none());
    assert!(e.store.search("cola", 10).await.unwrap().is_empty());
    let b = e
        .catalog
        .create(
            &e.ctx,
            product(
                &e,
                "New cola",
                vec![variant(130, Some("COLA"), &["4006381333931"])],
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        e.store
            .lookup_code("4006381333931")
            .await
            .unwrap()
            .unwrap()
            .product_id,
        b.id
    );
    // History is kept.
    assert!(e.store.product(a.id).await.unwrap().unwrap().archived);
}

#[tokio::test]
async fn edits_round_trip_and_conflict_when_stale() {
    let e = env().await;
    let mut tee = Vec::new();
    for (i, size) in ["S", "M", "L"].into_iter().enumerate() {
        let mut v = variant(1999, Some(&format!("TEE-{size}")), &[]);
        v.options = BTreeMap::from([("size".into(), size.into())]);
        v.barcodes = vec![BarcodeInput::alias(&format!("TEE{i}"))];
        tee.push(v);
    }
    let p = e
        .catalog
        .create(&e.ctx, product(&e, "T-shirt", tee))
        .await
        .unwrap();
    let loaded = e.store.product(p.id).await.unwrap().unwrap();
    assert_eq!(loaded, p, "the stored aggregate reads back identically");

    // Drop size L, reprice S, add a pack barcode to M.
    let mut s = variant(1799, Some("TEE-S"), &["TEE0"]);
    s.id = Some(p.variants[0].id);
    s.options = BTreeMap::from([("size".into(), "S".into())]);
    let mut m = variant(1999, Some("TEE-M"), &["TEE1"]);
    m.id = Some(p.variants[1].id);
    m.options = BTreeMap::from([("size".into(), "M".into())]);
    m.barcodes.push(BarcodeInput {
        raw: "TEEBOX".into(),
        symbology_hint: None,
        qualifier: BarcodeQualifier::Pack,
        pack_qty: Quantity::from_whole(6).unwrap(),
    });
    let edited = e
        .catalog
        .update(&e.ctx, p.id, 1, product(&e, "T-shirt", vec![s.clone(), m]))
        .await
        .unwrap();
    assert_eq!(edited.version, 2);
    assert!(
        e.store.lookup_code("TEE2").await.unwrap().is_none(),
        "archived variant's barcode no longer resolves"
    );
    let pack = e.store.lookup_code("teebox").await.unwrap().unwrap();
    assert_eq!(
        pack.scan_qty.milli(),
        6_000,
        "a pack barcode adds its pack quantity"
    );
    assert_eq!(
        e.store.lookup_code("TEE0").await.unwrap().unwrap().price.minor(),
        1799
    );
    assert_eq!(e.store.search("tee", 10).await.unwrap().len(), 2);

    // A second edit based on version 1 is a conflict (ADR-011).
    let err = e
        .catalog
        .update(&e.ctx, p.id, 1, product(&e, "T-shirt v2", vec![s]))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::ConflictVersion);
}

#[tokio::test]
async fn cashiers_cannot_edit_the_catalogue() {
    let e = env().await;
    let mut ctx = e.ctx.clone();
    ctx.actor = Some(Actor {
        user_id: UserId::generate(&UuidV7Generator),
        role: Role::Cashier,
        session_id: SessionId::generate(&UuidV7Generator),
    });
    let err = e
        .catalog
        .create(&ctx, product(&e, "Cola", vec![variant(120, None, &[])]))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::PermissionDenied);
    let _: &dyn LocalStore = &e.store;
}
