//! Payment terminals and providers (E10-T02, PAY-006/007, PRD §12.5, 06 §10).
//!
//! The port every terminal connector implements (the mock in `pos-device-virtual` first,
//! a real acquirer with PAY-007). It is pull-based: [`PaymentPort::initiate`] sends the
//! request and returns what the terminal answered so far; [`PaymentPort::query`] asks
//! again. A connector that receives callbacks keeps the latest status and answers `query`
//! from it, so a duplicated callback is just the same status twice.
//!
//! ## Idempotency
//!
//! `PaymentRequest::attempt_id` is the idempotency key. Initiating the same attempt twice
//! must never charge twice: the connector passes the key to the provider (or remembers it)
//! and answers the second call with the first call's transaction.
//!
//! ## Errors and the "was it sent?" rule
//!
//! The application decides what an error means for the money (see
//! `pos_application::payments`), from this rule, which connectors must follow:
//!
//! - [`DeviceError::NotConfigured`], [`DeviceError::Disconnected`] and
//!   [`DeviceError::Unsupported`] mean **the request did not leave the till** (no terminal
//!   set up, connection refused, port not open, an operation it cannot do). Nothing can have
//!   been charged.
//! - [`DeviceError::Timeout`] and [`DeviceError::Io`] mean **it may have been sent**: the
//!   terminal may have charged the card. The attempt becomes `reconciliation_required`,
//!   never declined.
//!
//! A connector that cannot tell must return `Io`, the safe side.
//!
//! ## PAY-005
//!
//! Nothing crossing this port may carry a card number, CVV, PIN or track data. An approval
//! carries the provider's reference, an authorisation code, the brand and the last four
//! digits at most; [`PaymentStatus::report`] re-checks all of it before the till keeps it.

use crate::DeviceError;
use pos_domain::payment::{CardApproval, Report};
use pos_domain::{CurrencyCode, Money, PaymentAttemptId};

pub use pos_domain::payment::PaymentKind;

/// One request to the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentRequest {
    /// The idempotency key: the same attempt is never charged twice.
    pub attempt_id: PaymentAttemptId,
    pub amount: Money,
    pub currency: CurrencyCode,
    pub kind: PaymentKind,
    /// Shown on the terminal and its slip (the cart or receipt reference). Never card data.
    pub reference: String,
}

/// What the terminal says about an attempt.
#[derive(Clone, PartialEq, Eq)]
pub enum PaymentStatus {
    /// Still in progress (waiting for the card, the PIN, the bank).
    Pending,
    Approved {
        /// The provider's transaction reference: required.
        provider_ref: String,
        auth_code: Option<String>,
        card_brand: Option<String>,
        /// Last four digits only (e.g. `****4242`); anything more is cut to four.
        masked_pan: Option<String>,
    },
    Declined {
        reason: String,
    },
    Cancelled,
    /// Approved, then voided or reversed.
    Reversed,
    /// The terminal itself gave up waiting: the outcome is not known.
    TimedOut,
    /// The terminal cannot say what happened.
    Unknown,
}

impl std::fmt::Debug for PaymentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PaymentStatus::Pending => f.write_str("Pending"),
            // Only the reference: the rest never goes to logs.
            PaymentStatus::Approved { provider_ref, .. } => f
                .debug_struct("Approved")
                .field("provider_ref", provider_ref)
                .finish_non_exhaustive(),
            PaymentStatus::Declined { reason } => f.debug_struct("Declined").field("reason", reason).finish(),
            PaymentStatus::Cancelled => f.write_str("Cancelled"),
            PaymentStatus::Reversed => f.write_str("Reversed"),
            PaymentStatus::TimedOut => f.write_str("TimedOut"),
            PaymentStatus::Unknown => f.write_str("Unknown"),
        }
    }
}

impl PaymentStatus {
    /// The status as the domain understands it, with provider data checked (PAY-005):
    /// `TimedOut` and `Unknown` are uncertain (never declined), and an approval whose
    /// reference is unusable or looks like a card number is uncertain too — the card was
    /// charged, but the till will not keep that reference, so the operator settles it.
    pub fn report(&self) -> Report {
        match self {
            PaymentStatus::Pending => Report::Pending,
            PaymentStatus::Approved {
                provider_ref,
                auth_code,
                card_brand,
                masked_pan,
            } => match CardApproval::from_provider(
                provider_ref,
                auth_code.as_deref(),
                card_brand.as_deref(),
                masked_pan.as_deref(),
            ) {
                Ok(a) => Report::Approved(a),
                Err(_) => Report::Uncertain {
                    reason: "approved, but the provider reference was refused (missing, too long or looks like a card number)".into(),
                },
            },
            PaymentStatus::Declined { reason } => Report::Declined {
                reason: reason.clone(),
            },
            PaymentStatus::Cancelled => Report::Cancelled,
            PaymentStatus::Reversed => Report::Reversed,
            PaymentStatus::TimedOut => Report::Uncertain {
                reason: "the terminal timed out".into(),
            },
            PaymentStatus::Unknown => Report::Uncertain {
                reason: "the terminal reported an unknown outcome".into(),
            },
        }
    }
}

/// What a terminal or provider supports.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PaymentCapabilities {
    /// Card refunds (`PaymentKind::Refund`).
    pub refund: bool,
    /// Cancelling an attempt still in progress from the till.
    pub cancel: bool,
    /// Voiding or reversing an approval before settlement.
    pub reversal: bool,
    /// Approving less than asked. The till does not handle it yet: connectors report
    /// `false` and decline (or cancel) partial approvals.
    pub partial_approval: bool,
}

impl Default for PaymentCapabilities {
    fn default() -> Self {
        Self {
            refund: false,
            cancel: true,
            reversal: true,
            partial_approval: false,
        }
    }
}

/// Whether a failed call may have reached the terminal (see the module documentation).
/// `false` means nothing can have been charged.
pub fn may_have_been_sent(error: &DeviceError) -> bool {
    match error {
        DeviceError::NotConfigured | DeviceError::Disconnected(_) | DeviceError::Unsupported(_) => false,
        DeviceError::Timeout | DeviceError::Io(_) => true,
    }
}

/// A payment terminal or provider. Every call may take as long as the customer does;
/// the application bounds each with its own timeout and treats running out of time like
/// [`DeviceError::Timeout`].
#[async_trait::async_trait]
pub trait PaymentPort: Send + Sync {
    /// Which connector this is, as stored in `payment_attempts.provider` and
    /// `tenders.provider_code`.
    fn provider(&self) -> &'static str;

    fn capabilities(&self) -> PaymentCapabilities {
        PaymentCapabilities::default()
    }

    /// Sends the request (idempotent by `attempt_id`) and returns the status so far.
    async fn initiate(&self, request: &PaymentRequest) -> Result<PaymentStatus, DeviceError>;

    /// The attempt's current status.
    async fn query(&self, attempt: PaymentAttemptId) -> Result<PaymentStatus, DeviceError>;

    /// Asks the terminal to stop an attempt in progress. Returns the resulting status,
    /// which may still be `Approved` if the payment went through first.
    async fn cancel(&self, attempt: PaymentAttemptId) -> Result<PaymentStatus, DeviceError>;

    /// Voids or reverses an approved attempt. Returns the resulting status.
    async fn reverse(&self, attempt: PaymentAttemptId) -> Result<PaymentStatus, DeviceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_statuses_are_never_declines() {
        assert!(matches!(
            PaymentStatus::TimedOut.report(),
            Report::Uncertain { .. }
        ));
        assert!(matches!(
            PaymentStatus::Unknown.report(),
            Report::Uncertain { .. }
        ));
        assert!(matches!(
            PaymentStatus::Declined { reason: "x".into() }.report(),
            Report::Declined { .. }
        ));
    }

    #[test]
    fn provider_data_is_checked_before_the_till_keeps_it() {
        let approved = |r: &str, pan: &str| PaymentStatus::Approved {
            provider_ref: r.into(),
            auth_code: Some("A1B2C3".into()),
            card_brand: Some("VISA".into()),
            masked_pan: Some(pan.into()),
        };
        match approved("ch_test_1", "4111111111111111").report() {
            Report::Approved(a) => {
                assert_eq!(a.provider_ref(), "ch_test_1");
                assert_eq!(a.masked_pan().unwrap().as_str(), "****1111");
            }
            other => panic!("{other:?}"),
        }
        // A reference that looks like a card number is never kept: the outcome is unknown.
        assert!(matches!(
            approved("4111111111111111", "****1111").report(),
            Report::Uncertain { .. }
        ));
        let debug = format!("{:?}", approved("ch_test_1", "****4242"));
        assert!(!debug.contains("4242") && !debug.contains("A1B2C3"), "{debug}");
    }

    #[test]
    fn only_timeouts_and_io_errors_may_have_reached_the_terminal() {
        assert!(!may_have_been_sent(&DeviceError::NotConfigured));
        assert!(!may_have_been_sent(&DeviceError::Disconnected("refused".into())));
        assert!(!may_have_been_sent(&DeviceError::Unsupported("x".into())));
        assert!(may_have_been_sent(&DeviceError::Timeout));
        assert!(may_have_been_sent(&DeviceError::Io("reset".into())));
    }
}
