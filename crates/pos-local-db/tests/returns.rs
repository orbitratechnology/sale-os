//! E12-T01/T02: returns against a real database (AT-10, RET-002..007, INV-009).

mod common;

use pos_application::WriteError;
use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::context::{AppError, ApprovalToken, CommandContext};
use pos_application::queries::{QueryError, ReturnQueries};
use pos_application::returns::{CreateReturn, ReturnService};
use pos_application::scan::ScanSource;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::returns::{Disposition, ReturnLineRequest, ReturnReason, ReturnableSale};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{
    ApprovalId, ApprovalScope, ErrorCode, HasErrorCode, Money, Permission, Quantity, Role, SaleId,
    ScanEventId, Timestamp, UuidV7Generator,
};
use pos_local_db::SqliteStore;
use std::sync::Arc;

struct Shop {
    shop: common::Shop,
    sale: ReturnableSale,
}

fn cash(minor: i64) -> TenderInput {
    TenderInput {
        kind: TenderKind::Cash,
        amount: Money::from_minor(minor),
        external_reference: None,
    }
}

/// A completed sale: 3 x Tea 2.50 (stock) and Gift wrap 1.00 (service) = 8.50.
async fn after_a_sale() -> Shop {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    for (name, price, sku, ptype) in [
        ("Tea", 250, "TEA", ProductType::Stock),
        ("Gift wrap", 100, "WRAP", ProductType::Service),
    ] {
        let mut v = VariantInput::simple(Money::from_minor(price));
        v.sku = Some(sku.into());
        catalog
            .create(
                &shop.owner,
                ProductInput {
                    name: name.into(),
                    description: None,
                    category_id: None,
                    product_type: ptype,
                    unit: Unit::Ea,
                    tax_category_id: shop.tax_category,
                    quick_select: false,
                    variants: vec![v],
                },
            )
            .await
            .unwrap();
    }
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s.clone()),
    ));
    for code in ["TEA", "TEA", "TEA", "WRAP"] {
        carts
            .scan(
                &shop.owner,
                code,
                ScanEventId::generate(&UuidV7Generator),
                ScanSource::Manual,
            )
            .await
            .unwrap();
    }
    let cart = carts.current(&shop.owner).await.unwrap().cart;
    let done = CheckoutService {
        carts,
        store: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        stock: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
    }
    .complete_sale(
        &shop.owner,
        CompleteSale {
            cart_id: cart.id,
            revision: cart.revision,
            tenders: vec![cash(1000)],
        },
    )
    .await
    .unwrap();
    assert_eq!(done.sale.total.minor(), 850);
    let sale = s.returnable_sale(done.sale.id).await.unwrap().unwrap();
    Shop { shop, sale }
}

fn service(t: &Shop, store: SqliteStore) -> ReturnService {
    let q = t.shop.store.clone();
    ReturnService {
        store: Arc::new(store),
        returns: Arc::new(q.clone()),
        setup: Arc::new(q.clone()),
        receipts: Arc::new(q.clone()),
        pricing: Arc::new(q),
    }
}

fn line(t: &Shop, name: &str, units: i64, disposition: Disposition) -> ReturnLineRequest {
    let l = t.sale.lines.iter().find(|l| l.name == name).unwrap();
    ReturnLineRequest {
        sale_line_id: l.sale_line_id,
        quantity: Quantity::from_milli(units * 1000),
        disposition,
    }
}

fn request(t: &Shop, lines: Vec<ReturnLineRequest>, refund: i64) -> CreateReturn {
    CreateReturn {
        sale_id: t.sale.sale_id,
        lines,
        reason: ReturnReason::ChangedMind,
        note: Some("  receipt kept  ".into()),
        refunds: vec![cash(refund)],
    }
}

async fn count(t: &Shop, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_return_moves_stock_by_disposition_and_commits_everything_together() {
    let t = after_a_sale().await;
    let returns = service(&t, t.shop.store.clone());
    let before: Vec<(String, i64)> =
        sqlx::query_as("SELECT sale_line_id, total_minor FROM sale_lines ORDER BY line_no")
            .fetch_all(t.shop.store.db().readers())
            .await
            .unwrap();
    // Found by the number printed (and barcoded) on the receipt.
    let found = returns.find(&t.shop.owner, "r01-000001").await.unwrap();
    assert_eq!(found.sale_id, t.sale.sale_id);
    assert!(returns.find(&t.shop.owner, "R01-999999").await.is_err());

    // AT-10: one tea back to the shelf, one damaged, the wrap (a service) moves nothing
    // whatever was asked for.
    let done = returns
        .create(
            &t.shop.owner,
            request(
                &t,
                vec![
                    line(&t, "Tea", 1, Disposition::Restock),
                    line(&t, "Tea", 1, Disposition::Damaged),
                    line(&t, "Gift wrap", 1, Disposition::Restock),
                ],
                600,
            ),
        )
        .await
        .unwrap();
    assert_eq!(done.return_number, "R01-R-000001");
    assert_eq!(done.ret.total.minor(), 600);
    assert_eq!(done.ret.note.as_deref(), Some("receipt kept"));
    assert_eq!(done.ret.approver, None, "the owner needs no approval");

    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM returns WHERE return_number = 'R01-R-000001' AND total_minor = 600"
        )
        .await,
        1
    );
    assert_eq!(count(&t, "SELECT count(*) FROM return_lines").await, 3);
    // RET-005: the refund is its own record, never a tender on the sale.
    assert_eq!(count(&t, "SELECT sum(amount_minor) FROM refunds").await, 600);
    assert_eq!(count(&t, "SELECT count(*) FROM tenders").await, 1);
    // INV-009: +1 sellable, +1 damaged, nothing for the service line.
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM stock_movements WHERE source_type = 'return'"
        )
        .await,
        2
    );
    assert_eq!(
        count(&t, "SELECT count(*) FROM stock_movements WHERE movement_type = 'return_restock' AND quantity_delta_milli = 1000 AND bucket = 'sellable'").await,
        1
    );
    assert_eq!(
        count(&t, "SELECT count(*) FROM stock_movements WHERE movement_type = 'return_damaged' AND quantity_delta_milli = 1000 AND bucket = 'damaged'").await,
        1
    );
    assert_eq!(
        count(
            &t,
            "SELECT on_hand_milli FROM stock_balances WHERE bucket = 'sellable'"
        )
        .await,
        -2000
    );
    assert_eq!(
        count(
            &t,
            "SELECT on_hand_milli FROM stock_balances WHERE bucket = 'damaged'"
        )
        .await,
        1000
    );
    // The refund receipt, the audit row and the event, all in the same transaction.
    let doc: String = sqlx::query_scalar("SELECT document_json FROM receipts WHERE source_type = 'return'")
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap();
    let doc: pos_receipts::ReceiptDocument = serde_json::from_str(&doc).unwrap();
    assert_eq!(doc.meta.receipt_number, "R01-R-000001");
    assert_eq!(doc.original_receipt.as_deref(), Some("R01-000001"));
    let text = pos_receipts::text::to_plain_text(&pos_receipts::text::render(&doc, 32));
    assert!(text.contains("REFUND TOTAL"), "{text}");
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_events WHERE action = 'return.completed'"
        )
        .await,
        1
    );
    let payload: String =
        sqlx::query_scalar("SELECT payload_json FROM outbox_events WHERE event_type = 'return.completed'")
            .fetch_one(t.shop.store.db().readers())
            .await
            .unwrap();
    let p: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(p["receiptNumber"], "R01-R-000001");
    assert_eq!(p["originalReceiptNumber"], "R01-000001");
    assert_eq!(p["stockMovements"].as_array().unwrap().len(), 2);

    // RET-002: the original sale is untouched, and cannot be touched.
    let after: Vec<(String, i64)> =
        sqlx::query_as("SELECT sale_line_id, total_minor FROM sale_lines ORDER BY line_no")
            .fetch_all(t.shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(before, after);
    assert!(
        sqlx::query("UPDATE returns SET total_minor = 0")
            .execute(t.shop.store.db().writer())
            .await
            .unwrap_err()
            .to_string()
            .contains("immutable")
    );

    // RET-004: one tea left. Two more is refused; the last one is fine and gets the next number.
    let left = t
        .shop
        .store
        .returnable_sale(t.sale.sale_id)
        .await
        .unwrap()
        .unwrap();
    let tea = left.lines.iter().find(|l| l.name == "Tea").unwrap();
    assert_eq!(tea.available().milli(), 1000);
    let e = returns
        .create(
            &t.shop.owner,
            request(&t, vec![line(&t, "Tea", 2, Disposition::Restock)], 500),
        )
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ReturnQtyExceeds);
    let last = returns
        .create(
            &t.shop.owner,
            request(&t, vec![line(&t, "Tea", 1, Disposition::Restock)], 250),
        )
        .await
        .unwrap();
    assert_eq!(last.return_number, "R01-R-000002");
}

#[tokio::test]
async fn refunds_must_match_the_return_exactly_and_never_carry_a_card_number() {
    let t = after_a_sale().await;
    let returns = service(&t, t.shop.store.clone());
    let lines = || vec![line(&t, "Tea", 1, Disposition::Restock)];
    let e = returns
        .create(&t.shop.owner, request(&t, lines(), 300))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ReturnRefundMismatch);
    let mut card = request(&t, lines(), 250);
    card.refunds = vec![TenderInput {
        kind: TenderKind::CardExternal,
        amount: Money::from_minor(250),
        external_reference: Some("4111 1111 1111 1111".into()),
    }];
    let e = returns.create(&t.shop.owner, card).await.unwrap_err();
    assert!(!format!("{e:?}").contains("4111"), "never echoed");
    assert_eq!(count(&t, "SELECT count(*) FROM returns").await, 0);
    // Split refunds are separate records that add up.
    let mut split = request(&t, lines(), 0);
    split.refunds = vec![
        cash(100),
        TenderInput {
            kind: TenderKind::CardExternal,
            amount: Money::from_minor(150),
            external_reference: Some("AUTH 991".into()),
        },
    ];
    returns.create(&t.shop.owner, split).await.unwrap();
    assert_eq!(count(&t, "SELECT count(*) FROM refunds").await, 2);
    assert_eq!(count(&t, "SELECT sum(amount_minor) FROM refunds").await, 250);
}

#[tokio::test]
async fn a_cashier_needs_a_managers_approval_unless_the_shop_turns_it_off() {
    let t = after_a_sale().await;
    let returns = service(&t, t.shop.store.clone());
    let cashier = t.shop.as_role(Role::Cashier);
    let req = || request(&t, vec![line(&t, "Tea", 1, Disposition::Restock)], 250);
    let scope = match returns.create(&cashier, req()).await.unwrap_err() {
        AppError::ApprovalRequired(scope) => scope,
        e => panic!("{e:?}"),
    };
    assert_eq!(
        scope,
        ApprovalScope {
            permission: Permission::ReturnCreate,
            entity_id: t.sale.sale_id.to_string(),
            max_ppm: None,
        }
    );
    // An approval for another sale does not cover this one.
    let token = |entity: String| -> CommandContext {
        let mut c = cashier.clone();
        c.approval = Some(ApprovalToken {
            id: ApprovalId::generate(&UuidV7Generator),
            approver: t.shop.owner.actor.as_ref().unwrap().user_id,
            scope: ApprovalScope {
                permission: Permission::ReturnCreate,
                entity_id: entity,
                max_ppm: None,
            },
            expires_at: Timestamp::from_millis(i64::MAX),
        });
        c
    };
    let other = SaleId::generate(&UuidV7Generator).to_string();
    assert_eq!(
        returns
            .create(&token(other), req())
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::ApprovalInvalid
    );
    let approved = returns
        .create(&token(t.sale.sale_id.to_string()), req())
        .await
        .unwrap();
    assert_eq!(
        approved.ret.approver,
        Some(t.shop.owner.actor.as_ref().unwrap().user_id)
    );
    assert_eq!(
        count(&t, "SELECT count(*) FROM audit_events WHERE action = 'return.completed' AND approver_user_id IS NOT NULL").await,
        1
    );

    // returns.cashier_requires_approval = false: cashiers return on their own.
    sqlx::query("INSERT OR REPLACE INTO business_settings (business_id, key, value_json, updated_at) SELECT business_id, 'returns.cashier_requires_approval', 'false', 0 FROM businesses")
        .execute(t.shop.store.db().writer())
        .await
        .unwrap();
    let own = returns.create(&cashier, req()).await.unwrap();
    assert_eq!(own.ret.approver, None);
}

/// Queries that keep answering with what the sale looked like before any return: a till
/// that read the sale, then lost the race.
struct Stale(ReturnableSale);

#[async_trait::async_trait]
impl ReturnQueries for Stale {
    async fn returnable_sale(&self, _: SaleId) -> Result<Option<ReturnableSale>, QueryError> {
        Ok(Some(self.0.clone()))
    }
    async fn sale_by_receipt(&self, _: &str) -> Result<Option<SaleId>, QueryError> {
        Ok(Some(self.0.sale_id))
    }
    async fn cashier_returns_need_approval(&self) -> Result<bool, QueryError> {
        Ok(true)
    }
}

#[tokio::test]
async fn the_last_unit_is_checked_again_inside_the_transaction() {
    let t = after_a_sale().await;
    let fresh = service(&t, t.shop.store.clone());
    let mut stale = service(&t, t.shop.store.clone());
    stale.returns = Arc::new(Stale(t.sale.clone()));
    let all = || request(&t, vec![line(&t, "Tea", 3, Disposition::Restock)], 750);
    fresh.create(&t.shop.owner, all()).await.unwrap();
    // The stale till still believes 3 teas can come back; the store says no.
    match stale.create(&t.shop.owner, all()).await.unwrap_err() {
        AppError::Write(WriteError::ReturnQtyExceeds { available_milli, .. }) => {
            assert_eq!(available_milli, 0)
        }
        e => panic!("{e:?}"),
    }
    assert_eq!(count(&t, "SELECT count(*) FROM returns").await, 1);
    assert_eq!(
        count(&t, "SELECT sum(quantity_milli) FROM return_lines").await,
        3000
    );

    // And two tills racing for the same units: exactly one wins.
    let t = after_a_sale().await;
    let a = service(&t, t.shop.store.clone());
    let b = service(&t, t.shop.store.clone());
    let all = || request(&t, vec![line(&t, "Tea", 3, Disposition::Restock)], 750);
    let (ra, rb) = tokio::join!(a.create(&t.shop.owner, all()), b.create(&t.shop.owner, all()));
    assert_eq!(usize::from(ra.is_ok()) + usize::from(rb.is_ok()), 1);
    let loser = ra.err().or(rb.err()).unwrap();
    assert_eq!(loser.error_code(), ErrorCode::ReturnQtyExceeds);
    assert_eq!(
        count(&t, "SELECT sum(quantity_milli) FROM return_lines").await,
        3000
    );
}

#[tokio::test]
async fn a_failure_at_any_statement_leaves_no_return_behind() {
    let t = after_a_sale().await;
    let tables = [
        "returns",
        "return_lines",
        "refunds",
        "receipts WHERE source_type = 'return'",
        "stock_movements WHERE source_type = 'return'",
        "outbox_events WHERE event_type = 'return.completed'",
        "audit_events WHERE action = 'return.completed'",
    ];
    let mut statement = 0;
    loop {
        let returns = service(&t, t.shop.store.clone().with_fault_before_statement(statement));
        match returns
            .create(
                &t.shop.owner,
                request(
                    &t,
                    vec![
                        line(&t, "Tea", 1, Disposition::Restock),
                        line(&t, "Tea", 1, Disposition::Damaged),
                    ],
                    500,
                ),
            )
            .await
        {
            Ok(done) => {
                assert_eq!(
                    done.return_number, "R01-R-000001",
                    "failed attempts used no numbers"
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
                    count(&t, "SELECT sum(on_hand_milli) FROM stock_balances").await,
                    -3000
                );
                statement += 1;
            }
            Err(e) => panic!("unexpected error at statement {statement}: {e:?}"),
        }
    }
    assert!(statement > 8, "only {statement} statements");
}
