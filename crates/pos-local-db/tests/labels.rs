//! BAR-009: scale labels. With `scanner.label_rules` set, a label's weight or price is read
//! from its barcode: the product is found by the label's code with the value as zeros; each
//! label is its own line; the quantity cannot be edited; a price label sells at exactly the
//! printed price, and the sale keeps it.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::scan::{ScanOutcome, ScanSource};
use pos_application::settings::SettingsService;
use pos_domain::cart::CartLabel;
use pos_domain::catalog::{
    BarcodeInput, BarcodeQualifier, ProductInput, ProductType, Unit, VariantInput, gtin_check_digit,
};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, ScanEventId, UuidV7Generator};
use std::sync::Arc;

fn code(body12: &str) -> String {
    let d: Vec<u8> = body12.bytes().map(|b| b - b'0').collect();
    format!("{body12}{}", gtin_check_digit(&d))
}

async fn product(shop: &common::Shop, name: &str, unit: Unit, minor: i64, barcode: &str) {
    let a = Arc::new(shop.store.clone());
    let mut v = VariantInput::simple(Money::from_minor(minor));
    v.barcodes.push(BarcodeInput {
        raw: barcode.into(),
        symbology_hint: None,
        qualifier: BarcodeQualifier::Primary,
        pack_qty: Quantity::ONE,
    });
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
            unit,
            tax_category_id: shop.tax_category,
            quick_select: false,
            variants: vec![v],
        },
    )
    .await
    .unwrap();
}

async fn scan(
    c: &CartService,
    shop: &common::Shop,
    raw: &str,
) -> Result<ScanOutcome, pos_application::context::AppError> {
    c.scan(
        &shop.owner,
        raw,
        ScanEventId::generate(&UuidV7Generator),
        ScanSource::Scanner,
    )
    .await
}

#[tokio::test]
async fn weight_and_price_labels_sell_what_the_label_says() {
    let shop = common::shop().await;
    // Cheese at 12.50/kg (base code 25 01234 00000 C); a deli tub at 4.00 each (21 00042 …).
    product(&shop, "Cheddar", Unit::Kg, 1250, &code("250123400000")).await;
    product(&shop, "Olives tub", Unit::Ea, 400, &code("210004200000")).await;
    let a = Arc::new(shop.store.clone());
    let settings = SettingsService {
        store: a.clone(),
        settings: a.clone(),
    };
    let c = Arc::new(CartService::new(a.clone(), a.clone(), a.clone()));

    // No rules yet: a label is an unknown code.
    let label = code("250123400734");
    assert!(matches!(
        scan(&c, &shop, &label).await.unwrap(),
        ScanOutcome::NotFound { .. }
    ));

    // A bad layout is refused; then the shop's layouts.
    let bad = settings
        .update(
            &shop.owner,
            "scanner.label_rules",
            serde_json::json!(["25 kilos 5"]),
            1,
        )
        .await
        .unwrap_err();
    assert_eq!(bad.error_code(), ErrorCode::ValidationFailed);
    settings
        .update(
            &shop.owner,
            "scanner.label_rules",
            serde_json::json!(["25 weight 5", "21 price 5"]),
            1,
        )
        .await
        .unwrap();

    // 734 g of cheddar: 0.734 kg × 12.50 = 9.18 (9.175 rounded half up).
    let ScanOutcome::Added { view, .. } = scan(&c, &shop, &label).await.unwrap() else {
        panic!("not added")
    };
    let cheese = &view.cart.lines[0];
    assert_eq!(cheese.quantity, Quantity::from_milli(734));
    assert_eq!(cheese.label, Some(CartLabel::Weight));
    assert_eq!(view.priced.lines[0].gross, Money::from_minor(918));
    // A second label of the same cheese is a second line (another package).
    let ScanOutcome::Added { view, .. } = scan(&c, &shop, &code("250123400250")).await.unwrap() else {
        panic!("not added")
    };
    assert_eq!(view.cart.lines.len(), 2);
    // The tub labelled 3.99 (a markdown on the scale): sold at exactly 3.99, quantity 1.
    let ScanOutcome::Added { view, .. } = scan(&c, &shop, &code("210004200399")).await.unwrap() else {
        panic!("not added")
    };
    let tub = &view.cart.lines[2];
    assert_eq!(tub.quantity, Quantity::ONE);
    assert_eq!(
        tub.label,
        Some(CartLabel::Price {
            price: Money::from_minor(399)
        })
    );
    assert_eq!(view.priced.lines[2].gross, Money::from_minor(399));
    // A label's quantity cannot be changed.
    let e = c
        .set_quantity(&shop.owner, cheese.id, Quantity::from_milli(1000))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    // A weight label on an item sold each is refused, and changes nothing.
    let e = scan(&c, &shop, &code("250004200500")).await;
    assert!(
        matches!(e, Ok(ScanOutcome::NotFound { .. })),
        "no product has that base code"
    );
    settings
        .update(
            &shop.owner,
            "scanner.label_rules",
            serde_json::json!(["25 weight 5", "21 weight 5"]),
            2,
        )
        .await
        .unwrap();
    let e = scan(&c, &shop, &code("210004200500")).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ScanInvalid);

    // The lines survive a reload (the cart is stored), and the sale keeps the label prices.
    let view = c.current(&shop.owner).await.unwrap();
    assert_eq!(view.cart.lines.len(), 3);
    assert_eq!(
        view.cart.lines[2].label,
        Some(CartLabel::Price {
            price: Money::from_minor(399)
        })
    );
    let total = view.priced.total;
    assert_eq!(total, Money::from_minor(918 + 313 + 399), "250 g = 3.125 → 3.13");
    let sale = CheckoutService {
        carts: c.clone(),
        store: a.clone(),
        setup: a.clone(),
        stock: a.clone(),
        receipts: a,
    }
    .complete_sale(
        &shop.owner,
        CompleteSale {
            cart_id: view.cart.id,
            revision: view.cart.revision,
            tenders: vec![TenderInput {
                kind: TenderKind::Cash,
                amount: total,
                external_reference: None,
            }],
        },
    )
    .await
    .unwrap();
    assert_eq!(sale.sale.total, total);
    assert_eq!(sale.sale.lines[2].gross, Money::from_minor(399));
    assert!(
        !sale.sale.lines[2].price_overridden,
        "a label price is not a price override"
    );
}
