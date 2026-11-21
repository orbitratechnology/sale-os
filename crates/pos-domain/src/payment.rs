//! Payment terminal attempts (E10-T02, PAY-004..006, PRD §6.5 and §12.5).
//!
//! An attempt is one request to a payment terminal or provider for one amount on one cart.
//! Its status follows a small state machine:
//!
//! ```text
//! pending ──► approved ──► reversed
//!    │  ╲
//!    │   ╲──► declined | cancelled
//!    ▼
//! reconciliation_required ──► approved | declined | cancelled
//! ```
//!
//! - `reconciliation_required` means **the card may have been charged and nobody knows**:
//!   a timeout, an `Unknown` answer, or a transport failure after the request was sent. It
//!   is never treated as declined. A later query or the operator (after looking at the
//!   terminal) settles it.
//! - Reporting the status an attempt already has is a no-op ([`Step::Unchanged`]): a
//!   duplicate callback makes one transition, not two.
//! - `declined`, `cancelled` and `reversed` are final.
//!
//! PAY-005: nothing here ever holds a card number. Provider data is checked with the same
//! deny-list as operator references ([`crate::settlement::looks_like_card_number`]), and a
//! masked card number keeps the last four digits only.
//!
//! The type-level guarantee of E10-T01 holds: a tender confirmed by the provider
//! ([`ConfirmationSource::Provider`]) can only be built by [`PaymentAttempt::sale_tender`],
//! from an attempt the provider approved.

use crate::settlement::{
    ConfirmationSource, MAX_REFERENCE_LEN, ProviderApproval, SettledTender, SettlementError, TenderKind,
    TenderStatus, clean_reference, looks_like_card_number,
};
use crate::{
    CartId, CurrencyCode, ErrorCode, HasErrorCode, Money, PaymentAttemptId, TenderId, Timestamp, UserId,
};

/// Longest provider status text kept (decline reasons, why an outcome is unknown).
pub const MAX_REASON_LEN: usize = 200;
/// Longest card brand kept ("VISA", "MASTERCARD"...).
pub const MAX_BRAND_LEN: usize = 32;

/// What the terminal is asked to do.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PaymentKind {
    Sale,
    Refund,
}

impl PaymentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            PaymentKind::Sale => "sale",
            PaymentKind::Refund => "refund",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "sale" => Some(PaymentKind::Sale),
            "refund" => Some(PaymentKind::Refund),
            _ => None,
        }
    }
}

/// Where an attempt stands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AttemptStatus {
    /// Sent (or about to be); the terminal has not answered yet.
    Pending,
    Approved,
    Declined,
    Cancelled,
    /// Approved, then given back (a void or reversal before settlement).
    Reversed,
    /// The card may have been charged; the outcome is not known (PAY-004).
    ReconciliationRequired,
}

impl AttemptStatus {
    pub const ALL: [AttemptStatus; 6] = [
        AttemptStatus::Pending,
        AttemptStatus::Approved,
        AttemptStatus::Declined,
        AttemptStatus::Cancelled,
        AttemptStatus::Reversed,
        AttemptStatus::ReconciliationRequired,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AttemptStatus::Pending => "pending",
            AttemptStatus::Approved => "approved",
            AttemptStatus::Declined => "declined",
            AttemptStatus::Cancelled => "cancelled",
            AttemptStatus::Reversed => "reversed",
            AttemptStatus::ReconciliationRequired => "reconciliation_required",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }

    /// Nothing can change it any more.
    pub const fn is_final(self) -> bool {
        matches!(
            self,
            AttemptStatus::Declined | AttemptStatus::Cancelled | AttemptStatus::Reversed
        )
    }

    /// The outcome is still open: the cart cannot be paid another way meanwhile.
    pub const fn is_unresolved(self) -> bool {
        matches!(
            self,
            AttemptStatus::Pending | AttemptStatus::ReconciliationRequired
        )
    }
}

/// Whether a reported status changed anything.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Changed,
    /// The attempt already had that status (a duplicate callback, a repeated query): no-op.
    Unchanged,
}

/// The legal transitions (see the module documentation).
pub fn check_transition(from: AttemptStatus, to: AttemptStatus) -> Result<Step, PaymentError> {
    use AttemptStatus::*;
    if from == to {
        return Ok(Step::Unchanged);
    }
    match (from, to) {
        (Pending, Approved | Declined | Cancelled | ReconciliationRequired)
        | (ReconciliationRequired, Approved | Declined | Cancelled)
        | (Approved, Reversed) => Ok(Step::Changed),
        _ => Err(PaymentError::IllegalTransition { from, to }),
    }
}

/// A masked card number: the last four digits only (PAY-005), shown as `****1234`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MaskedPan(String);

impl MaskedPan {
    /// Keeps only the last four digits of whatever the provider sent, so even a provider
    /// that sends more (or the whole number) cannot make the till store it. `None` when it
    /// has fewer than four digits.
    pub fn from_provider(text: &str) -> Option<Self> {
        let digits: Vec<char> = text.chars().filter(char::is_ascii_digit).collect();
        if digits.len() < 4 {
            return None;
        }
        let last4: String = digits[digits.len() - 4..].iter().collect();
        Some(Self(format!("****{last4}")))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn last4(&self) -> &str {
        &self.0[4..]
    }
}

/// What a provider says about an approval, checked for PAY-005 and length limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardApproval {
    provider_ref: String,
    auth_code: Option<String>,
    card_brand: Option<String>,
    masked_pan: Option<MaskedPan>,
}

impl CardApproval {
    /// The provider reference is required and must be safe to keep; it is refused (and the
    /// approval with it) if it is empty, too long or looks like a card number. The optional
    /// fields are dropped instead when they are unsafe: they are details, the approval is not.
    pub fn from_provider(
        provider_ref: &str,
        auth_code: Option<&str>,
        card_brand: Option<&str>,
        masked_pan: Option<&str>,
    ) -> Result<Self, PaymentError> {
        let provider_ref = provider_ref.trim();
        if provider_ref.is_empty()
            || provider_ref.chars().count() > MAX_REFERENCE_LEN
            || looks_like_card_number(provider_ref)
        {
            return Err(PaymentError::UnsafeProviderData);
        }
        let keep = |v: Option<&str>, max: usize| {
            v.map(str::trim)
                .filter(|v| !v.is_empty() && v.chars().count() <= max && !looks_like_card_number(v))
                .map(str::to_owned)
        };
        Ok(Self {
            provider_ref: provider_ref.to_owned(),
            auth_code: keep(auth_code, MAX_REFERENCE_LEN),
            card_brand: keep(card_brand, MAX_BRAND_LEN),
            masked_pan: masked_pan.and_then(MaskedPan::from_provider),
        })
    }
    pub fn provider_ref(&self) -> &str {
        &self.provider_ref
    }
    pub fn auth_code(&self) -> Option<&str> {
        self.auth_code.as_deref()
    }
    pub fn card_brand(&self) -> Option<&str> {
        self.card_brand.as_deref()
    }
    pub fn masked_pan(&self) -> Option<&MaskedPan> {
        self.masked_pan.as_ref()
    }
}

/// Who said the payment went through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approval {
    /// The provider's authoritative answer: the only source of a provider confirmation.
    Provider(CardApproval),
    /// The operator settled a `reconciliation_required` attempt after looking at the
    /// terminal. The tender it pays is confirmed **by the operator** (PRD §6.5: never label
    /// a tender processor-approved without the provider's confirmation).
    Operator { reference: Option<String> },
}

impl Approval {
    /// Stored in `payment_attempts.approval_source`.
    pub const fn source(&self) -> &'static str {
        match self {
            Approval::Provider(_) => "provider",
            Approval::Operator { .. } => "operator",
        }
    }
}

/// A provider's answer, already checked (see `pos_device_ports::PaymentStatus::report`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    Pending,
    Approved(CardApproval),
    Declined {
        reason: String,
    },
    Cancelled,
    Reversed,
    /// No authoritative answer: a timeout, `Unknown`, or a failure after the request was
    /// sent. The card may have been charged.
    Uncertain {
        reason: String,
    },
}

impl Report {
    /// The status this report asks for.
    pub fn status(&self) -> AttemptStatus {
        match self {
            Report::Pending => AttemptStatus::Pending,
            Report::Approved(_) => AttemptStatus::Approved,
            Report::Declined { .. } => AttemptStatus::Declined,
            Report::Cancelled => AttemptStatus::Cancelled,
            Report::Reversed => AttemptStatus::Reversed,
            Report::Uncertain { .. } => AttemptStatus::ReconciliationRequired,
        }
    }
}

/// The operator's verdict on a `reconciliation_required` attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The terminal shows the payment went through. `reference` is what its slip shows
    /// (checked like any operator reference: never a card number).
    Approved {
        reference: Option<String>,
    },
    Declined,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaymentError {
    #[error("a payment amount must be greater than zero")]
    NonPositiveAmount,
    #[error("a payment cannot go from {} to {}", from.as_str(), to.as_str())]
    IllegalTransition { from: AttemptStatus, to: AttemptStatus },
    #[error("the provider reported a different approval for the same payment")]
    ConflictingApproval,
    /// PAY-005. The value is deliberately not in the message.
    #[error("the provider's reference is missing, too long or looks like a card number")]
    UnsafeProviderData,
    #[error("only an approved card payment can pay for a sale")]
    NotApproved,
    #[error("this card payment already pays for a sale")]
    AlreadyUsed,
    #[error("only a payment whose outcome is unknown can be settled by the operator")]
    NotAwaitingReconciliation,
    #[error("only a sale payment can pay for a sale")]
    WrongKind,
    #[error(transparent)]
    Reference(#[from] SettlementError),
    #[error("stored payment attempt is inconsistent: {0}")]
    Corrupt(&'static str),
}

impl HasErrorCode for PaymentError {
    fn error_code(&self) -> ErrorCode {
        match self {
            PaymentError::Reference(e) => e.error_code(),
            PaymentError::Corrupt(_) => ErrorCode::StoreCorrupt,
            _ => ErrorCode::ValidationFailed,
        }
    }
}

/// One request to a payment terminal (see the module documentation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentAttempt {
    id: PaymentAttemptId,
    cart_id: CartId,
    kind: PaymentKind,
    provider: String,
    amount: Money,
    currency: CurrencyCode,
    status: AttemptStatus,
    approval: Option<Approval>,
    status_reason: Option<String>,
    tender_id: Option<TenderId>,
    created_by: UserId,
    created_at: Timestamp,
    updated_at: Timestamp,
}

/// An attempt as stored, for [`PaymentAttempt::restore`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredAttempt {
    pub id: PaymentAttemptId,
    pub cart_id: CartId,
    pub kind: PaymentKind,
    pub provider: String,
    pub amount: Money,
    pub currency: CurrencyCode,
    pub status: AttemptStatus,
    pub approval: Option<Approval>,
    pub status_reason: Option<String>,
    pub tender_id: Option<TenderId>,
    pub created_by: UserId,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl PaymentAttempt {
    /// A new attempt, `pending`. It is stored before the terminal is asked (so a crash in
    /// between leaves a pending attempt that a query can reconcile).
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        id: PaymentAttemptId,
        cart_id: CartId,
        kind: PaymentKind,
        provider: &str,
        amount: Money,
        currency: CurrencyCode,
        created_by: UserId,
        at: Timestamp,
    ) -> Result<Self, PaymentError> {
        if amount.minor() <= 0 {
            return Err(PaymentError::NonPositiveAmount);
        }
        Ok(Self {
            id,
            cart_id,
            kind,
            provider: provider.to_owned(),
            amount,
            currency,
            status: AttemptStatus::Pending,
            approval: None,
            status_reason: None,
            tender_id: None,
            created_by,
            created_at: at,
            updated_at: at,
        })
    }

    /// Rebuilds an attempt from the store's row. Only the store calls this, with rows it
    /// wrote from attempts; the consistency rules are checked again.
    pub fn restore(s: StoredAttempt) -> Result<Self, PaymentError> {
        if s.amount.minor() <= 0 {
            return Err(PaymentError::Corrupt("amount"));
        }
        let approved = matches!(s.status, AttemptStatus::Approved | AttemptStatus::Reversed);
        if approved != s.approval.is_some() {
            return Err(PaymentError::Corrupt("approval does not match the status"));
        }
        if s.tender_id.is_some() && s.status != AttemptStatus::Approved {
            return Err(PaymentError::Corrupt("a used attempt must be approved"));
        }
        Ok(Self {
            id: s.id,
            cart_id: s.cart_id,
            kind: s.kind,
            provider: s.provider,
            amount: s.amount,
            currency: s.currency,
            status: s.status,
            approval: s.approval,
            status_reason: s.status_reason,
            tender_id: s.tender_id,
            created_by: s.created_by,
            created_at: s.created_at,
            updated_at: s.updated_at,
        })
    }

    pub fn id(&self) -> PaymentAttemptId {
        self.id
    }
    pub fn cart_id(&self) -> CartId {
        self.cart_id
    }
    pub fn kind(&self) -> PaymentKind {
        self.kind
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn amount(&self) -> Money {
        self.amount
    }
    pub fn currency(&self) -> CurrencyCode {
        self.currency
    }
    pub fn status(&self) -> AttemptStatus {
        self.status
    }
    pub fn approval(&self) -> Option<&Approval> {
        self.approval.as_ref()
    }
    /// Why it was declined, why its outcome is unknown, or the operator's resolution note.
    pub fn status_reason(&self) -> Option<&str> {
        self.status_reason.as_deref()
    }
    /// The tender of the completed sale this approval paid for, if any.
    pub fn tender_id(&self) -> Option<TenderId> {
        self.tender_id
    }
    pub fn created_by(&self) -> UserId {
        self.created_by
    }
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }
    pub fn updated_at(&self) -> Timestamp {
        self.updated_at
    }

    /// Applies what the provider reported. Changes nothing on error.
    ///
    /// - `Pending` or `Uncertain` about an attempt already awaiting reconciliation tells
    ///   nothing new: unchanged.
    /// - The same approval again (a duplicate callback) is unchanged; a *different*
    ///   approval for the same attempt is refused.
    pub fn record(&mut self, report: Report, at: Timestamp) -> Result<Step, PaymentError> {
        if self.tender_id.is_some() {
            return Err(PaymentError::AlreadyUsed);
        }
        let to = report.status();
        if self.status == AttemptStatus::ReconciliationRequired
            && matches!(report, Report::Pending | Report::Uncertain { .. })
        {
            return Ok(Step::Unchanged);
        }
        if let (AttemptStatus::Approved, Report::Approved(again)) = (self.status, &report) {
            return match &self.approval {
                Some(Approval::Provider(a)) if a.provider_ref == again.provider_ref => Ok(Step::Unchanged),
                _ => Err(PaymentError::ConflictingApproval),
            };
        }
        if check_transition(self.status, to)? == Step::Unchanged {
            return Ok(Step::Unchanged);
        }
        match report {
            Report::Approved(a) => {
                self.approval = Some(Approval::Provider(a));
                self.status_reason = None;
            }
            Report::Declined { reason } | Report::Uncertain { reason } => {
                self.status_reason = Some(clean_reason(&reason));
            }
            Report::Pending | Report::Cancelled | Report::Reversed => {}
        }
        self.status = to;
        self.updated_at = at;
        Ok(Step::Changed)
    }

    /// The operator settles an attempt whose outcome is unknown, with a reason (audited by
    /// the caller). Changes nothing on error.
    pub fn resolve(
        &mut self,
        resolution: Resolution,
        reason: &str,
        at: Timestamp,
    ) -> Result<Step, PaymentError> {
        if self.tender_id.is_some() {
            return Err(PaymentError::AlreadyUsed);
        }
        if self.status != AttemptStatus::ReconciliationRequired {
            return Err(PaymentError::NotAwaitingReconciliation);
        }
        let (to, approval) = match resolution {
            Resolution::Approved { reference } => (
                AttemptStatus::Approved,
                Some(Approval::Operator {
                    reference: clean_reference(reference.as_deref())?,
                }),
            ),
            Resolution::Declined => (AttemptStatus::Declined, None),
            Resolution::Cancelled => (AttemptStatus::Cancelled, None),
        };
        check_transition(self.status, to)?;
        self.status = to;
        self.approval = approval;
        self.status_reason = Some(clean_reason(reason));
        self.updated_at = at;
        Ok(Step::Changed)
    }

    /// Whether the approval can still be given back: approved and not part of a sale.
    pub fn reversible(&self) -> bool {
        self.status == AttemptStatus::Approved && self.tender_id.is_none()
    }

    /// The tender this approval pays in a sale: a card payment, confirmed. Confirmed **by
    /// the provider** only when the provider approved it; by the operator when the operator
    /// settled an unknown outcome. The reference is the provider's transaction reference
    /// (or the one the operator read off the terminal).
    pub fn sale_tender(&self) -> Result<SettledTender, PaymentError> {
        if self.kind != PaymentKind::Sale {
            return Err(PaymentError::WrongKind);
        }
        if self.tender_id.is_some() {
            return Err(PaymentError::AlreadyUsed);
        }
        let (confirmation, reference) = match (&self.status, &self.approval) {
            (AttemptStatus::Approved, Some(Approval::Provider(a))) => (
                ConfirmationSource::Provider(ProviderApproval::from_approved_attempt(a.provider_ref.clone())),
                Some(a.provider_ref.clone()),
            ),
            (AttemptStatus::Approved, Some(Approval::Operator { reference })) => {
                (ConfirmationSource::Operator, reference.clone())
            }
            _ => return Err(PaymentError::NotApproved),
        };
        Ok(SettledTender {
            kind: TenderKind::CardExternal,
            amount: self.amount,
            received: None,
            change: Money::ZERO,
            status: TenderStatus::Confirmed,
            confirmation,
            external_reference: reference,
        })
    }
}

/// Provider text kept on an attempt: trimmed, at most [`MAX_REASON_LEN`] characters, and
/// never a card number (PAY-005).
pub fn clean_reason(text: &str) -> String {
    let text = text.trim();
    if looks_like_card_number(text) {
        return "(redacted: looked like a card number)".to_owned();
    }
    text.chars().take(MAX_REASON_LEN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{PricingPolicy, TaxMode};
    use crate::settlement::{TenderInput, settle_with_confirmed};
    use crate::time::UuidV7Generator;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    fn attempt(amount: i64) -> PaymentAttempt {
        let ids = UuidV7Generator;
        PaymentAttempt::start(
            PaymentAttemptId::generate(&ids),
            CartId::generate(&ids),
            PaymentKind::Sale,
            "mock",
            Money::from_minor(amount),
            CurrencyCode::new("GBP").unwrap(),
            UserId::generate(&ids),
            at(1),
        )
        .unwrap()
    }

    fn approval(r: &str) -> CardApproval {
        CardApproval::from_provider(r, Some("A1B2C3"), Some("VISA"), Some("****4242")).unwrap()
    }

    #[test]
    fn the_transition_table() {
        use AttemptStatus::*;
        let legal = [
            (Pending, Approved),
            (Pending, Declined),
            (Pending, Cancelled),
            (Pending, ReconciliationRequired),
            (ReconciliationRequired, Approved),
            (ReconciliationRequired, Declined),
            (ReconciliationRequired, Cancelled),
            (Approved, Reversed),
        ];
        for from in AttemptStatus::ALL {
            for to in AttemptStatus::ALL {
                let got = check_transition(from, to);
                if from == to {
                    assert_eq!(got, Ok(Step::Unchanged), "{from:?} -> {to:?}");
                } else if legal.contains(&(from, to)) {
                    assert_eq!(got, Ok(Step::Changed), "{from:?} -> {to:?}");
                } else {
                    assert_eq!(
                        got,
                        Err(PaymentError::IllegalTransition { from, to }),
                        "{from:?} -> {to:?}"
                    );
                }
            }
            assert_eq!(AttemptStatus::parse(from.as_str()), Some(from));
        }
        for s in [Declined, Cancelled, Reversed] {
            assert!(s.is_final());
            for to in AttemptStatus::ALL.into_iter().filter(|t| *t != s) {
                assert!(check_transition(s, to).is_err(), "{s:?} is final");
            }
        }
    }

    #[test]
    fn approve_then_a_duplicate_callback_is_one_transition() {
        let mut a = attempt(1000);
        assert_eq!(
            a.record(Report::Approved(approval("ch_1")), at(2)),
            Ok(Step::Changed)
        );
        assert_eq!(a.status(), AttemptStatus::Approved);
        assert_eq!(a.updated_at(), at(2));
        assert_eq!(
            a.record(Report::Approved(approval("ch_1")), at(3)),
            Ok(Step::Unchanged)
        );
        assert_eq!(a.updated_at(), at(2), "nothing changed");
        let before = a.clone();
        assert_eq!(
            a.record(Report::Approved(approval("ch_2")), at(4)),
            Err(PaymentError::ConflictingApproval)
        );
        assert_eq!(a, before, "an error changes nothing");
    }

    #[test]
    fn uncertain_outcomes_need_reconciliation_and_never_decline() {
        let mut a = attempt(1000);
        let r = Report::Uncertain {
            reason: "no answer".into(),
        };
        assert_eq!(a.record(r.clone(), at(2)), Ok(Step::Changed));
        assert_eq!(a.status(), AttemptStatus::ReconciliationRequired);
        assert_eq!(a.status_reason(), Some("no answer"));
        // Further "don't know"s change nothing; a real answer settles it.
        assert_eq!(a.record(r, at(3)), Ok(Step::Unchanged));
        assert_eq!(a.record(Report::Pending, at(3)), Ok(Step::Unchanged));
        assert_eq!(
            a.record(Report::Approved(approval("ch_9")), at(4)),
            Ok(Step::Changed)
        );
        assert_eq!(a.status(), AttemptStatus::Approved);
        assert!(matches!(
            a.sale_tender().unwrap().confirmation,
            ConfirmationSource::Provider(_)
        ));
    }

    #[test]
    fn the_operator_resolves_only_unknown_outcomes() {
        let mut a = attempt(1000);
        assert_eq!(
            a.resolve(Resolution::Declined, "looked", at(2)),
            Err(PaymentError::NotAwaitingReconciliation)
        );
        a.record(
            Report::Uncertain {
                reason: "timeout".into(),
            },
            at(2),
        )
        .unwrap();
        assert_eq!(
            a.resolve(
                Resolution::Approved {
                    reference: Some("4111111111111111".into())
                },
                "slip",
                at(3)
            ),
            Err(PaymentError::Reference(SettlementError::ReferenceLooksLikeCard))
        );
        assert_eq!(a.status(), AttemptStatus::ReconciliationRequired);
        a.resolve(
            Resolution::Approved {
                reference: Some("AUTH 004211".into()),
            },
            "terminal slip shows approved",
            at(3),
        )
        .unwrap();
        assert_eq!(a.status(), AttemptStatus::Approved);
        assert_eq!(a.status_reason(), Some("terminal slip shows approved"));
        // Settled by the operator: the tender is confirmed by the operator, not the provider.
        let t = a.sale_tender().unwrap();
        assert_eq!(t.confirmation, ConfirmationSource::Operator);
        assert_eq!(t.external_reference.as_deref(), Some("AUTH 004211"));
        assert_eq!(
            (t.kind, t.status),
            (TenderKind::CardExternal, TenderStatus::Confirmed)
        );
    }

    #[test]
    fn only_an_approved_attempt_makes_a_provider_confirmed_tender() {
        let mut a = attempt(500);
        assert_eq!(a.sale_tender(), Err(PaymentError::NotApproved));
        a.record(
            Report::Declined {
                reason: "insufficient funds".into(),
            },
            at(2),
        )
        .unwrap();
        assert_eq!(a.sale_tender(), Err(PaymentError::NotApproved));
        assert!(a.record(Report::Approved(approval("x")), at(3)).is_err(), "final");

        let mut a = attempt(500);
        a.record(Report::Approved(approval("ch_7")), at(2)).unwrap();
        let t = a.sale_tender().unwrap();
        match &t.confirmation {
            ConfirmationSource::Provider(p) => assert_eq!(p.reference(), "ch_7"),
            other => panic!("{other:?}"),
        }
        assert_eq!(t.external_reference.as_deref(), Some("ch_7"));
        // It settles with cash for the rest; change comes from the cash only.
        let s = settle_with_confirmed(
            Money::from_minor(1345),
            vec![t],
            &[TenderInput {
                kind: TenderKind::Cash,
                amount: Money::from_minor(1000),
                external_reference: None,
            }],
            &PricingPolicy::new(TaxMode::Inclusive),
        )
        .unwrap();
        assert_eq!(s.change, Money::from_minor(155));
        assert_eq!(s.tenders.len(), 2);

        assert!(a.reversible());
        a.record(Report::Reversed, at(3)).unwrap();
        assert_eq!(a.status(), AttemptStatus::Reversed);
        assert_eq!(a.sale_tender(), Err(PaymentError::NotApproved));
    }

    #[test]
    fn a_confirmed_tender_must_fit_what_is_due() {
        let mut a = attempt(2000);
        a.record(Report::Approved(approval("ch_1")), at(2)).unwrap();
        let policy = PricingPolicy::new(TaxMode::Inclusive);
        assert_eq!(
            settle_with_confirmed(
                Money::from_minor(1345),
                vec![a.sale_tender().unwrap()],
                &[],
                &policy
            ),
            Err(SettlementError::OverTenderNonCash)
        );
        assert_eq!(
            settle_with_confirmed(Money::from_minor(1345), vec![], &[], &policy),
            Err(SettlementError::NoTenders)
        );
    }

    #[test]
    fn provider_data_never_keeps_a_card_number() {
        assert_eq!(
            CardApproval::from_provider("4111111111111111", None, None, None),
            Err(PaymentError::UnsafeProviderData)
        );
        assert_eq!(
            CardApproval::from_provider("  ", None, None, None),
            Err(PaymentError::UnsafeProviderData)
        );
        let a = CardApproval::from_provider(
            "ch_1",
            Some("4111 1111 1111 1111"),
            Some("5500005555555559"),
            Some("4111111111111111"),
        )
        .unwrap();
        assert_eq!(a.auth_code(), None);
        assert_eq!(a.card_brand(), None);
        assert_eq!(a.masked_pan().unwrap().as_str(), "****1111", "last four only");
        assert_eq!(a.masked_pan().unwrap().last4(), "1111");
        assert_eq!(MaskedPan::from_provider("**12"), None);
        assert_eq!(
            MaskedPan::from_provider("**** **** **** 4242").unwrap().as_str(),
            "****4242"
        );
        assert_eq!(
            clean_reason("card 4111-1111-1111-1111 declined"),
            "(redacted: looked like a card number)"
        );
        assert_eq!(clean_reason(&"x".repeat(500)).len(), MAX_REASON_LEN);
    }

    #[test]
    fn a_used_attempt_is_final_and_restore_checks_consistency() {
        let mut a = attempt(500);
        a.record(Report::Approved(approval("ch_1")), at(2)).unwrap();
        let stored = StoredAttempt {
            id: a.id(),
            cart_id: a.cart_id(),
            kind: a.kind(),
            provider: a.provider().into(),
            amount: a.amount(),
            currency: a.currency(),
            status: a.status(),
            approval: a.approval().cloned(),
            status_reason: None,
            tender_id: Some(TenderId::generate(&UuidV7Generator)),
            created_by: a.created_by(),
            created_at: a.created_at(),
            updated_at: a.updated_at(),
        };
        let mut used = PaymentAttempt::restore(stored.clone()).unwrap();
        assert_eq!(used.sale_tender(), Err(PaymentError::AlreadyUsed));
        assert_eq!(
            used.record(Report::Reversed, at(3)),
            Err(PaymentError::AlreadyUsed)
        );
        assert!(!used.reversible());
        assert_eq!(
            PaymentAttempt::restore(StoredAttempt {
                approval: None,
                ..stored.clone()
            }),
            Err(PaymentError::Corrupt("approval does not match the status"))
        );
        assert_eq!(
            PaymentAttempt::restore(StoredAttempt {
                status: AttemptStatus::Declined,
                approval: None,
                ..stored
            }),
            Err(PaymentError::Corrupt("a used attempt must be approved"))
        );
        let refund = PaymentAttempt::start(
            PaymentAttemptId::generate(&UuidV7Generator),
            a.cart_id(),
            PaymentKind::Refund,
            "mock",
            Money::from_minor(1),
            a.currency(),
            a.created_by(),
            at(1),
        );
        assert_eq!(refund.unwrap().sale_tender(), Err(PaymentError::WrongKind));
        assert!(
            PaymentAttempt::start(
                PaymentAttemptId::generate(&UuidV7Generator),
                a.cart_id(),
                PaymentKind::Sale,
                "mock",
                Money::ZERO,
                a.currency(),
                a.created_by(),
                at(1),
            )
            .is_err()
        );
    }
}
