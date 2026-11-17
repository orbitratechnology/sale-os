//! E09-T08 (complete sale), E10-T01 (settlement) and E11-T01 (stock ledger) against a real
//! database.

mod common;

use pos_application::WriteError;
use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale, CompletedSale};
use pos_application::context::AppError;
use pos_application::scan::ScanSource;
use pos_domain::catalog::{BarcodeInput, Product, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, Money, ScanEventId, UuidV7Generator};
use pos_local_db::SqliteStore;
use std::sync::Arc;

struct Till {
    shop: common::Shop,
    carts: Arc<CartService>,
    catalog: CatalogService,
}

fn till(shop: common::Shop) -> Till {
    let s = shop.store.clone();
    Till {
        carts: Arc::new(CartService::new(
            Arc::new(s.clone()),
            Arc::new(s.clone()),
            Arc::new(s.clone()),
        )),
        catalog: CatalogService {
            store: Arc::new(s.clone()),
            catalog: Arc::new(s),
        },
        shop,
    }
}

fn checkout(t: &Till, store: SqliteStore) -> CheckoutService {
    let q = t.shop.store.clone();
    CheckoutService {
        carts: t.carts.clone(),
        store: Arc::new(store),
        setup: Arc::new(q.clone()),
        stock: Arc::new(q.clone()),
        receipts: Arc::new(q),
    }
}

async fn product(t: &Till, name: &str, price: i64, code: &str, ptype: ProductType) -> Product {
    let mut v = VariantInput::simple(Money::from_minor(price));
    v.sku = Some(code.into());
    v.barcodes = vec![BarcodeInput::alias(&format!("2000000{code}"))];
    t.catalog
        .create(
            &t.shop.owner,
            ProductInput {
                name: name.into(),
                description: None,
                category_id: None,
                product_type: ptype,
                unit: Unit::Ea,
                tax_category_id: t.shop.tax_category,
                quick_select: false,
                variants: vec![v],
            },
        )
        .await
        .unwrap()
}

async fn scan(t: &Till, code: &str) {
    let id = ScanEventId::generate(&UuidV7Generator);
    t.carts
        .scan(&t.shop.owner, code, id, ScanSource::Manual)
        .await
        .unwrap();
}

fn cash(minor: i64) -> TenderInput {
    TenderInput {
        kind: TenderKind::Cash,
        amount: Money::from_minor(minor),
        external_reference: None,
    }
}

async fn pay(t: &Till, store: SqliteStore, tenders: Vec<TenderInput>) -> Result<CompletedSale, AppError> {
    let cart = t.carts.current(&t.shop.owner).await.unwrap().cart;
    checkout(t, store)
        .complete_sale(
            &t.shop.owner,
            CompleteSale {
                cart_id: cart.id,
                revision: cart.revision,
                tenders,
            },
        )
        .await
}

async fn count(t: &Till, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap()
}

/// Tea 2.50 (stock) and Gift wrap 1.00 (service). Cart: 2 teas and a wrap = 6.00.
async fn basket(t: &Till) {
    product(t, "Tea", 250, "TEA", ProductType::Stock).await;
    product(t, "Gift wrap", 100, "WRAP", ProductType::Service).await;
    scan(t, "TEA").await;
    scan(t, "TEA").await;
    scan(t, "WRAP").await;
}

#[tokio::test]
async fn a_cash_sale_commits_everything_in_one_transaction() {
    let t = till(common::shop().await);
    basket(&t).await;
    let done = pay(&t, t.shop.store.clone(), vec![cash(1000)]).await.unwrap();
    assert_eq!(done.receipt_number, "R01-000001");
    assert_eq!((done.sale.total.minor(), done.sale.change.minor()), (600, 400));
    assert!(done.outbox_sequence > 0);

    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM sales WHERE receipt_number = 'R01-000001' AND mode = 'sandbox'"
        )
        .await,
        1
    );
    assert_eq!(count(&t, "SELECT count(*) FROM sale_lines").await, 2);
    assert_eq!(count(&t, "SELECT sum(total_minor) FROM sale_lines").await, 600);
    assert_eq!(count(&t, "SELECT count(*) FROM tenders WHERE status = 'confirmed' AND confirmation_source = 'operator' AND change_minor = 400").await, 1);
    assert_eq!(count(&t, "SELECT count(*) FROM tender_status_events").await, 1);
    // Only the stock item moves: -2, and the balance follows in the same transaction.
    assert_eq!(count(&t, "SELECT count(*) FROM stock_movements WHERE movement_type = 'sale' AND quantity_delta_milli = -2000").await, 1);
    assert_eq!(count(&t, "SELECT count(*) FROM stock_movements").await, 1);
    assert_eq!(count(&t, "SELECT on_hand_milli FROM stock_balances").await, -2000);
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_events WHERE action = 'sale.completed'"
        )
        .await,
        1
    );
    // Default policy: allowed, and flagged because there was no stock.
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_events WHERE action = 'inventory.negative_flagged'"
        )
        .await,
        1
    );
    // The event is self-contained and carries the receipt number allocated in the transaction.
    let payload: String =
        sqlx::query_scalar("SELECT payload_json FROM outbox_events WHERE event_type = 'sale.completed'")
            .fetch_one(t.shop.store.db().readers())
            .await
            .unwrap();
    let p: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        (p["receiptNumber"].as_str(), p["receiptSeq"].as_u64()),
        (Some("R01-000001"), Some(1))
    );
    assert_eq!(p["lines"].as_array().unwrap().len(), 2);
    assert_eq!(p["stockMovements"].as_array().unwrap().len(), 1);
    assert_eq!(p["lines"][0]["name"], "Tea");
    // The cart is closed; the next one is new and empty.
    assert_eq!(
        count(&t, "SELECT count(*) FROM carts WHERE status = 'completed'").await,
        1
    );
    let next = t.carts.current(&t.shop.owner).await.unwrap().cart;
    assert!(next.lines.is_empty());
    // The receipt was written in the same transaction, with the allocated number.
    let doc: String = sqlx::query_scalar("SELECT document_json FROM receipts WHERE source_type = 'sale'")
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&doc).unwrap();
    assert_eq!(
        (doc["meta"]["receiptNumber"].as_str(), doc["barcode"].as_str()),
        (Some("R01-000001"), Some("R01-000001"))
    );
    assert_eq!(doc["meta"]["cashierName"], "Olu");
    assert_eq!(doc["lines"][0]["name"], "Tea");
    assert_eq!(doc["change"], 400);
    // Sales are immutable.
    let update = sqlx::query("UPDATE sales SET total_minor = 0")
        .execute(t.shop.store.db().writer())
        .await;
    assert!(update.unwrap_err().to_string().contains("immutable"));
}

#[tokio::test]
async fn a_failure_at_any_statement_leaves_nothing_behind() {
    // AT-04: inject a failure before every statement in turn; until the sale succeeds,
    // nothing may be written and the cart must be intact.
    let t = till(common::shop().await);
    basket(&t).await;
    let tables = [
        "receipts",
        "sales",
        "sale_lines",
        "tenders",
        "tender_status_events",
        "stock_movements",
        "stock_balances",
    ];
    let mut statement = 0;
    loop {
        match pay(
            &t,
            t.shop.store.clone().with_fault_before_statement(statement),
            vec![cash(600)],
        )
        .await
        {
            Ok(done) => {
                assert_eq!(
                    done.receipt_number, "R01-000001",
                    "failed attempts used no receipt numbers"
                );
                break;
            }
            Err(AppError::Write(WriteError::FaultInjected { .. })) => {
                for table in tables {
                    assert_eq!(
                        count(&t, &format!("SELECT count(*) FROM {table}")).await,
                        0,
                        "{table} after a fault at {statement}"
                    );
                }
                assert_eq!(
                    count(
                        &t,
                        "SELECT count(*) FROM outbox_events WHERE event_type = 'sale.completed'"
                    )
                    .await,
                    0
                );
                assert_eq!(
                    t.carts.current(&t.shop.owner).await.unwrap().cart.lines.len(),
                    2,
                    "cart intact"
                );
                statement += 1;
            }
            Err(e) => panic!("unexpected error at statement {statement}: {e:?}"),
        }
    }
    assert!(
        statement > 10,
        "every statement of the sale was exercised ({statement})"
    );
}

#[tokio::test]
async fn receipt_numbers_are_gapless_per_register() {
    let t = till(common::shop().await);
    product(&t, "Tea", 250, "TEA", ProductType::Stock).await;
    for _ in 0..200 {
        scan(&t, "TEA").await;
        pay(&t, t.shop.store.clone(), vec![cash(250)]).await.unwrap();
    }
    let seqs: Vec<i64> = sqlx::query_scalar("SELECT receipt_seq FROM sales ORDER BY receipt_seq")
        .fetch_all(t.shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(seqs, (1..=200).collect::<Vec<_>>());
    assert_eq!(
        count(&t, "SELECT on_hand_milli FROM stock_balances").await,
        -200_000,
        "balance = sum of movements"
    );
    assert_eq!(
        count(&t, "SELECT sum(quantity_delta_milli) FROM stock_movements").await,
        -200_000
    );
}

#[tokio::test]
async fn the_sale_keeps_its_snapshot_when_the_product_changes() {
    // POS-010: rename, reprice and the sale detail stays as sold.
    let t = till(common::shop().await);
    let tea = product(&t, "Tea", 250, "TEA", ProductType::Stock).await;
    scan(&t, "TEA").await;
    pay(&t, t.shop.store.clone(), vec![cash(250)]).await.unwrap();
    let mut input = ProductInput {
        name: "Green tea".into(),
        description: None,
        category_id: None,
        product_type: ProductType::Stock,
        unit: Unit::Ea,
        tax_category_id: t.shop.tax_category,
        quick_select: false,
        variants: vec![VariantInput::simple(Money::from_minor(999))],
    };
    input.variants[0].id = Some(tea.variants[0].id);
    t.catalog
        .update(&t.shop.owner, tea.id, tea.version, input)
        .await
        .unwrap();
    let (name, price): (String, i64) =
        sqlx::query_as("SELECT name_snapshot, unit_price_minor FROM sale_lines")
            .fetch_one(t.shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!((name.as_str(), price), ("Tea", 250));
    // The stored receipt is unchanged too: receipts are never rebuilt from the catalogue.
    let receipts = pos_application::receipts::ReceiptService {
        receipts: Arc::new(t.shop.store.clone()),
    };
    let r = receipts.for_sale(&t.shop.owner, None, Some(58)).await.unwrap();
    assert_eq!(r.document.lines[0].name, "Tea");
    assert_eq!(r.columns, 32);
    let text = pos_receipts::text::to_plain_text(&r.lines);
    assert!(text.contains("Tea") && !text.contains("Green tea"), "{text}");
    assert!(text.contains("R01-000001"));
}

#[tokio::test]
async fn refusals_write_nothing() {
    let t = till(common::shop().await);
    basket(&t).await;
    let cart = t.carts.current(&t.shop.owner).await.unwrap().cart;
    let co = checkout(&t, t.shop.store.clone());
    let complete = |revision, tenders| CompleteSale {
        cart_id: cart.id,
        revision,
        tenders,
    };

    // A stale revision (the screen was behind).
    let e = co
        .complete_sale(&t.shop.owner, complete(cart.revision - 1, vec![cash(600)]))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::CartRevisionMismatch);
    // Not enough, carries what is still due.
    let e = co
        .complete_sale(&t.shop.owner, complete(cart.revision, vec![cash(255)]))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::TenderInsufficient);
    assert!(
        matches!(e, AppError::Settlement(pos_domain::settlement::SettlementError::Insufficient { remaining }) if remaining.minor() == 345)
    );
    // A card over the amount due, and a card number typed as the reference.
    let card = |minor, reference: Option<&str>| TenderInput {
        kind: TenderKind::CardExternal,
        amount: Money::from_minor(minor),
        external_reference: reference.map(Into::into),
    };
    let e = co
        .complete_sale(&t.shop.owner, complete(cart.revision, vec![card(700, None)]))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::TenderOverpayNonCash);
    let e = co
        .complete_sale(
            &t.shop.owner,
            complete(cart.revision, vec![card(600, Some("4111 1111 1111 1111"))]),
        )
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    assert_eq!(count(&t, "SELECT count(*) FROM sales").await, 0);
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_events WHERE metadata_json LIKE '%4111%'"
        )
        .await,
        0
    );

    // Split: card 5.00 + cash 2.00 on 6.00 gives 1.00 change from the cash.
    let done = co
        .complete_sale(
            &t.shop.owner,
            complete(cart.revision, vec![card(500, Some("AUTH 0042")), cash(200)]),
        )
        .await
        .unwrap();
    assert_eq!(done.sale.change.minor(), 100);
    let card_ref: Option<String> =
        sqlx::query_scalar("SELECT external_reference FROM tenders WHERE tender_type = 'card_external'")
            .fetch_one(t.shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(card_ref.as_deref(), Some("AUTH 0042"));
}

#[tokio::test]
async fn the_block_policy_refuses_a_sale_that_would_go_negative() {
    let t = till(common::shop().await);
    basket(&t).await;
    sqlx::query("UPDATE business_settings SET value_json = 'false' WHERE key = 'inventory.allow_negative'")
        .execute(t.shop.store.db().writer())
        .await
        .unwrap();
    let e = pay(&t, t.shop.store.clone(), vec![cash(600)]).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::StockNegativeBlocked);
    assert_eq!(count(&t, "SELECT count(*) FROM sales").await, 0);
    assert_eq!(
        t.carts.current(&t.shop.owner).await.unwrap().cart.lines.len(),
        2,
        "the cart is kept"
    );
}

#[tokio::test]
async fn an_empty_cart_cannot_be_completed() {
    let t = till(common::shop().await);
    product(&t, "Tea", 250, "TEA", ProductType::Stock).await;
    scan(&t, "TEA").await;
    let cart = t.carts.current(&t.shop.owner).await.unwrap().cart;
    let line = cart.lines[0].id;
    let cart = t.carts.remove_line(&t.shop.owner, line).await.unwrap().cart;
    let e = checkout(&t, t.shop.store.clone())
        .complete_sale(
            &t.shop.owner,
            CompleteSale {
                cart_id: cart.id,
                revision: cart.revision,
                tenders: vec![cash(100)],
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::CartEmpty);
}
