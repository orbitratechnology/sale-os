//! E15-T01: the owner's audit viewer against a real database.

mod common;

use pos_application::audit_log::{AuditFilter, AuditService};
use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::scan::ScanSource;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, Money, Role, ScanEventId, UserId, UuidV7Generator};
use std::sync::Arc;

#[tokio::test]
async fn the_owner_filters_and_pages_the_audit_log() {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let owner = &shop.owner;
    let mut v = VariantInput::simple(Money::from_minor(250));
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
    shop.clock.advance_ms(60_000);
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s.clone()),
    ));
    carts
        .scan(
            owner,
            "TEA",
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Manual,
        )
        .await
        .unwrap();
    let cart = carts.current(owner).await.unwrap().cart;
    CheckoutService {
        carts,
        store: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        stock: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
    }
    .complete_sale(
        owner,
        CompleteSale {
            cart_id: cart.id,
            revision: cart.revision,
            tenders: vec![TenderInput {
                kind: TenderKind::CardExternal,
                amount: Money::from_minor(250),
                external_reference: Some("AUTH 5".into()),
            }],
        },
    )
    .await
    .unwrap();

    let audit = AuditService {
        audit: Arc::new(s.clone()),
        pricing: Arc::new(s.clone()),
    };
    let all = audit
        .list(owner, &AuditFilter::default(), None, Some(200))
        .await
        .unwrap();
    let actions: Vec<&str> = all.rows.iter().map(|r| r.action.as_str()).collect();
    // Newest first: the sale (with its external tender) after the catalogue and setup rows.
    // Both written in the sale's transaction, at the same instant: either order.
    // (the tea had no stock, so the sale also flagged it).
    let mut newest = actions[..3].to_vec();
    newest.sort_unstable();
    assert_eq!(
        newest,
        [
            "inventory.negative_flagged",
            "sale.completed",
            "tender.recorded_external"
        ]
    );
    assert!(actions.contains(&"catalog.product_created"));
    assert!(actions.contains(&"business.created"));
    assert!(actions.contains(&"location.upserted"));
    let sale = all.rows.iter().find(|r| r.action == "sale.completed").unwrap();
    assert_eq!(
        (sale.entity_type.as_str(), sale.entity_label.as_deref()),
        ("sale", Some("R01-000001"))
    );
    assert_eq!(sale.actor_name.as_deref(), Some("Olu"));
    assert_eq!(sale.local_time, "2026-01-01 00:01");
    let tender = all
        .rows
        .iter()
        .find(|r| r.action == "tender.recorded_external")
        .unwrap();
    let m: serde_json::Value = serde_json::from_str(&tender.metadata).unwrap();
    assert_eq!(
        (m["tender"].as_str(), m["hasReference"].as_bool()),
        (Some("card_external"), Some(true))
    );
    let product = all
        .rows
        .iter()
        .find(|r| r.action == "catalog.product_created")
        .unwrap();
    assert_eq!(product.entity_label.as_deref(), Some("Tea"));

    // Filters: exact action, group prefix, user, dates.
    let f = |action: Option<&str>, user: Option<UserId>, from: Option<&str>, to: Option<&str>| AuditFilter {
        action: action.map(Into::into),
        user,
        from: from.map(|d| d.parse().unwrap()),
        to: to.map(|d| d.parse().unwrap()),
    };
    let list = |filter: AuditFilter| {
        let a = &audit;
        async move { a.list(owner, &filter, None, Some(200)).await.unwrap().rows }
    };
    assert_eq!(list(f(Some("sale.completed"), None, None, None)).await.len(), 1);
    let catalog = list(f(Some("catalog."), None, None, None)).await;
    assert!(!catalog.is_empty() && catalog.iter().all(|r| r.action.starts_with("catalog.")));
    let olu = owner.actor.as_ref().unwrap().user_id;
    assert_eq!(list(f(None, Some(olu), None, None)).await.len(), all.rows.len());
    assert!(
        list(f(None, Some(UserId::generate(&UuidV7Generator)), None, None))
            .await
            .is_empty()
    );
    assert_eq!(
        list(f(None, None, Some("2026-01-01"), Some("2026-01-01")))
            .await
            .len(),
        all.rows.len()
    );
    assert!(list(f(None, None, Some("2026-01-02"), None)).await.is_empty());
    assert!(
        audit
            .list(
                owner,
                &f(None, None, Some("2026-01-02"), Some("2026-01-01")),
                None,
                None
            )
            .await
            .is_err()
    );

    // Pages of 2 cover everything once, in order.
    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = audit
            .list(owner, &AuditFilter::default(), token.as_deref(), Some(2))
            .await
            .unwrap();
        seen.extend(page.rows.into_iter().map(|r| r.audit_id));
        match page.next {
            Some(n) => token = Some(n),
            None => break,
        }
    }
    assert_eq!(
        seen,
        all.rows.iter().map(|r| r.audit_id.clone()).collect::<Vec<_>>()
    );

    // Owner only.
    for role in [Role::Cashier, Role::Manager] {
        assert_eq!(
            audit
                .list(&shop.as_role(role), &AuditFilter::default(), None, None)
                .await
                .unwrap_err()
                .error_code(),
            ErrorCode::PermissionDenied
        );
    }
}
