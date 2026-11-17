//! E06-T06a backend: the product list query (filters, paging, summary columns).

mod common;

use pos_application::catalog::CatalogService;
use pos_application::queries::{ProductFilter, ProductStatusFilter};
use pos_domain::catalog::{BarcodeInput, Product, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::{ErrorCode, HasErrorCode, Money, Role};
use std::collections::BTreeMap;
use std::sync::Arc;

fn service(shop: &common::Shop) -> CatalogService {
    CatalogService {
        store: Arc::new(shop.store.clone()),
        catalog: Arc::new(shop.store.clone()),
    }
}

fn input(shop: &common::Shop, name: &str, ptype: ProductType, variants: Vec<VariantInput>) -> ProductInput {
    ProductInput {
        name: name.into(),
        description: None,
        category_id: None,
        product_type: ptype,
        unit: Unit::Ea,
        tax_category_id: shop.tax_category,
        quick_select: false,
        variants,
    }
}

fn variant(sku: &str, price: i64, barcode: Option<&str>) -> VariantInput {
    let mut v = VariantInput::simple(Money::from_minor(price));
    v.sku = Some(sku.into());
    v.barcodes = barcode.map(BarcodeInput::alias).into_iter().collect();
    v
}

/// A T-shirt in 3 sizes × 2 colours (E06-T02), plus simple products and one archived.
async fn catalogue(shop: &common::Shop) -> (CatalogService, Product) {
    let c = service(shop);
    let mut shirt_variants = Vec::new();
    for (i, size) in ["S", "M", "L"].iter().enumerate() {
        for colour in ["Red", "Blue"] {
            let mut v = variant(&format!("TEE-{size}-{colour}"), 1500 + i as i64 * 100, None);
            v.options = BTreeMap::from([("size".into(), (*size).into()), ("colour".into(), colour.into())]);
            shirt_variants.push(v);
        }
    }
    let shirt = c
        .create(
            &shop.owner,
            input(shop, "T-shirt", ProductType::Stock, shirt_variants),
        )
        .await
        .unwrap();
    c.create(
        &shop.owner,
        input(
            shop,
            "Coca-Cola 330ml",
            ProductType::Stock,
            vec![variant("COKE", 120, Some("5449000000996"))],
        ),
    )
    .await
    .unwrap();
    c.create(
        &shop.owner,
        input(
            shop,
            "Café latte",
            ProductType::NonStock,
            vec![variant("LATTE", 290, None)],
        ),
    )
    .await
    .unwrap();
    let old = c
        .create(
            &shop.owner,
            input(
                shop,
                "Discontinued mug",
                ProductType::Stock,
                vec![variant("MUG", 800, None)],
            ),
        )
        .await
        .unwrap();
    c.archive(&shop.owner, old.id, old.version).await.unwrap();
    (c, shirt)
}

fn names(page: &pos_application::queries::ProductPage) -> Vec<&str> {
    page.rows.iter().map(|r| r.name.as_str()).collect()
}

#[tokio::test]
async fn lists_active_products_by_name_with_summaries() {
    let shop = common::shop().await;
    let (c, shirt) = catalogue(&shop).await;
    let page = c
        .list(&shop.owner, &ProductFilter::default(), 0, 50)
        .await
        .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(names(&page), ["Café latte", "Coca-Cola 330ml", "T-shirt"]);
    let tee = &page.rows[2];
    assert_eq!(tee.product_id, shirt.id);
    assert_eq!(
        (tee.variant_count, tee.price_min.minor(), tee.price_max.minor()),
        (6, 1500, 1700)
    );
    assert_eq!(
        tee.sku.as_deref(),
        Some("TEE-S-Red"),
        "the default (first) variant's SKU"
    );
    let coke = &page.rows[1];
    assert_eq!((coke.barcode_count, coke.track_stock), (1, true));
    // A non-stock product never tracks stock (E06-T02).
    assert!(!page.rows[0].track_stock);
}

#[tokio::test]
async fn filters_by_text_status_and_stock_tracking_and_pages() {
    let shop = common::shop().await;
    let (c, _) = catalogue(&shop).await;
    let q = |query: &str| ProductFilter {
        query: query.into(),
        ..ProductFilter::default()
    };
    assert_eq!(
        names(&c.list(&shop.owner, &q("coca"), 0, 50).await.unwrap()),
        ["Coca-Cola 330ml"]
    );
    assert_eq!(
        names(&c.list(&shop.owner, &q("cafe"), 0, 50).await.unwrap()),
        ["Café latte"],
        "diacritics"
    );
    assert_eq!(
        names(&c.list(&shop.owner, &q("TEE-M"), 0, 50).await.unwrap()),
        ["T-shirt"],
        "SKU prefix"
    );
    assert_eq!(
        names(&c.list(&shop.owner, &q("5449000000996"), 0, 50).await.unwrap()),
        ["Coca-Cola 330ml"],
        "barcode"
    );
    assert_eq!(c.list(&shop.owner, &q("--"), 0, 50).await.unwrap().total, 0);

    let archived = ProductFilter {
        status: ProductStatusFilter::Archived,
        ..ProductFilter::default()
    };
    let page = c.list(&shop.owner, &archived, 0, 50).await.unwrap();
    assert_eq!(names(&page), ["Discontinued mug"]);
    assert!(page.rows[0].archived);
    let all = ProductFilter {
        status: ProductStatusFilter::All,
        ..ProductFilter::default()
    };
    assert_eq!(c.list(&shop.owner, &all, 0, 50).await.unwrap().total, 4);
    let untracked = ProductFilter {
        track_stock: Some(false),
        ..ProductFilter::default()
    };
    assert_eq!(
        names(&c.list(&shop.owner, &untracked, 0, 50).await.unwrap()),
        ["Café latte"]
    );

    let first = c
        .list(&shop.owner, &ProductFilter::default(), 0, 2)
        .await
        .unwrap();
    let second = c
        .list(&shop.owner, &ProductFilter::default(), 2, 2)
        .await
        .unwrap();
    assert_eq!(
        (first.total, names(&first), names(&second)),
        (3, vec!["Café latte", "Coca-Cola 330ml"], vec!["T-shirt"])
    );
}

#[tokio::test]
async fn every_role_can_view_the_list() {
    let shop = common::shop().await;
    let (c, _) = catalogue(&shop).await;
    let page = c
        .list(&shop.as_role(Role::Cashier), &ProductFilter::default(), 0, 1)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    // But only managers and owners edit.
    let err = c
        .create(
            &shop.as_role(Role::Cashier),
            input(&shop, "X", ProductType::Stock, vec![variant("X", 1, None)]),
        )
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::PermissionDenied);
}
