//! E21-T01/T02 acceptance: every scan from the 2026-11-20 symbology matrix that used to be
//! "not found" now reaches the product the shop registered, through the real scan
//! pipeline; scale and GS1 labels set the line's quantity or price exactly.

mod common;

use pos_application::cart::{CartService, CartView};
use pos_application::catalog::CatalogService;
use pos_application::scan::{ScanHint, ScanOutcome, ScanSource};
use pos_domain::catalog::{BarcodeInput, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, ScanEventId, UuidV7Generator};
use std::sync::Arc;

struct Till {
    shop: common::Shop,
    carts: CartService,
    catalog: CatalogService,
}

fn till(shop: common::Shop) -> Till {
    let s = shop.store.clone();
    Till {
        carts: CartService::new(Arc::new(s.clone()), Arc::new(s.clone()), Arc::new(s.clone())),
        catalog: CatalogService {
            store: Arc::new(s.clone()),
            catalog: Arc::new(s),
        },
        shop,
    }
}

/// A product with the barcodes as the shop typed them from the pack or label.
async fn product(t: &Till, name: &str, unit: Unit, price: i64, barcodes: &[&str]) {
    let mut v = VariantInput::simple(Money::from_minor(price));
    v.barcodes = barcodes.iter().map(|b| BarcodeInput::alias(b)).collect();
    t.catalog
        .create(
            &t.shop.owner,
            ProductInput {
                name: name.into(),
                description: None,
                category_id: None,
                product_type: ProductType::Stock,
                unit,
                tax_category_id: t.shop.tax_category,
                quick_select: false,
                variants: vec![v],
            },
        )
        .await
        .unwrap();
}

async fn scan(t: &Till, raw: &str) -> ScanOutcome {
    t.carts
        .scan(
            &t.shop.owner,
            raw,
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Scanner,
        )
        .await
        .unwrap()
}

fn added_name(o: &ScanOutcome) -> Option<String> {
    match o {
        ScanOutcome::Added { view, .. } => view.cart.lines.last().map(|l| l.variant.name.clone()),
        _ => None,
    }
}

fn view(o: ScanOutcome) -> CartView {
    match o {
        ScanOutcome::Added { view, .. } => view,
        other => panic!("expected Added, got {other:?}"),
    }
}

async fn set_rules(t: &Till, rules: &[&str]) {
    sqlx::query("UPDATE business_settings SET value_json = ? WHERE key = 'scanner.variable_measure_rules'")
        .bind(serde_json::to_string(rules).unwrap())
        .execute(t.shop.store.db().writer())
        .await
        .unwrap();
}

#[tokio::test]
async fn every_formerly_unmatched_scan_now_finds_the_registered_product() {
    let t = till(common::shop().await);
    product(&t, "Ketchup (UPC-A)", Unit::Ea, 199, &["042100005264"]).await;
    product(&t, "Magazine", Unit::Ea, 450, &["9771234567003"]).await;
    product(&t, "Book", Unit::Ea, 2499, &["9780201379624"]).await;
    product(&t, "Bolt (Code 39)", Unit::Ea, 10, &["ABC-1234"]).await;
    product(&t, "Library card (Codabar)", Unit::Ea, 100, &["A40156B"]).await;
    product(&t, "Tea (EAN-13)", Unit::Ea, 250, &["4006381333931"]).await;
    product(&t, "Coffee (GTIN)", Unit::Ea, 899, &["9501101530003"]).await;
    product(&t, "Aspirin (pharma)", Unit::Ea, 350, &["03453120000011"]).await;

    // What scanners actually send, as measured on 2026-11-20 through the wedge emulator.
    let cases = [
        ("04252614", "Ketchup (UPC-A)"),                     // UPC-E, compressed
        ("977123456700303", "Magazine"),                     // EAN-13 + 2 add-on
        ("978020137962452499", "Book"),                      // EAN-13 + 5 add-on
        ("0201379627", "Book"),                              // ISBN-10
        ("*ABC-1234*", "Bolt (Code 39)"),                    // start/stop sent
        ("40156", "Library card (Codabar)"),                 // start/stop stripped
        ("]E04006381333931", "Tea (EAN-13)"),                // AIM prefix
        ("01095011015300031727010110AB12", "Coffee (GTIN)"), // GS1-128
        ("0109501101530003\u{1d}10LOT1\u{1d}21SER", "Coffee (GTIN)"), // GS1 DataBar, GS kept
        ("(01)09501101530003(17)270101", "Coffee (GTIN)"),   // human-readable GS1
        ("https://id.gs1.org/01/09501101530003/10/AB12", "Coffee (GTIN)"), // GS1 Digital Link
        ("01034531200000111727010110ABC\u{1d}21XYZ123", "Aspirin (pharma)"), // GS1 DataMatrix
    ];
    for (raw, expected) in cases {
        let out = scan(&t, raw).await;
        assert_eq!(
            added_name(&out).as_deref(),
            Some(expected),
            "{raw:?} gave {out:?}"
        );
    }
}

#[tokio::test]
async fn the_exact_code_still_wins_and_repeats_still_count() {
    let t = till(common::shop().await);
    // Two products whose codes are related by UPC-E expansion: the scanned form wins.
    product(&t, "Compressed", Unit::Ea, 100, &["04252614"]).await;
    product(&t, "Expanded", Unit::Ea, 200, &["042100005264"]).await;
    assert_eq!(
        added_name(&scan(&t, "04252614").await).as_deref(),
        Some("Compressed")
    );
    assert_eq!(
        added_name(&scan(&t, "042100005264").await).as_deref(),
        Some("Expanded")
    );
    // AT-03: ten scans are ten items.
    for _ in 0..9 {
        scan(&t, "04252614").await;
    }
    let v = view(scan(&t, "042100005264").await);
    let qty: Vec<i64> = v.cart.lines.iter().map(|l| l.quantity.milli()).collect();
    assert_eq!(qty, vec![10_000, 2_000]);
}

#[tokio::test]
async fn scale_labels_set_weight_or_exact_price() {
    let t = till(common::shop().await);
    // Cheese at 7.98/kg, registered by its PLU barcode (value digits zero).
    product(&t, "Cheese", Unit::Kg, 798, &["2012345000001"]).await;
    // Ham at 12.00/kg, by its bare item code.
    product(&t, "Ham", Unit::Kg, 1200, &["54321"]).await;
    set_rules(&t, &["20 PPIIIIIVVVVVC price 2", "21 PPIIIIIVVVVVC weight 3"]).await;

    // Price label 12.34: the line costs exactly 12.34, about 1.546 kg of stock.
    let v = view(scan(&t, "2012345012349").await);
    let line = v.cart.lines.last().unwrap();
    assert_eq!(line.variant.name, "Cheese");
    assert_eq!(line.quantity, Quantity::from_milli(1546));
    assert_eq!(v.priced.lines.last().unwrap().total, Money::from_minor(1234));

    // Weight label 1.500 kg of ham: 18.00 at the catalogue price.
    let code = with_check("215432101500");
    let v = view(scan(&t, &code).await);
    let line = v.cart.lines.last().unwrap();
    assert_eq!(
        (line.variant.name.as_str(), line.quantity),
        ("Ham", Quantity::from_milli(1500))
    );
    assert_eq!(v.priced.lines.last().unwrap().total, Money::from_minor(1800));

    // A second label of the same cheese is its own line, never merged.
    let v = view(scan(&t, "2012345012349").await);
    assert_eq!(v.cart.lines.len(), 3);

    // Without rules the same code is just an unknown EAN-13.
    set_rules(&t, &[]).await;
    assert!(matches!(
        scan(&t, "2012345012349").await,
        ScanOutcome::NotFound { .. }
    ));
}

#[tokio::test]
async fn price_labels_on_items_sold_by_the_each_are_audited_overrides() {
    let t = till(common::shop().await);
    product(&t, "Reduced bread", Unit::Ea, 150, &[&with_check("209999900000")]).await;
    set_rules(&t, &["20 PPIIIIIVVVVVC price 2"]).await;
    let code = with_check("209999900075");
    let v = view(scan(&t, &code).await);
    assert_eq!(v.priced.lines.last().unwrap().total, Money::from_minor(75));
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'cart.price_overridden' AND metadata_json LIKE '%barcode_label%'",
    )
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn gs1_weight_on_fresh_produce_sets_the_quantity() {
    let t = till(common::shop().await);
    product(&t, "Apples", Unit::Kg, 299, &["9501101530003"]).await;
    // GTIN + AI 3103 net weight 0.750 kg.
    let v = view(scan(&t, "01095011015300033103000750").await);
    let line = v.cart.lines.last().unwrap();
    assert_eq!(line.quantity, Quantity::from_milli(750));
    // A weight on an item sold by the each is refused clearly, not silently miscounted.
    let t2 = till(common::shop().await);
    product(&t2, "Bag", Unit::Ea, 100, &["9501101530003"]).await;
    let e = t2
        .carts
        .scan(
            &t2.shop.owner,
            "01095011015300033103000750",
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Scanner,
        )
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ScanInvalid);
}

#[tokio::test]
async fn a_scanner_on_the_wrong_keyboard_layout_still_sells_and_says_why() {
    let t = till(common::shop().await);
    product(&t, "Tea (EAN-13)", Unit::Ea, 250, &["4006381333931"]).await;
    // US-configured scanner typing 4006381333931 on a French AZERTY layout.
    let out = scan(&t, "'àà§\"_&\"\"\"ç\"&").await;
    if let ScanOutcome::Added { hint, .. } = &out {
        assert_eq!(*hint, Some(ScanHint::KeyboardLayout("azerty_fr")));
    }
    // The French layout maps 6 to '-' (Belgian: '§'); try the French form too.
    let fr = scan(&t, "'àà-\"_&\"\"\"ç\"&").await;
    assert_eq!(added_name(&fr).as_deref(), Some("Tea (EAN-13)"), "{fr:?}");
    match fr {
        ScanOutcome::Added { hint, .. } => assert_eq!(hint, Some(ScanHint::KeyboardLayout("azerty_fr"))),
        _ => unreachable!(),
    }
    // Unknown, but the hint still explains the likely cause.
    match scan(&t, "&é\"'(-è_").await {
        ScanOutcome::NotFound { hint, .. } => assert!(matches!(hint, Some(ScanHint::KeyboardLayout(_)))),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn malformed_scans_are_still_refused() {
    let t = till(common::shop().await);
    for bad in ["", "   ", "café\u{7}"] {
        let e = t
            .carts
            .scan(
                &t.shop.owner,
                bad,
                ScanEventId::generate(&UuidV7Generator),
                ScanSource::Scanner,
            )
            .await
            .unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::ScanInvalid, "{bad:?}");
    }
}

/// Appends the GS1 check digit.
fn with_check(body: &str) -> String {
    let d: Vec<u8> = body.bytes().map(|b| b - b'0').collect();
    format!("{body}{}", pos_domain::catalog::gtin_check_digit(&d))
}
