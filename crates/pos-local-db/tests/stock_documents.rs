//! E11-T02/T03: opening stock, receiving, adjustments and write-offs against a real database.

mod common;

use pos_application::WriteError;
use pos_application::catalog::CatalogService;
use pos_application::context::AppError;
use pos_application::stock::StockService;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::stock_docs::{AdjustReason, DocumentDraft, DocumentLine, DocumentType};
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, Role, VariantId};
use pos_local_db::SqliteStore;
use std::sync::Arc;

struct Shop {
    shop: common::Shop,
    tea: VariantId,
    flour: VariantId,
    cake: VariantId,
    wrap: VariantId,
}

async fn shop() -> Shop {
    let shop = common::shop().await;
    let catalog = CatalogService {
        store: Arc::new(shop.store.clone()),
        catalog: Arc::new(shop.store.clone()),
    };
    let mut ids = Vec::new();
    for (name, unit, ptype) in [
        ("Tea", Unit::Ea, ProductType::Stock),
        ("Flour", Unit::Kg, ProductType::Stock),
        ("Cake", Unit::Ea, ProductType::Stock),
        ("Gift wrap", Unit::Ea, ProductType::Service),
    ] {
        let p = catalog
            .create(
                &shop.owner,
                ProductInput {
                    name: name.into(),
                    description: None,
                    category_id: None,
                    product_type: ptype,
                    unit,
                    tax_category_id: shop.tax_category,
                    quick_select: false,
                    variants: vec![VariantInput::simple(Money::from_minor(100))],
                },
            )
            .await
            .unwrap();
        ids.push(p.variants[0].id);
    }
    Shop {
        tea: ids[0],
        flour: ids[1],
        cake: ids[2],
        wrap: ids[3],
        shop,
    }
}

fn service(t: &Shop, store: SqliteStore) -> StockService {
    let q = t.shop.store.clone();
    StockService {
        store: Arc::new(store),
        carts: Arc::new(q.clone()),
        stock: Arc::new(q),
    }
}

fn line(v: VariantId, milli: i64) -> DocumentLine {
    DocumentLine {
        variant_id: v,
        quantity: Quantity::from_milli(milli),
        unit_cost: None,
    }
}

fn doc(doc_type: DocumentType, reason: Option<AdjustReason>, lines: Vec<DocumentLine>) -> DocumentDraft {
    DocumentDraft {
        doc_type,
        reason,
        supplier_name: None,
        reference: None,
        note: None,
        lines,
    }
}

async fn count(t: &Shop, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap()
}

async fn balance(t: &Shop, v: VariantId, bucket: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT coalesce(sum(on_hand_milli), 0) FROM stock_balances WHERE variant_id = ? AND bucket = ?",
    )
    .bind(v.to_string())
    .bind(bucket)
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_receipt_of_three_lines_is_one_atomic_document() {
    let t = shop().await;
    let stock = service(&t, t.shop.store.clone());
    let mut receipt = doc(
        DocumentType::Receipt,
        None,
        vec![line(t.tea, 24_000), line(t.flour, 12_500), line(t.cake, 6_000)],
    );
    receipt.supplier_name = Some("Wholesale Ltd".into());
    receipt.reference = Some("INV-8812".into());
    receipt.lines[0].unit_cost = Some(Money::from_minor(35));
    let posted = stock.post(&t.shop.owner, receipt).await.unwrap();
    assert_eq!(posted.document_no, "R01-IN-000001");

    // §7.4: 3 movements, the balances, 1 audit entry, 1 event, together.
    assert_eq!(
        count(&t, "SELECT count(*) FROM stock_movements WHERE movement_type = 'receipt' AND source_type = 'inventory_document'").await,
        3
    );
    assert_eq!(balance(&t, t.tea, "sellable").await, 24_000);
    assert_eq!(balance(&t, t.flour, "sellable").await, 12_500);
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM stock_movements WHERE unit_cost_minor = 35"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_events WHERE action = 'inventory.receipt_posted'"
        )
        .await,
        1
    );
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM outbox_events WHERE event_type = 'inventory.document_posted'",
    )
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap();
    let p: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        (p["documentNo"].as_str(), p["documentSeq"].as_u64()),
        (Some("R01-IN-000001"), Some(1))
    );
    assert_eq!(p["supplierName"], "Wholesale Ltd");
    assert_eq!(p["stockMovements"].as_array().unwrap().len(), 3);
    assert!(p.get("receiptNumber").is_none());

    // The number increments per register, for every kind of document.
    let opening = stock
        .post(
            &t.shop.owner,
            doc(DocumentType::Opening, None, vec![line(t.cake, 2_000)]),
        )
        .await
        .unwrap();
    assert_eq!(opening.document_no, "R01-IN-000002");
    assert_eq!(balance(&t, t.cake, "sellable").await, 8_000);

    // Refused: ≤ 0, half a cake, a service item. Nothing written, no number used.
    for (bad, what) in [
        (doc(DocumentType::Receipt, None, vec![line(t.tea, 0)]), "zero"),
        (
            doc(DocumentType::Receipt, None, vec![line(t.tea, -1_000)]),
            "negative",
        ),
        (
            doc(DocumentType::Receipt, None, vec![line(t.cake, 500)]),
            "fraction",
        ),
        (
            doc(DocumentType::Receipt, None, vec![line(t.wrap, 1_000)]),
            "service",
        ),
    ] {
        let e = stock.post(&t.shop.owner, bad).await.unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::ValidationFailed, "{what}");
    }
    assert_eq!(count(&t, "SELECT count(*) FROM inventory_documents").await, 2);
    let next = stock
        .post(
            &t.shop.owner,
            doc(DocumentType::Receipt, None, vec![line(t.tea, 1_000)]),
        )
        .await
        .unwrap();
    assert_eq!(next.document_no, "R01-IN-000003");

    // Posted documents are final: no edit, no delete.
    for sql in [
        "UPDATE inventory_documents SET note = 'x'",
        "DELETE FROM inventory_documents",
    ] {
        let e = sqlx::query(sql)
            .execute(t.shop.store.db().writer())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("immutable"), "{e}");
    }
}

#[tokio::test]
async fn adjustments_by_reason_and_write_offs_from_damaged_stock() {
    let t = shop().await;
    let stock = service(&t, t.shop.store.clone());
    let owner = &t.shop.owner;
    stock
        .post(owner, doc(DocumentType::Receipt, None, vec![line(t.tea, 10_000)]))
        .await
        .unwrap();

    // Damage: 3 from sellable to damaged. Theft: −1. Count correction: −2 (counted fewer).
    stock
        .post(
            owner,
            doc(
                DocumentType::Adjustment,
                Some(AdjustReason::Damage),
                vec![line(t.tea, 3_000)],
            ),
        )
        .await
        .unwrap();
    stock
        .post(
            owner,
            doc(
                DocumentType::Adjustment,
                Some(AdjustReason::Theft),
                vec![line(t.tea, 1_000)],
            ),
        )
        .await
        .unwrap();
    stock
        .post(
            owner,
            doc(
                DocumentType::Adjustment,
                Some(AdjustReason::CountCorrection),
                vec![line(t.tea, -2_000)],
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        balance(&t, t.tea, "sellable").await,
        10_000 - 3_000 - 1_000 - 2_000
    );
    assert_eq!(balance(&t, t.tea, "damaged").await, 3_000);

    // Write-off takes damaged stock, and never more than there is.
    let e = stock
        .post(owner, doc(DocumentType::WriteOff, None, vec![line(t.tea, 4_000)]))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    stock
        .post(
            owner,
            doc(
                DocumentType::WriteOff,
                Some(AdjustReason::Expired),
                vec![line(t.tea, 2_000)],
            ),
        )
        .await
        .unwrap();
    assert_eq!(balance(&t, t.tea, "damaged").await, 1_000);
    assert_eq!(
        count(&t, "SELECT count(*) FROM stock_movements WHERE movement_type = 'write_off' AND bucket = 'damaged' AND quantity_delta_milli = -2000").await,
        1
    );

    // A reason is required, and "other" needs a note; the actor and reason are recorded.
    let e = stock
        .post(
            owner,
            doc(DocumentType::Adjustment, None, vec![line(t.tea, 1_000)]),
        )
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    let e = stock
        .post(
            owner,
            doc(
                DocumentType::Adjustment,
                Some(AdjustReason::Other),
                vec![line(t.tea, 1_000)],
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    assert_eq!(
        count(&t, "SELECT count(*) FROM inventory_documents WHERE doc_type = 'adjustment' AND reason_code = 'theft' AND actor_user_id IS NOT NULL").await,
        1
    );
    assert_eq!(
        count(&t, "SELECT count(*) FROM audit_events WHERE action IN ('inventory.adjustment_posted', 'inventory.write_off_posted')").await,
        4
    );

    // Permissions: a cashier can do neither; a manager can do both.
    let cashier = t.shop.as_role(Role::Cashier);
    for d in [
        doc(DocumentType::Receipt, None, vec![line(t.tea, 1_000)]),
        doc(
            DocumentType::Adjustment,
            Some(AdjustReason::Loss),
            vec![line(t.tea, 1_000)],
        ),
    ] {
        let e = stock.post(&cashier, d).await.unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::PermissionDenied);
    }
    let manager = t.shop.as_role(Role::Manager);
    stock
        .post(
            &manager,
            doc(
                DocumentType::Adjustment,
                Some(AdjustReason::Loss),
                vec![line(t.tea, 1_000)],
            ),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failure_at_any_statement_leaves_nothing_and_uses_no_number() {
    let t = shop().await;
    let mut statement = 0;
    loop {
        let stock = service(&t, t.shop.store.clone().with_fault_before_statement(statement));
        match stock
            .post(
                &t.shop.owner,
                doc(
                    DocumentType::Receipt,
                    None,
                    vec![line(t.tea, 5_000), line(t.cake, 2_000)],
                ),
            )
            .await
        {
            Ok(posted) => {
                assert_eq!(posted.document_no, "R01-IN-000001");
                break;
            }
            Err(AppError::Write(WriteError::FaultInjected { .. })) => {
                for table in [
                    "inventory_documents",
                    "stock_movements",
                    "stock_balances",
                    "outbox_events WHERE event_type = 'inventory.document_posted'",
                    "audit_events WHERE action = 'inventory.receipt_posted'",
                ] {
                    assert_eq!(
                        count(&t, &format!("SELECT count(*) FROM {table}")).await,
                        0,
                        "{table} at {statement}"
                    );
                }
                statement += 1;
            }
            Err(e) => panic!("unexpected error at statement {statement}: {e:?}"),
        }
    }
    assert!(statement >= 6, "only {statement} statements");
}
