//! E17-T01: the events the end-of-day scenario does not produce (categories, tax changes,
//! a business profile edit, product edits with barcodes and options, a fixed discount, an
//! audit-only event) match the sync contract too; together the two runs cover the whole
//! catalogue.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::config::{ConfigService, RateInput};
use pos_application::export::{Dataset, ExportService};
use pos_domain::cart::AddVia;
use pos_domain::catalog::config::BusinessProfile;
use pos_domain::catalog::{BarcodeInput, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::pricing::Discount;
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{Money, Quantity, RatePpm};
use std::collections::BTreeSet;
use std::sync::Arc;

/// What `end_of_day.rs` checks against the contract (it asserts it produces these).
const FROM_THE_DAY: [&str; 5] = [
    "sale.completed",
    "return.completed",
    "inventory.document_posted",
    "cash_session.opened",
    "cash_session.closed",
];

#[tokio::test]
async fn master_data_and_audit_events_match_the_contract() {
    let shop = common::shop_with(&common::coverage::triggers_sql()).await;
    let s = shop.store.clone();
    let owner = &shop.owner;
    let config = ConfigService {
        store: Arc::new(s.clone()),
        config: Arc::new(s.clone()),
    };
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    let drinks = config
        .save_category(owner, None, "Drinks", None, None)
        .await
        .unwrap();
    config
        .save_rate(
            owner,
            None,
            RateInput {
                name: "Reduced 5%".into(),
                rate: RatePpm::new(50_000).unwrap(),
                from: Some("2026-04-01".parse().unwrap()),
                to: None,
            },
            None,
        )
        .await
        .unwrap();
    let profile = config.business_profile(owner).await.unwrap();
    config
        .save_business_profile(
            owner,
            &BusinessProfile {
                name: "Corner Shop".into(),
                legal_name: Some("Corner Shop Ltd".into()),
                tax_registration_no: Some("GB123456789".into()),
                address_lines: vec!["1 High Street".into()],
                phone: None,
                email: Some("hello@corner.example".into()),
                register_name: "Front till".into(),
            },
            profile.business.version,
            profile.register.version,
        )
        .await
        .unwrap();
    // A product with options, a barcode, a cost and a low-stock alert; then an edit.
    let mut small = VariantInput::simple(Money::from_minor(120));
    small.options.insert("Size".into(), "330ml".into());
    small.sku = Some("COLA-330".into());
    small.cost = Some(Money::from_minor(55));
    small.low_stock_threshold = Some(Quantity::from_milli(12_000));
    small.barcodes = vec![BarcodeInput::alias("4006381333931")];
    let mut big = VariantInput::simple(Money::from_minor(200));
    big.options.insert("Size".into(), "1L".into());
    let input = ProductInput {
        name: "Cola".into(),
        description: Some("Fizzy".into()),
        category_id: Some(drinks.category_id),
        product_type: ProductType::Stock,
        unit: Unit::Ea,
        tax_category_id: shop.tax_category,
        quick_select: true,
        variants: vec![small, big],
    };
    let cola = catalog.create(owner, input.clone()).await.unwrap();
    let mut edited = input;
    for (v, existing) in edited.variants.iter_mut().zip(&cola.variants) {
        v.id = Some(existing.id);
    }
    edited.variants[1].price = Money::from_minor(210);
    catalog
        .update(owner, cola.id, cola.version, edited)
        .await
        .unwrap();

    // A sandbox sale with a fixed order discount (no drawer needed in practice mode).
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s.clone()),
    ));
    carts
        .add_variant(
            owner,
            cola.variants[0].id,
            Some(Quantity::from_milli(2000)),
            AddVia::Search,
        )
        .await
        .unwrap();
    carts
        .set_order_discount(
            owner,
            Some(Discount {
                reason: Some("Dented can".into()),
                ..Discount::fixed(Money::from_minor(40))
            }),
        )
        .await
        .unwrap();
    let cart = carts.current(owner).await.unwrap().cart;
    CheckoutService {
        carts: carts.clone(),
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
                kind: TenderKind::QrExternal,
                amount: Money::from_minor(200),
                external_reference: Some("QR 55".into()),
            }],
        },
    )
    .await
    .unwrap();

    // An audit-only event.
    let exports = ExportService {
        store: Arc::new(s.clone()),
        exports: Arc::new(s.clone()),
        reports: Arc::new(s.clone()),
    };
    let export = exports.build(owner, Dataset::Products, None).await.unwrap();
    exports.record(owner, &export).await.unwrap();

    let seen = common::contract::check_outbox(&s).await;
    // E17-T02: every syncable write had its outbox event, in its transaction.
    let written = common::coverage::check(&s).await;
    eprintln!("syncable tables written: {written:?}");
    let covered: BTreeSet<&str> = seen.iter().map(String::as_str).chain(FROM_THE_DAY).collect();
    let catalogue: BTreeSet<&str> = pos_sync::CATALOGUE.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        covered, catalogue,
        "every event type in the contract is exercised by a test"
    );
    // Product edits carry the version they were based on.
    let bases: Vec<Option<i64>> = sqlx::query_scalar(
        "SELECT base_version FROM outbox_events WHERE event_type = 'product.upserted' ORDER BY local_sequence",
    )
    .fetch_all(s.db().readers())
    .await
    .unwrap();
    assert_eq!(bases, vec![Some(0), Some(1)]);
}

/// The coverage check is not vacuous: a syncable write that bypasses the outbox fails it.
#[tokio::test]
#[should_panic(expected = "syncable writes without an outbox event")]
async fn a_write_without_its_outbox_event_fails_the_coverage_check() {
    let shop = common::shop_with(&common::coverage::triggers_sql()).await;
    sqlx::query("UPDATE businesses SET name = 'Sneaky rename'")
        .execute(shop.store.db().writer())
        .await
        .unwrap();
    common::coverage::check(&shop.store).await;
}
