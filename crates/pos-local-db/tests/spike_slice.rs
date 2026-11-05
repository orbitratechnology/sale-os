//! E00-T06 store-level acceptance for the Stage 0 vertical slice (spike tables).

use pos_application::audit::{self, AuditEntry};
use pos_application::records::{BusinessRecord, LocationRecord, RegisterRecord};
use pos_application::spike::{self, SpikeProduct};
use pos_application::{LocalStore, OutboxDraft, WriteContext, WriteError, WriteSet};
use pos_domain::{
    BusinessId, Currency, IdGenerator, LocationId, Money, ProductId, Quantity, RegisterId, Timestamp,
    UuidV7Generator,
};
use pos_local_db::spike::{SPIKE_TABLES, cart, ensure_spike_tables, receipt_number};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use tempfile::TempDir;

async fn setup() -> (TempDir, SqliteStore, WriteContext, SpikeProduct) {
    let dir = TempDir::new().unwrap();
    let db = LocalDb::open(&DbOptions::in_dir(dir.path())).await.unwrap();
    ensure_spike_tables(&db).await.unwrap();
    let store = SqliteStore::new(db);
    let ids = UuidV7Generator;
    let ctx = WriteContext {
        business_id: BusinessId::generate(&ids),
        location_id: LocationId::generate(&ids),
        register_id: RegisterId::generate(&ids),
        actor: None,
        occurred_at: Timestamp::from_millis(1_767_225_600_000),
    };
    store.apply(workspace(&ctx, &ids)).await.unwrap();
    let product = SpikeProduct {
        product_id: ProductId::generate(&ids),
        name: "Demo Cola 330ml".into(),
        barcode: "5012345678900".into(),
        price: Money::from_minor(120),
    };
    store
        .apply(spike::create_product(&ctx, &ids, product.clone()).unwrap())
        .await
        .unwrap();
    (dir, store, ctx, product)
}

fn workspace(c: &WriteContext, ids: &dyn IdGenerator) -> WriteSet {
    let t = c.occurred_at;
    let business = BusinessRecord {
        business_id: c.business_id,
        name: "Demo".into(),
        legal_name: None,
        tax_registration_no: None,
        address: serde_json::json!({}),
        phone: None,
        email: None,
        logo_path: None,
        country_code: "GB".into(),
        timezone: "Europe/London".into(),
        currency: Currency::from_iso("GBP").unwrap(),
        prices_include_tax: true,
        version: 1,
        created_at: t,
        updated_at: t,
    };
    let location = LocationRecord {
        location_id: c.location_id,
        business_id: c.business_id,
        name: "Main".into(),
        address: serde_json::json!({}),
        archived: false,
        version: 1,
        created_at: t,
        updated_at: t,
    };
    let register = RegisterRecord {
        register_id: c.register_id,
        business_id: c.business_id,
        location_id: c.location_id,
        name: "Till 1".into(),
        code: "R01".into(),
        receipt_prefix: String::new(),
        retired: false,
        version: 1,
        created_at: t,
        updated_at: t,
    };
    WriteSet::builder(c.clone())
        .outbox(OutboxDraft::business_updated(ids, &business).unwrap())
        .outbox(OutboxDraft::location_upserted(ids, &location).unwrap())
        .outbox(OutboxDraft::register_upserted(ids, &register).unwrap())
        .put_business(business, None)
        .put_location(location, None)
        .put_register(register, None)
        .audit(AuditEntry::new(
            ids,
            audit::BUSINESS_CREATED,
            "business",
            c.business_id,
        ))
        .build()
        .unwrap()
}

async fn counts(store: &SqliteStore) -> Vec<i64> {
    let mut tables: Vec<&str> = SPIKE_TABLES.to_vec();
    tables.extend(["audit_events", "outbox_events", "register_counters"]);
    let mut out = Vec::new();
    for t in tables {
        out.push(
            sqlx::query_scalar(&format!("SELECT count(*) FROM {t}"))
                .fetch_one(store.db().readers())
                .await
                .unwrap(),
        );
    }
    out
}

#[tokio::test]
async fn scan_then_cash_sale_commits_every_record_once() {
    let (_dir, store, ctx, product) = setup().await;
    let ids = UuidV7Generator;
    store
        .apply(spike::add_to_cart(&ctx, &ids, product.product_id, Quantity::ONE).unwrap())
        .await
        .unwrap();
    let lines = cart(store.db()).await.unwrap();
    assert_eq!(lines.len(), 1);

    let (set, sale) = spike::complete_cash_sale(&ctx, &ids, &lines, Money::from_minor(500)).unwrap();
    let applied = store.apply(set).await.unwrap();
    assert_eq!(applied.numbers[&spike::RECEIPT_SLOT], 1);
    assert_eq!(
        receipt_number(store.db(), &sale.sale_id.to_string())
            .await
            .unwrap()
            .as_deref(),
        Some("R01-000001")
    );
    assert_eq!(sale.tender.change, Money::from_minor(380));

    let r = store.db().readers();
    for (table, expected) in [
        ("spike_sales", 1),
        ("spike_sale_lines", 1),
        ("spike_tenders", 1),
        ("spike_stock_movements", 1),
        ("spike_cart_lines", 0),
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(r)
            .await
            .unwrap();
        assert_eq!(n, expected, "{table}");
    }
    let delta: i64 = sqlx::query_scalar("SELECT delta_milli FROM spike_stock_movements")
        .fetch_one(r)
        .await
        .unwrap();
    assert_eq!(delta, -1000);
    let (seq, max): (i64, i64) = sqlx::query_as(
        "SELECT local_sequence, (SELECT max(local_sequence) FROM outbox_events) FROM outbox_events WHERE event_type = 'sale.completed'",
    )
    .fetch_one(r)
    .await
    .unwrap();
    // Workspace (3 events) + product (1) come first under ADR-009, so the sale is event 5.
    assert_eq!((seq, max), (5, 5));
    let sale_audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'sale.completed'")
            .fetch_one(r)
            .await
            .unwrap();
    assert_eq!(sale_audits, 1);
}

#[tokio::test]
async fn a_failure_anywhere_in_the_sale_leaves_no_trace() {
    let (_dir, store, ctx, product) = setup().await;
    let ids = UuidV7Generator;
    store
        .apply(spike::add_to_cart(&ctx, &ids, product.product_id, Quantity::ONE).unwrap())
        .await
        .unwrap();
    let lines = cart(store.db()).await.unwrap();
    let before = counts(&store).await;
    let mut n = 0;
    loop {
        let (set, _) = spike::complete_cash_sale(&ctx, &ids, &lines, Money::from_minor(500)).unwrap();
        match store.clone().with_fault_before_statement(n).apply(set).await {
            Err(WriteError::FaultInjected { .. }) => {
                assert_eq!(
                    counts(&store).await,
                    before,
                    "partial sale with a fault before statement {n}"
                );
                n += 1;
            }
            Ok(_) => break,
            Err(e) => panic!("{e}"),
        }
    }
    // number, register lookup, sale, line, MOVEMENT, tender, cart clear, outbox (2), audit, commit
    assert_eq!(n, 11);
    // The cart is still there after every failed attempt: nothing was lost.
    assert_eq!(
        counts(&store).await[1],
        0,
        "the successful attempt cleared the cart"
    );
}

#[tokio::test]
async fn the_movement_insert_specifically_rolls_everything_back() {
    let (_dir, store, ctx, product) = setup().await;
    let ids = UuidV7Generator;
    store
        .apply(spike::add_to_cart(&ctx, &ids, product.product_id, Quantity::ONE).unwrap())
        .await
        .unwrap();
    let lines = cart(store.db()).await.unwrap();
    let before = counts(&store).await;
    // Statement 4 is the stock-movement insert (see the order above).
    let (set, _) = spike::complete_cash_sale(&ctx, &ids, &lines, Money::from_minor(500)).unwrap();
    let err = store
        .clone()
        .with_fault_before_statement(4)
        .apply(set)
        .await
        .unwrap_err();
    assert_eq!(err, WriteError::FaultInjected { statement: 4 });
    assert_eq!(counts(&store).await, before);
    assert_eq!(
        cart(store.db()).await.unwrap().len(),
        1,
        "the cart survives a failed sale"
    );
}
