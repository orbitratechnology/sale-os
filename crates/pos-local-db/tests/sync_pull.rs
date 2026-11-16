//! E17-T07: applying what the register pulls. Master data from other registers and the cloud
//! goes through the same local writes as a local edit, under the inbound rule (newer, and no
//! unacknowledged local edit, or it is held); other registers' sales move stock here; a page
//! can be applied twice; a bootstrap resumes after an interruption.

mod common;

use pos_application::approval::Approvals;
use pos_application::auth::AuthService;
use pos_application::catalog::CatalogService;
use pos_application::queries::CatalogQueries;
use pos_application::staff::{NewStaff, StaffService};
use pos_application::sync::{InboundReport, Next, SyncStore, SyncTransport, SyncWorker, TransportError};
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::staff::Pin;
use pos_domain::{Money, Role, Timestamp};
use pos_local_db::SqliteStore;
use pos_sync::pull::{BootstrapEntity, BootstrapPage, Change, ChangeKind, ChangesResponse};
use pos_sync::{PushRequest, PushResponse};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

async fn link_and_ack_everything(store: &SqliteStore) {
    sqlx::query(
        "INSERT INTO sync_state (id, link_state, last_ack_sequence) VALUES (1, 'linked', (SELECT coalesce(max(local_sequence), 0) FROM outbox_events))
         ON CONFLICT (id) DO UPDATE SET link_state = 'linked', last_ack_sequence = excluded.last_ack_sequence",
    )
    .execute(store.db().writer())
    .await
    .unwrap();
}

/// The latest outbox payload written for an aggregate: the wire snapshot of it.
async fn snapshot_of(store: &SqliteStore, aggregate_type: &str, id: &str) -> Value {
    let p: String = sqlx::query_scalar(
        "SELECT payload_json FROM outbox_events WHERE aggregate_type = ? AND aggregate_id = ? ORDER BY local_sequence DESC LIMIT 1",
    )
    .bind(aggregate_type)
    .bind(id)
    .fetch_one(store.db().readers())
    .await
    .unwrap();
    serde_json::from_str(&p).unwrap()
}

fn change(
    seq: i64,
    origin: Option<Uuid>,
    event_type: &str,
    aggregate_type: &str,
    id: &str,
    version: Option<u64>,
    snapshot: Value,
) -> Change {
    Change {
        server_seq: seq,
        aggregate_type: aggregate_type.into(),
        aggregate_id: id.into(),
        version,
        kind: if version.is_some() {
            ChangeKind::Upsert
        } else {
            ChangeKind::Fact
        },
        origin_register_id: origin,
        event_type: event_type.into(),
        schema_version: 1,
        snapshot,
    }
}

fn page(changes: Vec<Change>, cursor: &str, has_more: bool) -> ChangesResponse {
    ChangesResponse {
        changes,
        next_cursor: cursor.into(),
        has_more,
    }
}

fn product_input(name: &str, price: i64, tax_category: pos_domain::TaxCategoryId) -> ProductInput {
    ProductInput {
        name: name.into(),
        description: None,
        category_id: None,
        product_type: ProductType::Stock,
        unit: Unit::Ea,
        tax_category_id: tax_category,
        quick_select: false,
        variants: vec![VariantInput::simple(Money::from_minor(price))],
    }
}

async fn balance(store: &SqliteStore, variant: &str) -> i64 {
    sqlx::query_scalar("SELECT coalesce(sum(on_hand_milli), 0) FROM stock_balances WHERE variant_id = ? AND bucket = 'sellable'")
        .bind(variant)
        .fetch_one(store.db().readers())
        .await
        .unwrap()
}

/// A sale of `qty_milli` of `variant` on another register, as the feed carries it.
fn foreign_sale(location: &str, variant: &str, qty_milli: i64) -> Value {
    let (sale, line) = (Uuid::now_v7(), Uuid::now_v7());
    let movement = json!({"id": Uuid::now_v7(), "variantId": variant, "locationId": location, "bucket": "sellable",
        "movementType": "sale", "delta": -qty_milli, "unitCost": null,
        "source": {"type": "sale", "sale_id": sale, "line_id": line}, "actor": Uuid::now_v7(),
        "occurredAt": 1_767_225_600_000_i64, "businessDate": "2026-01-01"});
    let line_json = json!({"id": line, "lineNo": 1, "productId": Uuid::now_v7(), "variantId": variant, "name": "Tea",
        "variantLabel": "", "sku": null, "productType": "stock", "unit": "ea", "trackStock": true,
        "quantity": qty_milli, "unitPrice": 100, "unitCost": null, "priceOverridden": false, "gross": 100,
        "discount": null, "lineDiscount": 0, "orderDiscountAlloc": 0, "tax": 0, "taxLines": [], "total": 100});
    let tender = json!({"id": Uuid::now_v7(), "seq": 1, "kind": "cash", "amount": 100, "received": 100, "change": 0,
        "external_reference": null, "status": "confirmed", "confirmation": "operator"});
    let mut s = json!({"id": sale, "cartId": Uuid::now_v7(), "registerId": Uuid::now_v7(), "locationId": location,
        "cashier": Uuid::now_v7(), "approver": null, "mode": "live", "receiptSeq": 1, "receiptNumber": "R02-000001",
        "occurredAt": 1_767_225_600_000_i64, "businessDate": "2026-01-01", "timezone": "Europe/London",
        "currency": {"code": "GBP", "exponent": 2}, "pricesIncludeTax": true, "orderDiscount": null, "subtotal": 100,
        "lineDiscountTotal": 0, "orderDiscountTotal": 0, "taxTotal": 0, "taxBreakdown": [], "rounding": 0,
        "total": 100, "tenderedTotal": 100, "change": 0});
    s["lines"] = json!([line_json]);
    s["tenders"] = json!([tender]);
    s["stockMovements"] = json!([movement]);
    s
}

#[test]
fn every_master_snapshot_of_the_day_maps_back_to_its_local_write() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/sync/end_of_day.json"
    );
    let day: Vec<pos_sync::EventEnvelope> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let business = pos_domain::BusinessId::from_uuid(day[0].business_id);
    let mut n = 0;
    for e in day.iter().filter(|e| pos_sync::events::is_master(&e.event_type)) {
        pos_application::sync::inbound_master(&e.event_type, e.schema_version, &e.payload, None, business)
            .unwrap_or_else(|err| panic!("{err}"));
        n += 1;
    }
    assert_eq!(n, 22);
    // Another business's snapshot is refused.
    let other = pos_domain::BusinessId::from_uuid(Uuid::now_v7());
    let staff = day.iter().find(|e| e.event_type == "staff.upserted").unwrap();
    assert!(
        pos_application::sync::inbound_master(&staff.event_type, 1, &staff.payload, None, other).is_err()
    );
}

#[tokio::test]
async fn a_pulled_page_updates_the_catalogue_staff_and_stock_once() {
    let shop = common::shop().await;
    let s = &shop.store;
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    let staff = StaffService {
        store: Arc::new(s.clone()),
        staff: Arc::new(s.clone()),
        hasher: shop.hasher.clone(),
    };
    let tea = catalog
        .create(&shop.owner, product_input("Tea", 100, shop.tax_category))
        .await
        .unwrap();
    let bea = staff
        .create(
            &shop.owner,
            NewStaff {
                display_name: "Bea".into(),
                role: Role::Cashier,
                pin: Pin::parse("2468").unwrap(),
            },
        )
        .await
        .unwrap();
    link_and_ack_everything(s).await;
    let other = Some(Uuid::now_v7());
    let location: String = sqlx::query_scalar("SELECT location_id FROM locations")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    let variant = tea.variants[0].id.to_string();

    // Elsewhere: Tea repriced (v2), Coffee created (v1), Bea deactivated (v2), two Teas sold.
    let mut tea_v2 = snapshot_of(s, "product", &tea.id.to_string()).await;
    tea_v2["version"] = json!(2);
    tea_v2["name"] = json!("Green tea");
    tea_v2["variants"][0]["price"] = json!(150);
    let mut coffee = tea_v2.clone();
    let coffee_id = Uuid::now_v7();
    coffee["id"] = json!(coffee_id);
    coffee["name"] = json!("Coffee");
    coffee["version"] = json!(1);
    coffee["variants"][0]["id"] = json!(Uuid::now_v7());
    let mut bea_v2 = snapshot_of(s, "staff_user", &bea.user_id.to_string()).await;
    bea_v2["version"] = json!(2);
    bea_v2["active"] = json!(false);
    let p = page(
        vec![
            change(
                11,
                other,
                "product.upserted",
                "product",
                &tea.id.to_string(),
                Some(2),
                tea_v2,
            ),
            change(
                12,
                None,
                "product.upserted",
                "product",
                &coffee_id.to_string(),
                Some(1),
                coffee,
            ),
            change(
                13,
                other,
                "staff.upserted",
                "staff_user",
                &bea.user_id.to_string(),
                Some(2),
                bea_v2,
            ),
            change(
                14,
                other,
                "sale.completed",
                "sale",
                &Uuid::now_v7().to_string(),
                None,
                foreign_sale(&location, &variant, 2000),
            ),
        ],
        "v1:14",
        false,
    );
    let at = Timestamp::from_millis(1_767_225_700_000);
    let r = s.apply_changes(&p, at).await.unwrap();
    assert_eq!(
        r,
        InboundReport {
            applied: 4,
            ..InboundReport::default()
        }
    );

    let tea_now = catalog.get(&shop.owner, tea.id).await.unwrap();
    assert_eq!(
        (tea_now.name.as_str(), tea_now.variants[0].price, tea_now.version),
        ("Green tea", Money::from_minor(150), 2)
    );
    let found = s.search("coffee", 5).await.unwrap();
    assert_eq!(found.len(), 1, "a pulled product is searchable");
    assert_eq!(balance(s, &variant).await, -2000);
    // Bea was deactivated on another register: she cannot sign in here any more.
    let auth = AuthService {
        store: Arc::new(s.clone()),
        staff: Arc::new(s.clone()),
        hasher: shop.hasher.clone(),
        approvals: Arc::new(Approvals::default()),
    };
    let mut nobody = shop.owner.clone();
    nobody.actor = None;
    assert!(
        auth.login(&nobody, bea.user_id, Pin::parse("2468").unwrap())
            .await
            .is_err()
    );
    // Nothing was queued to push back: inbound writes are not local edits.
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox_events WHERE local_sequence > (SELECT last_ack_sequence FROM sync_state)")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    assert_eq!(pending, 0);
    assert_eq!(s.pull_state().await.unwrap().cursor.as_deref(), Some("v1:14"));

    // The same page again: nothing changes.
    let again = s.apply_changes(&p, at).await.unwrap();
    assert_eq!(
        again,
        InboundReport {
            already: 4,
            ..InboundReport::default()
        }
    );
    assert_eq!(balance(s, &variant).await, -2000);
    assert_eq!(catalog.get(&shop.owner, tea.id).await.unwrap().version, 2);
}

#[tokio::test]
async fn a_change_to_an_aggregate_with_a_pending_edit_is_held_and_others_are_skipped() {
    let shop = common::shop().await;
    let s = &shop.store;
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    let tea = catalog
        .create(&shop.owner, product_input("Tea", 100, shop.tax_category))
        .await
        .unwrap();
    let cake = catalog
        .create(&shop.owner, product_input("Cake", 300, shop.tax_category))
        .await
        .unwrap();
    link_and_ack_everything(s).await;
    let me: String = sqlx::query_scalar("SELECT register_id FROM device_identity")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    let base = snapshot_of(s, "product", &tea.id.to_string()).await;
    // A local edit the cloud has not acknowledged yet.
    let mut edit = product_input("Tea (local)", 110, shop.tax_category);
    edit.variants[0].id = Some(tea.variants[0].id);
    catalog.update(&shop.owner, tea.id, 1, edit).await.unwrap();
    let mut theirs = base.clone();
    theirs["version"] = json!(2);
    theirs["name"] = json!("Tea (theirs)");
    let cake_v1 = snapshot_of(s, "product", &cake.id.to_string()).await;
    let p = page(
        vec![
            change(
                21,
                Some(Uuid::now_v7()),
                "product.upserted",
                "product",
                &tea.id.to_string(),
                Some(2),
                theirs,
            ),
            // Not newer than what is here.
            change(
                22,
                Some(Uuid::now_v7()),
                "product.upserted",
                "product",
                &cake.id.to_string(),
                Some(1),
                cake_v1.clone(),
            ),
            // This register's own change, coming back.
            change(
                23,
                Some(me.parse().unwrap()),
                "product.upserted",
                "product",
                &cake.id.to_string(),
                Some(1),
                cake_v1,
            ),
        ],
        "v1:23",
        false,
    );
    let r = s
        .apply_changes(&p, Timestamp::from_millis(1_767_225_700_000))
        .await
        .unwrap();
    assert_eq!(
        r,
        InboundReport {
            held: 1,
            skipped_older: 1,
            skipped_own: 1,
            ..InboundReport::default()
        }
    );
    assert_eq!(
        catalog.get(&shop.owner, tea.id).await.unwrap().name,
        "Tea (local)",
        "the local edit stands"
    );
    let outcomes: Vec<String> = sqlx::query_scalar("SELECT outcome FROM inbound_changes ORDER BY server_seq")
        .fetch_all(s.db().readers())
        .await
        .unwrap();
    assert_eq!(outcomes, ["conflict_pending", "skipped_older", "skipped_own"]);
}

/// Serves bootstrap pages by token, failing once on the second page.
struct Pages {
    pages: Vec<BootstrapPage>,
    asked: Mutex<Vec<Option<String>>>,
    fail_once_on: Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl SyncTransport for Pages {
    async fn push(&self, _r: PushRequest) -> Result<PushResponse, TransportError> {
        Err(TransportError::Offline)
    }
    async fn bootstrap(&self, page: Option<String>) -> Result<BootstrapPage, TransportError> {
        self.asked.lock().unwrap().push(page.clone());
        if page.is_some() && *self.fail_once_on.lock().unwrap() == page {
            *self.fail_once_on.lock().unwrap() = None;
            return Err(TransportError::Timeout);
        }
        let i = match page.as_deref() {
            None => 0,
            Some(t) => t.trim_start_matches("p").parse().unwrap(),
        };
        Ok(self.pages[i].clone())
    }
}

#[tokio::test]
async fn a_bootstrap_resumes_where_it_was_interrupted_and_then_pulls_from_its_cursor() {
    let shop = common::shop().await;
    let s = &shop.store;
    link_and_ack_everything(s).await;
    let business: String = sqlx::query_scalar("SELECT business_id FROM businesses")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    // Three pages of categories made elsewhere.
    let category = |name: &str| {
        let id = Uuid::now_v7();
        BootstrapEntity {
            aggregate_type: "category".into(),
            aggregate_id: id.to_string(),
            event_type: "category.upserted".into(),
            schema_version: 1,
            version: 1,
            snapshot: json!({"categoryId": id, "businessId": business, "parentId": null, "name": name, "sortOrder": 0,
                             "archived": false, "version": 1, "createdAt": 1, "updatedAt": 1}),
        }
    };
    let pages: Vec<BootstrapPage> = (0..3)
        .map(|i| BootstrapPage {
            snapshot_cursor: "v1:77".into(),
            entities: (0..4).map(|j| category(&format!("Category {i}-{j}"))).collect(),
            next_page: (i < 2).then(|| format!("p{}", i + 1)),
        })
        .collect();
    let transport = Arc::new(Pages {
        pages,
        asked: Mutex::new(Vec::new()),
        fail_once_on: Mutex::new(Some("p1".into())),
    });
    let worker = SyncWorker::new(
        Arc::new(s.clone()),
        transport.clone(),
        shop.clock.clone(),
        Arc::new(pos_application::sync::OsRandom),
    );
    // Page 0, then the connection drops on page 1.
    assert_eq!(worker.bootstrap_step().await.unwrap().next, Next::Immediately);
    assert!(matches!(
        worker.bootstrap_step().await.unwrap().next,
        Next::RetryAfter(_)
    ));
    let state = s.pull_state().await.unwrap();
    assert_eq!(
        (state.bootstrap_page.as_deref(), state.cursor.as_deref()),
        (Some("p1"), None)
    );
    // While it is incomplete, the feed is not pulled.
    assert_eq!(worker.pull_once().await.unwrap().received, 0);
    // Resumed: pages 1 and 2, then done.
    assert_eq!(worker.bootstrap_step().await.unwrap().next, Next::Immediately);
    assert_eq!(worker.bootstrap_step().await.unwrap().next, Next::Idle);
    assert_eq!(
        *transport.asked.lock().unwrap(),
        [None, Some("p1".into()), Some("p1".into()), Some("p2".into())]
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM categories WHERE name LIKE 'Category %'")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    assert_eq!(count, 12);
    let state = s.pull_state().await.unwrap();
    assert_eq!(
        (state.bootstrap_page, state.cursor.as_deref()),
        (None, Some("v1:77"))
    );
}

/// Serves the feed in pages after the cursor it is given.
struct Feed(Vec<Change>);

#[async_trait::async_trait]
impl SyncTransport for Feed {
    async fn push(&self, _r: PushRequest) -> Result<PushResponse, TransportError> {
        Err(TransportError::Offline)
    }
    async fn changes(&self, cursor: Option<String>, _limit: u32) -> Result<ChangesResponse, TransportError> {
        let after = cursor
            .as_deref()
            .and_then(pos_sync::pull::parse_cursor)
            .unwrap_or(0);
        let rest: Vec<Change> = self
            .0
            .iter()
            .filter(|c| c.server_seq > after)
            .take(2)
            .cloned()
            .collect();
        let next = rest.last().map_or(after, |c| c.server_seq);
        let has_more = self.0.iter().any(|c| c.server_seq > next);
        Ok(page(rest, &pos_sync::pull::cursor(next), has_more))
    }
}

#[tokio::test]
async fn the_worker_pulls_page_after_page_until_the_feed_is_drained() {
    let shop = common::shop().await;
    let s = &shop.store;
    link_and_ack_everything(s).await;
    let location: String = sqlx::query_scalar("SELECT location_id FROM locations")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    let variant = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    }
    .create(&shop.owner, product_input("Tea", 100, shop.tax_category))
    .await
    .unwrap()
    .variants[0]
        .id
        .to_string();
    link_and_ack_everything(s).await;
    let feed: Vec<Change> = (1..=5)
        .map(|i| {
            change(
                i,
                Some(Uuid::now_v7()),
                "sale.completed",
                "sale",
                &Uuid::now_v7().to_string(),
                None,
                foreign_sale(&location, &variant, 1000),
            )
        })
        .collect();
    let worker = SyncWorker::new(
        Arc::new(s.clone()),
        Arc::new(Feed(feed)),
        shop.clock.clone(),
        Arc::new(pos_application::sync::OsRandom),
    );
    let mut nexts = Vec::new();
    loop {
        let r = worker.pull_once().await.unwrap();
        nexts.push(r.next.clone());
        if r.next != Next::Immediately {
            break;
        }
    }
    assert_eq!(nexts, [Next::Immediately, Next::Immediately, Next::Idle]);
    assert_eq!(balance(s, &variant).await, -5000);
    assert_eq!(s.pull_state().await.unwrap().cursor.as_deref(), Some("v1:5"));
}

/// E17-T07 acceptance bench: bootstrapping 50,000 products (one variant and barcode each)
/// in pages of 2,000. Reports the local apply time and the bytes on the wire, from which the
/// transfer time on a 20 Mbps link follows. Run with
/// `cargo test -p pos-local-db --test sync_pull --release -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "bench"]
async fn bench_bootstrap_fifty_thousand_variants() {
    let shop = common::shop().await;
    let s = &shop.store;
    link_and_ack_everything(s).await;
    let business: String = sqlx::query_scalar("SELECT business_id FROM businesses")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    let tax: String = sqlx::query_scalar("SELECT tax_category_id FROM tax_categories")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    let _ = business;
    let total = 50_000;
    let mut bytes = 0usize;
    let mut apply = std::time::Duration::ZERO;
    for p in 0..total / 2000 {
        let entities: Vec<BootstrapEntity> = (0..2000)
            .map(|i| {
                let n = p * 2000 + i;
                let id = Uuid::now_v7();
                let barcode = json!({"id": Uuid::now_v7(), "value": format!("2{n:011}0"), "raw": format!("2{n:011}0"),
                                     "symbology": "ean13", "qualifier": "primary", "packQty": 1000});
                let mut variant = json!({"id": Uuid::now_v7(), "isDefault": true, "sku": format!("SKU-{n}"),
                    "options": {}, "price": 100 + n % 900, "cost": null, "trackStock": true, "lowStockThreshold": null,
                    "scanQty": 1000, "archived": false});
                variant["barcodes"] = json!([barcode]);
                let mut product = json!({"id": id, "name": format!("Product {n}"), "description": null, "categoryId": null,
                    "productType": "stock", "unit": "ea", "taxCategoryId": tax, "quickSelect": false, "archived": false,
                    "version": 1});
                product["variants"] = json!([variant]);
                BootstrapEntity {
                    aggregate_type: "product".into(),
                    aggregate_id: id.to_string(),
                    event_type: "product.upserted".into(),
                    schema_version: 1,
                    version: 1,
                    snapshot: product,
                }
            })
            .collect();
        let page = BootstrapPage {
            snapshot_cursor: "v1:1".into(),
            entities,
            next_page: (p + 1 < total / 2000).then(|| format!("p{}", p + 1)),
        };
        bytes += serde_json::to_vec(&page).unwrap().len();
        let t = std::time::Instant::now();
        let r = s
            .apply_bootstrap(&page, Timestamp::from_millis(1_767_225_700_000))
            .await
            .unwrap();
        apply += t.elapsed();
        assert_eq!(r.applied, 2000);
    }
    // 20 Mbps = 2,500,000 bytes a second.
    let wire_secs = bytes / 2_500_000;
    eprintln!(
        "50,000 variants: {} kB uncompressed, about {wire_secs} s at 20 Mbps; local apply {apply:?}",
        bytes / 1000
    );
    let found = s.search("Product 49999", 5).await.unwrap();
    assert!(!found.is_empty());
}
