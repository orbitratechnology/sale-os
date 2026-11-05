//! Pricing, tax and discounts (E08-T01, POS-004/005, SET-004). The normative algorithm is
//! ADR-004 (docs/backlog/03, 06 §3). Every step rounds once, to minor units, half away
//! from zero by default. Totals are sums of rounded line values and are never re-rounded.
//!
//! 1. `gross = round(unit_price × qty)`
//! 2. line discount: percent → `round(gross × p)`; fixed → the value (≤ gross)
//! 3. `net1 = gross − line_discount`
//! 4. order discount: percent → `round(Σ net1 × p)`; fixed → the value (≤ Σ net1)
//! 5. allocate the order discount to lines by `net1`, largest remainder (exact)
//! 6. `net2 = net1 − allocation`
//! 7. tax per line: inclusive `T = round(net2 × R / (1 + R))`; exclusive
//!    `tax_r = round(net2 × r)`, where `R = Σ r`. With several inclusive rates, T is split
//!    across them in proportion to the rates by largest remainder, so the split is exact.
//! 8. line total: inclusive `net2` (the customer pays the shelf price); exclusive
//!    `net2 + Σ tax`
//! 9. cart total `Σ line totals`; subtotal `Σ gross`
//!
//! The UI never re-computes any of this (ADR-018); it displays `PricedCart`.

use crate::{
    ArithmeticError, BusinessDate, CartLineId, ErrorCode, HasErrorCode, Money, Quantity, RatePpm,
    RoundingMode, TaxRateId, UserId, allocate, round_to_minor,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum TaxMode {
    /// Shelf prices include tax (common for VAT/GST retail).
    Inclusive,
    /// Tax is added on top of shelf prices.
    Exclusive,
}

/// A configured tax rate with its validity window (`tax_rates`, docs/backlog/04).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxRate {
    pub id: TaxRateId,
    pub name: String,
    pub rate: RatePpm,
    /// First business date the rate applies (inclusive); `None` = always.
    pub effective_from: Option<BusinessDate>,
    /// First business date it no longer applies (exclusive); `None` = open-ended.
    pub effective_to: Option<BusinessDate>,
}

/// The rate as applied to a sale, snapshotted onto the sale line (POS-010).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxRateSnapshot {
    pub id: TaxRateId,
    pub name: String,
    pub rate: RatePpm,
}

/// Rates of a tax category that apply on `date`.
pub fn resolve_rates(rates: &[TaxRate], date: BusinessDate) -> Vec<TaxRateSnapshot> {
    rates
        .iter()
        .filter(|r| {
            r.effective_from.is_none_or(|from| from <= date) && r.effective_to.is_none_or(|to| date < to)
        })
        .map(|r| TaxRateSnapshot {
            id: r.id,
            name: r.name.clone(),
            rate: r.rate,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscountValue {
    Percent(RatePpm),
    Fixed(Money),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discount {
    pub value: DiscountValue,
    pub reason: Option<String>,
    /// Approver for discounts above the cashier limit (E08-T02).
    pub authorized_by: Option<UserId>,
}

impl Discount {
    pub fn percent(rate: RatePpm) -> Self {
        Self {
            value: DiscountValue::Percent(rate),
            reason: None,
            authorized_by: None,
        }
    }
    pub fn fixed(amount: Money) -> Self {
        Self {
            value: DiscountValue::Fixed(amount),
            reason: None,
            authorized_by: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricingLine {
    pub line_id: CartLineId,
    pub quantity: Quantity,
    /// Shelf price per unit, including or excluding tax per [`TaxMode`].
    pub unit_price: Money,
    pub line_discount: Option<Discount>,
    /// Rates already resolved for the business date. Empty = exempt.
    pub taxes: Vec<TaxRateSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricingPolicy {
    pub tax_mode: TaxMode,
    pub rounding: RoundingMode,
    /// Cash rounding increment, e.g. 5 minor units. Applied only to all-cash tenders.
    pub cash_rounding_increment: Option<Money>,
}

impl PricingPolicy {
    pub fn new(tax_mode: TaxMode) -> Self {
        Self {
            tax_mode,
            rounding: RoundingMode::HalfAwayFromZero,
            cash_rounding_increment: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaxLine {
    pub tax_rate_id: TaxRateId,
    pub name: String,
    pub rate: RatePpm,
    /// Amount the tax was computed on, excluding tax.
    pub taxable: Money,
    pub tax: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PricedLine {
    pub line_id: CartLineId,
    pub gross: Money,
    pub line_discount: Money,
    pub order_discount_alloc: Money,
    pub tax_lines: Vec<TaxLine>,
    pub tax: Money,
    /// Customer-facing line total.
    pub total: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PricedCart {
    pub lines: Vec<PricedLine>,
    /// Σ gross.
    pub subtotal: Money,
    pub line_discount_total: Money,
    pub order_discount_total: Money,
    pub tax_total: Money,
    /// Aggregated by tax rate, in first-seen order.
    pub tax_breakdown: Vec<TaxLine>,
    /// Before cash rounding.
    pub total: Money,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PricingError {
    #[error("the discount on line {line} is larger than the line")]
    DiscountExceedsLine { line: CartLineId },
    #[error("the order discount is larger than the order")]
    DiscountExceedsOrder,
    #[error("line {line} has a negative price or discount")]
    Negative { line: CartLineId },
    #[error("line {line} must have a quantity greater than zero")]
    NonPositiveQuantity { line: CartLineId },
    #[error(transparent)]
    Arithmetic(#[from] ArithmeticError),
}

impl HasErrorCode for PricingError {
    fn error_code(&self) -> ErrorCode {
        match self {
            PricingError::DiscountExceedsLine { .. } | PricingError::DiscountExceedsOrder => {
                ErrorCode::PricingDiscountExceeds
            }
            _ => ErrorCode::ValidationFailed,
        }
    }
}

fn dec(m: Money) -> Decimal {
    Decimal::from(m.minor())
}

fn percent_of(base: Money, rate: RatePpm, mode: RoundingMode) -> Result<Money, ArithmeticError> {
    round_to_minor(dec(base) * rate.as_decimal(), mode)
}

fn discount_amount(base: Money, d: &Discount, mode: RoundingMode) -> Result<Money, ArithmeticError> {
    match &d.value {
        DiscountValue::Percent(p) => percent_of(base, *p, mode),
        DiscountValue::Fixed(m) => Ok(*m),
    }
}

/// Prices a cart (ADR-004). Pure and deterministic.
pub fn price_cart(
    lines: &[PricingLine],
    order_discount: Option<&Discount>,
    policy: &PricingPolicy,
) -> Result<PricedCart, PricingError> {
    let mode = policy.rounding;

    // Steps 1–3.
    let mut gross = Vec::with_capacity(lines.len());
    let mut line_discount = Vec::with_capacity(lines.len());
    let mut net1 = Vec::with_capacity(lines.len());
    for l in lines {
        if !l.quantity.is_positive() {
            return Err(PricingError::NonPositiveQuantity { line: l.line_id });
        }
        if l.unit_price.is_negative() {
            return Err(PricingError::Negative { line: l.line_id });
        }
        let g = round_to_minor(dec(l.unit_price) * Decimal::new(l.quantity.milli(), 3), mode)?;
        let ld = match &l.line_discount {
            Some(d) => discount_amount(g, d, mode)?,
            None => Money::ZERO,
        };
        if ld.is_negative() {
            return Err(PricingError::Negative { line: l.line_id });
        }
        if ld > g {
            return Err(PricingError::DiscountExceedsLine { line: l.line_id });
        }
        gross.push(g);
        line_discount.push(ld);
        net1.push(g.checked_sub(ld)?);
    }

    // Steps 4–6.
    let net1_total = Money::checked_sum(net1.iter().copied())?;
    let order_total = match order_discount {
        Some(d) => discount_amount(net1_total, d, mode)?,
        None => Money::ZERO,
    };
    if order_total.is_negative() || order_total > net1_total {
        return Err(PricingError::DiscountExceedsOrder);
    }
    let alloc = if lines.is_empty() {
        Vec::new()
    } else {
        allocate(order_total, &net1)?
    };

    // Steps 7–8.
    let mut priced = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        let net2 = net1[i].checked_sub(alloc[i])?;
        let (tax_lines, tax, total) = tax_line(net2, &l.taxes, policy)?;
        priced.push(PricedLine {
            line_id: l.line_id,
            gross: gross[i],
            line_discount: line_discount[i],
            order_discount_alloc: alloc[i],
            tax_lines,
            tax,
            total,
        });
    }

    // Step 9.
    let mut breakdown: Vec<TaxLine> = Vec::new();
    let mut index: BTreeMap<TaxRateId, usize> = BTreeMap::new();
    for t in priced.iter().flat_map(|l| &l.tax_lines) {
        match index.get(&t.tax_rate_id) {
            Some(&i) => {
                breakdown[i].taxable = breakdown[i].taxable.checked_add(t.taxable)?;
                breakdown[i].tax = breakdown[i].tax.checked_add(t.tax)?;
            }
            None => {
                index.insert(t.tax_rate_id, breakdown.len());
                breakdown.push(t.clone());
            }
        }
    }
    Ok(PricedCart {
        subtotal: Money::checked_sum(gross.iter().copied())?,
        line_discount_total: Money::checked_sum(line_discount.iter().copied())?,
        order_discount_total: order_total,
        tax_total: Money::checked_sum(priced.iter().map(|l| l.tax))?,
        tax_breakdown: breakdown,
        total: Money::checked_sum(priced.iter().map(|l| l.total))?,
        lines: priced,
    })
}

/// Tax for one line's discounted amount `net` (step 7) and the line total (step 8).
fn tax_line(
    net: Money,
    taxes: &[TaxRateSnapshot],
    policy: &PricingPolicy,
) -> Result<(Vec<TaxLine>, Money, Money), PricingError> {
    let mode = policy.rounding;
    if taxes.is_empty() {
        return Ok((Vec::new(), Money::ZERO, net));
    }
    match policy.tax_mode {
        TaxMode::Exclusive => {
            let mut lines = Vec::with_capacity(taxes.len());
            for t in taxes {
                let tax = percent_of(net, t.rate, mode)?;
                lines.push(TaxLine {
                    tax_rate_id: t.id,
                    name: t.name.clone(),
                    rate: t.rate,
                    taxable: net,
                    tax,
                });
            }
            let tax = Money::checked_sum(lines.iter().map(|l| l.tax))?;
            Ok((lines, tax, net.checked_add(tax)?))
        }
        TaxMode::Inclusive => {
            let combined: Decimal = taxes.iter().map(|t| t.rate.as_decimal()).sum();
            let total_tax = if combined.is_zero() {
                Money::ZERO
            } else {
                round_to_minor(dec(net) * combined / (Decimal::ONE + combined), mode)?
            };
            let weights: Vec<Money> = taxes
                .iter()
                .map(|t| Money::from_minor(i64::from(t.rate.ppm())))
                .collect();
            let split = allocate(total_tax, &weights)?;
            let taxable = net.checked_sub(total_tax)?;
            let lines = taxes
                .iter()
                .zip(split)
                .map(|(t, tax)| TaxLine {
                    tax_rate_id: t.id,
                    name: t.name.clone(),
                    rate: t.rate,
                    taxable,
                    tax,
                })
                .collect();
            Ok((lines, total_tax, net))
        }
    }
}

/// Cash rounding delta for an all-cash tender (ADR-004), e.g. total 1.02 with increment 0.05
/// gives -0.02. Recorded as `rounding_minor` on the sale, never folded into lines.
pub fn cash_rounding(total: Money, policy: &PricingPolicy) -> Result<Money, ArithmeticError> {
    let Some(inc) = policy.cash_rounding_increment.filter(|i| i.minor() > 0) else {
        return Ok(Money::ZERO);
    };
    let steps = round_to_minor(dec(total) / dec(inc), policy.rounding)?;
    let rounded = Money::from_minor(
        steps
            .minor()
            .checked_mul(inc.minor())
            .ok_or(ArithmeticError::Overflow)?,
    );
    rounded.checked_sub(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UuidV7Generator;
    use proptest::prelude::*;

    fn rate(name: &str, ppm: u32) -> TaxRateSnapshot {
        TaxRateSnapshot {
            id: TaxRateId::generate(&UuidV7Generator),
            name: name.into(),
            rate: RatePpm::new(ppm).unwrap(),
        }
    }

    fn line(price: i64, qty_milli: i64, taxes: Vec<TaxRateSnapshot>) -> PricingLine {
        PricingLine {
            line_id: CartLineId::generate(&UuidV7Generator),
            quantity: Quantity::from_milli(qty_milli),
            unit_price: Money::from_minor(price),
            line_discount: None,
            taxes,
        }
    }

    fn m(v: i64) -> Money {
        Money::from_minor(v)
    }

    fn incl() -> PricingPolicy {
        PricingPolicy::new(TaxMode::Inclusive)
    }
    fn excl() -> PricingPolicy {
        PricingPolicy::new(TaxMode::Exclusive)
    }

    #[test]
    fn inclusive_15_percent_on_three_items() {
        // 9.99 × 3 = 29.97; tax = round(2997 × 0.15 / 1.15) = round(390.913) = 3.91.
        let c = price_cart(&[line(999, 3000, vec![rate("VAT 15%", 150_000)])], None, &incl()).unwrap();
        assert_eq!((c.subtotal, c.tax_total, c.total), (m(2997), m(391), m(2997)));
        assert_eq!(c.tax_breakdown[0].taxable, m(2606));
    }

    #[test]
    fn exclusive_7_25_percent_on_weighed_item() {
        // 0.333 kg × 12.49 = 4.15917 → 4.16; tax = round(416 × 0.0725) = round(30.16) = 0.30.
        let c = price_cart(&[line(1249, 333, vec![rate("Sales tax", 72_500)])], None, &excl()).unwrap();
        assert_eq!((c.subtotal, c.tax_total, c.total), (m(416), m(30), m(446)));
    }

    #[test]
    fn zero_rated_and_exempt() {
        let zero = price_cart(&[line(500, 1000, vec![rate("Zero", 0)])], None, &incl()).unwrap();
        assert_eq!(
            (zero.tax_total, zero.total, zero.tax_breakdown.len()),
            (m(0), m(500), 1)
        );
        let exempt = price_cart(&[line(500, 1000, vec![])], None, &excl()).unwrap();
        assert_eq!(
            (exempt.tax_total, exempt.total, exempt.tax_breakdown.len()),
            (m(0), m(500), 0)
        );
    }

    #[test]
    fn two_rates_on_one_line() {
        let taxes = vec![rate("State 10%", 100_000), rate("City 5%", 50_000)];
        // Inclusive: T = round(1000 × 0.15/1.15) = 130, split 2:1 exactly → 87 + 43.
        let c = price_cart(&[line(1000, 1000, taxes.clone())], None, &incl()).unwrap();
        assert_eq!(
            c.lines[0].tax_lines.iter().map(|t| t.tax).collect::<Vec<_>>(),
            vec![m(87), m(43)]
        );
        assert_eq!((c.tax_total, c.total), (m(130), m(1000)));
        // Exclusive: each rate on the net.
        let c = price_cart(&[line(1000, 1000, taxes)], None, &excl()).unwrap();
        assert_eq!((c.tax_total, c.total), (m(150), m(1150)));
    }

    #[test]
    fn rates_resolve_by_effective_date() {
        let today: BusinessDate = "2026-11-05".parse().unwrap();
        let tomorrow: BusinessDate = "2026-11-06".parse().unwrap();
        let old = TaxRate {
            id: TaxRateId::generate(&UuidV7Generator),
            name: "VAT 15%".into(),
            rate: RatePpm::new(150_000).unwrap(),
            effective_from: None,
            effective_to: Some(tomorrow),
        };
        let new = TaxRate {
            id: TaxRateId::generate(&UuidV7Generator),
            name: "VAT 16%".into(),
            rate: RatePpm::new(160_000).unwrap(),
            effective_from: Some(tomorrow),
            effective_to: None,
        };
        let rates = [old, new];
        assert_eq!(
            resolve_rates(&rates, today)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["VAT 15%"]
        );
        assert_eq!(
            resolve_rates(&rates, tomorrow)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["VAT 16%"]
        );
    }

    #[test]
    fn order_discount_allocates_exactly() {
        let lines = [
            line(333, 1000, vec![]),
            line(333, 1000, vec![]),
            line(334, 1000, vec![]),
        ];
        let c = price_cart(&lines, Some(&Discount::fixed(m(1000))), &excl()).unwrap();
        assert_eq!(c.order_discount_total, m(1000));
        assert_eq!(
            c.lines
                .iter()
                .map(|l| l.order_discount_alloc.minor())
                .sum::<i64>(),
            1000
        );
        assert_eq!(c.total, m(0));
    }

    #[test]
    fn line_and_order_discounts_with_inclusive_tax() {
        let mut l = line(999, 1000, vec![rate("VAT 20%", 200_000)]);
        l.line_discount = Some(Discount::percent(RatePpm::new(100_000).unwrap())); // 10% of 9.99 = 0.999 → 1.00
        let c = price_cart(&[l], Some(&Discount::fixed(m(99))), &incl()).unwrap();
        let p = &c.lines[0];
        assert_eq!(
            (p.gross, p.line_discount, p.order_discount_alloc),
            (m(999), m(100), m(99))
        );
        // net2 = 800; tax = round(800 × 0.2/1.2) = round(133.33) = 133.
        assert_eq!((p.tax, p.total), (m(133), m(800)));
    }

    #[test]
    fn discount_limits() {
        let mut l = line(500, 1000, vec![]);
        l.line_discount = Some(Discount::fixed(m(501)));
        assert!(matches!(
            price_cart(&[l], None, &excl()),
            Err(PricingError::DiscountExceedsLine { .. })
        ));
        let err = price_cart(
            &[line(500, 1000, vec![])],
            Some(&Discount::fixed(m(501))),
            &excl(),
        )
        .unwrap_err();
        assert_eq!(err, PricingError::DiscountExceedsOrder);
        assert_eq!(err.error_code(), ErrorCode::PricingDiscountExceeds);
        // A 100% discount is allowed (free item), and its tax is zero.
        let mut free = line(500, 1000, vec![rate("VAT", 200_000)]);
        free.line_discount = Some(Discount::percent(RatePpm::FULL));
        let c = price_cart(&[free], None, &incl()).unwrap();
        assert_eq!((c.total, c.tax_total), (m(0), m(0)));
    }

    #[test]
    fn invalid_lines() {
        assert!(matches!(
            price_cart(&[line(100, 0, vec![])], None, &excl()),
            Err(PricingError::NonPositiveQuantity { .. })
        ));
        assert!(matches!(
            price_cart(&[line(-1, 1000, vec![])], None, &excl()),
            Err(PricingError::Negative { .. })
        ));
        let empty = price_cart(&[], None, &excl()).unwrap();
        assert_eq!(empty.total, m(0));
    }

    #[test]
    fn cash_rounding_to_five() {
        let mut p = excl();
        p.cash_rounding_increment = Some(m(5));
        assert_eq!(cash_rounding(m(102), &p).unwrap(), m(-2));
        assert_eq!(cash_rounding(m(103), &p).unwrap(), m(2));
        assert_eq!(cash_rounding(m(105), &p).unwrap(), m(0));
        assert_eq!(cash_rounding(m(102), &excl()).unwrap(), m(0));
    }

    fn arb_line() -> impl Strategy<Value = PricingLine> {
        (
            0i64..100_000,
            1i64..20_000,
            0u32..300_000,
            any::<bool>(),
            0u32..1_000_000,
        )
            .prop_map(|(price, qty, r, disc, d)| {
                let mut l = line(
                    price,
                    qty,
                    if r == 0 {
                        vec![]
                    } else {
                        vec![rate("T", r), rate("U", r / 3)]
                    },
                );
                if disc {
                    l.line_discount = Some(Discount::percent(RatePpm::new(d).unwrap()));
                }
                l
            })
    }

    proptest! {
        /// Inclusive: the customer pays the shelf price minus discounts; tax never exceeds it.
        #[test]
        fn inclusive_invariants(lines in proptest::collection::vec(arb_line(), 1..8), od in 0u32..500_000) {
            let c = price_cart(&lines, Some(&Discount::percent(RatePpm::new(od).unwrap())), &incl()).unwrap();
            let expected = c.subtotal.minor() - c.line_discount_total.minor() - c.order_discount_total.minor();
            prop_assert_eq!(c.total.minor(), expected);
            for l in &c.lines {
                prop_assert!(l.tax <= l.total && !l.tax.is_negative());
                prop_assert_eq!(l.tax_lines.iter().map(|t| t.tax.minor()).sum::<i64>(), l.tax.minor());
            }
            prop_assert_eq!(c.lines.iter().map(|l| l.total.minor()).sum::<i64>(), c.total.minor());
            prop_assert_eq!(c.tax_breakdown.iter().map(|t| t.tax.minor()).sum::<i64>(), c.tax_total.minor());
        }

        /// Exclusive: total = net + tax, and allocations are exact.
        #[test]
        fn exclusive_invariants(lines in proptest::collection::vec(arb_line(), 1..8), od in 0u32..500_000) {
            let c = price_cart(&lines, Some(&Discount::percent(RatePpm::new(od).unwrap())), &excl()).unwrap();
            let net = c.subtotal.minor() - c.line_discount_total.minor() - c.order_discount_total.minor();
            prop_assert_eq!(c.total.minor(), net + c.tax_total.minor());
            prop_assert_eq!(c.lines.iter().map(|l| l.order_discount_alloc.minor()).sum::<i64>(), c.order_discount_total.minor());
        }
    }
}
