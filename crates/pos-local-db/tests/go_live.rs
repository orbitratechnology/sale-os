//! E04-T02: the owner switches a till from sandbox to live with their PIN (PRD §7.1).

mod common;

use pos_application::cart::CartService;
use pos_application::cash::CashService;
use pos_application::catalog::CatalogService;
use pos_application::mode::ModeService;
use pos_application::queries::SetupQueries;
use pos_application::records::RegisterMode;
use pos_application::scan::ScanSource;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::staff::Pin;
use pos_domain::{ErrorCode, HasErrorCode, Money, Role, ScanEventId, UuidV7Generator};
use std::sync::Arc;

#[tokio::test]
async fn only_the_owner_with_their_pin_and_nothing_in_progress() {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let owner = &shop.owner;
    let mode = ModeService {
        store: Arc::new(s.clone()),
        staff: Arc::new(s.clone()),
        hasher: shop.hasher.clone(),
        setup: Arc::new(s.clone()),
        carts: Arc::new(s.clone()),
        cash: Arc::new(s.clone()),
    };
    let pin = |p: &str| Pin::parse(p).unwrap();
    let current = || async { s.device_identity().await.unwrap().unwrap().mode };
    assert_eq!(current().await, RegisterMode::Sandbox);

    for role in [Role::Cashier, Role::Manager] {
        let e = mode.go_live(&shop.as_role(role), pin("1234")).await.unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::PermissionDenied);
    }

    // A practice cart in progress, then a practice drawer session: both refuse.
    let mut v = VariantInput::simple(Money::from_minor(100));
    v.sku = Some("TEA".into());
    CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    }
    .create(
        owner,
        ProductInput {
            name: "Tea".into(),
            description: None,
            category_id: None,
            product_type: ProductType::Stock,
            unit: Unit::Ea,
            tax_category_id: shop.tax_category,
            quick_select: false,
            variants: vec![v],
        },
    )
    .await
    .unwrap();
    let carts = CartService::new(Arc::new(s.clone()), Arc::new(s.clone()), Arc::new(s.clone()));
    carts
        .scan(
            owner,
            "TEA",
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Manual,
        )
        .await
        .unwrap();
    assert_eq!(
        mode.go_live(owner, pin("1234")).await.unwrap_err().error_code(),
        ErrorCode::ValidationFailed
    );
    let cart = carts.current(owner).await.unwrap().cart;
    carts.clear(owner, cart.revision).await.unwrap();
    let cash = CashService {
        store: Arc::new(s.clone()),
        cash: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        pricing: Arc::new(s.clone()),
    };
    cash.open(owner, Money::from_minor(1000)).await.unwrap();
    assert!(mode.go_live(owner, pin("1234")).await.is_err());
    cash.close(owner, Money::from_minor(1000), None).await.unwrap();

    // A wrong PIN counts towards the lockout like a sign-in, and changes nothing.
    match mode.go_live(owner, pin("9999")).await.unwrap_err().error_code() {
        ErrorCode::AuthPinInvalid => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(current().await, RegisterMode::Sandbox);
    let failed: i64 = sqlx::query_scalar("SELECT failed_attempts FROM staff_login_throttle")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    assert_eq!(failed, 1);

    mode.go_live(owner, pin("1234")).await.unwrap();
    assert_eq!(current().await, RegisterMode::Live);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'mode.changed' AND json_extract(metadata_json, '$.to') = 'live'",
    )
    .fetch_one(s.db().readers())
    .await
    .unwrap();
    assert_eq!(audited, 1);
    // One way only.
    assert_eq!(
        mode.go_live(owner, pin("1234")).await.unwrap_err().error_code(),
        ErrorCode::ValidationFailed
    );
    // The successful PIN reset the counter.
    let failed: i64 = sqlx::query_scalar("SELECT failed_attempts FROM staff_login_throttle")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    assert_eq!(failed, 0);
}
