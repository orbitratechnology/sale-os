//! Returns and refunds (E12-T01, RET-003..007, ADR-004).
//!
//! **Cumulative refund rule.** Returning `q` more units of a line sold as `Q` units for
//! `total`, after `r` were already returned, refunds
//! `share(r + q) − share(r)` where `share(n) = round(total × n / Q)`. Every partial return of
//! a line therefore adds up to exactly what the customer paid for it: three single returns
//! of a 10.00 line of 3 give 3.33, 3.34 and 3.33. The line total is the customer-facing
//! total, after its discount and its share of any order discount. Tax is refunded with the
//! same rule. Cash rounding on the original sale is not refunded.

use crate::catalog::Unit;
use crate::inventory::{MovementSource, MovementType, StockBucket, StockMovement};
use crate::sale::SaleMode;
use crate::settlement::{ConfirmationSource, SettledTender, TenderInput, TenderStatus};
use crate::{
    BusinessDate, Currency, ErrorCode, HasErrorCode, IdGenerator, LocationId, Money, MovementId, ProductId,
    Quantity, RefundId, ReturnId, ReturnLineId, SaleId, SaleLineId, Timestamp, UserId, VariantId,
};
use serde::{Deserialize, Serialize};

/// What happens to the returned goods (INV-009).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Back on the shelf: +sellable.
    Restock,
    /// Kept but not sellable: +damaged.
    Damaged,
    /// Nothing comes back into stock (thrown away, or a service).
    NoStockChange,
}

impl Disposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Disposition::Restock => "restock",
            Disposition::Damaged => "damaged",
            Disposition::NoStockChange => "no_stock_change",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ReturnReason {
    ChangedMind,
    Defective,
    WrongItem,
    DamagedInTransit,
    Other,
}

impl ReturnReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            ReturnReason::ChangedMind => "changed_mind",
            ReturnReason::Defective => "defective",
            ReturnReason::WrongItem => "wrong_item",
            ReturnReason::DamagedInTransit => "damaged_in_transit",
            ReturnReason::Other => "other",
        }
    }
}

/// A sold line as a return sees it: what was sold, and how much already came back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnableLine {
    pub sale_line_id: SaleLineId,
    pub line_no: u32,
    pub product_id: ProductId,
    pub variant_id: VariantId,
    pub name: String,
    pub variant_label: String,
    pub unit: Unit,
    pub track_stock: bool,
    pub quantity: Quantity,
    /// Customer-facing line total (after discounts, including tax).
    pub total: Money,
    pub tax: Money,
    pub unit_cost: Option<Money>,
    pub returned: Quantity,
}

impl ReturnableLine {
    pub fn available(&self) -> Quantity {
        Quantity::from_milli(self.quantity.milli() - self.returned.milli())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnableSale {
    pub sale_id: SaleId,
    pub receipt_number: String,
    pub mode: SaleMode,
    pub currency: Currency,
    pub lines: Vec<ReturnableLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnLineRequest {
    pub sale_line_id: SaleLineId,
    pub quantity: Quantity,
    pub disposition: Disposition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedReturnLine {
    pub sale_line_id: SaleLineId,
    pub product_id: ProductId,
    pub variant_id: VariantId,
    pub name: String,
    pub variant_label: String,
    pub unit: Unit,
    pub quantity: Quantity,
    pub refund: Money,
    pub tax: Money,
    pub disposition: Disposition,
    pub track_stock: bool,
    pub unit_cost: Option<Money>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnPlan {
    pub original_sale: SaleId,
    pub lines: Vec<PlannedReturnLine>,
    /// `total − tax`.
    pub subtotal: Money,
    pub tax: Money,
    pub total: Money,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReturnError {
    #[error("only {available:?} of that line can still be returned")]
    QuantityExceedsUnreturned { line: SaleLineId, available: Quantity },
    #[error("that line is not part of the sale")]
    LineNotInSale,
    #[error("a line can appear only once in a return")]
    LineRepeated,
    #[error("refunds add up to {got:?}, the return is {expected:?}")]
    RefundMismatch { expected: Money, got: Money },
    #[error("a practice (sandbox) sale cannot be returned in live mode")]
    SaleIsSandbox,
    #[error("choose at least one item to return")]
    EmptyReturn,
    #[error("refund amounts must be greater than zero")]
    NonPositiveRefund,
    #[error(transparent)]
    Quantity(#[from] crate::quantity::QuantityError),
    #[error(transparent)]
    Reference(#[from] crate::settlement::SettlementError),
    #[error("amount out of range")]
    Overflow,
}

impl HasErrorCode for ReturnError {
    fn error_code(&self) -> ErrorCode {
        match self {
            ReturnError::QuantityExceedsUnreturned { .. } => ErrorCode::ReturnQtyExceeds,
            ReturnError::RefundMismatch { .. } => ErrorCode::ReturnRefundMismatch,
            ReturnError::Reference(e) => e.error_code(),
            _ => ErrorCode::ValidationFailed,
        }
    }
}

/// `round(amount × n / of)`, half away from zero, in integers.
fn share(amount: Money, n: i64, of: i64) -> Result<Money, ReturnError> {
    if of <= 0 {
        return Err(ReturnError::Overflow);
    }
    let x = i128::from(amount.minor()) * i128::from(n);
    let d = i128::from(of);
    let q = if x >= 0 {
        (2 * x + d) / (2 * d)
    } else {
        -((-2 * x + d) / (2 * d))
    };
    i64::try_from(q)
        .map(Money::from_minor)
        .map_err(|_| ReturnError::Overflow)
}

/// The refund for returning `quantity` more of `line` (the cumulative rule).
/// `before` is what was returned already, including earlier parts of this same return.
fn cumulative(
    amount: Money,
    line: &ReturnableLine,
    before: i64,
    quantity: Quantity,
) -> Result<Money, ReturnError> {
    let after = before + quantity.milli();
    let total = line.quantity.milli();
    share(amount, after, total)?
        .checked_sub(share(amount, before, total)?)
        .map_err(|_| ReturnError::Overflow)
}

/// RET-003/004: what a return of `lines` refunds. `current_mode` is the till's mode now.
pub fn plan_return(
    sale: &ReturnableSale,
    current_mode: SaleMode,
    lines: &[ReturnLineRequest],
) -> Result<ReturnPlan, ReturnError> {
    if sale.mode == SaleMode::Sandbox && current_mode == SaleMode::Live {
        return Err(ReturnError::SaleIsSandbox);
    }
    let requested: Vec<&ReturnLineRequest> = lines.iter().filter(|l| l.quantity.milli() != 0).collect();
    if requested.is_empty() {
        return Err(ReturnError::EmptyReturn);
    }
    let mut planned = Vec::with_capacity(requested.len());
    for (i, r) in requested.iter().enumerate() {
        // One sale line may be split across dispositions (two back on the shelf, one
        // damaged), but not listed twice with the same one.
        if requested[..i]
            .iter()
            .any(|p| p.sale_line_id == r.sale_line_id && p.disposition == r.disposition)
        {
            return Err(ReturnError::LineRepeated);
        }
        let line = sale
            .lines
            .iter()
            .find(|l| l.sale_line_id == r.sale_line_id)
            .ok_or(ReturnError::LineNotInSale)?;
        r.quantity.validate_for_unit(line.unit.allows_fraction())?;
        let earlier: i64 = planned
            .iter()
            .filter(|p: &&PlannedReturnLine| p.sale_line_id == line.sale_line_id)
            .map(|p| p.quantity.milli())
            .sum();
        let available = Quantity::from_milli(line.available().milli() - earlier);
        if r.quantity.milli() > available.milli() {
            return Err(ReturnError::QuantityExceedsUnreturned {
                line: line.sale_line_id,
                available,
            });
        }
        let before = line.returned.milli() + earlier;
        planned.push(PlannedReturnLine {
            sale_line_id: line.sale_line_id,
            product_id: line.product_id,
            variant_id: line.variant_id,
            name: line.name.clone(),
            variant_label: line.variant_label.clone(),
            unit: line.unit,
            quantity: r.quantity,
            refund: cumulative(line.total, line, before, r.quantity)?,
            tax: cumulative(line.tax, line, before, r.quantity)?,
            // A service or non-stock item never moves stock, whatever was chosen.
            disposition: if line.track_stock {
                r.disposition
            } else {
                Disposition::NoStockChange
            },
            track_stock: line.track_stock,
            unit_cost: line.unit_cost,
        });
    }
    let sum = |f: fn(&PlannedReturnLine) -> Money| {
        planned
            .iter()
            .try_fold(Money::ZERO, |a, l| a.checked_add(f(l)))
            .map_err(|_| ReturnError::Overflow)
    };
    let total = sum(|l| l.refund)?;
    let tax = sum(|l| l.tax)?;
    Ok(ReturnPlan {
        original_sale: sale.sale_id,
        subtotal: total.checked_sub(tax).map_err(|_| ReturnError::Overflow)?,
        tax,
        total,
        lines: planned,
    })
}

/// Refund tenders must add up to the return exactly: no change is given on a refund.
pub fn settle_refunds(total: Money, refunds: &[TenderInput]) -> Result<Vec<SettledTender>, ReturnError> {
    let mut out = Vec::with_capacity(refunds.len());
    let mut sum = Money::ZERO;
    for r in refunds {
        if r.amount.minor() <= 0 {
            return Err(ReturnError::NonPositiveRefund);
        }
        sum = sum.checked_add(r.amount).map_err(|_| ReturnError::Overflow)?;
        out.push(SettledTender {
            kind: r.kind,
            amount: r.amount,
            received: None,
            change: Money::ZERO,
            status: TenderStatus::Confirmed,
            confirmation: ConfirmationSource::Operator,
            external_reference: if r.kind.is_cash() {
                None
            } else {
                crate::settlement::clean_reference(r.external_reference.as_deref())?
            },
        });
    }
    if sum != total {
        return Err(ReturnError::RefundMismatch {
            expected: total,
            got: sum,
        });
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnLineSnapshot {
    pub id: ReturnLineId,
    #[serde(flatten)]
    pub line: PlannedReturnLine,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefundSnapshot {
    pub id: RefundId,
    pub seq: u32,
    #[serde(flatten)]
    pub tender: SettledTender,
}

/// A completed return, as stored and as the `return.completed` v1 payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnSnapshot {
    pub id: ReturnId,
    pub original_sale: SaleId,
    pub original_receipt_number: String,
    pub reason: ReturnReason,
    pub note: Option<String>,
    pub currency: Currency,
    pub subtotal: Money,
    pub tax: Money,
    pub total: Money,
    pub lines: Vec<ReturnLineSnapshot>,
    pub refunds: Vec<RefundSnapshot>,
    pub actor: UserId,
    pub approver: Option<UserId>,
    pub occurred_at: Timestamp,
    pub business_date: BusinessDate,
    pub timezone: String,
}

impl ReturnSnapshot {
    /// Stock comes back by disposition (INV-009): restock to sellable, damaged to damaged.
    pub fn movements(&self, location: LocationId, ids: &dyn IdGenerator) -> Vec<StockMovement> {
        self.lines
            .iter()
            .filter(|l| l.line.track_stock && l.line.disposition != Disposition::NoStockChange)
            .map(|l| {
                let (bucket, kind) = match l.line.disposition {
                    Disposition::Damaged => (StockBucket::Damaged, MovementType::ReturnDamaged),
                    _ => (StockBucket::Sellable, MovementType::ReturnRestock),
                };
                StockMovement {
                    id: MovementId::generate(ids),
                    location_id: location,
                    variant_id: l.line.variant_id,
                    bucket,
                    movement_type: kind,
                    delta: l.line.quantity,
                    unit_cost: l.line.unit_cost,
                    source: MovementSource::Return {
                        return_id: self.id,
                        line_id: l.id,
                    },
                    actor: self.actor,
                    occurred_at: self.occurred_at,
                    business_date: self.business_date,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CurrencyCode, UuidV7Generator};

    fn line(qty: i64, total: i64, tax: i64, returned: i64) -> ReturnableLine {
        let ids: &dyn IdGenerator = &UuidV7Generator;
        ReturnableLine {
            sale_line_id: SaleLineId::generate(ids),
            line_no: 1,
            product_id: ProductId::generate(ids),
            variant_id: VariantId::generate(ids),
            name: "Tea".into(),
            variant_label: String::new(),
            unit: Unit::Ea,
            track_stock: true,
            quantity: Quantity::from_whole(qty).unwrap(),
            total: Money::from_minor(total),
            tax: Money::from_minor(tax),
            unit_cost: None,
            returned: Quantity::from_whole(returned).unwrap(),
        }
    }

    fn sale(lines: Vec<ReturnableLine>) -> ReturnableSale {
        ReturnableSale {
            sale_id: SaleId::generate(&UuidV7Generator),
            receipt_number: "R01-000001".into(),
            mode: SaleMode::Live,
            currency: Currency::new(CurrencyCode::new("GBP").unwrap(), 2).unwrap(),
            lines,
        }
    }

    fn one(l: &ReturnableLine) -> Vec<ReturnLineRequest> {
        vec![ReturnLineRequest {
            sale_line_id: l.sale_line_id,
            quantity: Quantity::ONE,
            disposition: Disposition::Restock,
        }]
    }

    /// Returns the line one unit at a time, as separate returns, and collects the refunds.
    fn one_by_one(qty: i64, total: i64, tax: i64) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        for r in 0..qty {
            let l = line(qty, total, tax, r);
            let p = plan_return(&sale(vec![l.clone()]), SaleMode::Live, &one(&l)).unwrap();
            out.push((p.total.minor(), p.tax.minor()));
        }
        out
    }

    #[test]
    fn partial_returns_add_up_to_exactly_what_was_paid() {
        // 3 × 3.33 = 9.99
        assert_eq!(
            one_by_one(3, 999, 0).iter().map(|x| x.0).collect::<Vec<_>>(),
            [333, 333, 333]
        );
        // 10.00 over 3: 3.33, 3.34, 3.33
        assert_eq!(
            one_by_one(3, 1000, 0).iter().map(|x| x.0).collect::<Vec<_>>(),
            [333, 334, 333]
        );
        // Tax follows the same rule and also adds up: 1.67 over 3.
        let r = one_by_one(3, 1000, 167);
        assert_eq!(r.iter().map(|x| x.1).sum::<i64>(), 167);
        assert_eq!(r.iter().map(|x| x.1).collect::<Vec<_>>(), [56, 55, 56]);
    }

    #[test]
    fn the_net_line_total_is_refunded_so_order_discounts_are_honoured() {
        // Sold 2 for 10.00 but the line's net after an order-discount share is 9.00.
        let l = line(2, 900, 150, 0);
        let p = plan_return(&sale(vec![l.clone()]), SaleMode::Live, &one(&l)).unwrap();
        assert_eq!(
            (p.total.minor(), p.tax.minor(), p.subtotal.minor()),
            (450, 75, 375)
        );
    }

    #[test]
    fn one_line_split_across_dispositions_refunds_like_one_by_one() {
        // 3 for 10.00: two back on the shelf, one damaged, in one return.
        let l = line(3, 1000, 167, 0);
        let s = sale(vec![l.clone()]);
        let part = |n: i64, d: Disposition| ReturnLineRequest {
            sale_line_id: l.sale_line_id,
            quantity: Quantity::from_whole(n).unwrap(),
            disposition: d,
        };
        let p = plan_return(
            &s,
            SaleMode::Live,
            &[part(2, Disposition::Restock), part(1, Disposition::Damaged)],
        )
        .unwrap();
        assert_eq!(
            p.lines.iter().map(|l| l.refund.minor()).collect::<Vec<_>>(),
            [667, 333]
        );
        assert_eq!((p.total.minor(), p.tax.minor()), (1000, 167));
        // The parts together cannot exceed what is left.
        let e = plan_return(
            &s,
            SaleMode::Live,
            &[part(2, Disposition::Restock), part(2, Disposition::Damaged)],
        )
        .unwrap_err();
        assert_eq!(
            e,
            ReturnError::QuantityExceedsUnreturned {
                line: l.sale_line_id,
                available: Quantity::ONE
            }
        );
    }

    #[test]
    fn refusals() {
        let l = line(3, 999, 0, 2);
        let s = sale(vec![l.clone()]);
        let two = vec![ReturnLineRequest {
            sale_line_id: l.sale_line_id,
            quantity: Quantity::from_whole(2).unwrap(),
            disposition: Disposition::Restock,
        }];
        let e = plan_return(&s, SaleMode::Live, &two).unwrap_err();
        assert_eq!(
            e,
            ReturnError::QuantityExceedsUnreturned {
                line: l.sale_line_id,
                available: Quantity::ONE
            }
        );
        assert_eq!(e.error_code(), ErrorCode::ReturnQtyExceeds);
        assert_eq!(
            plan_return(&s, SaleMode::Live, &[]),
            Err(ReturnError::EmptyReturn)
        );
        let twice = [one(&l), one(&l)].concat();
        assert_eq!(
            plan_return(&s, SaleMode::Live, &twice),
            Err(ReturnError::LineRepeated)
        );
        let other = line(1, 100, 0, 0);
        assert_eq!(
            plan_return(&s, SaleMode::Live, &one(&other)),
            Err(ReturnError::LineNotInSale)
        );
        let half = vec![ReturnLineRequest {
            sale_line_id: l.sale_line_id,
            quantity: Quantity::from_milli(500),
            disposition: Disposition::Restock,
        }];
        assert!(matches!(
            plan_return(&s, SaleMode::Live, &half),
            Err(ReturnError::Quantity(_))
        ));
        let mut sandbox = s.clone();
        sandbox.mode = SaleMode::Sandbox;
        assert_eq!(
            plan_return(&sandbox, SaleMode::Live, &one(&l)),
            Err(ReturnError::SaleIsSandbox)
        );
        assert!(plan_return(&sandbox, SaleMode::Sandbox, &one(&l)).is_ok());
    }

    #[test]
    fn refunds_must_match_exactly_and_never_store_a_card_number() {
        use crate::settlement::TenderKind;
        let t = |kind, minor, r: Option<&str>| TenderInput {
            kind,
            amount: Money::from_minor(minor),
            external_reference: r.map(Into::into),
        };
        assert!(settle_refunds(Money::from_minor(500), &[t(TenderKind::Cash, 500, None)]).is_ok());
        let split = settle_refunds(
            Money::from_minor(500),
            &[
                t(TenderKind::CardExternal, 300, Some("RF 12")),
                t(TenderKind::Cash, 200, None),
            ],
        )
        .unwrap();
        assert_eq!(split[0].confirmation, ConfirmationSource::Operator);
        let e = settle_refunds(Money::from_minor(500), &[t(TenderKind::Cash, 600, None)]).unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::ReturnRefundMismatch);
        let e = settle_refunds(
            Money::from_minor(500),
            &[t(TenderKind::CardExternal, 500, Some("4111111111111111"))],
        )
        .unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    }

    #[test]
    fn a_service_line_never_moves_stock() {
        let mut l = line(1, 500, 0, 0);
        l.track_stock = false;
        let req = vec![ReturnLineRequest {
            sale_line_id: l.sale_line_id,
            quantity: Quantity::ONE,
            disposition: Disposition::Damaged,
        }];
        let p = plan_return(&sale(vec![l]), SaleMode::Live, &req).unwrap();
        assert_eq!(p.lines[0].disposition, Disposition::NoStockChange);
    }
}
