//! E09-T01 and E07-T02 acceptance: the cart backend and the scan pipeline against a real
//! database.

mod common;

use pos_application::cart::{CartService, CartView};
use pos_application::catalog::CatalogService;
use pos_application::context::AppError;
use pos_application::scan::{ScanOutcome, ScanSource};
use pos_application::{LocalStore, WriteError, WriteSet};
use pos_domain::cart::{AddVia, Cart};
use pos_domain::catalog::{
    BarcodeInput, BarcodeQualifier, Product, ProductInput, ProductType, Unit, VariantInput,
};
use pos_domain::pricing::Discount;
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, RatePpm, Role, ScanEventId, UuidV7Generator};
use std::sync::Arc;

struct Till {
    shop: common::Shop,
    carts: CartService,
    catalog: CatalogService,
}

fn cart_service(shop: &common::Shop) -> CartService {
    let s = shop.store.clone();
    CartService::new(Arc::new(s.clone()), Arc::new(s.clone()), Arc::new(s))
}

fn till(shop: common::Shop) -> Till {
    let s = shop.store.clone();
    Till {
        carts: cart_service(&shop),
        catalog: CatalogService {
            store: Arc::new(s.clone()),
            catalog: Arc::new(s),
        },
        shop,
    }
}

async fn cola(t: &Till) -> Product {
    let mut v = VariantInput::simple(Money::from_minor(120));
    v.sku = Some("COLA".into());
    v.barcodes = vec![
        BarcodeInput::alias("4006381333931"),
        BarcodeInput {
            raw: "CASE12".into(),
            symbology_hint: None,
            qualifier: BarcodeQualifier::Pack,
            pack_qty: Quantity::from_whole(12).unwrap(),
        },
    ];
    let input = ProductInput {
        name: "Cola 330ml".into(),
        description: None,
        category_id: None,
        product_type: ProductType::Stock,
        unit: Unit::Ea,
        tax_category_id: t.shop.tax_category,
        quick_select: true,
        variants: vec![v],
    };
    t.catalog.create(&t.shop.owner, input).await.unwrap()
}

fn new_id() -> ScanEventId {
    ScanEventId::generate(&UuidV7Generator)
}

/// One scanner delivery with a fresh event id.
async fn scan(t: &Till, code: &str) -> ScanOutcome {
    t.carts
        .scan(&t.shop.owner, code, new_id(), ScanSource::Scanner)
        .await
        .unwrap()
}

fn added(o: ScanOutcome) -> CartView {
    match o {
        ScanOutcome::Added { view, .. } => view,
        other => panic!("expected Added, got {other:?}"),
    }
}

async fn count(t: &Till, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap()
}

#[tokio::test]
async fn scanning_builds_a_priced_cart() {
    let t = till(common::shop().await);
    cola(&t).await;
    let ctx = &t.shop.owner;
    for _ in 0..3 {
        added(scan(&t, "4006381333931").await);
    }
    let view = added(scan(&t, "case12").await);
    assert_eq!(
        view.cart.lines.len(),
        1,
        "repeat scans and the pack code merge (BAR-005)"
    );
    assert_eq!(view.cart.lines[0].quantity.milli(), 15_000);
    // 15 × 1.20 inclusive of 20% VAT: total 18.00, tax round(1800 × 0.2 / 1.2) = 3.00.
    assert_eq!(
        (view.priced.total.minor(), view.priced.tax_total.minor()),
        (1800, 300)
    );
    assert_eq!(view.cart.revision, 4);
    // Unknown code: nothing changes (BAR-006).
    assert_eq!(
        scan(&t, "999999").await,
        ScanOutcome::NotFound {
            code: "999999".into()
        }
    );
    assert_eq!(t.carts.current(ctx).await.unwrap().cart.revision, 4);
}

#[tokio::test]
async fn the_cart_survives_a_restart() {
    let t = till(common::shop().await);
    cola(&t).await;
    let ctx = &t.shop.owner;
    t.carts
        .scan(ctx, "COLA", new_id(), ScanSource::Manual)
        .await
        .unwrap();
    let before = t.carts.current(ctx).await.unwrap();
    // A brand-new service over the same database (the app restarted): POS-011.
    let after = cart_service(&t.shop).current(ctx).await.unwrap();
    assert_eq!(after.cart.id, before.cart.id);
    assert_eq!(after.cart.lines[0].quantity, before.cart.lines[0].quantity);
    assert_eq!(after.cart.lines[0].added_via, AddVia::ManualCode);
    assert_eq!(after.priced.total, before.priced.total);
}

#[tokio::test]
async fn line_edits_discounts_and_overrides() {
    let t = till(common::shop().await);
    let p = cola(&t).await;
    let ctx = &t.shop.owner;
    let (view, _) = t
        .carts
        .add_variant(
            ctx,
            p.variants[0].id,
            Some(Quantity::from_whole(2).unwrap()),
            AddVia::Search,
        )
        .await
        .unwrap();
    let line = view.cart.lines[0].id;
    let view = t
        .carts
        .set_quantity(ctx, line, Quantity::from_whole(5).unwrap())
        .await
        .unwrap();
    assert_eq!(view.priced.total.minor(), 600);
    let ten_percent = Discount::percent(RatePpm::new(100_000).unwrap());
    let view = t
        .carts
        .set_line_discount(ctx, line, Some(ten_percent))
        .await
        .unwrap();
    assert_eq!(view.priced.total.minor(), 540);
    let view = t
        .carts
        .set_order_discount(ctx, Some(Discount::fixed(Money::from_minor(40))))
        .await
        .unwrap();
    assert_eq!(view.priced.total.minor(), 500);
    // A discount larger than the order is refused and nothing is saved.
    let err = t
        .carts
        .set_order_discount(ctx, Some(Discount::fixed(Money::from_minor(10_000))))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::PricingDiscountExceeds);
    assert_eq!(t.carts.current(ctx).await.unwrap().priced.total.minor(), 500);
    // Fractional quantity for "ea" is refused, and so is zero (use remove).
    for bad in [Quantity::parse_decimal("1.5").unwrap(), Quantity::ZERO] {
        let err = t.carts.set_quantity(ctx, line, bad).await.unwrap_err();
        assert_eq!(err.error_code(), ErrorCode::ValidationFailed, "{bad:?}");
    }
    let view = t
        .carts
        .override_price(ctx, line, Money::from_minor(100))
        .await
        .unwrap();
    assert_eq!(view.cart.lines[0].price_override, Some(Money::from_minor(100)));
    // Audited: two discounts and the override.
    let audits = count(
        &t,
        "SELECT count(*) FROM audit_events WHERE action IN ('cart.discount_applied', 'cart.price_overridden')",
    )
    .await;
    assert_eq!(audits, 3);
    // Carts are local only: nothing in the outbox (ADR-009).
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM outbox_events WHERE aggregate_type = 'cart'"
        )
        .await,
        0
    );
    let view = t.carts.remove_line(ctx, line).await.unwrap();
    assert!(view.cart.lines.is_empty());
}

#[tokio::test]
async fn clear_needs_the_current_revision_and_void_needs_permission() {
    let t = till(common::shop().await);
    cola(&t).await;
    let owner = &t.shop.owner;
    added(scan(&t, "COLA").await);
    let err = t.carts.clear(owner, 0).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::CartRevisionMismatch);
    let view = t.carts.clear(owner, 1).await.unwrap();
    assert!(view.cart.lines.is_empty());

    added(scan(&t, "COLA").await);
    let rev = t.carts.current(owner).await.unwrap().cart.revision;
    // A cashier cannot void a cart with lines (approval flow: E05-T04).
    let cashier = t.shop.as_role(Role::Cashier);
    assert_eq!(
        t.carts.void(&cashier, rev, "changed mind").await.unwrap_err(),
        AppError::PermissionDenied(pos_domain::Permission::CartVoid)
    );
    let fresh = t.carts.void(owner, rev, "customer left").await.unwrap();
    assert!(fresh.cart.lines.is_empty());
    assert_ne!(
        fresh.cart.id, view.cart.id,
        "a voided cart is closed; the next one is new"
    );
    let voided = count(
        &t,
        "SELECT count(*) FROM carts WHERE status = 'voided' AND void_reason = 'customer left'",
    )
    .await;
    assert_eq!(voided, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_scans_are_never_lost() {
    // Tauri runs commands concurrently. Before the service lock and the store's revision
    // check, these collided on the first insert, and later ones overwrote each other.
    let t = Arc::new(till(common::shop().await));
    cola(&t).await;
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let t = t.clone();
            tokio::spawn(async move { scan(&t, "4006381333931").await })
        })
        .collect();
    for task in tasks {
        added(task.await.unwrap());
    }
    let view = t.carts.current(&t.shop.owner).await.unwrap();
    assert_eq!(
        view.cart.lines[0].quantity.milli(),
        20_000,
        "every scan counts (BAR-007)"
    );
    assert_eq!(view.cart.revision, 20);
}

#[tokio::test]
async fn a_repeated_delivery_counts_once() {
    let t = till(common::shop().await);
    cola(&t).await;
    let ctx = &t.shop.owner;
    let id = new_id();
    added(
        t.carts
            .scan(ctx, "4006381333931", id, ScanSource::Scanner)
            .await
            .unwrap(),
    );
    let again = t
        .carts
        .scan(ctx, "4006381333931", id, ScanSource::Scanner)
        .await
        .unwrap();
    let ScanOutcome::Duplicate { view } = again else {
        panic!("expected Duplicate, got {again:?}")
    };
    assert_eq!(view.cart.lines[0].quantity.milli(), 1000);
    assert_eq!(view.cart.revision, 1);
    // Unknown codes are recorded too: a repeated delivery is not a second "not found".
    let id = new_id();
    let first = t.carts.scan(ctx, "NOPE", id, ScanSource::Scanner).await.unwrap();
    assert!(matches!(first, ScanOutcome::NotFound { .. }));
    let second = t.carts.scan(ctx, "NOPE", id, ScanSource::Scanner).await.unwrap();
    assert!(matches!(second, ScanOutcome::Duplicate { .. }));
    // The line keeps the delivery that added it.
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM cart_lines WHERE last_scan_event_id IS NOT NULL"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn ten_fast_scans_are_ten_items() {
    // AT-03: never de-duplicate by value or time.
    let t = till(common::shop().await);
    cola(&t).await;
    for _ in 0..10 {
        added(scan(&t, "4006381333931").await);
    }
    let view = t.carts.current(&t.shop.owner).await.unwrap();
    assert_eq!(
        (view.cart.lines.len(), view.cart.lines[0].quantity.milli()),
        (1, 10_000)
    );
}

#[tokio::test]
async fn pack_then_unit_and_discounted_lines() {
    let t = till(common::shop().await);
    cola(&t).await;
    added(scan(&t, "CASE12").await);
    let view = added(scan(&t, "4006381333931").await);
    assert_eq!(
        (view.cart.lines.len(), view.cart.lines[0].quantity.milli()),
        (1, 13_000)
    );
    // A discounted line is not merged into: the next scan appends a new line (BAR-005).
    let line = view.cart.lines[0].id;
    let five_percent = Discount::percent(RatePpm::new(50_000).unwrap());
    t.carts
        .set_line_discount(&t.shop.owner, line, Some(five_percent))
        .await
        .unwrap();
    let view = added(scan(&t, "4006381333931").await);
    assert_eq!(view.cart.lines.len(), 2);
    assert_eq!(view.cart.lines[1].quantity.milli(), 1000);
}

#[tokio::test]
async fn malformed_codes_are_rejected_before_lookup() {
    let t = till(common::shop().await);
    let long = "9".repeat(65);
    for bad in ["", "   ", "40063\u{7}81333931", long.as_str()] {
        let err = t
            .carts
            .scan(&t.shop.owner, bad, new_id(), ScanSource::Scanner)
            .await
            .unwrap_err();
        assert_eq!(err.error_code(), ErrorCode::ScanInvalid, "{bad:?}");
    }
}

#[tokio::test]
async fn a_stale_write_is_refused_by_the_store() {
    // The service lock serializes changes in-process; the store's revision check is the
    // backstop. Exercise it directly.
    let t = till(common::shop().await);
    cola(&t).await;
    let view = added(scan(&t, "COLA").await);
    let owner = t.shop.owner.actor.as_ref().unwrap().user_id;
    let mut stale = view.cart.clone();
    stale.revision += 1;
    let set = WriteSet::builder(t.shop.owner.write_context())
        .put_cart(stale, owner, Some(0))
        .build()
        .unwrap();
    assert_eq!(
        t.shop.store.apply(set).await.unwrap_err(),
        WriteError::CartRevisionMismatch { expected: Some(0) }
    );
    // A second never-saved cart for the register trips the one-open-cart index (E09-T01).
    let mut other = Cart::new(&UuidV7Generator);
    other.revision = 1;
    let set = WriteSet::builder(t.shop.owner.write_context())
        .put_cart(other, owner, None)
        .build()
        .unwrap();
    assert_eq!(
        t.shop.store.apply(set).await.unwrap_err(),
        WriteError::CartRevisionMismatch { expected: None }
    );
    assert_eq!(
        count(&t, "SELECT count(*) FROM carts WHERE status = 'open'").await,
        1
    );
}

#[tokio::test]
async fn the_dedupe_window_keeps_the_last_500() {
    let t = till(common::shop().await);
    let mut ids = Vec::new();
    for _ in 0..510 {
        let id = new_id();
        ids.push(id);
        t.carts
            .scan(&t.shop.owner, "NOPE", id, ScanSource::Scanner)
            .await
            .unwrap();
    }
    assert_eq!(count(&t, "SELECT count(*) FROM scan_events_seen").await, 500);
    // The newest are kept: re-delivering the last one is still a duplicate.
    let last = t
        .carts
        .scan(&t.shop.owner, "NOPE", ids[509], ScanSource::Scanner)
        .await
        .unwrap();
    assert!(matches!(last, ScanOutcome::Duplicate { .. }));
}
