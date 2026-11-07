//! E15-T02: cash sessions against a real database (DER-001).

mod common;

use pos_application::WriteError;
use pos_application::cart::CartService;
use pos_application::cash::CashService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale, CompletedSale};
use pos_application::context::{AppError, CommandContext};
use pos_application::queries::{CashQueries, QueryError};
use pos_application::records::CashSessionRecord;
use pos_application::returns::{CreateReturn, ReturnService};
use pos_application::scan::ScanSource;
use pos_domain::cash::SessionTotals;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::returns::{Disposition, ReturnLineRequest, ReturnReason};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{
    CashSessionId, ErrorCode, HasErrorCode, Money, Quantity, ReceiptId, RegisterId, Role, ScanEventId,
    UuidV7Generator,
};
use pos_local_db::SqliteStore;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn tender(kind: TenderKind, minor: i64) -> TenderInput {
    TenderInput {
        kind,
        amount: Money::from_minor(minor),
        external_reference: None,
    }
}

struct Till {
    shop: common::Shop,
    carts: Arc<CartService>,
}

impl Till {
    fn checkout(&self) -> CheckoutService {
        let s = self.shop.store.clone();
        CheckoutService {
            carts: self.carts.clone(),
            store: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            stock: Arc::new(s.clone()),
            receipts: Arc::new(s),
        }
    }

    fn cash(&self, store: SqliteStore) -> CashService {
        let s = self.shop.store.clone();
        CashService {
            store: Arc::new(store),
            cash: Arc::new(s.clone()),
            receipts: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            pricing: Arc::new(s),
        }
    }

    fn returns(&self) -> ReturnService {
        let s = self.shop.store.clone();
        ReturnService {
            store: Arc::new(s.clone()),
            returns: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            receipts: Arc::new(s.clone()),
            pricing: Arc::new(s),
        }
    }

    async fn sell(
        &self,
        ctx: &CommandContext,
        codes: &[&str],
        tenders: Vec<TenderInput>,
    ) -> Result<CompletedSale, AppError> {
        for code in codes {
            self.carts
                .scan(
                    ctx,
                    code,
                    ScanEventId::generate(&UuidV7Generator),
                    ScanSource::Manual,
                )
                .await
                .unwrap();
        }
        let cart = self.carts.current(ctx).await.unwrap().cart;
        self.checkout()
            .complete_sale(
                ctx,
                CompleteSale {
                    cart_id: cart.id,
                    revision: cart.revision,
                    tenders,
                },
            )
            .await
    }

    async fn refund(&self, receipt: &str, item: &str, kind: TenderKind) -> Result<(), AppError> {
        let r = self.returns();
        let sale = r.find(&self.shop.owner, receipt).await.unwrap();
        let line = sale.lines.iter().find(|l| l.name == item).unwrap();
        let unit = line.total.minor() * 1000 / line.quantity.milli();
        r.create(
            &self.shop.owner,
            CreateReturn {
                sale_id: sale.sale_id,
                lines: vec![ReturnLineRequest {
                    sale_line_id: line.sale_line_id,
                    quantity: Quantity::from_milli(1000),
                    disposition: Disposition::Restock,
                }],
                reason: ReturnReason::ChangedMind,
                note: None,
                refunds: vec![tender(kind, unit)],
            },
        )
        .await
        .map(|_| ())
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.shop.store.db().readers())
            .await
            .unwrap()
    }
}

/// A live till with Basket 13.45, Box 15.00 and Tea 2.50 (all stock items).
async fn live_till() -> Till {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    for (name, price) in [("Basket", 1345), ("Box", 1500), ("Tea", 250)] {
        let mut v = VariantInput::simple(Money::from_minor(price));
        v.sku = Some(name.to_uppercase());
        catalog
            .create(
                &shop.owner,
                ProductInput {
                    name: name.into(),
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
    }
    sqlx::query("UPDATE device_identity SET mode = 'live'")
        .execute(s.db().writer())
        .await
        .unwrap();
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s),
    ));
    Till { shop, carts }
}

#[tokio::test]
async fn a_day_in_the_drawer() {
    let t = live_till().await;
    let owner = &t.shop.owner;
    let cashier = t.shop.as_role(Role::Cashier);
    let cash = t.cash(t.shop.store.clone());

    // Live, setting on (the default): no sale without an open drawer, and nothing is used up.
    let state = cash.current(&cashier).await.unwrap();
    assert!(state.required && state.session.is_none());
    let e = t
        .sell(owner, &["BASKET"], vec![tender(TenderKind::Cash, 2000)])
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::CashSessionRequired);
    assert_eq!(t.count("SELECT count(*) FROM sales").await, 0);
    assert_eq!(
        t.carts.current(owner).await.unwrap().cart.lines.len(),
        1,
        "the cart is kept"
    );

    // A cashier opens with a 100.00 float; a second open is refused.
    let opened = cash.open(&cashier, Money::from_minor(10_000)).await.unwrap();
    assert_eq!((opened.number.as_str(), opened.open), ("R01-Z-0001", true));
    assert_eq!(
        cash.open(owner, Money::from_minor(5_000))
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::CashSessionAlreadyOpen
    );
    assert!(cash.open(owner, Money::from_minor(-1)).await.is_err());

    // The fixture day. Cash kept: 13.45 + 5.00; cash refunded: 2.50.
    let s1 = t
        .sell(owner, &[], vec![tender(TenderKind::Cash, 2000)])
        .await
        .unwrap(); // Basket 13.45, change 6.55
    assert_eq!(s1.sale.change.minor(), 655);
    t.sell(
        owner,
        &["BOX"],
        vec![
            tender(TenderKind::CardExternal, 1000),
            tender(TenderKind::Cash, 500),
        ],
    )
    .await
    .unwrap();
    let s3 = t
        .sell(
            owner,
            &["TEA", "TEA"],
            vec![tender(TenderKind::CardExternal, 500)],
        )
        .await
        .unwrap();
    t.refund(&s3.receipt_number, "Tea", TenderKind::Cash)
        .await
        .unwrap();
    t.refund(&s1.receipt_number, "Basket", TenderKind::CardExternal)
        .await
        .unwrap();
    assert_eq!(
        t.count("SELECT count(*) FROM sales WHERE cash_session_id IS NOT NULL")
            .await,
        3
    );
    assert_eq!(
        t.count("SELECT count(*) FROM returns WHERE cash_session_id IS NOT NULL")
            .await,
        2
    );

    // A manager sees the running figures; a cashier does not (blind count).
    let now = cash.current(owner).await.unwrap();
    let expected = 10_000 + 1345 + 500 - 250;
    assert_eq!(now.expected.map(|m| m.minor()), Some(expected)); // 115.95
    let totals = now.totals.unwrap();
    assert_eq!(
        (totals.sales_count, totals.sales_total.minor()),
        (3, 1345 + 1500 + 500)
    );
    assert_eq!(
        (totals.returns_count, totals.refunds_total.minor()),
        (2, 250 + 1345)
    );
    let blind = cash.current(&cashier).await.unwrap();
    assert!(blind.session.is_some() && blind.totals.is_none() && blind.expected.is_none());

    // The cashier closes with a count: only the count comes back.
    let closed = cash
        .close(
            &cashier,
            Money::from_minor(11_550),
            Some(" counted twice ".into()),
        )
        .await
        .unwrap();
    assert_eq!(closed.counted.minor(), 11_550);
    assert!(closed.expected.is_none() && closed.variance.is_none() && closed.totals.is_none());
    assert!(!closed.session.open);
    let row: (i64, i64, i64, String) = sqlx::query_as(
        "SELECT expected_cash_minor, counted_cash_minor, variance_minor, note FROM cash_sessions",
    )
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap();
    assert_eq!(row, (expected, 11_550, 11_550 - expected, "counted twice".into())); // 0.45 short

    // The Z summary: the manager can read it; the cashier cannot.
    let z = cash.z_summary(owner, None, Some(58)).await.unwrap();
    let text: String = z.lines.iter().map(|l| format!("{}\n", l.text)).collect();
    assert!(
        text.contains("EXPECTED CASH") && text.contains("115.95"),
        "{text}"
    );
    assert!(
        text.contains("VARIANCE (SHORT)") && text.contains("-0.45"),
        "{text}"
    );
    assert_eq!(
        cash.z_summary(&cashier, None, None)
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::PermissionDenied
    );

    // Events and audit, both halves of the fact.
    assert_eq!(
        t.count("SELECT count(*) FROM outbox_events WHERE event_type IN ('cash_session.opened', 'cash_session.closed')")
            .await,
        2
    );
    assert_eq!(
        t.count(
            "SELECT count(*) FROM audit_events WHERE action IN ('cash_session.opened', 'cash_session.closed')"
        )
        .await,
        2
    );
    // Closing is final.
    let e = sqlx::query("UPDATE cash_sessions SET counted_cash_minor = 0")
        .execute(t.shop.store.db().writer())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("final"), "{e}");
    assert_eq!(
        cash.close(owner, Money::from_minor(0), None)
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::CashSessionRequired
    );

    // Closed: no sales, no cash refunds; a card refund is fine. The next session is Z-0002.
    let e = t
        .sell(owner, &["TEA"], vec![tender(TenderKind::Cash, 250)])
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::CashSessionRequired);
    let e = t
        .refund(&s3.receipt_number, "Tea", TenderKind::Cash)
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::CashSessionRequired);
    t.refund(&s3.receipt_number, "Tea", TenderKind::CardExternal)
        .await
        .unwrap();
    let next = cash.open(owner, Money::from_minor(10_000)).await.unwrap();
    assert_eq!(next.number, "R01-Z-0002");
    assert_eq!(
        cash.current(owner).await.unwrap().expected.unwrap().minor(),
        10_000
    );
}

#[tokio::test]
async fn sandbox_and_the_setting_off_need_no_session() {
    let t = live_till().await;
    let owner = &t.shop.owner;
    sqlx::query("INSERT OR REPLACE INTO business_settings (business_id, key, value_json, updated_at) SELECT business_id, 'cash.require_session_for_sale', 'false', 0 FROM businesses")
        .execute(t.shop.store.db().writer())
        .await
        .unwrap();
    t.sell(owner, &["TEA"], vec![tender(TenderKind::Cash, 250)])
        .await
        .unwrap();
    sqlx::query("UPDATE device_identity SET mode = 'sandbox'")
        .execute(t.shop.store.db().writer())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE business_settings SET value_json = 'true' WHERE key = 'cash.require_session_for_sale'",
    )
    .execute(t.shop.store.db().writer())
    .await
    .unwrap();
    t.sell(owner, &["TEA"], vec![tender(TenderKind::Cash, 250)])
        .await
        .unwrap();
    assert!(
        !t.cash(t.shop.store.clone())
            .current(owner)
            .await
            .unwrap()
            .required
    );
    // Without an open session the sales simply belong to none.
    assert_eq!(
        t.count("SELECT count(*) FROM sales WHERE cash_session_id IS NULL")
            .await,
        2
    );
}

/// Totals that are stale on the first `stale` reads: a sale "landed" after they were read.
struct Racing {
    inner: SqliteStore,
    stale: usize,
    reads: AtomicUsize,
}

#[async_trait::async_trait]
impl CashQueries for Racing {
    async fn open_cash_session(&self, r: RegisterId) -> Result<Option<CashSessionRecord>, QueryError> {
        self.inner.open_cash_session(r).await
    }
    async fn cash_session(&self, id: CashSessionId) -> Result<Option<CashSessionRecord>, QueryError> {
        self.inner.cash_session(id).await
    }
    async fn cash_session_totals(&self, id: CashSessionId) -> Result<SessionTotals, QueryError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) < self.stale {
            return Ok(SessionTotals::default());
        }
        self.inner.cash_session_totals(id).await
    }
    async fn last_closed_cash_session(&self, r: RegisterId) -> Result<Option<CashSessionId>, QueryError> {
        self.inner.last_closed_cash_session(r).await
    }
    async fn cash_session_receipt(
        &self,
        id: CashSessionId,
    ) -> Result<Option<(ReceiptId, String)>, QueryError> {
        self.inner.cash_session_receipt(id).await
    }
}

#[tokio::test]
async fn a_close_never_records_totals_that_moved() {
    let t = live_till().await;
    let owner = &t.shop.owner;
    t.cash(t.shop.store.clone())
        .open(owner, Money::from_minor(1000))
        .await
        .unwrap();
    t.sell(owner, &["TEA"], vec![tender(TenderKind::Cash, 250)])
        .await
        .unwrap();

    // Stale forever: refused with a version conflict and nothing written.
    let mut racing = t.cash(t.shop.store.clone());
    racing.cash = Arc::new(Racing {
        inner: t.shop.store.clone(),
        stale: usize::MAX,
        reads: AtomicUsize::new(0),
    });
    let e = racing
        .close(owner, Money::from_minor(1250), None)
        .await
        .unwrap_err();
    assert!(
        matches!(e, AppError::Write(WriteError::CashSessionChanged)),
        "{e:?}"
    );
    assert_eq!(e.error_code(), ErrorCode::ConflictVersion);
    assert_eq!(
        t.count("SELECT count(*) FROM cash_sessions WHERE status = 'open'")
            .await,
        1
    );
    assert_eq!(
        t.count("SELECT count(*) FROM receipts WHERE source_type = 'cash_session'")
            .await,
        0
    );

    // Stale once: read again and close with the true figures.
    racing.cash = Arc::new(Racing {
        inner: t.shop.store.clone(),
        stale: 1,
        reads: AtomicUsize::new(0),
    });
    let closed = racing.close(owner, Money::from_minor(1250), None).await.unwrap();
    assert_eq!(
        (closed.expected.unwrap().minor(), closed.variance.unwrap().minor()),
        (1250, 0)
    );
}

#[tokio::test]
async fn a_failure_at_any_statement_leaves_the_session_open() {
    let t = live_till().await;
    let owner = &t.shop.owner;
    t.cash(t.shop.store.clone())
        .open(owner, Money::from_minor(1000))
        .await
        .unwrap();
    t.sell(owner, &["TEA"], vec![tender(TenderKind::Cash, 500)])
        .await
        .unwrap();
    let mut statement = 0;
    loop {
        match t
            .cash(t.shop.store.clone().with_fault_before_statement(statement))
            .close(owner, Money::from_minor(1250), None)
            .await
        {
            Ok(closed) => {
                assert!(!closed.session.open);
                break;
            }
            Err(AppError::Write(WriteError::FaultInjected { .. })) => {
                assert_eq!(
                    t.count("SELECT count(*) FROM cash_sessions WHERE status = 'open'")
                        .await,
                    1
                );
                assert_eq!(
                    t.count("SELECT count(*) FROM receipts WHERE source_type = 'cash_session'")
                        .await,
                    0
                );
                assert_eq!(
                    t.count("SELECT count(*) FROM outbox_events WHERE event_type = 'cash_session.closed'")
                        .await,
                    0
                );
                statement += 1;
            }
            Err(e) => panic!("unexpected error at statement {statement}: {e:?}"),
        }
    }
    assert!(statement >= 5, "only {statement} statements");
}
