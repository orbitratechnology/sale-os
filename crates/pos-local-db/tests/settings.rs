//! E04-T03: typed settings against a real database.

mod common;

use pos_application::cart::CartService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::queries::CartQueries;
use pos_application::settings::{REGISTRY, SettingsService};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{CartId, ErrorCode, HasErrorCode, Money, Role, UuidV7Generator};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn owners_change_settings_one_versioned_audited_write_at_a_time() {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let owner = &shop.owner;
    let settings = SettingsService {
        store: Arc::new(s.clone()),
        settings: Arc::new(s.clone()),
    };

    // Setup stored every registered default, at version 1.
    let all = settings.list(owner).await.unwrap();
    assert_eq!(all.len(), REGISTRY.len());
    assert!(
        all.iter().all(|v| v.version == 1),
        "{:?}",
        all.iter().map(|v| (v.def.key, v.version)).collect::<Vec<_>>()
    );
    let limit = all
        .iter()
        .find(|v| v.def.key == "discount.cashier_max_ppm")
        .unwrap();
    assert_eq!(limit.value, json!(100_000));

    // A change is versioned: the version seen must still be current.
    let changed = settings
        .update(owner, "discount.cashier_max_ppm", json!(200_000), 1)
        .await
        .unwrap();
    assert_eq!(changed.version, 2);
    assert_eq!(
        s.pricing_settings().await.unwrap().discount_limit_ppm,
        200_000,
        "the till uses it"
    );
    let stale = settings
        .update(owner, "discount.cashier_max_ppm", json!(50_000), 1)
        .await
        .unwrap_err();
    assert_eq!(stale.error_code(), ErrorCode::ConflictVersion);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'settings.updated'
           AND json_extract(metadata_json, '$.from') = 100000 AND json_extract(metadata_json, '$.to') = 200000",
    )
    .fetch_one(s.db().readers())
    .await
    .unwrap();
    assert_eq!(audited, 1);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events WHERE event_type = 'setting.updated' AND aggregate_id LIKE '%discount.cashier_max_ppm%'",
    )
    .fetch_one(s.db().readers())
    .await
    .unwrap();
    assert_eq!(events, 2, "setup and the change");

    // Unknown keys and wrong types are refused; only the owner may change settings.
    for (key, value) in [
        ("nope", json!(1)),
        ("receipt.paper_width_mm", json!(70)),
        ("inventory.allow_negative", json!("no")),
    ] {
        assert_eq!(
            settings
                .update(owner, key, value, 1)
                .await
                .unwrap_err()
                .error_code(),
            ErrorCode::ValidationFailed
        );
    }
    for role in [Role::Cashier, Role::Manager] {
        assert_eq!(
            settings.list(&shop.as_role(role)).await.unwrap_err().error_code(),
            ErrorCode::PermissionDenied
        );
    }

    // Preferences reach everyone signed in.
    settings
        .update(owner, "scanner.suffix", json!("Tab"), 1)
        .await
        .unwrap();
    settings
        .update(owner, "security.lock_timeout_secs", json!(60), 1)
        .await
        .unwrap();
    let prefs = settings.preferences(&shop.as_role(Role::Cashier)).await.unwrap();
    assert_eq!(
        (
            prefs.scanner_suffix.as_str(),
            prefs.scanner_max_key_gap_ms,
            prefs.lock_timeout_secs
        ),
        ("Tab", 35, 60)
    );

    // payments.cashier_external_tender off: a cashier cannot record a card payment.
    settings
        .update(owner, "payments.cashier_external_tender", json!(false), 1)
        .await
        .unwrap();
    let checkout = CheckoutService {
        carts: Arc::new(CartService::new(
            Arc::new(s.clone()),
            Arc::new(s.clone()),
            Arc::new(s.clone()),
        )),
        store: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        stock: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
    };
    let card = || CompleteSale {
        cart_id: CartId::generate(&UuidV7Generator),
        revision: 0,
        tenders: vec![TenderInput {
            kind: TenderKind::CardExternal,
            amount: Money::from_minor(100),
            external_reference: None,
        }],
    };
    assert_eq!(
        checkout
            .complete_sale(&shop.as_role(Role::Cashier), card())
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::PermissionDenied
    );
    // A manager still may (and gets past the rule to the cart check).
    assert_ne!(
        checkout
            .complete_sale(&shop.as_role(Role::Manager), card())
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::PermissionDenied
    );
}
