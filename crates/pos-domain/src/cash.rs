//! Cash sessions (E15-T02, DER-001): a drawer opened with a float and closed with a count.
//!
//! What should be in the drawer is the float plus the cash kept from sales (the amount
//! applied to each sale: what was handed over less the change given) less the cash paid
//! out as refunds. The difference between the count and that is the variance.

use crate::settlement::TenderKind;
use crate::{CashSessionId, Money, Timestamp, UserId};
use serde::{Deserialize, Serialize};

/// Per tender kind: how many and how much.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenderTotal {
    pub kind: TenderKind,
    pub count: u32,
    pub amount: Money,
}

/// What happened during a session, from the recorded sales and returns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTotals {
    pub sales_count: u32,
    /// Sum of sale totals (what customers were charged).
    pub sales_total: Money,
    pub tax_total: Money,
    /// Line and order discounts together.
    pub discount_total: Money,
    pub rounding_total: Money,
    /// Amounts applied to sales, by tender kind (cash net of change).
    pub tenders: Vec<TenderTotal>,
    pub returns_count: u32,
    pub refunds_total: Money,
    pub refunds: Vec<TenderTotal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CashError {
    #[error("the amount is too large")]
    Overflow,
    #[error("an amount of cash cannot be negative")]
    Negative,
}

impl SessionTotals {
    fn of(list: &[TenderTotal], kind: TenderKind) -> Result<Money, CashError> {
        list.iter()
            .filter(|t| t.kind == kind)
            .try_fold(Money::ZERO, |a, t| a.checked_add(t.amount))
            .map_err(|_| CashError::Overflow)
    }

    /// Cash kept from sales: handed over less change.
    pub fn cash_in(&self) -> Result<Money, CashError> {
        Self::of(&self.tenders, TenderKind::Cash)
    }

    /// Cash paid out as refunds.
    pub fn cash_out(&self) -> Result<Money, CashError> {
        Self::of(&self.refunds, TenderKind::Cash)
    }

    /// float + cash kept from sales − cash refunds.
    pub fn expected_cash(&self, float: Money) -> Result<Money, CashError> {
        let (cash_in, cash_out) = (self.cash_in()?, self.cash_out()?);
        float
            .checked_add(cash_in)
            .and_then(|m| m.checked_sub(cash_out))
            .map_err(|_| CashError::Overflow)
    }
}

/// A float or a count: never negative.
pub fn cash_amount(m: Money) -> Result<Money, CashError> {
    if m.minor() < 0 {
        Err(CashError::Negative)
    } else {
        Ok(m)
    }
}

/// counted − expected: positive is over, negative is short.
pub fn variance(counted: Money, expected: Money) -> Result<Money, CashError> {
    counted.checked_sub(expected).map_err(|_| CashError::Overflow)
}

impl crate::HasErrorCode for CashError {
    fn error_code(&self) -> crate::ErrorCode {
        crate::ErrorCode::ValidationFailed
    }
}

/// The `cash_session.opened` v1 payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedSession {
    pub id: CashSessionId,
    pub opened_by: UserId,
    pub opened_at: Timestamp,
    pub opening_float: Money,
    pub business_date: crate::time::BusinessDate,
}

/// The `cash_session.closed` v1 payload. `expected` and `variance` are what the store
/// recomputed inside the closing transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedSession {
    pub id: CashSessionId,
    pub closed_by: UserId,
    pub closed_at: Timestamp,
    pub opening_float: Money,
    pub counted: Money,
    pub expected: Money,
    pub variance: Money,
    pub totals: SessionTotals,
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(kind: TenderKind, count: u32, minor: i64) -> TenderTotal {
        TenderTotal {
            kind,
            count,
            amount: Money::from_minor(minor),
        }
    }

    #[test]
    fn expected_is_float_plus_cash_kept_less_cash_refunds() {
        // Float 100.00. Cash sales 12.50 and 7.30 (net of change), a card sale 20.00, a split
        // 5.00 card + 3.00 cash; refunds 2.50 cash and 4.00 to a card.
        let totals = SessionTotals {
            tenders: vec![
                t(TenderKind::Cash, 3, 1250 + 730 + 300),
                t(TenderKind::CardExternal, 2, 2000 + 500),
            ],
            refunds: vec![t(TenderKind::Cash, 1, 250), t(TenderKind::CardExternal, 1, 400)],
            ..SessionTotals::default()
        };
        let expected = totals.expected_cash(Money::from_minor(10_000)).unwrap();
        assert_eq!(expected.minor(), 10_000 + 1250 + 730 + 300 - 250);
        assert_eq!(
            variance(Money::from_minor(12_000), expected).unwrap().minor(),
            -30
        );
        assert_eq!(variance(Money::from_minor(12_100), expected).unwrap().minor(), 70);
        assert_eq!(cash_amount(Money::from_minor(-1)), Err(CashError::Negative));
        assert!(
            SessionTotals::default()
                .expected_cash(Money::from_minor(i64::MAX))
                .is_ok()
        );
        let huge = SessionTotals {
            tenders: vec![t(TenderKind::Cash, 1, i64::MAX)],
            ..SessionTotals::default()
        };
        assert_eq!(huge.expected_cash(Money::from_minor(1)), Err(CashError::Overflow));
        let two = SessionTotals {
            tenders: vec![t(TenderKind::Cash, 1, i64::MAX), t(TenderKind::Cash, 1, 1)],
            ..SessionTotals::default()
        };
        assert_eq!(two.cash_in(), Err(CashError::Overflow));
    }
}
