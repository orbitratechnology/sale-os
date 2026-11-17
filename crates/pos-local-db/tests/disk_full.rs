//! E19-T02: a full disk. SQLite's `max_page_count` makes the database refuse to grow, which
//! SQLite reports exactly as it does a full disk (SQLITE_FULL), so this runs on any machine;
//! `crash-harness disk-full` does the same on a real, small file system.
//!
//! The sale is not saved, the cashier is told "disk full" (`STORE_DISK_FULL`), the cart is
//! kept, nothing half-written remains, and once there is room the same cart completes.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_domain::cart::AddVia;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, Money};
use std::sync::Arc;

async fn count(shop: &common::Shop, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_full_disk_refuses_the_sale_keeps_the_cart_and_corrupts_nothing() {
    let shop = common::shop().await;
    let s = &shop.store;
    let a = Arc::new(s.clone());
    let catalog = CatalogService {
        store: a.clone(),
        catalog: a.clone(),
    };
    let tea = catalog
        .create(
            &shop.owner,
            ProductInput {
                name: "Tea".into(),
                description: None,
                category_id: None,
                product_type: ProductType::Stock,
                unit: Unit::Ea,
                tax_category_id: shop.tax_category,
                quick_select: false,
                variants: vec![VariantInput::simple(Money::from_minor(250))],
            },
        )
        .await
        .unwrap()
        .variants[0]
        .id;
    let carts = Arc::new(CartService::new(a.clone(), a.clone(), a.clone()));
    let checkout = CheckoutService {
        carts: carts.clone(),
        store: a.clone(),
        setup: a.clone(),
        stock: a.clone(),
        receipts: a.clone(),
    };
    let owner = &shop.owner;
    let snapshot = || async {
        let mut v = Vec::new();
        for sql in [
            "SELECT count(*) FROM sales",
            "SELECT count(*) FROM sale_lines",
            "SELECT count(*) FROM tenders",
            "SELECT count(*) FROM stock_movements",
            "SELECT count(*) FROM outbox_events",
            "SELECT count(*) FROM audit_events",
            "SELECT coalesce(max(value), 0) FROM register_counters WHERE name = 'receipt_seq'",
        ] {
            v.push(count(&shop, sql).await);
        }
        v
    };

    // The disk fills up: the database may not grow by a single page (the writer is the only
    // connection that writes, and the setting is per connection).
    let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(s.db().writer())
        .await
        .unwrap();
    sqlx::query(&format!("PRAGMA max_page_count = {pages}"))
        .execute(s.db().writer())
        .await
        .unwrap();

    // Trading carries on until a write no longer fits in the pages there are: adding an item
    // or completing a sale. Whichever is refused must leave nothing behind.
    let mut sold = 0;
    let refused = loop {
        assert!(sold < 500, "500 sales fitted in a database that may not grow");
        let cart_before = carts.current(owner).await.unwrap();
        let before = snapshot().await;
        if let Err(e) = carts.add_variant(owner, tea, None, AddVia::Search).await {
            let unchanged = snapshot().await == before;
            break (e, cart_before, unchanged);
        }
        let view = carts.current(owner).await.unwrap();
        let before = snapshot().await;
        let sale = CompleteSale {
            cart_id: view.cart.id,
            revision: view.cart.revision,
            tenders: vec![TenderInput {
                kind: TenderKind::Cash,
                amount: view.priced.total,
                external_reference: None,
            }],
        };
        match checkout.complete_sale(owner, sale).await {
            Ok(_) => sold += 1,
            Err(e) => {
                let unchanged = snapshot().await == before;
                break (e, view, unchanged);
            }
        }
    };
    let (e, kept_view, unchanged) = refused;
    assert_eq!(e.error_code(), ErrorCode::StoreDiskFull, "{e}");
    assert!(unchanged, "the refused write left rows behind");
    eprintln!("{sold} sales fitted before the disk was full");

    // The cart is as it was; the database is sound.
    let kept = carts.current(owner).await.unwrap();
    assert_eq!(kept.cart.id, kept_view.cart.id);
    assert_eq!(kept.cart.revision, kept_view.cart.revision);
    assert_eq!(kept.priced.total, kept_view.priced.total);
    let check: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    assert_eq!(check, "ok");

    // Space is freed: the same cart completes with the next receipt number.
    sqlx::query("PRAGMA max_page_count = 1073741823")
        .execute(s.db().writer())
        .await
        .unwrap();
    let done = checkout
        .complete_sale(
            owner,
            CompleteSale {
                cart_id: kept.cart.id,
                revision: kept.cart.revision,
                tenders: vec![TenderInput {
                    kind: TenderKind::Cash,
                    amount: kept.priced.total,
                    external_reference: None,
                }],
            },
        )
        .await
        .unwrap();
    assert!(
        done.receipt_number.ends_with(&format!("{:06}", sold + 1)),
        "{}",
        done.receipt_number
    );
}
