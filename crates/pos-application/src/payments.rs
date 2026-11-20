//! Card payments taken on a payment terminal (E10-T02, PAY-004..006, PRD §6.5, §12.5).
//!
//! [`ProviderPaymentFlow`] drives one [`PaymentPort`] through the attempt state machine of
//! `pos_domain::payment`. It is the base every real terminal connector plugs into (PAY-007);
//! in the MVP only the mock provider implements the port, and the flow is not exposed in
//! the production UI.
//!
//! # The money rules
//!
//! 1. **Persist before asking.** An attempt is stored as `pending` *before* the terminal is
//!    asked. A crash between the two leaves a pending attempt, which [`ProviderPaymentFlow::poll`]
//!    reconciles with the provider (the attempt id is the idempotency key, so asking again
//!    never charges twice).
//! 2. **Unknown is never declined.** A timeout (the terminal's or ours), an `Unknown` or
//!    `TimedOut` answer, or a transport error *after the request may have been sent*
//!    (`DeviceError::Timeout`, `DeviceError::Io`) makes the attempt `reconciliation_required`:
//!    the card may have been charged. A later query, or a manager who has looked at the
//!    terminal ([`ProviderPaymentFlow::resolve`]), settles it.
//! 3. **Not sent is safe.** A failure *before anything was sent* (no terminal, connection
//!    refused: `DeviceError::NotConfigured`, `Disconnected`, `Unsupported`) cannot have
//!    charged anything: the attempt is declined, with the reason.
//! 4. **One open outcome per cart.** A second attempt cannot start while one is pending or
//!    awaiting reconciliation (a unique index backs this), and a sale cannot complete while
//!    any attempt of its cart is open or approved but unused (see `checkout`).
//! 5. **A duplicate is a no-op.** Reporting the status an attempt already has writes nothing,
//!    and a concurrent writer that got there first wins (`PaymentAttemptChanged`), so a
//!    duplicate callback makes exactly one transition.
//!
//! Every transition is one `WriteSet` with an audit row (`tender.status_changed`) and an
//! entry in `payment_attempt_events`.
//!
//! # Why attempts are local only (ADR-009)
//!
//! Attempts are working state of a cart, like the cart itself (which never syncs). Their
//! money outcome reaches the cloud through the record that owns it: an attempt that pays for
//! a sale becomes that sale's tender, carried by `sale.completed` with
//! `confirmation = {"provider": {"reference": …}}`. Attempts that end without a sale moved no
//! money (declined, cancelled) or gave it back (reversed). The two manager overrides on
//! money — settling an unknown outcome and reversing an approval — are additionally sent as
//! `audit.recorded`, like other sensitive standalone actions. A settlement-reconciliation
//! feed (PAY-008) can add an attempt event type later without changing this.

use crate::audit::{self, AuditEntry};
use crate::checkout::{CheckoutService, CompleteSale, CompletedSale, authorize_external_tender};
use crate::context::{AppError, CommandContext};
use crate::queries::{CartQueries, QueryError, SetupQueries};
use crate::records::AttemptEventSource;
use crate::{LocalStore, OutboxDraft, WriteError, WriteSet};
use pos_device_ports::{DeviceError, PaymentPort, PaymentRequest, may_have_been_sent};
use pos_domain::payment::{
    AttemptStatus, PaymentAttempt, PaymentError, PaymentKind, Report, Resolution, Step,
};
use pos_domain::{CartId, ErrorCode, Money, PaymentAttemptId, Permission};
use std::sync::Arc;
use std::time::Duration;

/// How long one call to the terminal may take before its outcome counts as unknown. A
/// card payment waits for the customer (card, PIN, bank), so this is generous.
pub const DEFAULT_PAYMENT_TIMEOUT: Duration = Duration::from_secs(90);

/// Reads of stored attempts (implemented by `pos-local-db`).
#[async_trait::async_trait]
pub trait PaymentQueries: Send + Sync {
    async fn payment_attempt(&self, id: PaymentAttemptId) -> Result<Option<PaymentAttempt>, QueryError>;
    /// A cart's attempts, oldest first.
    async fn cart_payment_attempts(&self, cart: CartId) -> Result<Vec<PaymentAttempt>, QueryError>;
    /// This register's attempts whose outcome is open (pending or awaiting reconciliation),
    /// oldest first: what to look at after a restart.
    async fn open_payment_attempts(
        &self,
        register: pos_domain::RegisterId,
    ) -> Result<Vec<PaymentAttempt>, QueryError>;
}

/// How [`ProviderPaymentFlow::wait`] polls: first after `first_delay`, then doubling up to
/// `max_delay`, for at most `budget` in all.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WaitPolicy {
    pub budget: Duration,
    pub first_delay: Duration,
    pub max_delay: Duration,
}

impl Default for WaitPolicy {
    fn default() -> Self {
        Self {
            budget: Duration::from_secs(120),
            first_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(5),
        }
    }
}

impl WaitPolicy {
    /// The delays between polls, in order, until the budget is spent.
    pub fn delays(&self) -> impl Iterator<Item = Duration> + '_ {
        let mut spent = Duration::ZERO;
        let mut next = self.first_delay.max(Duration::from_millis(1));
        std::iter::from_fn(move || {
            if spent >= self.budget {
                return None;
            }
            let d = next.min(self.budget - spent);
            spent += d;
            next = (next * 2).min(self.max_delay.max(Duration::from_millis(1)));
            Some(d)
        })
    }
}

/// An approved attempt the sale uses, with the amount the operator saw for it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProviderTender {
    pub attempt_id: PaymentAttemptId,
    pub amount: Money,
}

pub struct ProviderPaymentFlow {
    pub store: Arc<dyn LocalStore>,
    pub payments: Arc<dyn PaymentQueries>,
    pub carts: Arc<dyn CartQueries>,
    pub setup: Arc<dyn SetupQueries>,
    pub port: Arc<dyn PaymentPort>,
    /// Bound on each call to the terminal ([`DEFAULT_PAYMENT_TIMEOUT`]).
    pub timeout: Duration,
}

fn invalid(msg: impl Into<String>) -> AppError {
    AppError::Validation(msg.into(), ErrorCode::ValidationFailed)
}

/// A terminal call's outcome, as the domain sees it.
enum Answer {
    Report(Report),
    /// The call failed before anything reached the terminal.
    NotSent(String),
}

impl ProviderPaymentFlow {
    /// Starts a card payment of `amount` for the register's open cart `cart_id`: stores the
    /// attempt (pending), then asks the terminal and records its answer (rules 1–3).
    pub async fn start(
        &self,
        ctx: &CommandContext,
        cart_id: CartId,
        amount: Money,
    ) -> Result<PaymentAttempt, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        authorize_external_tender(ctx, self.setup.as_ref()).await?;
        let actor = ctx.actor()?.clone();
        let cart = self.carts.open_cart(ctx.register.register_id).await?;
        if cart
            .as_ref()
            .is_none_or(|c| c.cart_id != cart_id || c.lines.is_empty())
        {
            return Err(AppError::Validation(
                "that cart is not the open cart of this till; reload it".into(),
                ErrorCode::CartRevisionMismatch,
            ));
        }
        if self
            .payments
            .cart_payment_attempts(cart_id)
            .await?
            .iter()
            .any(|a| a.status().is_unresolved())
        {
            return Err(invalid(
                "a card payment for this cart is still in progress; finish or resolve it first",
            ));
        }
        let currency = self.carts.pricing_settings().await?.currency.code;
        let attempt = PaymentAttempt::start(
            PaymentAttemptId::generate(ctx.ids.as_ref()),
            cart_id,
            PaymentKind::Sale,
            self.port.provider(),
            amount,
            currency,
            actor.user_id,
            ctx.now(),
        )?;
        // Rule 1: stored before the terminal hears of it.
        let detail =
            serde_json::json!({ "amountMinor": amount.minor().to_string(), "currency": currency.as_str() });
        let set = WriteSet::builder(ctx.write_context())
            .put_payment_attempt(attempt.clone(), None, AttemptEventSource::Operator, detail)
            .audit(transition_audit(ctx, &attempt, None, None))
            .build()?;
        self.store.apply(set).await?;

        let request = PaymentRequest {
            attempt_id: attempt.id(),
            amount,
            currency,
            kind: PaymentKind::Sale,
            reference: cart_id.to_string(),
        };
        let answer = self.call(self.port.initiate(&request)).await;
        self.record_answer(ctx, attempt, answer).await
    }

    /// Asks the terminal for the attempt's status and records any change. Final and used
    /// attempts are returned as they are, without asking.
    pub async fn poll(&self, ctx: &CommandContext, id: PaymentAttemptId) -> Result<PaymentAttempt, AppError> {
        authorize_external_tender(ctx, self.setup.as_ref()).await?;
        let attempt = self.load(id).await?;
        if attempt.status().is_final() || attempt.tender_id().is_some() {
            return Ok(attempt);
        }
        let pending = attempt.status() == AttemptStatus::Pending;
        match self.call(self.port.query(id)).await {
            // A query that could not be sent says nothing about the payment, which was sent
            // earlier: for a pending attempt the outcome is now unknown (rule 2).
            Answer::NotSent(reason) if pending => {
                self.record_answer(ctx, attempt, Answer::Report(Report::Uncertain { reason }))
                    .await
            }
            Answer::NotSent(_) => Ok(attempt),
            Answer::Report(r) if pending => self.record_answer(ctx, attempt, Answer::Report(r)).await,
            // Approved or awaiting reconciliation: only an answer that legally moves it
            // counts (a reversal; a real outcome for an unknown one). Anything else — still
            // unknown, a stale "pending" — leaves it as it is.
            Answer::Report(r) => match attempt.clone().record(r.clone(), ctx.now()) {
                Ok(_) => self.record_answer(ctx, attempt, Answer::Report(r)).await,
                Err(PaymentError::ConflictingApproval) => Err(PaymentError::ConflictingApproval.into()),
                Err(_) => Ok(attempt),
            },
        }
    }

    /// Polls until the attempt is no longer pending or `policy.budget` is spent, backing off
    /// between polls. Returns the attempt as it then is (possibly still pending: the caller
    /// may keep waiting or cancel).
    pub async fn wait(
        &self,
        ctx: &CommandContext,
        id: PaymentAttemptId,
        policy: WaitPolicy,
    ) -> Result<PaymentAttempt, AppError> {
        let mut attempt = self.poll(ctx, id).await?;
        for delay in policy.delays() {
            if attempt.status() != AttemptStatus::Pending {
                break;
            }
            tokio::time::sleep(delay).await;
            attempt = self.poll(ctx, id).await?;
        }
        Ok(attempt)
    }

    /// Asks the terminal to stop an attempt still in progress. The terminal may answer that
    /// it was approved after all; that is recorded as such.
    pub async fn cancel(
        &self,
        ctx: &CommandContext,
        id: PaymentAttemptId,
    ) -> Result<PaymentAttempt, AppError> {
        authorize_external_tender(ctx, self.setup.as_ref()).await?;
        let attempt = self.load(id).await?;
        if !attempt.status().is_unresolved() || attempt.tender_id().is_some() {
            return Err(invalid("only a card payment still in progress can be cancelled"));
        }
        if !self.port.capabilities().cancel {
            return Err(invalid(
                "this terminal cannot be cancelled from the till; cancel on the terminal, then refresh",
            ));
        }
        match self.call(self.port.cancel(id)).await {
            Answer::NotSent(reason) => Err(invalid(format!(
                "the terminal could not be reached ({reason}); the payment may still be in progress"
            ))),
            answer => self.record_answer(ctx, attempt, answer).await,
        }
    }

    /// A manager settles an attempt whose outcome is unknown, after looking at the terminal
    /// (its screen, slip or back office). Needs `CartVoid` (a cashier needs a manager's
    /// approval for this attempt) and a reason; audited and sent to the cloud audit.
    ///
    /// Settled as approved, the payment is confirmed **by the operator**, not the provider:
    /// the sale's tender says so (PRD §6.5).
    pub async fn resolve(
        &self,
        ctx: &CommandContext,
        id: PaymentAttemptId,
        resolution: Resolution,
        reason: &str,
    ) -> Result<PaymentAttempt, AppError> {
        let approver = ctx.authorize_with_approval(Permission::CartVoid, id, None)?;
        let reason = required_reason(reason)?;
        let mut attempt = self.load(id).await?;
        let from = attempt.status();
        attempt.resolve(resolution, &reason, ctx.now())?;
        self.save_override(
            ctx,
            attempt,
            from,
            AttemptEventSource::Operator,
            &reason,
            approver,
        )
        .await
    }

    /// Gives back an approval no sale used (the customer changed their mind, the cart was
    /// voided). Needs `CartVoid` (a cashier needs a manager's approval for this attempt) and
    /// a reason; audited and sent to the cloud audit. If the terminal does not confirm the
    /// reversal, nothing changes and the error says so: it can be tried again (the attempt
    /// id keeps it idempotent at the provider).
    pub async fn reverse(
        &self,
        ctx: &CommandContext,
        id: PaymentAttemptId,
        reason: &str,
    ) -> Result<PaymentAttempt, AppError> {
        let approver = ctx.authorize_with_approval(Permission::CartVoid, id, None)?;
        let reason = required_reason(reason)?;
        let mut attempt = self.load(id).await?;
        if !attempt.reversible() {
            return Err(invalid(
                "only an approved card payment that no sale used can be reversed",
            ));
        }
        if !self.port.capabilities().reversal {
            return Err(invalid(
                "this terminal cannot reverse from the till; refund the card on the terminal",
            ));
        }
        let report = match self.call(self.port.reverse(id)).await {
            Answer::Report(Report::Reversed) => Report::Reversed,
            Answer::Report(Report::Uncertain { reason }) | Answer::NotSent(reason) => {
                return Err(invalid(format!(
                    "the terminal did not confirm the reversal ({reason}); the payment is still approved, try again"
                )));
            }
            Answer::Report(other) => {
                return Err(invalid(format!(
                    "the terminal answered {} instead of reversing; the payment is still approved",
                    other.status().as_str()
                )));
            }
        };
        let from = attempt.status();
        attempt.record(report, ctx.now())?;
        self.save_override(
            ctx,
            attempt,
            from,
            AttemptEventSource::Provider,
            &reason,
            approver,
        )
        .await
    }

    /// A cart's attempts, oldest first.
    pub async fn attempts(
        &self,
        ctx: &CommandContext,
        cart: CartId,
    ) -> Result<Vec<PaymentAttempt>, AppError> {
        ctx.authorize(Permission::SaleComplete)?;
        Ok(self.payments.cart_payment_attempts(cart).await?)
    }

    /// Completes the sale paid (in part) by these approved attempts: each must be for the
    /// amount the operator saw. See [`CheckoutService::complete_sale_with_attempts`].
    pub async fn complete_sale(
        &self,
        ctx: &CommandContext,
        checkout: &CheckoutService,
        input: CompleteSale,
        provider: &[ProviderTender],
    ) -> Result<CompletedSale, AppError> {
        let mut attempts = Vec::with_capacity(provider.len());
        for p in provider {
            let a = self.load(p.attempt_id).await?;
            if a.amount() != p.amount {
                return Err(invalid(
                    "the card payment's amount differs from the tender; take a new payment",
                ));
            }
            attempts.push(a);
        }
        checkout.complete_sale_with_attempts(ctx, input, attempts).await
    }

    async fn load(&self, id: PaymentAttemptId) -> Result<PaymentAttempt, AppError> {
        self.payments
            .payment_attempt(id)
            .await?
            .ok_or_else(|| invalid("no such card payment"))
    }

    /// Runs one terminal call within `self.timeout` and classifies the outcome (rules 2, 3).
    async fn call(
        &self,
        fut: impl std::future::Future<Output = Result<pos_device_ports::PaymentStatus, DeviceError>>,
    ) -> Answer {
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(Ok(status)) => Answer::Report(status.report()),
            Ok(Err(e)) if !may_have_been_sent(&e) => Answer::NotSent(e.to_string()),
            Ok(Err(e)) => Answer::Report(Report::Uncertain {
                reason: e.to_string(),
            }),
            Err(_) => Answer::Report(Report::Uncertain {
                reason: format!("no answer within {} s", self.timeout.as_secs()),
            }),
        }
    }

    /// Records a terminal answer (or a safe "not sent") on the attempt. A duplicate writes
    /// nothing. An answer that makes no sense for a pending attempt makes it uncertain; for
    /// any other attempt it is refused and nothing changes.
    async fn record_answer(
        &self,
        ctx: &CommandContext,
        mut attempt: PaymentAttempt,
        answer: Answer,
    ) -> Result<PaymentAttempt, AppError> {
        let from = attempt.status();
        let (report, source, detail) = match answer {
            Answer::Report(r) => {
                let detail = serde_json::json!({ "reported": r.status().as_str() });
                (r, AttemptEventSource::Provider, detail)
            }
            // Rule 3: nothing reached the terminal, nothing can be charged.
            Answer::NotSent(reason) => (
                Report::Declined {
                    reason: format!("not sent: {reason}"),
                },
                AttemptEventSource::System,
                serde_json::json!({ "notSent": true }),
            ),
        };
        let step = match attempt.record(report.clone(), ctx.now()) {
            Ok(step) => step,
            Err(PaymentError::IllegalTransition { .. } | PaymentError::ConflictingApproval)
                if from == AttemptStatus::Pending =>
            {
                attempt.record(
                    Report::Uncertain {
                        reason: format!(
                            "unexpected answer from the terminal: {}",
                            report.status().as_str()
                        ),
                    },
                    ctx.now(),
                )?
            }
            Err(e) => return Err(e.into()),
        };
        if step == Step::Unchanged {
            return Ok(attempt);
        }
        let set = WriteSet::builder(ctx.write_context())
            .put_payment_attempt(attempt.clone(), Some(from), source, detail)
            .audit(transition_audit(ctx, &attempt, Some(from), None))
            .build()?;
        self.apply_transition(set, attempt).await
    }

    /// Resolution and reversal: audited with the approver and the reason, and sent to the
    /// cloud as `audit.recorded`.
    async fn save_override(
        &self,
        ctx: &CommandContext,
        attempt: PaymentAttempt,
        from: AttemptStatus,
        source: AttemptEventSource,
        reason: &str,
        approver: Option<pos_domain::UserId>,
    ) -> Result<PaymentAttempt, AppError> {
        let mut entry = transition_audit(ctx, &attempt, Some(from), Some(reason));
        if let Some(a) = approver {
            entry = entry.with_approver(a);
        }
        let event = OutboxDraft::audit_recorded(ctx.ids.as_ref(), &entry)?;
        let entry = entry.linked_to(event.event_id);
        let detail = serde_json::json!({ "reason": reason, "approver": approver });
        let set = WriteSet::builder(ctx.write_context())
            .put_payment_attempt(attempt.clone(), Some(from), source, detail)
            .outbox(event)
            .audit(entry)
            .build()?;
        self.apply_transition(set, attempt).await
    }

    /// Applies a transition. If another writer changed the attempt first (rule 5), the
    /// stored attempt is the answer.
    async fn apply_transition(
        &self,
        set: WriteSet,
        attempt: PaymentAttempt,
    ) -> Result<PaymentAttempt, AppError> {
        match self.store.apply(set).await {
            Ok(_) => Ok(attempt),
            Err(WriteError::PaymentAttemptChanged) => self.load(attempt.id()).await,
            Err(e) => Err(e.into()),
        }
    }
}

/// `tender.status_changed` for an attempt. Never carries card data: the reference only
/// as "has one", no auth code, brand or digits.
fn transition_audit(
    ctx: &CommandContext,
    attempt: &PaymentAttempt,
    from: Option<AttemptStatus>,
    reason: Option<&str>,
) -> AuditEntry {
    AuditEntry::new(
        ctx.ids.as_ref(),
        audit::TENDER_STATUS_CHANGED,
        "payment_attempt",
        attempt.id(),
    )
    .with_metadata(serde_json::json!({
        "cartId": attempt.cart_id(),
        "provider": attempt.provider(),
        "amountMinor": attempt.amount().minor().to_string(),
        "from": from.map(AttemptStatus::as_str),
        "to": attempt.status().as_str(),
        "approvalSource": attempt.approval().map(|a| a.source()),
        "reason": reason,
    }))
}

fn required_reason(reason: &str) -> Result<String, AppError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(invalid("a reason is required"));
    }
    Ok(pos_domain::payment::clean_reason(reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_backs_off_within_its_budget() {
        let p = WaitPolicy {
            budget: Duration::from_millis(1000),
            first_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(300),
        };
        let d: Vec<u128> = p.delays().map(|d| d.as_millis()).collect();
        assert_eq!(d, vec![100, 200, 300, 300, 100]);
        assert_eq!(d.iter().sum::<u128>(), 1000);
        let none = WaitPolicy {
            budget: Duration::ZERO,
            ..p
        };
        assert_eq!(none.delays().count(), 0);
    }
}
