//! Paying for a sale (E10-T01, PAY-001..005, POS-006/007, docs/backlog/06 §5).
//!
//! Tenders are applied in order against the amount due. Only cash gives change, and only the
//! last tender may leave change. External tenders (card on a separate terminal, bank
//! transfer, QR) are **recorded by the operator**: they are stored as confirmed by the
//! operator, never as confirmed by a provider (PAY-003). A provider confirmation can only be
//! made from a payment provider's approval, which does not exist in the MVP.

use crate::pricing::{PricingPolicy, cash_rounding};
use crate::{ErrorCode, HasErrorCode, Money};
use serde::{Deserialize, Serialize};

/// Longest external reference kept (PAY-005).
pub const MAX_REFERENCE_LEN: usize = 64;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum TenderKind {
    Cash,
    CardExternal,
    BankTransferExternal,
    QrExternal,
    OtherExternal,
}

impl TenderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            TenderKind::Cash => "cash",
            TenderKind::CardExternal => "card_external",
            TenderKind::BankTransferExternal => "bank_transfer_external",
            TenderKind::QrExternal => "qr_external",
            TenderKind::OtherExternal => "other_external",
        }
    }
    pub const fn is_cash(self) -> bool {
        matches!(self, TenderKind::Cash)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenderStatus {
    Pending,
    Confirmed,
    Declined,
    Reversed,
    Refunded,
    ReconciliationRequired,
}

impl TenderStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            TenderStatus::Pending => "pending",
            TenderStatus::Confirmed => "confirmed",
            TenderStatus::Declined => "declined",
            TenderStatus::Reversed => "reversed",
            TenderStatus::Refunded => "refunded",
            TenderStatus::ReconciliationRequired => "reconciliation_required",
        }
    }
}

/// Who confirmed a tender. `Provider` carries proof that cannot be built outside this module:
/// there is no code path from operator input to a provider confirmation (AT-13).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationSource {
    Operator,
    Provider(ProviderApproval),
}

impl ConfirmationSource {
    pub const fn as_str(&self) -> &'static str {
        match self {
            ConfirmationSource::Operator => "operator",
            ConfirmationSource::Provider(_) => "provider",
        }
    }
}

/// A payment provider's approval. Private field: only a `PaymentPort` result may make one,
/// and the MVP has none (E10-T02 builds the flow behind a non-production port).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderApproval {
    reference: String,
}

/// One tender as entered at the till.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenderInput {
    pub kind: TenderKind,
    /// For cash: what the customer handed over. Otherwise: the amount charged.
    pub amount: Money,
    /// A note from the external terminal or bank, e.g. an approval code. Never a card number.
    pub external_reference: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SettledTender {
    pub kind: TenderKind,
    /// The part of the sale this tender pays.
    pub amount: Money,
    /// Cash handed over (cash only).
    pub received: Option<Money>,
    pub change: Money,
    pub status: TenderStatus,
    pub confirmation: ConfirmationSource,
    pub external_reference: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Settlement {
    pub tenders: Vec<SettledTender>,
    /// Cash rounding (ADR-004): `total_due − priced total`. Zero unless every tender is cash.
    pub rounding: Money,
    pub total_due: Money,
    pub tendered_total: Money,
    pub change: Money,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettlementError {
    #[error("{remaining:?} is still due")]
    Insufficient { remaining: Money },
    #[error("a non-cash tender cannot be more than the amount still due")]
    OverTenderNonCash,
    #[error("change can only be given from the last tender, and only from cash")]
    ChangeFromNonCash,
    #[error("tender amounts must be greater than zero")]
    NonPositiveAmount,
    #[error("the sale is already paid; remove the extra tender")]
    TenderAfterPaid,
    #[error("there are no tenders")]
    NoTenders,
    #[error("references can be at most {MAX_REFERENCE_LEN} characters")]
    ReferenceTooLong,
    /// PAY-005: card numbers are never stored. The value is deliberately not in the message.
    #[error("the reference looks like a card number; enter the terminal's approval code instead")]
    ReferenceLooksLikeCard,
    #[error("amount out of range")]
    Overflow,
}

impl HasErrorCode for SettlementError {
    fn error_code(&self) -> ErrorCode {
        match self {
            SettlementError::Insufficient { .. } => ErrorCode::TenderInsufficient,
            SettlementError::OverTenderNonCash => ErrorCode::TenderOverpayNonCash,
            SettlementError::ChangeFromNonCash => ErrorCode::TenderChangeFromNonCash,
            _ => ErrorCode::ValidationFailed,
        }
    }
}

impl From<crate::ArithmeticError> for SettlementError {
    fn from(_: crate::ArithmeticError) -> Self {
        SettlementError::Overflow
    }
}

/// Settles `total` (the priced total) with `tenders`, in order.
pub fn settle(
    total: Money,
    tenders: &[TenderInput],
    policy: &PricingPolicy,
) -> Result<Settlement, SettlementError> {
    if tenders.is_empty() {
        return Err(SettlementError::NoTenders);
    }
    let all_cash = tenders.iter().all(|t| t.kind.is_cash());
    let rounding = if all_cash {
        cash_rounding(total, policy)?
    } else {
        Money::ZERO
    };
    let total_due = total.checked_add(rounding)?;
    let mut remaining = total_due;
    let mut settled = Vec::with_capacity(tenders.len());
    for (i, t) in tenders.iter().enumerate() {
        if t.amount.minor() <= 0 {
            return Err(SettlementError::NonPositiveAmount);
        }
        if remaining.minor() == 0 {
            return Err(SettlementError::TenderAfterPaid);
        }
        let last = i + 1 == tenders.len();
        let (amount, received, change) = if t.kind.is_cash() {
            let applied = t.amount.min(remaining);
            let change = t.amount.checked_sub(applied)?;
            if change.minor() > 0 && !last {
                return Err(SettlementError::ChangeFromNonCash);
            }
            (applied, Some(t.amount), change)
        } else {
            if t.amount > remaining {
                return Err(SettlementError::OverTenderNonCash);
            }
            (t.amount, None, Money::ZERO)
        };
        remaining = remaining.checked_sub(amount)?;
        settled.push(SettledTender {
            kind: t.kind,
            amount,
            received,
            change,
            status: TenderStatus::Confirmed,
            confirmation: ConfirmationSource::Operator,
            external_reference: if t.kind.is_cash() {
                None
            } else {
                clean_reference(t.external_reference.as_deref())?
            },
        });
    }
    if remaining.minor() > 0 {
        return Err(SettlementError::Insufficient { remaining });
    }
    let tendered_total = settled
        .iter()
        .try_fold(Money::ZERO, |a, t| a.checked_add(t.received.unwrap_or(t.amount)))?;
    let change = settled
        .iter()
        .try_fold(Money::ZERO, |a, t| a.checked_add(t.change))?;
    Ok(Settlement {
        tenders: settled,
        rounding,
        total_due,
        tendered_total,
        change,
    })
}

fn clean_reference(r: Option<&str>) -> Result<Option<String>, SettlementError> {
    let Some(r) = r.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if r.chars().count() > MAX_REFERENCE_LEN {
        return Err(SettlementError::ReferenceTooLong);
    }
    if looks_like_card_number(r) {
        return Err(SettlementError::ReferenceLooksLikeCard);
    }
    Ok(Some(r.to_owned()))
}

/// PAY-005 deny-list: a Luhn-valid run of 13–19 digits, ignoring spaces and dashes (how card
/// numbers are usually written). Whole runs only: a longer numeric reference is not a card
/// number just because some 16 digits inside it pass Luhn.
pub fn looks_like_card_number(text: &str) -> bool {
    let joined: String = text.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    joined.split(|c: char| !c.is_ascii_digit()).any(|run| {
        let digits: Vec<u8> = run.bytes().map(|b| b - b'0').collect();
        (13..=19).contains(&digits.len()) && luhn(&digits)
    })
}

fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = u32::from(d);
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::TaxMode;

    fn m(minor: i64) -> Money {
        Money::from_minor(minor)
    }
    fn cash(minor: i64) -> TenderInput {
        TenderInput {
            kind: TenderKind::Cash,
            amount: m(minor),
            external_reference: None,
        }
    }
    fn card(minor: i64, reference: Option<&str>) -> TenderInput {
        TenderInput {
            kind: TenderKind::CardExternal,
            amount: m(minor),
            external_reference: reference.map(Into::into),
        }
    }
    fn policy() -> PricingPolicy {
        PricingPolicy::new(TaxMode::Inclusive)
    }

    #[test]
    fn cash_gives_change() {
        let s = settle(m(1345), &[cash(2000)], &policy()).unwrap();
        assert_eq!(
            (s.change, s.tendered_total, s.total_due),
            (m(655), m(2000), m(1345))
        );
        assert_eq!(s.tenders[0].amount, m(1345));
        assert_eq!(s.tenders[0].received, Some(m(2000)));
    }

    #[test]
    fn split_gives_change_from_cash_only() {
        let s = settle(m(1345), &[card(1000, Some("A1B2C3")), cash(500)], &policy()).unwrap();
        assert_eq!(s.change, m(155));
        assert_eq!((s.tenders[0].change, s.tenders[1].change), (m(0), m(155)));
        assert_eq!(s.tenders[0].confirmation, ConfirmationSource::Operator);
        assert_eq!(s.tenders[0].status, TenderStatus::Confirmed);
        assert_eq!(s.tenders[0].external_reference.as_deref(), Some("A1B2C3"));
    }

    #[test]
    fn refusals() {
        assert_eq!(
            settle(m(1345), &[card(1500, None)], &policy()),
            Err(SettlementError::OverTenderNonCash)
        );
        assert_eq!(
            settle(m(1345), &[cash(1000)], &policy()),
            Err(SettlementError::Insufficient { remaining: m(345) })
        );
        assert_eq!(
            settle(m(1345), &[cash(2000), card(100, None)], &policy()),
            Err(SettlementError::ChangeFromNonCash)
        );
        assert_eq!(
            settle(m(1345), &[card(1345, None), cash(100)], &policy()),
            Err(SettlementError::TenderAfterPaid)
        );
        assert_eq!(
            settle(m(1345), &[cash(0)], &policy()),
            Err(SettlementError::NonPositiveAmount)
        );
        assert_eq!(settle(m(1345), &[], &policy()), Err(SettlementError::NoTenders));
        assert_eq!(
            settle(m(1345), &[cash(1000)], &policy())
                .unwrap_err()
                .error_code(),
            ErrorCode::TenderInsufficient
        );
    }

    #[test]
    fn a_card_number_is_never_accepted_as_a_reference() {
        for pan in [
            "4111111111111111",
            "4111 1111 1111 1111",
            "ref 4111-1111-1111-1111",
            "5500005555555559",
        ] {
            let e = settle(m(1345), &[card(1345, Some(pan))], &policy()).unwrap_err();
            assert_eq!(e, SettlementError::ReferenceLooksLikeCard);
            assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
            assert!(!e.to_string().contains("4111"), "the value is never echoed");
        }
        // Approval codes, short numbers and non-Luhn long numbers are fine.
        for ok in [
            "123456",
            "AUTH 004211",
            "4111111111111112",
            "41111111111111110000000",
        ] {
            assert!(
                settle(m(1345), &[card(1345, Some(ok))], &policy()).is_ok(),
                "{ok}"
            );
        }
        let long = "x".repeat(65);
        assert_eq!(
            settle(m(1), &[card(1, Some(&long))], &policy()),
            Err(SettlementError::ReferenceTooLong)
        );
    }

    #[test]
    fn cash_rounding_only_when_all_cash() {
        let mut p = policy();
        p.cash_rounding_increment = Some(m(5));
        let s = settle(m(1347), &[cash(2000)], &p).unwrap();
        assert_eq!((s.rounding, s.total_due, s.change), (m(-2), m(1345), m(655)));
        let s = settle(m(1347), &[card(1000, None), cash(500)], &p).unwrap();
        assert_eq!((s.rounding, s.total_due, s.change), (m(0), m(1347), m(153)));
    }

    #[test]
    fn invariants_hold() {
        let s = settle(m(999), &[card(400, None), cash(1000)], &policy()).unwrap();
        let applied: i64 = s.tenders.iter().map(|t| t.amount.minor()).sum();
        assert_eq!(applied, s.total_due.minor());
        assert_eq!(s.tendered_total.minor() - s.change.minor(), s.total_due.minor());
    }
}
