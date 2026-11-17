//! E18 / AT-07 end to end: two registers of one business edit the same thing offline.

mod common;

use common::{Register, cloud, register};
use pos_application::conflict::{Choice, FieldChoice};
use pos_application::sync::Next;
use pos_domain::catalog::{BarcodeInput, ProductInput};
use pos_domain::{Money, ProductId};
use serde_json::json;

/// Register 1 linked with a product; register 2 joined. Both synced.
async fn two_registers(cloud: &common::Cloud) -> (Register, Register, ProductId) {
    let (email, password) = cloud.owner().await;
    let one = register(cloud, "1234").await;
    one.product("Cola", 120).await;
    let owner = one.sign_in(&email, &password).await;
    let linked = one.link.create(&one.owner, &owner, false).await.unwrap();
    assert_eq!(one.sync().await, Next::Idle);
    let mut two = register(cloud, "9999").await;
    let (owner2, _) = two.link.start(&two.owner, &email, &password).await.unwrap();
    two.link
        .join(&two.owner, &owner2, linked.business_id)
        .await
        .unwrap();
    two.become_joined_owner().await;
    assert_eq!(two.sync().await, Next::Idle);
    let id: String = sqlx::query_scalar("SELECT product_id FROM products WHERE name = 'Cola'")
        .fetch_one(one.store.db().readers())
        .await
        .unwrap();
    (one, two, id.parse().unwrap())
}

async fn reprice(r: &Register, id: ProductId, price: i64) {
    let catalog = r.catalog();
    let p = catalog.get(&r.owner, id).await.unwrap();
    let mut input = ProductInput {
        name: p.name.clone(),
        description: p.description.clone(),
        category_id: p.category_id,
        product_type: p.product_type,
        unit: p.unit,
        tax_category_id: p.tax_category_id,
        quick_select: p.quick_select,
        variants: vec![pos_domain::catalog::VariantInput::simple(Money::from_minor(
            price,
        ))],
    };
    input.variants[0].id = Some(p.variants[0].id);
    catalog.update(&r.owner, id, p.version, input).await.unwrap();
}

async fn price(r: &Register, id: ProductId) -> i64 {
    r.catalog().get(&r.owner, id).await.unwrap().variants[0]
        .price
        .minor()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at07_two_registers_reprice_offline_one_conflicts_and_nothing_is_overwritten() {
    let Some(cloud) = cloud().await else { return };
    let (one, two, cola) = two_registers(&cloud).await;
    // Both offline: register 1 makes it 1.30, register 2 makes it 1.50.
    reprice(&one, cola, 130).await;
    reprice(&two, cola, 150).await;
    assert_eq!(one.sync().await, Next::Idle);
    assert_eq!(two.sync().await, Next::Idle);
    // Register 1's edit was applied; register 2's is a conflict, and register 2 keeps
    // selling at its own price meanwhile.
    let cloud_price = || async {
        sqlx::query_scalar::<_, i64>("SELECT price_minor FROM product_variants WHERE product_id = $1")
            .bind(uuid::Uuid::parse_str(&cola.to_string()).unwrap())
            .fetch_one(&cloud.pool)
            .await
            .unwrap()
    };
    assert_eq!(cloud_price().await, 130);
    assert_eq!(price(&two, cola).await, 150);
    assert_eq!(
        two.local(&format!(
            "SELECT count(*) FROM products WHERE product_id = '{cola}' AND sync_status = 'conflict'"
        ))
        .await,
        1
    );
    let list = two.conflicts().list(&two.owner).await.unwrap();
    assert_eq!(list.len(), 1);
    let fields: Vec<(&str, &serde_json::Value, &serde_json::Value)> = list[0]
        .fields
        .iter()
        .map(|f| (f.label.as_str(), &f.local, &f.server))
        .collect();
    assert_eq!(fields, [("Price", &json!(150), &json!(130))]);
    // Register 2's manager keeps the local price.
    let field = list[0].fields[0].field.clone();
    two.conflicts()
        .resolve(
            &two.owner,
            &list[0].record.conflict_id,
            vec![FieldChoice {
                field,
                choice: Choice::Local,
            }],
        )
        .await
        .unwrap();
    assert!(two.conflicts().list(&two.owner).await.unwrap().is_empty());
    assert_eq!(
        two.local("SELECT count(*) FROM audit_events WHERE action = 'sync.conflict_resolved'")
            .await,
        1
    );
    assert_eq!(two.sync().await, Next::Idle);
    assert_eq!(cloud_price().await, 150, "the resolution went to the cloud");
    // And register 1 receives it.
    assert_eq!(one.sync().await, Next::Idle);
    assert_eq!(price(&one, cola).await, 150);
    assert_eq!(
        two.local("SELECT count(*) FROM sync_conflicts WHERE status = 'open'")
            .await,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_barcode_two_registers_gave_two_products_moves_to_one_of_them() {
    let Some(cloud) = cloud().await else { return };
    let (one, two, _) = two_registers(&cloud).await;
    let code = "4006381333931";
    let new_product = |name: &str, price: i64| {
        let mut v = pos_domain::catalog::VariantInput::simple(Money::from_minor(price));
        v.barcodes = vec![BarcodeInput::alias(code)];
        ProductInput {
            name: name.into(),
            description: None,
            category_id: None,
            product_type: pos_domain::catalog::ProductType::Stock,
            unit: pos_domain::catalog::Unit::Ea,
            tax_category_id: one.tax_category,
            quick_select: false,
            variants: vec![v],
        }
    };
    let tax: String = sqlx::query_scalar("SELECT tax_category_id FROM tax_categories LIMIT 1")
        .fetch_one(two.store.db().readers())
        .await
        .unwrap();
    let mut lemonade = new_product("Lemonade", 90);
    lemonade.tax_category_id = tax.parse().unwrap();
    let water = one
        .catalog()
        .create(&one.owner, new_product("Water", 80))
        .await
        .unwrap();
    let lemonade = two.catalog().create(&two.owner, lemonade).await.unwrap();
    assert_eq!(one.sync().await, Next::Idle);
    assert_eq!(two.sync().await, Next::Idle);
    let list = two.conflicts().list(&two.owner).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].record.reason.as_str(), list[0].taken.clone()),
        ("BARCODE_TAKEN", vec![code.to_owned()])
    );
    // Move the barcode to Lemonade: Water loses it, Lemonade gets into the cloud with it.
    two.conflicts()
        .resolve_identifiers(&two.owner, &list[0].record.conflict_id, true)
        .await
        .unwrap();
    assert_eq!(two.sync().await, Next::Idle);
    let business: String = sqlx::query_scalar("SELECT business_id FROM device_identity")
        .fetch_one(one.store.db().readers())
        .await
        .unwrap();
    let business = uuid::Uuid::parse_str(&business).unwrap();
    let owner_of = |code: &'static str| {
        let pool = cloud.pool.clone();
        async move {
            sqlx::query_scalar::<_, uuid::Uuid>(
                "SELECT product_id FROM product_barcodes
                  WHERE business_id = $2 AND value_normalized = $1 AND status = 'active'",
            )
            .bind(code)
            .bind(business)
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    let holders = owner_of("4006381333931").await;
    assert_eq!(
        holders,
        vec![uuid::Uuid::parse_str(&lemonade.id.to_string()).unwrap()]
    );
    // Register 1 hears it: Water no longer has the barcode.
    assert_eq!(one.sync().await, Next::Idle);
    let water_now = one.catalog().get(&one.owner, water.id).await.unwrap();
    assert!(water_now.variants[0].barcodes.is_empty());
}
