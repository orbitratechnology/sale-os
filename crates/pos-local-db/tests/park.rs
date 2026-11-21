//! POS-012: parking and resuming carts. A parked cart keeps its lines; the till serves the
//! next customer; any cashier resumes it at today's prices; discarding one is a void
//! (approval and reason for a cashier); all of it is audited.

mod common;

use pos_application::cart::{CartService, MAX_PARKED};
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_domain::cart::AddVia;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, Role, VariantId};
use std::sync::Arc;

async fn product(shop: &common::Shop, name: &str, minor: i64) -> VariantId {
    let a = Arc::new(shop.store.clone());
    CatalogService {
        store: a.clone(),
        catalog: a,
    }
    .create(
        &shop.owner,
        ProductInput {
            name: name.into(),
            description: None,
            category_id: None,
            product_type: ProductType::Stock,
            unit: Unit::Ea,
            tax_category_id: shop.tax_category,
            quick_select: false,
            variants: vec![VariantInput::simple(Money::from_minor(minor))],
        },
    )
    .await
    .unwrap()
    .variants[0]
        .id
}

fn carts(shop: &common::Shop) -> Arc<CartService> {
    let a = Arc::new(shop.store.clone());
    Arc::new(CartService::new(a.clone(), a.clone(), a))
}

/// A real cashier (the cart tables reference staff).
async fn staff(shop: &common::Shop, name: &str) -> pos_application::context::CommandContext {
    let ctx = shop.as_role(Role::Cashier);
    let user = ctx.actor.as_ref().unwrap().user_id;
    sqlx::query(
        "INSERT INTO staff_users (user_id, business_id, display_name, role, pin_hash, pin_updated_at, created_at, updated_at)
         VALUES (?, ?, ?, 'cashier', 'x', 0, 0, 0)",
    )
    .bind(user.to_string())
    .bind(shop.owner.register.business_id.to_string())
    .bind(name)
    .execute(shop.store.db().writer())
    .await
    .unwrap();
    ctx
}

async fn audits(shop: &common::Shop, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = ?")
        .bind(action)
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap()
}

#[tokio::test]
async fn park_serve_someone_else_then_resume_and_sell() {
    let shop = common::shop().await;
    let tea = product(&shop, "Tea", 250).await;
    let milk = product(&shop, "Milk", 120).await;
    let c = carts(&shop);
    let cashier = staff(&shop, "Ada").await;

    // An empty cart cannot be parked.
    let empty = c.current(&cashier).await.unwrap();
    let e = c.park(&cashier, empty.cart.revision, None).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::CartEmpty);

    // Customer A forgot their wallet: park their two teas.
    c.add_variant(&cashier, tea, Some(Quantity::from_milli(2000)), AddVia::Search)
        .await
        .unwrap();
    let a = c.current(&cashier).await.unwrap();
    let a_id = a.cart.id;
    let fresh = c
        .park(&cashier, a.cart.revision, Some("  blue coat "))
        .await
        .unwrap();
    assert!(fresh.cart.lines.is_empty());
    assert_ne!(fresh.cart.id, a_id);
    let parked = c.parked(&cashier).await.unwrap();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].label.as_deref(), Some("blue coat"));
    assert_eq!((parked[0].lines, parked[0].total), (1, Money::from_minor(500)));
    assert_eq!(parked[0].parked_by_name, "Ada");

    // Customer B buys milk meanwhile.
    c.add_variant(&cashier, milk, None, AddVia::Search).await.unwrap();
    let b = c.current(&cashier).await.unwrap();
    // Resuming while B's cart has lines is refused (park or finish it first).
    let e = c.resume(&cashier, a_id).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    let a_store = Arc::new(shop.store.clone());
    let checkout = CheckoutService {
        carts: c.clone(),
        store: a_store.clone(),
        setup: a_store.clone(),
        stock: a_store.clone(),
        receipts: a_store,
    };
    let pay = |id, revision, total| CompleteSale {
        cart_id: id,
        revision,
        tenders: vec![TenderInput {
            kind: TenderKind::Cash,
            amount: total,
            external_reference: None,
        }],
    };
    checkout
        .complete_sale(&cashier, pay(b.cart.id, b.cart.revision, b.priced.total))
        .await
        .unwrap();

    // A comes back, at today's price: tea went up to 3.00 meanwhile.
    sqlx::query("UPDATE product_variants SET price_minor = 300 WHERE variant_id = ?")
        .bind(tea.to_string())
        .execute(shop.store.db().writer())
        .await
        .unwrap();
    // Another cashier resumes it (an empty open cart is simply closed).
    let other = staff(&shop, "Bo").await;
    c.current(&other).await.unwrap();
    let back = c.resume(&other, a_id).await.unwrap();
    assert_eq!(back.cart.id, a_id);
    assert_eq!(back.cart.lines.len(), 1);
    assert_eq!(back.priced.total, Money::from_minor(600));
    assert!(c.parked(&other).await.unwrap().is_empty());
    // Resuming twice: the second finds nothing parked.
    let e = c.resume(&other, a_id).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::CartRevisionMismatch);
    let sale = checkout
        .complete_sale(&other, pay(back.cart.id, back.cart.revision, back.priced.total))
        .await
        .unwrap();
    assert_eq!(sale.sale.total, Money::from_minor(600));

    assert_eq!(audits(&shop, "cart.parked").await, 1);
    assert_eq!(audits(&shop, "cart.resumed").await, 1);
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM carts WHERE status IN ('open', 'parked')")
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap();
    assert!(open <= 1, "at most the fresh open cart is left");
}

#[tokio::test]
async fn discarding_needs_approval_and_a_reason_and_parking_is_capped() {
    let shop = common::shop().await;
    let tea = product(&shop, "Tea", 250).await;
    let c = carts(&shop);
    let cashier = staff(&shop, "Ada").await;
    for i in 0..MAX_PARKED {
        c.add_variant(&cashier, tea, None, AddVia::Search).await.unwrap();
        let v = c.current(&cashier).await.unwrap();
        c.park(&cashier, v.cart.revision, Some(&format!("customer {i}")))
            .await
            .unwrap();
    }
    c.add_variant(&cashier, tea, None, AddVia::Search).await.unwrap();
    let v = c.current(&cashier).await.unwrap();
    let e = c.park(&cashier, v.cart.revision, None).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    // A label over 40 characters is refused too.
    let e = c
        .park(&shop.owner, v.cart.revision, Some(&"x".repeat(41)))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);

    let parked = c.parked(&cashier).await.unwrap();
    assert_eq!(parked.len(), MAX_PARKED);
    assert_eq!(parked[0].label.as_deref(), Some("customer 0"), "oldest first");
    let first = parked[0].cart_id;
    // A cashier needs approval to discard; an owner needs a reason.
    let e = c.discard_parked(&cashier, first, "left").await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ApprovalRequired);
    let e = c.discard_parked(&shop.owner, first, " ").await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    let left = c
        .discard_parked(&shop.owner, first, "customer left")
        .await
        .unwrap();
    assert_eq!(left.len(), MAX_PARKED - 1);
    assert_eq!(audits(&shop, "cart.parked_discarded").await, 1);
    let reason: String = sqlx::query_scalar("SELECT void_reason FROM carts WHERE cart_id = ?")
        .bind(first.to_string())
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(reason, "customer left");
    // Discarded once is gone: a second discard finds nothing.
    let e = c.discard_parked(&shop.owner, first, "again").await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::CartRevisionMismatch);
}
