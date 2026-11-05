//! E09-T01 acceptance: the cart backend against a real database.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::context::AppError;
use pos_domain::cart::AddVia;
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

fn till(shop: common::Shop) -> Till {
    let s = shop.store.clone();
    Till {
        carts: CartService {
            store: Arc::new(s.clone()),
            carts: Arc::new(s.clone()),
            catalog: Arc::new(s.clone()),
        },
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

fn scan() -> AddVia {
    AddVia::Scan {
        event_id: ScanEventId::generate(&UuidV7Generator),
    }
}

#[tokio::test]
async fn scanning_builds_a_priced_cart() {
    let t = till(common::shop().await);
    cola(&t).await;
    let ctx = &t.shop.owner;
    for _ in 0..3 {
        t.carts
            .add_code(ctx, "4006381333931", scan())
            .await
            .unwrap()
            .unwrap();
    }
    let (view, _) = t.carts.add_code(ctx, "case12", scan()).await.unwrap().unwrap();
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
    assert!(t.carts.add_code(ctx, "999999", scan()).await.unwrap().is_none());
    assert_eq!(t.carts.current(ctx).await.unwrap().cart.revision, 4);
}

#[tokio::test]
async fn the_cart_survives_a_restart() {
    let t = till(common::shop().await);
    cola(&t).await;
    let ctx = &t.shop.owner;
    t.carts.add_code(ctx, "COLA", AddVia::ManualCode).await.unwrap();
    let before = t.carts.current(ctx).await.unwrap();
    // A brand-new service over the same database (the app restarted): POS-011.
    let restarted = till_from(&t);
    let after = restarted.current(ctx).await.unwrap();
    assert_eq!(after.cart.id, before.cart.id);
    assert_eq!(after.cart.lines[0].quantity, before.cart.lines[0].quantity);
    assert_eq!(after.priced.total, before.priced.total);
}

fn till_from(t: &Till) -> CartService {
    let s = t.shop.store.clone();
    CartService {
        store: Arc::new(s.clone()),
        carts: Arc::new(s.clone()),
        catalog: Arc::new(s),
    }
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
    let view = t
        .carts
        .set_line_discount(ctx, line, Some(Discount::percent(RatePpm::new(100_000).unwrap())))
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
    // Fractional quantity for "ea" is refused.
    let err = t
        .carts
        .set_quantity(ctx, line, Quantity::parse_decimal("1.5").unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::ValidationFailed);
    let view = t
        .carts
        .override_price(ctx, line, Money::from_minor(100))
        .await
        .unwrap();
    assert_eq!(view.cart.lines[0].price_override, Some(Money::from_minor(100)));
    // Audited: two discounts and the override.
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action IN ('cart.discount_applied', 'cart.price_overridden')")
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(audits, 3);
    // Carts are local only: nothing in the outbox (ADR-009).
    let synced: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox_events WHERE aggregate_type = 'cart'")
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(synced, 0);
}

#[tokio::test]
async fn clear_needs_the_current_revision_and_void_needs_permission() {
    let t = till(common::shop().await);
    cola(&t).await;
    let owner = &t.shop.owner;
    t.carts.add_code(owner, "COLA", AddVia::ManualCode).await.unwrap();
    let err = t.carts.clear(owner, 0).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::CartRevisionMismatch);
    let view = t.carts.clear(owner, 1).await.unwrap();
    assert!(view.cart.lines.is_empty());

    t.carts.add_code(owner, "COLA", AddVia::ManualCode).await.unwrap();
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
    let voided: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM carts WHERE status = 'voided' AND void_reason = 'customer left'",
    )
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap();
    assert_eq!(voided, 1);
}
