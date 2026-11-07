//! E13-T06: the print queue with the virtual printer (AT-09, RCP-006/007).

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::printing::PrintService;
use pos_application::records::PrintStatus;
use pos_application::scan::ScanSource;
use pos_device_ports::PrintFailure;
use pos_device_virtual::printer::{Scenario, VirtualPrinter};
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, Money, ScanEventId, UuidV7Generator};
use std::sync::Arc;
use std::time::Duration;

struct Shop {
    shop: common::Shop,
    printer: Arc<VirtualPrinter>,
    prints: PrintService,
    _captures: tempfile::TempDir,
}

/// A shop with one completed sale (Tea 2.50, cash 5.00).
async fn after_a_sale() -> Shop {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let mut v = VariantInput::simple(Money::from_minor(250));
    v.sku = Some("TEA".into());
    CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    }
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
            variants: vec![v],
        },
    )
    .await
    .unwrap();
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s.clone()),
    ));
    carts
        .scan(
            &shop.owner,
            "TEA",
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Manual,
        )
        .await
        .unwrap();
    let cart = carts.current(&shop.owner).await.unwrap().cart;
    CheckoutService {
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
            tenders: vec![TenderInput {
                kind: TenderKind::Cash,
                amount: Money::from_minor(500),
                external_reference: None,
            }],
        },
    )
    .await
    .unwrap();
    let captures = tempfile::tempdir().unwrap();
    let printer = Arc::new(VirtualPrinter::new(captures.path()));
    let prints = PrintService {
        store: Arc::new(s.clone()),
        prints: Arc::new(s.clone()),
        receipts: Arc::new(s),
        printer: printer.clone(),
        timeout: Duration::from_millis(100),
    };
    Shop {
        shop,
        printer,
        prints,
        _captures: captures,
    }
}

async fn events(t: &Shop, job: pos_domain::PrintJobId) -> Vec<String> {
    sqlx::query_scalar("SELECT status FROM print_job_events WHERE job_id = ? ORDER BY seq")
        .bind(job.to_string())
        .fetch_all(t.shop.store.db().readers())
        .await
        .unwrap()
}

async fn count(t: &Shop, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap()
}

#[tokio::test]
async fn prints_the_stored_receipt_and_marks_later_copies_as_reprints() {
    let t = after_a_sale().await;
    let job = t.prints.print_sale(&t.shop.owner, None).await.unwrap();
    assert_eq!(
        (job.status, job.reprint, job.attempt_count),
        (PrintStatus::Completed, false, 1)
    );
    assert_eq!(events(&t, job.job_id).await, ["queued", "submitted", "completed"]);
    // AT-09: the captured bytes are exactly the stored receipt's rendering.
    let doc: String = sqlx::query_scalar("SELECT document_json FROM receipts")
        .fetch_one(t.shop.store.db().readers())
        .await
        .unwrap();
    let doc: pos_receipts::ReceiptDocument = serde_json::from_str(&doc).unwrap();
    let expected = pos_receipts::text::to_escpos(&pos_receipts::text::render(&doc, 48));
    assert_eq!(
        std::fs::read(t.printer.out_dir().join("R01-000001.bin")).unwrap(),
        expected
    );

    // A second copy is a reprint: marked, audited, and permitted for every role.
    let again = t
        .prints
        .print_sale(&t.shop.as_role(pos_domain::Role::Cashier), None)
        .await
        .unwrap();
    assert!(again.reprint);
    let reprint = std::fs::read_to_string(t.printer.out_dir().join("R01-000001-reprint.txt")).unwrap();
    assert!(reprint.contains("REPRINT"), "{reprint}");
    assert!(
        !std::fs::read_to_string(t.printer.out_dir().join("R01-000001.txt"))
            .unwrap()
            .contains("REPRINT")
    );
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_events WHERE action = 'receipt.reprinted'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn no_answer_is_unknown_and_never_resubmitted() {
    let t = after_a_sale().await;
    t.printer.script([Scenario::Timeout]);
    let job = t.prints.print_sale(&t.shop.owner, None).await.unwrap();
    assert_eq!(job.status, PrintStatus::Unknown);
    assert_eq!(
        t.printer.submissions(),
        1,
        "exactly one submission: no automatic resubmit"
    );
    assert_eq!(events(&t, job.job_id).await, ["queued", "submitted", "unknown"]);
    // An unknown job cannot simply be retried: the operator must say what happened.
    let e = t.prints.retry(&t.shop.owner, job.job_id).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    assert_eq!(t.printer.submissions(), 1);

    // "Not printed": a new job, printed as a reprint (the first may yet appear).
    let reprint = t
        .prints
        .resolve_unknown(&t.shop.owner, job.job_id, false)
        .await
        .unwrap();
    assert_ne!(reprint.job_id, job.job_id);
    assert_eq!((reprint.status, reprint.reprint), (PrintStatus::Completed, true));
    assert_eq!(
        events(&t, job.job_id).await,
        ["queued", "submitted", "unknown", "failed"]
    );
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_events WHERE action = 'print.unknown_resolved'"
        )
        .await,
        1
    );
    // The sale was never affected.
    assert_eq!(count(&t, "SELECT count(*) FROM sales").await, 1);
}

#[tokio::test]
async fn the_operator_can_confirm_an_unknown_job_printed() {
    let t = after_a_sale().await;
    t.printer.script([Scenario::Timeout]);
    let job = t.prints.print_sale(&t.shop.owner, None).await.unwrap();
    let done = t
        .prints
        .resolve_unknown(&t.shop.owner, job.job_id, true)
        .await
        .unwrap();
    assert_eq!(done.status, PrintStatus::Completed);
    assert_eq!(t.printer.submissions(), 1);
    // Resolving twice is refused.
    assert!(
        t.prints
            .resolve_unknown(&t.shop.owner, job.job_id, true)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_refused_job_can_be_retried_and_stays_the_original() {
    let t = after_a_sale().await;
    t.printer.script([Scenario::Fail(PrintFailure::PaperOut)]);
    let job = t.prints.print_sale(&t.shop.owner, None).await.unwrap();
    assert_eq!(
        (job.status, job.last_error_code.as_deref()),
        (PrintStatus::Failed, Some("paper_out"))
    );
    assert!(
        !t.printer.out_dir().join("R01-000001.bin").exists(),
        "nothing printed"
    );
    let retried = t.prints.retry(&t.shop.owner, job.job_id).await.unwrap();
    assert_eq!(
        (
            retried.job_id,
            retried.status,
            retried.reprint,
            retried.attempt_count
        ),
        (job.job_id, PrintStatus::Completed, false, 2)
    );
    assert!(
        !std::fs::read_to_string(t.printer.out_dir().join("R01-000001.txt"))
            .unwrap()
            .contains("REPRINT")
    );
    assert_eq!(
        events(&t, job.job_id).await,
        ["queued", "submitted", "failed", "submitted", "completed"]
    );
    // A completed job is not retried.
    assert!(t.prints.retry(&t.shop.owner, job.job_id).await.is_err());
    // A failed (never printed) original does not make the next print a reprint.
    assert_eq!(
        count(
            &t,
            "SELECT count(*) FROM audit_events WHERE action = 'receipt.reprinted'"
        )
        .await,
        0
    );
}
