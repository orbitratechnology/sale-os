//! E10-T02 (PAY-004..006, PRD §12.5): card payments on a terminal, through the mock
//! provider, against a real database. One test per scenario of the acceptance list, plus the
//! refusals, atomicity, crash recovery and the PAY-005 guarantees of the schema.
//!
//! Test data only: the mock's references are `mock_ch_…`, its masked card `****4242`.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale, CompletedSale};
use pos_application::context::{AppError, ApprovalToken, CommandContext};
use pos_application::payments::{ProviderPaymentFlow, ProviderTender, WaitPolicy};
use pos_application::scan::ScanSource;
use pos_application::settings::SettingsService;
use pos_application::{AppliedWriteSet, LocalStore, WriteError, WriteSet};
use pos_device_ports::{PaymentKind, PaymentPort, PaymentRequest};
use pos_device_virtual::payment::{MockPaymentProvider, PaymentScenario};
use pos_domain::catalog::{BarcodeInput, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::payment::{Approval, AttemptStatus, PaymentAttempt, Resolution};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{
    ApprovalId, ApprovalScope, CurrencyCode, ErrorCode, HasErrorCode, Money, Permission, Role, ScanEventId,
    Timestamp, UserId, UuidV7Generator,
};
use pos_local_db::SqliteStore;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct Till {
    shop: common::Shop,
    carts: Arc<CartService>,
    provider: Arc<MockPaymentProvider>,
}

async fn till() -> Till {
    till_on(common::shop().await).await
}

/// Tea 2.50 (stock) and Gift wrap 1.00 (service) in the cart: 2 teas and a wrap = 6.00.
async fn till_on(shop: common::Shop) -> Till {
    let s = shop.store.clone();
    let t = Till {
        carts: Arc::new(CartService::new(
            Arc::new(s.clone()),
            Arc::new(s.clone()),
            Arc::new(s.clone()),
        )),
        provider: Arc::new(MockPaymentProvider::new()),
        shop,
    };
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s),
    };
    for (name, price, code, ptype) in [
        ("Tea", 250, "TEA", ProductType::Stock),
        ("Gift wrap", 100, "WRAP", ProductType::Service),
    ] {
        let mut v = VariantInput::simple(Money::from_minor(price));
        v.sku = Some(code.into());
        v.barcodes = vec![BarcodeInput::alias(&format!("2000000{code}"))];
        catalog
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
            .unwrap();
    }
    for code in ["TEA", "TEA", "WRAP"] {
        t.carts
            .scan(
                &t.shop.owner,
                code,
                ScanEventId::generate(&UuidV7Generator),
                ScanSource::Manual,
            )
            .await
            .unwrap();
    }
    t
}

impl Till {
    fn flow(&self) -> ProviderPaymentFlow {
        self.flow_on(Arc::new(self.shop.store.clone()), Duration::from_secs(5))
    }

    fn flow_on(&self, store: Arc<dyn LocalStore>, timeout: Duration) -> ProviderPaymentFlow {
        let q = self.shop.store.clone();
        ProviderPaymentFlow {
            store,
            payments: Arc::new(q.clone()),
            carts: Arc::new(q.clone()),
            setup: Arc::new(q),
            port: self.provider.clone(),
            timeout,
        }
    }

    fn checkout(&self, store: SqliteStore) -> CheckoutService {
        let q = self.shop.store.clone();
        CheckoutService {
            carts: self.carts.clone(),
            store: Arc::new(store),
            setup: Arc::new(q.clone()),
            stock: Arc::new(q.clone()),
            receipts: Arc::new(q),
        }
    }

    fn owner(&self) -> &CommandContext {
        &self.shop.owner
    }

    async fn cart(&self) -> (pos_domain::CartId, u64) {
        let c = self.carts.current(self.owner()).await.unwrap().cart;
        (c.id, c.revision)
    }

    async fn start(&self, minor: i64) -> PaymentAttempt {
        let (cart, _) = self.cart().await;
        self.flow()
            .start(self.owner(), cart, Money::from_minor(minor))
            .await
            .unwrap()
    }

    /// Completes the open cart with these approved attempts and operator tenders.
    async fn pay(
        &self,
        ctx: &CommandContext,
        attempts: &[&PaymentAttempt],
        tenders: Vec<TenderInput>,
    ) -> Result<CompletedSale, AppError> {
        self.pay_on(self.shop.store.clone(), ctx, attempts, tenders).await
    }

    async fn pay_on(
        &self,
        store: SqliteStore,
        ctx: &CommandContext,
        attempts: &[&PaymentAttempt],
        tenders: Vec<TenderInput>,
    ) -> Result<CompletedSale, AppError> {
        let (cart_id, revision) = self.cart().await;
        let provider: Vec<ProviderTender> = attempts
            .iter()
            .map(|a| ProviderTender {
                attempt_id: a.id(),
                amount: a.amount(),
            })
            .collect();
        self.flow()
            .complete_sale(
                ctx,
                &self.checkout(store),
                CompleteSale {
                    cart_id,
                    revision,
                    tenders,
                },
                &provider,
            )
            .await
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.shop.store.db().readers())
            .await
            .unwrap()
    }

    async fn text(&self, sql: &str) -> String {
        sqlx::query_scalar(sql)
            .fetch_one(self.shop.store.db().readers())
            .await
            .unwrap()
    }

    /// `(from, to, source)` of an attempt's events, in order.
    async fn events(&self, attempt: &PaymentAttempt) -> Vec<(Option<String>, String, String)> {
        sqlx::query_as(
            "SELECT from_status, to_status, source FROM payment_attempt_events WHERE attempt_id = ? ORDER BY seq",
        )
        .bind(attempt.id().to_string())
        .fetch_all(self.shop.store.db().readers())
        .await
        .unwrap()
    }

    async fn audits(&self, attempt: &PaymentAttempt) -> Vec<serde_json::Value> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT metadata_json FROM audit_events WHERE action = 'tender.status_changed' AND entity_id = ?
             ORDER BY rowid",
        )
        .bind(attempt.id().to_string())
        .fetch_all(self.shop.store.db().readers())
        .await
        .unwrap();
        rows.iter().map(|r| serde_json::from_str(r).unwrap()).collect()
    }

    fn cashier(&self) -> CommandContext {
        self.shop.as_role(Role::Cashier)
    }

    fn manager(&self) -> CommandContext {
        self.shop.as_role(Role::Manager)
    }

    /// A cashier holding a manager's approval for `permission` on `entity`.
    fn cashier_approved(
        &self,
        permission: Permission,
        entity: impl ToString,
        manager: UserId,
    ) -> CommandContext {
        let mut c = self.cashier();
        c.approval = Some(ApprovalToken {
            id: ApprovalId::generate(&UuidV7Generator),
            approver: manager,
            scope: ApprovalScope {
                permission,
                entity_id: entity.to_string(),
                max_ppm: None,
            },
            expires_at: Timestamp::from_millis(c.now().as_millis() + 60_000),
        });
        c
    }
}

fn cash(minor: i64) -> TenderInput {
    TenderInput {
        kind: TenderKind::Cash,
        amount: Money::from_minor(minor),
        external_reference: None,
    }
}

fn code(e: &AppError) -> ErrorCode {
    e.error_code()
}

// ---------- Approve ----------

#[tokio::test]
async fn approve_gives_a_provider_confirmed_tender_on_the_completed_sale() {
    let t = till().await;
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::Approved);
    let Some(Approval::Provider(card)) = a.approval() else {
        panic!("{:?}", a.approval())
    };
    assert!(card.provider_ref().starts_with("mock_ch_"));
    assert_eq!(card.masked_pan().unwrap().as_str(), "****4242");

    let done = t.pay(t.owner(), &[&a], vec![]).await.unwrap();
    assert_eq!((done.sale.total.minor(), done.sale.change.minor()), (600, 0));
    let tender = &done.sale.tenders[0];

    // The tender as stored: card_external, confirmed by the provider, with its reference.
    let row: (String, String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT tender_type, status, confirmation_source, provider_code, external_reference FROM tenders",
    )
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "card_external".into(),
            "confirmed".into(),
            "provider".into(),
            Some("mock".into()),
            Some(card.provider_ref().to_owned())
        )
    );
    assert_eq!(
        t.text("SELECT source FROM tender_status_events").await,
        "provider"
    );
    // The attempt is linked to the tender in the same transaction, and is now final.
    assert_eq!(
        t.text(&format!(
            "SELECT tender_id FROM payment_attempts WHERE attempt_id = '{}'",
            a.id()
        ))
        .await,
        tender.id.to_string()
    );
    assert_eq!(
        t.text(&format!(
            "SELECT masked_pan FROM payment_attempts WHERE attempt_id = '{}'",
            a.id()
        ))
        .await,
        "****4242"
    );
    assert_eq!(
        t.events(&a).await,
        vec![
            (None, "pending".into(), "operator".into()),
            (Some("pending".into()), "approved".into(), "provider".into()),
        ]
    );
    let audits = t.audits(&a).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[1]["to"], "approved");
    assert_eq!(audits[1]["approvalSource"], "provider");
    let sale_audit: serde_json::Value = serde_json::from_str(
        &t.text("SELECT metadata_json FROM audit_events WHERE action = 'tender.status_changed' AND entity_type = 'sale'")
            .await,
    )
    .unwrap();
    assert_eq!(sale_audit["confirmationSource"], "provider");
    assert_eq!(sale_audit["paymentAttemptId"], a.id().to_string());
    assert_eq!(
        t.count("SELECT count(*) FROM audit_events WHERE action = 'tender.recorded_external'")
            .await,
        0,
        "not recorded by the operator"
    );

    // The sync payload carries the provider confirmation (sale.completed v1, unchanged), and
    // every event still matches its contract.
    let payload: serde_json::Value = serde_json::from_str(
        &t.text("SELECT payload_json FROM outbox_events WHERE event_type = 'sale.completed'")
            .await,
    )
    .unwrap();
    assert_eq!(
        payload["tenders"][0]["confirmation"],
        serde_json::json!({ "provider": { "reference": card.provider_ref() } })
    );
    assert_eq!(payload["tenders"][0]["external_reference"], card.provider_ref());
    common::contract::check_outbox(&t.shop.store).await;

    assert_eq!(t.provider.charges().len(), 1);
    // Used: it cannot pay again or be reversed.
    let used = t.flow().poll(t.owner(), a.id()).await.unwrap();
    assert_eq!(used.tender_id(), Some(tender.id));
    let manager = t.manager();
    assert_eq!(
        code(
            &t.flow()
                .reverse(&manager, a.id(), "customer changed mind")
                .await
                .unwrap_err()
        ),
        ErrorCode::ValidationFailed
    );
}

#[tokio::test]
async fn a_card_approval_and_cash_split_the_sale() {
    let t = till().await;
    let a = t.start(400).await;
    let done = t.pay(t.owner(), &[&a], vec![cash(500)]).await.unwrap();
    assert_eq!(done.sale.change.minor(), 300, "change from the cash only");
    assert_eq!(
        t.count("SELECT count(*) FROM tenders WHERE confirmation_source = 'provider' AND amount_minor = 400")
            .await,
        1
    );
    assert_eq!(
        t.count("SELECT count(*) FROM tenders WHERE tender_type = 'cash' AND change_minor = 300")
            .await,
        1
    );
    assert_eq!(done.sale.rounding.minor(), 0, "no cash rounding with a card");
}

// ---------- Decline ----------

#[tokio::test]
async fn a_declined_payment_cannot_pay_for_the_sale() {
    let t = till().await;
    t.provider
        .script([PaymentScenario::Decline("insufficient funds".into())]);
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::Declined);
    assert_eq!(a.status_reason(), Some("insufficient funds"));

    let e = t.pay(t.owner(), &[&a], vec![]).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::ValidationFailed);
    assert_eq!(t.count("SELECT count(*) FROM sales").await, 0);
    assert!(t.provider.charges().is_empty());

    // A decline settles the outcome: the cart can be paid another way, or by a new attempt.
    let again = t.start(600).await;
    assert_eq!(again.status(), AttemptStatus::Approved);
    t.pay(t.owner(), &[&again], vec![]).await.unwrap();
}

// ---------- Timeout / Unknown ----------

#[tokio::test]
async fn a_timeout_needs_reconciliation_blocks_the_sale_and_a_manager_settles_it() {
    let t = till().await;
    t.provider.script([PaymentScenario::Timeout]);
    let a = t.start(600).await;
    assert_eq!(
        a.status(),
        AttemptStatus::ReconciliationRequired,
        "never declined"
    );
    assert_eq!(
        t.events(&a).await[1],
        (
            Some("pending".into()),
            "reconciliation_required".into(),
            "provider".into()
        )
    );

    // The sale is blocked: not with cash, and not with a second card payment.
    let e = t.pay(t.owner(), &[], vec![cash(600)]).await.unwrap_err();
    assert!(
        matches!(e, AppError::Write(WriteError::PaymentUnresolved)),
        "{e:?}"
    );
    assert_eq!(code(&e), ErrorCode::ValidationFailed);
    assert_eq!(t.count("SELECT count(*) FROM sales").await, 0);
    let (cart, _) = t.cart().await;
    assert_eq!(
        code(
            &t.flow()
                .start(t.owner(), cart, Money::from_minor(600))
                .await
                .unwrap_err()
        ),
        ErrorCode::ValidationFailed
    );
    // A query that still cannot tell changes nothing.
    let polled = t.flow().poll(t.owner(), a.id()).await.unwrap();
    assert_eq!(polled.status(), AttemptStatus::ReconciliationRequired);
    assert_eq!(t.events(&a).await.len(), 2);

    // Settling needs a manager (a cashier, their approval for this attempt) and a reason.
    let cashier = t.cashier();
    let e = t
        .flow()
        .resolve(&cashier, a.id(), Resolution::Declined, "no slip")
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ApprovalRequired);
    let manager = t.manager();
    let e = t
        .flow()
        .resolve(&manager, a.id(), Resolution::Declined, "  ")
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ValidationFailed);
    // A card number is never accepted as the slip's reference (PAY-005).
    let e = t
        .flow()
        .resolve(
            &manager,
            a.id(),
            Resolution::Approved {
                reference: Some("4111111111111111".into()),
            },
            "slip",
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ValidationFailed);

    let mgr_id = manager.actor.as_ref().unwrap().user_id;
    let approved = t.cashier_approved(Permission::CartVoid, a.id(), mgr_id);
    let settled = t
        .flow()
        .resolve(
            &approved,
            a.id(),
            Resolution::Approved {
                reference: Some("AUTH 004211".into()),
            },
            "terminal slip shows APPROVED",
        )
        .await
        .unwrap();
    assert_eq!(settled.status(), AttemptStatus::Approved);
    assert_eq!(
        settled.approval(),
        Some(&Approval::Operator {
            reference: Some("AUTH 004211".into())
        })
    );
    // Audited with the approver and the reason, and sent to the cloud audit.
    let (approver, metadata, linked): (Option<String>, String, Option<String>) = sqlx::query_as(
        "SELECT approver_user_id, metadata_json, outbox_event_id FROM audit_events
         WHERE action = 'tender.status_changed' AND entity_id = ? ORDER BY rowid DESC LIMIT 1",
    )
    .bind(a.id().to_string())
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap();
    assert_eq!(approver, Some(mgr_id.to_string()));
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["reason"], "terminal slip shows APPROVED");
    assert_eq!(metadata["from"], "reconciliation_required");
    assert!(linked.is_some());
    assert_eq!(
        t.count("SELECT count(*) FROM outbox_events WHERE event_type = 'audit.recorded'")
            .await,
        1
    );
    assert_eq!(t.events(&a).await[2].2, "operator");

    // Now it pays for the sale, confirmed by the operator (not the provider: PRD §6.5).
    t.pay(t.owner(), &[&settled], vec![]).await.unwrap();
    let row: (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT confirmation_source, provider_code, external_reference FROM tenders")
            .fetch_one(t.shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(
        row,
        ("operator".into(), Some("mock".into()), Some("AUTH 004211".into()))
    );
    common::contract::check_outbox(&t.shop.store).await;
}

#[tokio::test]
async fn an_unknown_outcome_settled_as_declined_frees_the_cart() {
    let t = till().await;
    t.provider.script([PaymentScenario::Unknown]);
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::ReconciliationRequired);
    assert!(t.pay(t.owner(), &[], vec![cash(600)]).await.is_err());
    let manager = t.manager();
    let settled = t
        .flow()
        .resolve(
            &manager,
            a.id(),
            Resolution::Declined,
            "terminal shows no transaction",
        )
        .await
        .unwrap();
    assert_eq!(settled.status(), AttemptStatus::Declined);
    // Final: it cannot be settled again.
    assert!(
        t.flow()
            .resolve(&manager, a.id(), Resolution::Cancelled, "again")
            .await
            .is_err()
    );
    t.pay(t.owner(), &[], vec![cash(600)]).await.unwrap();
}

#[tokio::test]
async fn a_later_query_settles_an_unknown_outcome() {
    let t = till().await;
    t.provider.script([PaymentScenario::Timeout]);
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::ReconciliationRequired);
    // The terminal's own record shows it went through.
    t.provider.finish_on_terminal(a.id(), true);
    let polled = t.flow().poll(t.owner(), a.id()).await.unwrap();
    assert_eq!(polled.status(), AttemptStatus::Approved);
    assert!(matches!(polled.approval(), Some(Approval::Provider(_))));
    t.pay(t.owner(), &[&polled], vec![]).await.unwrap();
    assert_eq!(
        t.text("SELECT confirmation_source FROM tenders").await,
        "provider"
    );
    assert_eq!(t.provider.charges().len(), 1);
}

#[tokio::test]
async fn a_terminal_that_never_answers_needs_reconciliation() {
    let t = till().await;
    t.provider.script([PaymentScenario::Hang]);
    let (cart, _) = t.cart().await;
    let a = t
        .flow_on(Arc::new(t.shop.store.clone()), Duration::from_millis(50))
        .start(t.owner(), cart, Money::from_minor(600))
        .await
        .unwrap();
    assert_eq!(a.status(), AttemptStatus::ReconciliationRequired);
    assert!(a.status_reason().unwrap().contains("no answer"));
}

// ---------- not sent ----------

#[tokio::test]
async fn a_terminal_that_cannot_be_reached_declines_safely() {
    let t = till().await;
    t.provider.script([PaymentScenario::Disconnected]);
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::Declined, "nothing was sent");
    assert!(a.status_reason().unwrap().starts_with("not sent"));
    assert_eq!(t.events(&a).await[1].2, "system");
    assert!(!t.provider.knows(a.id()));
    assert!(t.provider.charges().is_empty());
    // The cart is free.
    t.pay(t.owner(), &[], vec![cash(600)]).await.unwrap();
}

// ---------- DuplicateCallback ----------

#[tokio::test]
async fn a_duplicate_callback_makes_one_transition() {
    let t = till().await;
    t.provider.script([PaymentScenario::DuplicateCallback]);
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::Approved);
    // The approval arrives again, twice and concurrently: nothing more is written.
    let flow = t.flow();
    let (x, y) = tokio::join!(flow.poll(t.owner(), a.id()), flow.poll(t.owner(), a.id()));
    assert_eq!(x.unwrap().status(), AttemptStatus::Approved);
    assert_eq!(y.unwrap().status(), AttemptStatus::Approved);
    assert_eq!(t.provider.deliveries(a.id()), 2);
    assert_eq!(t.events(&a).await.len(), 2, "pending, approved");
    assert_eq!(t.audits(&a).await.len(), 2);
    assert_eq!(t.provider.charges().len(), 1);
}

#[tokio::test]
async fn concurrent_answers_for_a_pending_payment_make_one_transition() {
    let t = till().await;
    t.provider.script([PaymentScenario::Delayed(1)]);
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::Pending);
    // Both polls read "pending"; the first query approves, the second repeats it. Only one
    // of them may record the approval.
    let flow = t.flow();
    let (x, y) = tokio::join!(flow.poll(t.owner(), a.id()), flow.poll(t.owner(), a.id()));
    assert_eq!(x.unwrap().status(), AttemptStatus::Approved);
    assert_eq!(y.unwrap().status(), AttemptStatus::Approved);
    assert_eq!(t.events(&a).await.len(), 2);
}

// ---------- Delayed ----------

#[tokio::test]
async fn a_delayed_payment_is_pending_then_approved() {
    let t = till().await;
    t.provider.script([PaymentScenario::Delayed(2)]);
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::Pending);
    // While pending the sale is blocked.
    assert!(t.pay(t.owner(), &[], vec![cash(600)]).await.is_err());
    let p = t.flow().poll(t.owner(), a.id()).await.unwrap();
    assert_eq!(p.status(), AttemptStatus::Pending);
    assert_eq!(t.events(&a).await.len(), 1, "still pending: nothing written");
    let p = t.flow().poll(t.owner(), a.id()).await.unwrap();
    assert_eq!(p.status(), AttemptStatus::Approved);
    assert_eq!(
        t.events(&a).await,
        vec![
            (None, "pending".into(), "operator".into()),
            (Some("pending".into()), "approved".into(), "provider".into()),
        ]
    );
    t.pay(t.owner(), &[&p], vec![]).await.unwrap();
}

#[tokio::test]
async fn waiting_backs_off_until_the_terminal_answers_or_the_budget_is_spent() {
    let t = till().await;
    t.provider
        .script([PaymentScenario::Delayed(3), PaymentScenario::Delayed(1000)]);
    let policy = WaitPolicy {
        budget: Duration::from_millis(200),
        first_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(20),
    };
    let a = t.start(600).await;
    let before = t.provider.query_calls();
    let done = t.flow().wait(t.owner(), a.id(), policy).await.unwrap();
    assert_eq!(done.status(), AttemptStatus::Approved);
    assert_eq!(t.provider.query_calls() - before, 3);

    // A payment that stays pending is returned pending when the budget runs out; the
    // operator can cancel it.
    let manager = t.manager();
    let reversed = t.flow().reverse(&manager, a.id(), "test").await.unwrap();
    assert_eq!(reversed.status(), AttemptStatus::Reversed);
    let b = t.start(600).await;
    let still = t.flow().wait(t.owner(), b.id(), policy).await.unwrap();
    assert_eq!(still.status(), AttemptStatus::Pending);
    let cancelled = t.flow().cancel(t.owner(), b.id()).await.unwrap();
    assert_eq!(cancelled.status(), AttemptStatus::Cancelled);
    assert_eq!(t.events(&b).await[1].1, "cancelled");
    t.pay(t.owner(), &[], vec![cash(600)]).await.unwrap();
}

// ---------- Reverse ----------

#[tokio::test]
async fn reversing_an_unused_approval_is_audited() {
    let t = till().await;
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::Approved);
    // An approval nobody used blocks the sale: the card would be charged twice.
    let e = t.pay(t.owner(), &[], vec![cash(600)]).await.unwrap_err();
    assert!(
        matches!(e, AppError::Write(WriteError::PaymentUnresolved)),
        "{e:?}"
    );

    let cashier = t.cashier();
    let e = t
        .flow()
        .reverse(&cashier, a.id(), "customer paid cash")
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ApprovalRequired);
    let manager = t.manager();
    let mgr_id = manager.actor.as_ref().unwrap().user_id;
    let approved = t.cashier_approved(Permission::CartVoid, a.id(), mgr_id);
    let r = t
        .flow()
        .reverse(&approved, a.id(), "customer paid cash")
        .await
        .unwrap();
    assert_eq!(r.status(), AttemptStatus::Reversed);
    assert_eq!(t.provider.net_charged(a.id()), Money::ZERO);
    let last = t.audits(&a).await.pop().unwrap();
    assert_eq!(
        (&last["from"], &last["to"], &last["reason"]),
        (
            &serde_json::json!("approved"),
            &serde_json::json!("reversed"),
            &serde_json::json!("customer paid cash")
        )
    );
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM audit_events WHERE entity_id = '{}' AND approver_user_id = '{mgr_id}'
               AND outbox_event_id IS NOT NULL",
            a.id()
        ))
        .await,
        1
    );
    assert_eq!(
        t.events(&a).await[2],
        (Some("approved".into()), "reversed".into(), "provider".into())
    );
    // Reversed: final, and the cart is free.
    assert!(t.flow().reverse(&manager, a.id(), "again").await.is_err());
    t.pay(t.owner(), &[], vec![cash(600)]).await.unwrap();
    common::contract::check_outbox(&t.shop.store).await;
}

#[tokio::test]
async fn a_provider_reversal_is_picked_up_by_the_next_query() {
    let t = till().await;
    t.provider.script([PaymentScenario::Reverse]);
    let a = t.start(600).await;
    assert_eq!(a.status(), AttemptStatus::Approved);
    let r = t.flow().poll(t.owner(), a.id()).await.unwrap();
    assert_eq!(r.status(), AttemptStatus::Reversed);
    assert_eq!(t.audits(&a).await.last().unwrap()["to"], "reversed");
    // It can no longer pay for the sale.
    assert!(t.pay(t.owner(), &[&a], vec![]).await.is_err());
    t.pay(t.owner(), &[], vec![cash(600)]).await.unwrap();
}

// ---------- idempotency and crash recovery ----------

/// Passes writes through to SQLite, except the `n`-th (1-based), which "crashes".
struct CrashOnApply {
    inner: SqliteStore,
    n: usize,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LocalStore for CrashOnApply {
    async fn apply(&self, set: WriteSet) -> Result<AppliedWriteSet, WriteError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.n {
            return Err(WriteError::Io("the till lost power".into()));
        }
        self.inner.apply(set).await
    }
}

#[tokio::test]
async fn a_crash_between_initiate_and_record_leaves_a_pending_attempt_that_a_query_reconciles() {
    let t = till().await;
    let (cart, _) = t.cart().await;
    // Write 1 stores the pending attempt; the terminal approves; write 2 (recording the
    // approval) never happens.
    let crashing = t.flow_on(
        Arc::new(CrashOnApply {
            inner: t.shop.store.clone(),
            n: 2,
            calls: AtomicUsize::new(0),
        }),
        Duration::from_secs(5),
    );
    assert!(
        crashing
            .start(t.owner(), cart, Money::from_minor(600))
            .await
            .is_err()
    );
    let stored: String = t
        .text("SELECT attempt_id FROM payment_attempts WHERE status = 'pending'")
        .await;
    assert_eq!(t.provider.charges().len(), 1, "the card was charged");
    let id: pos_domain::PaymentAttemptId = stored.parse().unwrap();
    assert!(t.provider.knows(id));

    // After the restart: the open attempt is found, and a query reconciles it.
    let open = t
        .flow()
        .payments
        .open_payment_attempts(t.owner().register.register_id)
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id(), id);
    let a = t.flow().poll(t.owner(), id).await.unwrap();
    assert_eq!(a.status(), AttemptStatus::Approved);

    // Initiating the same attempt again (a retry) never charges twice.
    let again = t
        .provider
        .initiate(&PaymentRequest {
            attempt_id: id,
            amount: Money::from_minor(600),
            currency: CurrencyCode::new("GBP").unwrap(),
            kind: PaymentKind::Sale,
            reference: cart.to_string(),
        })
        .await
        .unwrap();
    assert!(matches!(again, pos_device_ports::PaymentStatus::Approved { .. }));
    assert_eq!(t.provider.charges().len(), 1);
    assert_eq!(t.provider.net_charged(id), Money::from_minor(600));

    t.pay(t.owner(), &[&a], vec![]).await.unwrap();
}

// ---------- refusals ----------

#[tokio::test]
async fn an_attempt_pays_only_its_own_cart_once_for_its_amount() {
    let t = till().await;
    let a = t.start(600).await;

    // The operator's amount must match the approval's.
    let (cart_id, revision) = t.cart().await;
    let e = t
        .flow()
        .complete_sale(
            t.owner(),
            &t.checkout(t.shop.store.clone()),
            CompleteSale {
                cart_id,
                revision,
                tenders: vec![],
            },
            &[ProviderTender {
                attempt_id: a.id(),
                amount: Money::from_minor(500),
            }],
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ValidationFailed);
    // Listed twice.
    let e = t.pay(t.owner(), &[&a, &a], vec![]).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::ValidationFailed);
    // More than is due: no change from a card.
    let manager = t.manager();
    t.flow().reverse(&manager, a.id(), "too much").await.unwrap();
    let big = t.start(700).await;
    let e = t.pay(t.owner(), &[&big], vec![]).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::TenderOverpayNonCash);
    t.flow().reverse(&manager, big.id(), "too much").await.unwrap();

    let a = t.start(600).await;
    let done = t.pay(t.owner(), &[&a], vec![]).await.unwrap();
    assert_eq!(t.count("SELECT count(*) FROM sales").await, 1);

    // Already used: the stale copy in memory is refused by the checks in the transaction,
    // and the next cart cannot use it either.
    t.carts
        .scan(
            t.owner(),
            "TEA",
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Manual,
        )
        .await
        .unwrap();
    let (cart_id, revision) = t.cart().await;
    assert_ne!(cart_id, done.sale.cart_id);
    let e = t
        .checkout(t.shop.store.clone())
        .complete_sale_with_attempts(
            t.owner(),
            CompleteSale {
                cart_id,
                revision,
                tenders: vec![],
            },
            vec![a.clone()],
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ValidationFailed, "another cart");
    let stale = PaymentAttempt::restore(pos_domain::payment::StoredAttempt {
        id: a.id(),
        cart_id,
        kind: a.kind(),
        provider: a.provider().into(),
        amount: Money::from_minor(250),
        currency: a.currency(),
        status: a.status(),
        approval: a.approval().cloned(),
        status_reason: None,
        tender_id: None,
        created_by: a.created_by(),
        created_at: a.created_at(),
        updated_at: a.updated_at(),
    })
    .unwrap();
    let e = t
        .checkout(t.shop.store.clone())
        .complete_sale_with_attempts(
            t.owner(),
            CompleteSale {
                cart_id,
                revision,
                tenders: vec![],
            },
            vec![stale],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, AppError::Write(WriteError::PaymentAttemptUnusable)),
        "{e:?}"
    );
    assert_eq!(t.count("SELECT count(*) FROM sales").await, 1);
}

#[tokio::test]
async fn card_payments_follow_the_external_tender_rules() {
    let t = till().await;
    let (cart, _) = t.cart().await;
    // A cashier may take card payments by default...
    let cashier = t.cashier();
    let a = t
        .flow()
        .start(&cashier, cart, Money::from_minor(600))
        .await
        .unwrap();
    t.flow().reverse(&t.manager(), a.id(), "test").await.unwrap();
    // ...unless the shop keeps external payments to managers.
    let settings = SettingsService {
        store: Arc::new(t.shop.store.clone()),
        settings: Arc::new(t.shop.store.clone()),
    };
    settings
        .update(
            t.owner(),
            "payments.cashier_external_tender",
            serde_json::json!(false),
            1,
        )
        .await
        .unwrap();
    let e = t
        .flow()
        .start(&cashier, cart, Money::from_minor(600))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::PermissionDenied);
    let a = t
        .flow()
        .start(&t.manager(), cart, Money::from_minor(600))
        .await
        .unwrap();
    let e = t.pay(&cashier, &[&a], vec![]).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::PermissionDenied);
    // Not for a cart that is not the open one, nor for nothing.
    let other = pos_domain::CartId::generate(&UuidV7Generator);
    assert_eq!(
        code(
            &t.flow()
                .start(t.owner(), other, Money::from_minor(600))
                .await
                .unwrap_err()
        ),
        ErrorCode::CartRevisionMismatch
    );
    assert_eq!(t.provider.charges().len(), 2);
}

// ---------- atomicity ----------

#[tokio::test]
async fn a_failure_at_any_statement_of_a_card_sale_leaves_nothing_behind() {
    let t = till().await;
    let a = t.start(600).await;
    let mut statement = 0;
    loop {
        match t
            .pay_on(
                t.shop.store.clone().with_fault_before_statement(statement),
                t.owner(),
                &[&a],
                vec![],
            )
            .await
        {
            Ok(done) => {
                assert_eq!(done.receipt_number, "R01-000001");
                break;
            }
            Err(AppError::Write(WriteError::FaultInjected { .. })) => {
                assert_eq!(t.count("SELECT count(*) FROM sales").await, 0);
                assert_eq!(t.count("SELECT count(*) FROM tenders").await, 0);
                assert_eq!(
                    t.count("SELECT count(*) FROM payment_attempts WHERE tender_id IS NOT NULL")
                        .await,
                    0,
                    "the attempt is still unused after a fault at {statement}"
                );
                statement += 1;
            }
            Err(e) => panic!("unexpected error at statement {statement}: {e:?}"),
        }
    }
    assert!(statement > 10, "{statement}");
    assert_eq!(
        t.count("SELECT count(*) FROM payment_attempts WHERE tender_id IS NOT NULL")
            .await,
        1
    );
}

#[tokio::test]
async fn payment_writes_are_covered_by_the_outbox_rules() {
    // E17-T02: every syncable write has its event; attempts are classified local-only.
    let t = till_on(common::shop_with(&common::coverage::triggers_sql()).await).await;
    let a = t.start(400).await;
    t.provider.script([PaymentScenario::Timeout]);
    let (cart, _) = t.cart().await;
    let b = t
        .flow()
        .start(t.owner(), cart, Money::from_minor(200))
        .await
        .unwrap();
    t.flow()
        .resolve(
            &t.manager(),
            b.id(),
            Resolution::Approved { reference: None },
            "slip",
        )
        .await
        .unwrap();
    let b = t.flow().poll(t.owner(), b.id()).await.unwrap();
    t.pay(t.owner(), &[&a, &b], vec![]).await.unwrap();
    let touched = common::coverage::check(&t.shop.store).await;
    assert!(touched.contains("tenders"));
}

// ---------- PAY-005 and append-only, in the schema ----------

#[tokio::test]
async fn the_schema_keeps_no_card_number_and_no_rewritten_history() {
    let t = till().await;
    let a = t.start(600).await;
    let w = t.shop.store.db().writer();
    let id = a.id().to_string();
    for pan in ["4111111111111111", "411111******1111", "1111"] {
        let r = sqlx::query("UPDATE payment_attempts SET masked_pan = ? WHERE attempt_id = ?")
            .bind(pan)
            .bind(&id)
            .execute(w)
            .await;
        assert!(r.is_err(), "{pan} must be refused");
    }
    for sql in [
        "UPDATE payment_attempt_events SET to_status = 'declined'",
        "DELETE FROM payment_attempt_events",
        "DELETE FROM payment_attempts",
        "UPDATE payment_attempts SET amount_minor = 1",
    ] {
        assert!(sqlx::query(sql).execute(w).await.is_err(), "{sql}");
    }
    // A final attempt cannot change.
    t.flow().reverse(&t.manager(), a.id(), "test").await.unwrap();
    assert!(
        sqlx::query("UPDATE payment_attempts SET status = 'approved'")
            .execute(w)
            .await
            .is_err()
    );
}

/// A careless connector that puts card numbers everywhere it can.
struct LeakyProvider;

const PAN: &str = "4111111111111111";

#[async_trait::async_trait]
impl PaymentPort for LeakyProvider {
    fn provider(&self) -> &'static str {
        "leaky"
    }
    async fn initiate(
        &self,
        r: &PaymentRequest,
    ) -> Result<pos_device_ports::PaymentStatus, pos_device_ports::DeviceError> {
        Ok(if r.amount.minor() == 600 {
            pos_device_ports::PaymentStatus::Approved {
                provider_ref: "ch_leaky_1".into(),
                auth_code: Some(PAN.into()),
                card_brand: Some("4111 1111 1111 1111".into()),
                masked_pan: Some(PAN.into()),
            }
        } else {
            pos_device_ports::PaymentStatus::Declined {
                reason: format!("card {PAN} declined"),
            }
        })
    }
    async fn query(
        &self,
        _: pos_domain::PaymentAttemptId,
    ) -> Result<pos_device_ports::PaymentStatus, pos_device_ports::DeviceError> {
        Ok(pos_device_ports::PaymentStatus::Approved {
            provider_ref: PAN.into(),
            auth_code: None,
            card_brand: None,
            masked_pan: None,
        })
    }
    async fn cancel(
        &self,
        _: pos_domain::PaymentAttemptId,
    ) -> Result<pos_device_ports::PaymentStatus, pos_device_ports::DeviceError> {
        Ok(pos_device_ports::PaymentStatus::Cancelled)
    }
    async fn reverse(
        &self,
        _: pos_domain::PaymentAttemptId,
    ) -> Result<pos_device_ports::PaymentStatus, pos_device_ports::DeviceError> {
        Ok(pos_device_ports::PaymentStatus::Reversed)
    }
}

#[tokio::test]
async fn a_connector_that_sends_card_numbers_cannot_make_the_till_keep_them() {
    let t = till().await;
    let q = t.shop.store.clone();
    let flow = ProviderPaymentFlow {
        store: Arc::new(q.clone()),
        payments: Arc::new(q.clone()),
        carts: Arc::new(q.clone()),
        setup: Arc::new(q),
        port: Arc::new(LeakyProvider),
        timeout: Duration::from_secs(5),
    };
    let (cart, _) = t.cart().await;
    let declined = flow.start(t.owner(), cart, Money::from_minor(100)).await.unwrap();
    assert_eq!(declined.status(), AttemptStatus::Declined);
    let a = flow.start(t.owner(), cart, Money::from_minor(600)).await.unwrap();
    assert_eq!(a.status(), AttemptStatus::Approved);
    let Some(Approval::Provider(card)) = a.approval() else {
        panic!("{:?}", a.approval())
    };
    assert_eq!(
        (
            card.auth_code(),
            card.card_brand(),
            card.masked_pan().unwrap().as_str()
        ),
        (None, None, "****1111")
    );
    // A later approval whose reference is a card number is not taken: the attempt stays
    // as it was (that answer is "unknown", which tells nothing about an approval).
    let polled = flow.poll(t.owner(), a.id()).await.unwrap();
    assert_eq!(polled, a);
    t.pay(t.owner(), &[&a], vec![]).await.unwrap();

    let mut written = String::new();
    for sql in [
        "SELECT coalesce(group_concat(coalesce(provider_ref,'') || '|' || coalesce(auth_code,'') || '|' ||
           coalesce(card_brand,'') || '|' || coalesce(masked_pan,'') || '|' || coalesce(status_reason,''), ';'), '')
           FROM payment_attempts",
        "SELECT coalesce(group_concat(detail_json, ';'), '') FROM payment_attempt_events",
        "SELECT coalesce(group_concat(metadata_json, ';'), '') FROM audit_events",
        "SELECT coalesce(group_concat(coalesce(external_reference, ''), ';'), '') FROM tenders",
        "SELECT coalesce(group_concat(payload_json, ';'), '') FROM outbox_events",
    ] {
        written.push_str(&t.text(sql).await);
    }
    assert!(
        written.contains("ch_leaky_1") && written.contains("****1111"),
        "{written}"
    );
    assert!(!written.contains(PAN), "{written}");
    assert!(!written.contains("4111 1111"), "{written}");
}
