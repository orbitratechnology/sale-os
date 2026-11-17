//! E05-T04 (manager approval tokens) and E08-T02 (discount limits and permissions), against
//! a real database.

mod common;

use pos_application::approval::{APPROVAL_TTL_MS, Approvals};
use pos_application::auth::AuthService;
use pos_application::cart::{CartService, CartView};
use pos_application::catalog::CatalogService;
use pos_application::context::{Actor, AppError, ApprovalToken, CommandContext};
use pos_application::staff::{NewStaff, StaffService};
use pos_domain::cart::AddVia;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::pricing::Discount;
use pos_domain::staff::Pin;
use pos_domain::{
    ApprovalScope, ErrorCode, HasErrorCode, Money, Permission, RatePpm, Role, SessionId, UserId,
    UuidV7Generator, VariantId,
};
use std::sync::Arc;

struct Till {
    shop: common::Shop,
    carts: CartService,
    auth: AuthService,
    approvals: Arc<Approvals>,
    cashier: CommandContext,
    manager: UserId,
    other_cashier: UserId,
}

const MANAGER_PIN: &str = "2222";

async fn till() -> Till {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let staff = StaffService {
        store: Arc::new(s.clone()),
        staff: Arc::new(s.clone()),
        hasher: shop.hasher.clone(),
    };
    let mut ids = Vec::new();
    for (name, role, pin) in [
        ("Mia", Role::Manager, MANAGER_PIN),
        ("Cal", Role::Cashier, "3333"),
        ("Cat", Role::Cashier, "4444"),
    ] {
        let r = staff
            .create(
                &shop.owner,
                NewStaff {
                    display_name: name.into(),
                    role,
                    pin: Pin::parse(pin).unwrap(),
                },
            )
            .await
            .unwrap();
        ids.push(r.user_id);
    }
    let approvals = Arc::new(Approvals::default());
    let mut cashier = shop.owner.clone();
    cashier.actor = Some(Actor {
        user_id: ids[1],
        role: Role::Cashier,
        session_id: SessionId::generate(&UuidV7Generator),
    });
    Till {
        carts: CartService::new(Arc::new(s.clone()), Arc::new(s.clone()), Arc::new(s.clone())),
        auth: AuthService {
            store: Arc::new(s.clone()),
            staff: Arc::new(s),
            hasher: shop.hasher.clone(),
            approvals: approvals.clone(),
        },
        approvals,
        cashier,
        manager: ids[0],
        other_cashier: ids[2],
        shop,
    }
}

/// A product at `price_minor`, added once to the cart. Returns the variant.
async fn item(t: &Till, name: &str, price_minor: i64) -> VariantId {
    let catalog = CatalogService {
        store: Arc::new(t.shop.store.clone()),
        catalog: Arc::new(t.shop.store.clone()),
    };
    let p = catalog
        .create(
            &t.shop.owner,
            ProductInput {
                name: name.into(),
                description: None,
                category_id: None,
                product_type: ProductType::Stock,
                unit: Unit::Ea,
                tax_category_id: t.shop.tax_category,
                quick_select: false,
                variants: vec![VariantInput::simple(Money::from_minor(price_minor))],
            },
        )
        .await
        .unwrap();
    let v = p.variants[0].id;
    t.carts
        .add_variant(&t.cashier, v, None, AddVia::Search)
        .await
        .unwrap();
    v
}

fn percent(p: u32, reason: Option<&str>) -> Discount {
    let mut d = Discount::percent(RatePpm::new(p * 10_000).unwrap());
    d.reason = reason.map(Into::into);
    d
}

fn with(ctx: &CommandContext, token: ApprovalToken) -> CommandContext {
    let mut c = ctx.clone();
    c.approval = Some(token);
    c
}

async fn approve(t: &Till, scope: ApprovalScope) -> ApprovalToken {
    t.auth
        .request_approval(&t.cashier, t.manager, Pin::parse(MANAGER_PIN).unwrap(), scope)
        .await
        .unwrap()
}

async fn count(t: &Till, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap()
}

fn line(v: &CartView) -> pos_domain::CartLineId {
    v.cart.lines[0].id
}

#[tokio::test]
async fn a_cashier_above_the_limit_needs_a_reason_then_an_approval_for_that_rate() {
    let t = till().await;
    item(&t, "Kettle", 2000).await;
    let cart = t.carts.current(&t.cashier).await.unwrap();
    let l = line(&cart);
    // At the limit (10%): fine, no reason needed.
    t.carts
        .set_line_discount(&t.cashier, l, Some(percent(10, None)))
        .await
        .unwrap();
    // Above it without a reason: asked for the reason first, before bothering a manager.
    let e = t
        .carts
        .set_line_discount(&t.cashier, l, Some(percent(15, None)))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    // With a reason: APPROVAL_REQUIRED, scoped to this cart and exactly 15%.
    let e = t
        .carts
        .set_line_discount(&t.cashier, l, Some(percent(15, Some("damaged box"))))
        .await
        .unwrap_err();
    let AppError::ApprovalRequired(scope) = e else {
        panic!("{e:?}")
    };
    assert_eq!(
        scope,
        ApprovalScope {
            permission: Permission::DiscountAboveThreshold,
            entity_id: cart.cart.id.to_string(),
            max_ppm: Some(150_000)
        }
    );
    // Nothing changed.
    assert_eq!(
        t.carts.current(&t.cashier).await.unwrap().priced.total.minor(),
        1800
    );

    let token = approve(&t, scope).await;
    let v = t
        .carts
        .set_line_discount(
            &with(&t.cashier, token),
            l,
            Some(percent(15, Some("damaged box"))),
        )
        .await
        .unwrap();
    assert_eq!(v.priced.total.minor(), 1700);
    // The approver is on the discount and on the audit entry; the grant is audited too.
    assert_eq!(
        v.cart.lines[0].line_discount.as_ref().unwrap().authorized_by,
        Some(t.manager)
    );
    let approved = count(&t, &format!("SELECT count(*) FROM audit_events WHERE action = 'cart.discount_applied' AND approver_user_id = '{}'", t.manager)).await;
    assert_eq!(approved, 1);
    let granted = count(
        &t,
        "SELECT count(*) FROM audit_events WHERE action = 'auth.approval_granted'",
    )
    .await;
    assert_eq!(granted, 1);
    let before_after: String = sqlx::query_scalar(
        "SELECT json_extract(metadata_json, '$.beforeMinor') || '->' || json_extract(metadata_json, '$.afterMinor')
         FROM audit_events WHERE action = 'cart.discount_applied' ORDER BY occurred_at DESC, rowid DESC LIMIT 1",
    )
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap();
    assert_eq!(before_after, "1800->1700");
}

#[tokio::test]
async fn a_token_is_single_use_scoped_and_expires_after_120_s() {
    let t = till().await;
    item(&t, "Kettle", 2000).await;
    let cart = t.carts.current(&t.cashier).await.unwrap();
    let scope = |cart_id: String, ppm| ApprovalScope {
        permission: Permission::DiscountAboveThreshold,
        entity_id: cart_id,
        max_ppm: Some(ppm),
    };
    let l = line(&cart);
    let d = |p| Some(percent(p, Some("loyal customer")));

    // Single use: the registry hands a token out once.
    let token = approve(&t, scope(cart.cart.id.to_string(), 150_000)).await;
    let now = t.shop.clock.as_ref();
    let taken = t.approvals.take(token.id, pos_domain::Clock::now(now)).unwrap();
    let again = t
        .approvals
        .take(token.id, pos_domain::Clock::now(now))
        .unwrap_err();
    assert_eq!(again.error_code(), ErrorCode::ApprovalInvalid);

    // A larger discount than approved, or another cart, is refused.
    let e = t
        .carts
        .set_line_discount(&with(&t.cashier, taken.clone()), l, d(20))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ApprovalInvalid);
    let other = approve(&t, scope("some-other-cart".into(), 150_000)).await;
    let e = t
        .carts
        .set_line_discount(&with(&t.cashier, other), l, d(15))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ApprovalInvalid);

    // Expired after 120 s (fake clock).
    t.shop.clock.advance_ms(APPROVAL_TTL_MS);
    let e = t
        .carts
        .set_line_discount(&with(&t.cashier, taken), l, d(15))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ApprovalInvalid);
    assert!(t.approvals.take(token.id, pos_domain::Clock::now(now)).is_err());
}

#[tokio::test]
async fn only_someone_else_who_holds_the_permission_can_approve() {
    let t = till().await;
    let scope = ApprovalScope {
        permission: Permission::CartVoid,
        entity_id: "cart".into(),
        max_ppm: None,
    };
    // Another cashier does not hold CartVoid.
    let e = t
        .auth
        .request_approval(
            &t.cashier,
            t.other_cashier,
            Pin::parse("4444").unwrap(),
            scope.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ApprovalInvalid);
    // Nobody approves their own request (here: the manager acting as the requester).
    let mut as_manager = t.cashier.clone();
    as_manager.actor = Some(Actor {
        user_id: t.manager,
        role: Role::Manager,
        session_id: SessionId::generate(&UuidV7Generator),
    });
    let e = t
        .auth
        .request_approval(
            &as_manager,
            t.manager,
            Pin::parse(MANAGER_PIN).unwrap(),
            scope.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ApprovalInvalid);
    // A wrong approver PIN counts toward the approver's lockout, like a sign-in.
    let e = t
        .auth
        .request_approval(&t.cashier, t.manager, Pin::parse("9999").unwrap(), scope)
        .await
        .unwrap_err();
    assert_eq!(
        e,
        AppError::PinInvalid {
            remaining_attempts: 4
        }
    );
    let failed = count(
        &t,
        &format!(
            "SELECT failed_attempts FROM staff_login_throttle WHERE user_id = '{}'",
            t.manager
        ),
    )
    .await;
    assert_eq!(failed, 1);
}

#[tokio::test]
async fn managers_go_above_the_limit_with_a_reason_and_no_approval() {
    let t = till().await;
    item(&t, "Kettle", 2000).await;
    let mut manager = t.cashier.clone();
    manager.actor = Some(Actor {
        user_id: t.manager,
        role: Role::Manager,
        session_id: SessionId::generate(&UuidV7Generator),
    });
    let l = line(&t.carts.current(&manager).await.unwrap());
    let e = t
        .carts
        .set_line_discount(&manager, l, Some(percent(30, None)))
        .await
        .unwrap_err();
    assert_eq!(
        e.error_code(),
        ErrorCode::ValidationFailed,
        "a reason is still required"
    );
    let v = t
        .carts
        .set_line_discount(&manager, l, Some(percent(30, Some("end of line"))))
        .await
        .unwrap();
    assert_eq!(v.priced.total.minor(), 1400);
    assert_eq!(
        v.cart.lines[0].line_discount.as_ref().unwrap().authorized_by,
        None,
        "no approval was needed"
    );
}

#[tokio::test]
async fn a_fixed_amount_is_judged_by_its_effective_rate() {
    let t = till().await;
    item(&t, "Kettle", 1000).await;
    let l = line(&t.carts.current(&t.cashier).await.unwrap());
    let mut fixed = Discount::fixed(Money::from_minor(150)); // 15% of 10.00
    fixed.reason = Some("scuffed".into());
    let e = t
        .carts
        .set_line_discount(&t.cashier, l, Some(fixed))
        .await
        .unwrap_err();
    let AppError::ApprovalRequired(scope) = e else {
        panic!("{e:?}")
    };
    assert_eq!(scope.max_ppm, Some(150_000));
    // Rounded up: 1.01 of 10.00 is 10.1%, over a 10% limit.
    let mut just_over = Discount::fixed(Money::from_minor(101));
    just_over.reason = Some("x".into());
    assert!(matches!(
        t.carts.set_line_discount(&t.cashier, l, Some(just_over)).await,
        Err(AppError::ApprovalRequired(_))
    ));
}

#[tokio::test]
async fn an_approved_free_item_has_no_tax() {
    let t = till().await;
    item(&t, "Sample", 1200).await;
    let cart = t.carts.current(&t.cashier).await.unwrap();
    let l = line(&cart);
    let free = Some(percent(100, Some("free sample")));
    let AppError::ApprovalRequired(scope) = t
        .carts
        .set_line_discount(&t.cashier, l, free.clone())
        .await
        .unwrap_err()
    else {
        panic!()
    };
    let token = approve(&t, scope).await;
    let v = t
        .carts
        .set_line_discount(&with(&t.cashier, token), l, free)
        .await
        .unwrap();
    assert_eq!((v.priced.total.minor(), v.priced.tax_total.minor()), (0, 0));
}

#[tokio::test]
async fn an_order_discount_of_10_00_allocates_exactly_across_3_33_3_33_3_34() {
    let t = till().await;
    item(&t, "A", 333).await;
    item(&t, "B", 333).await;
    item(&t, "C", 334).await;
    let mut ten = Discount::fixed(Money::from_minor(1000));
    ten.reason = Some("goodwill".into());
    let v = t
        .carts
        .set_order_discount(&t.shop.owner, Some(ten))
        .await
        .unwrap();
    let allocated: i64 = v
        .priced
        .lines
        .iter()
        .map(|l| l.order_discount_alloc.minor())
        .sum();
    assert_eq!(allocated, 1000);
    assert_eq!(v.priced.total.minor(), 0);
    // Larger than the order: refused whatever the role.
    let mut too_much = Discount::fixed(Money::from_minor(1001));
    too_much.reason = Some("x".into());
    let e = t
        .carts
        .set_order_discount(&t.shop.owner, Some(too_much))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::PricingDiscountExceeds);
}

#[tokio::test]
async fn price_overrides_and_voids_go_through_approval_for_a_cashier() {
    let t = till().await;
    item(&t, "Kettle", 2000).await;
    let cart = t.carts.current(&t.cashier).await.unwrap();
    let l = line(&cart);
    let e = t
        .carts
        .override_price(&t.cashier, l, Money::from_minor(1500), "price match")
        .await
        .unwrap_err();
    let AppError::ApprovalRequired(scope) = e else {
        panic!("{e:?}")
    };
    let token = approve(&t, scope).await;
    let approved = with(&t.cashier, token);
    let e = t
        .carts
        .override_price(&approved, l, Money::from_minor(1500), " ")
        .await
        .unwrap_err();
    assert_eq!(
        e.error_code(),
        ErrorCode::ValidationFailed,
        "a reason is required"
    );
    let v = t
        .carts
        .override_price(&approved, l, Money::from_minor(1500), "price match")
        .await
        .unwrap();
    assert_eq!(v.priced.total.minor(), 1500);

    let rev = v.cart.revision;
    let AppError::ApprovalRequired(scope) = t.carts.void(&t.cashier, rev, "walked out").await.unwrap_err()
    else {
        panic!()
    };
    let token = approve(&t, scope).await;
    t.carts
        .void(&with(&t.cashier, token), rev, "walked out")
        .await
        .unwrap();
    let voids = count(
        &t,
        &format!(
            "SELECT count(*) FROM audit_events WHERE action = 'cart.voided' AND approver_user_id = '{}'",
            t.manager
        ),
    )
    .await;
    assert_eq!(voids, 1);
}
