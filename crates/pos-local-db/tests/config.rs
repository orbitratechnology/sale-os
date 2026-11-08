//! E06-T05: categories and tax configuration against a real database.

mod common;

use pos_application::catalog::CatalogService;
use pos_application::config::{ConfigService, RateInput, TaxCategoryInput};
use pos_application::context::AppError;
use pos_application::queries::{CartQueries, CatalogQueries};
use pos_domain::catalog::config::ConfigError;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::{ErrorCode, HasErrorCode, Money, RatePpm, Role};
use std::sync::Arc;

fn service(shop: &common::Shop) -> ConfigService {
    ConfigService {
        store: Arc::new(shop.store.clone()),
        config: Arc::new(shop.store.clone()),
    }
}

#[tokio::test]
async fn categories_form_a_tree_three_levels_deep() {
    let shop = common::shop().await;
    let cfg = service(&shop);
    let owner = &shop.owner;
    let food = cfg.save_category(owner, None, "Food", None, None).await.unwrap();
    let drinks = cfg
        .save_category(owner, None, "Drinks", Some(food.category_id), None)
        .await
        .unwrap();
    let hot = cfg
        .save_category(owner, None, "Hot drinks", Some(drinks.category_id), None)
        .await
        .unwrap();
    let e = cfg
        .save_category(owner, None, "Tea", Some(hot.category_id), None)
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Config(ConfigError::TooDeep)), "{e:?}");
    let e = cfg
        .save_category(
            owner,
            Some(food.category_id),
            "Food",
            Some(hot.category_id),
            Some(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Config(ConfigError::Cycle)), "{e:?}");
    assert_eq!(
        cfg.save_category(owner, None, "  ", None, None)
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::ValidationFailed
    );
    // A rename is versioned.
    let renamed = cfg
        .save_category(
            owner,
            Some(drinks.category_id),
            "Beverages",
            Some(food.category_id),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(renamed.version, 2);
    assert_eq!(
        cfg.save_category(
            owner,
            Some(drinks.category_id),
            "Drinks",
            Some(food.category_id),
            Some(1)
        )
        .await
        .unwrap_err()
        .error_code(),
        ErrorCode::ConflictVersion
    );
    // A category with subcategories cannot be archived; a leaf can.
    assert!(cfg.archive_category(owner, drinks.category_id, 2).await.is_err());
    cfg.archive_category(owner, hot.category_id, 1).await.unwrap();
    cfg.archive_category(owner, drinks.category_id, 2).await.unwrap();
    // Managers edit the catalogue; cashiers do not.
    cfg.save_category(&shop.as_role(Role::Manager), None, "Snacks", None, None)
        .await
        .unwrap();
    assert_eq!(
        cfg.save_category(&shop.as_role(Role::Cashier), None, "X", None, None)
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::PermissionDenied
    );
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM outbox_events WHERE event_type = 'category.upserted'")
            .fetch_one(shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(events, 7);
}

#[tokio::test]
async fn tax_rates_change_on_a_date_and_categories_keep_one_default() {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let cfg = service(&shop);
    let owner = &shop.owner;
    let (rates, categories) = cfg.tax_config(owner).await.unwrap();
    let vat = rates.iter().find(|r| r.name == "VAT 20%").unwrap().clone();
    let standard = categories.iter().find(|c| c.is_default).unwrap().clone();
    assert_eq!(standard.rate_ids, [vat.tax_rate_id]);
    let date = |d: &str| Some(d.parse().unwrap());
    let input = |name: &str, ppm: u32, from: Option<&str>, to: Option<&str>| RateInput {
        name: name.into(),
        rate: RatePpm::new(ppm).unwrap(),
        from: from.and_then(&date),
        to: to.and_then(&date),
    };

    // A second "VAT 20%" for April onwards overlaps the open-ended one.
    let e = cfg
        .save_rate(
            owner,
            None,
            input("VAT 20%", 220_000, Some("2026-04-01"), None),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, AppError::Config(ConfigError::OverlappingRate(_))),
        "{e:?}"
    );
    // End the old rate on 1 April, start the new one that day; the category gets both.
    let old = cfg
        .save_rate(
            owner,
            Some(vat.tax_rate_id),
            input("VAT 20%", 200_000, None, Some("2026-04-01")),
            Some(1),
        )
        .await
        .unwrap();
    let new = cfg
        .save_rate(
            owner,
            None,
            input("VAT 20%", 220_000, Some("2026-04-01"), None),
            None,
        )
        .await
        .unwrap();
    cfg.save_tax_category(
        owner,
        Some(standard.tax_category_id),
        TaxCategoryInput {
            name: standard.name.clone(),
            rates: vec![old.tax_rate_id, new.tax_rate_id],
            is_default: true,
        },
        Some(standard.version),
    )
    .await
    .unwrap();
    let e = cfg
        .save_rate(
            owner,
            None,
            input("Bad", 0, Some("2026-05-01"), Some("2026-04-01")),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Config(ConfigError::PeriodBackwards)));

    // Pricing uses the rate for the sale's business date.
    let p = CatalogService {
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
            tax_category_id: standard.tax_category_id,
            quick_select: false,
            variants: vec![VariantInput::simple(Money::from_minor(250))],
        },
    )
    .await
    .unwrap();
    let rate_on = |d: &str| {
        let s = s.clone();
        let v = p.variants[0].id;
        let d = d.parse().unwrap();
        async move {
            s.sellable(v, d)
                .await
                .unwrap()
                .unwrap()
                .taxes
                .iter()
                .map(|t| t.rate.ppm())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(rate_on("2026-03-31").await, [200_000]);
    assert_eq!(rate_on("2026-04-01").await, [220_000]);

    // A rate in use cannot be archived; the error names the categories.
    let e = cfg
        .archive_rate(owner, old.tax_rate_id, old.version)
        .await
        .unwrap_err();
    match e {
        AppError::Config(ConfigError::RateInUse(names)) => {
            assert_eq!(names, std::slice::from_ref(&standard.name))
        }
        other => panic!("{other:?}"),
    }

    // A new default takes the flag from the old one in the same transaction.
    let zero_rate = cfg
        .save_rate(owner, None, input("Zero", 0, None, None), None)
        .await
        .unwrap();
    let zero = cfg
        .save_tax_category(
            owner,
            None,
            TaxCategoryInput {
                name: "Zero-rated".into(),
                rates: vec![zero_rate.tax_rate_id],
                is_default: true,
            },
            None,
        )
        .await
        .unwrap();
    let (_, categories) = cfg.tax_config(owner).await.unwrap();
    let defaults: Vec<&str> = categories
        .iter()
        .filter(|c| c.is_default)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(defaults, ["Zero-rated"]);
    // Switching the default off without a replacement is refused; so are unknown rates.
    let e = cfg
        .save_tax_category(
            owner,
            Some(zero.tax_category_id),
            TaxCategoryInput {
                name: "Zero-rated".into(),
                rates: vec![],
                is_default: false,
            },
            Some(1),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, AppError::Config(ConfigError::DefaultRequired)),
        "{e:?}"
    );
    let e = cfg
        .save_tax_category(
            owner,
            None,
            TaxCategoryInput {
                name: "X".into(),
                rates: vec![pos_domain::TaxRateId::generate(&pos_domain::UuidV7Generator)],
                is_default: false,
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Config(ConfigError::UnknownRate)), "{e:?}");
    // The product editor's list follows: the new default first.
    assert_eq!(s.tax_categories().await.unwrap()[0].name, "Zero-rated");

    // Tax is the owner's.
    for role in [Role::Manager, Role::Cashier] {
        assert_eq!(
            cfg.tax_config(&shop.as_role(role))
                .await
                .unwrap_err()
                .error_code(),
            ErrorCode::PermissionDenied
        );
    }
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'tax.config_updated'")
            .fetch_one(s.db().readers())
            .await
            .unwrap();
    assert!(audited >= 6, "{audited}");
}

fn profile(name: &str, till: &str) -> pos_domain::catalog::config::BusinessProfile {
    pos_domain::catalog::config::BusinessProfile {
        name: name.into(),
        legal_name: Some("Corner Shop Ltd".into()),
        tax_registration_no: Some("GB123456789".into()),
        address_lines: vec!["1 High Street".into(), "Leeds LS1 1AA".into()],
        phone: Some("0113 496 0000".into()),
        email: None,
        register_name: till.into(),
    }
}

#[tokio::test]
async fn the_business_profile_reaches_new_receipts_and_is_audited() {
    use pos_application::queries::ReceiptQueries;
    let shop = common::shop().await;
    let cfg = service(&shop);
    let owner = &shop.owner;
    let count = |sql: &'static str| {
        let store = shop.store.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(store.db().readers())
                .await
                .unwrap()
        }
    };
    let before = cfg.business_profile(owner).await.unwrap();
    let (bv, rv) = (before.business.version, before.register.version);
    let business_events = "SELECT count(*) FROM outbox_events WHERE event_type = 'business.updated'";
    let register_events = "SELECT count(*) FROM outbox_events WHERE event_type = 'register.upserted'";
    let (b0, r0) = (count(business_events).await, count(register_events).await);

    // Owner only.
    for role in [Role::Manager, Role::Cashier] {
        let e = cfg.business_profile(&shop.as_role(role)).await.unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::PermissionDenied);
    }
    // Invalid input names its field and changes nothing.
    let e = cfg
        .save_business_profile(owner, &profile(" ", "Till 1"), bv, rv)
        .await
        .unwrap_err();
    assert!(
        matches!(e, AppError::Config(ConfigError::Required("name"))),
        "{e:?}"
    );

    let saved = cfg
        .save_business_profile(owner, &profile("Corner Shop & Deli", "Front till"), bv, rv)
        .await
        .unwrap();
    assert_eq!(saved.business.name, "Corner Shop & Deli");
    assert_eq!(saved.business.version, bv + 1);
    assert_eq!(saved.register.name, "Front till");
    assert_eq!(saved.register.version, rv + 1);
    assert_eq!(saved.address_lines(), vec!["1 High Street", "Leeds LS1 1AA"]);
    // Country, zone and currency are untouched.
    assert_eq!(
        (
            &saved.business.country_code,
            &saved.business.timezone,
            saved.business.currency
        ),
        (
            &before.business.country_code,
            &before.business.timezone,
            before.business.currency
        )
    );
    // New receipts print the new details.
    let actor = owner.actor.as_ref().unwrap();
    let info = shop
        .store
        .receipt_info(owner.register.register_id, actor.user_id)
        .await
        .unwrap();
    assert_eq!(info.business_name, "Corner Shop & Deli");
    assert_eq!(info.address_lines, vec!["1 High Street", "Leeds LS1 1AA"]);
    assert_eq!(info.tax_registration_no.as_deref(), Some("GB123456789"));
    assert_eq!(info.register_name, "Front till");
    // One transaction: both events, both audit rows (the business row lists what changed).
    assert_eq!(count(business_events).await, b0 + 1);
    assert_eq!(count(register_events).await, r0 + 1);
    let changed: String =
        sqlx::query_scalar("SELECT metadata_json FROM audit_events WHERE action = 'business.updated'")
            .fetch_one(shop.store.db().readers())
            .await
            .unwrap();
    let changed: serde_json::Value = serde_json::from_str(&changed).unwrap();
    assert_eq!(
        changed["changed"],
        serde_json::json!(["name", "legalName", "taxRegistrationNo", "addressLines", "phone"])
    );
    let renamed: String =
        sqlx::query_scalar("SELECT metadata_json FROM audit_events WHERE action = 'register.updated'")
            .fetch_one(shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&renamed).unwrap(),
        serde_json::json!({"from": before.register.name, "to": "Front till"})
    );

    // Saving the same values writes nothing.
    let again = cfg
        .save_business_profile(
            owner,
            &profile("Corner Shop & Deli", "Front till"),
            bv + 1,
            rv + 1,
        )
        .await
        .unwrap();
    assert_eq!(again.business.version, bv + 1);
    assert_eq!(
        count("SELECT count(*) FROM audit_events WHERE action IN ('business.updated', 'register.updated')")
            .await,
        2
    );
    // A stale version is refused.
    let e = cfg
        .save_business_profile(owner, &profile("Other", "Front till"), bv, rv + 1)
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ConflictVersion);
    // Only the till renamed: only the register changes.
    let renamed = cfg
        .save_business_profile(owner, &profile("Corner Shop & Deli", "Till A"), bv + 1, rv + 1)
        .await
        .unwrap();
    assert_eq!(
        (renamed.business.version, renamed.register.version),
        (bv + 1, rv + 2)
    );
    assert_eq!(count(business_events).await, b0 + 1);
    assert_eq!(count(register_events).await, r0 + 2);
}
