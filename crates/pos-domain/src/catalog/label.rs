//! What a weight, count or price carried *in* a barcode does to a cart line (E21-T02,
//! BAR-011). Sources: an in-store scale label (variable-measure rule, [`super::scan`]) or a
//! GS1 element string (AI 310n net weight, 30/37 count, 392n price).
//!
//! Rules:
//! - **Weight** sets the quantity of an item sold by weight (kg or g); the catalogue price per
//!   unit applies. An item sold by the each cannot take a weight label.
//! - **Count** sets the quantity of an item sold in whole units.
//! - **Price** is what the customer pays for the line, exactly. For an item sold by weight the
//!   quantity is derived (price ÷ unit price, to the gram) so stock moves by the weighed amount.
//!   If the catalogue price × that quantity does not round back to the label price, the line
//!   carries the label price as its override, so the receipt always shows the label's amount.
//!   An item sold by the each gets quantity 1 at the label price.
//!
//! No floats: everything is integer arithmetic on minor units and thousandths.

use super::Unit;
use crate::{Money, Quantity};

/// A measure read from a barcode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LabelMeasure {
    /// Kilograms: `value / 10^decimals`.
    WeightKg {
        value: u64,
        decimals: u8,
    },
    Count(u64),
    /// The line price: `value / 10^decimals` in the shop's currency.
    Price {
        value: u64,
        decimals: u8,
    },
}

/// What to put on the cart line.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LabelLine {
    pub quantity: Quantity,
    /// The unit price to use instead of the catalogue's, when the label fixes the amount.
    pub price_override: Option<Money>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LabelError {
    #[error("this barcode carries a weight, but the item is sold by {0}")]
    NotSoldByWeight(&'static str),
    #[error("this barcode carries a count, but the item is sold by {0}")]
    NotSoldByCount(&'static str),
    #[error("the label's amount is zero")]
    Zero,
    #[error("the label's amount is too large")]
    TooLarge,
    #[error("the label price has more decimals than the currency")]
    PricePrecision,
    #[error("the item has no price to derive a weight from")]
    NoUnitPrice,
}

impl crate::HasErrorCode for LabelError {
    fn error_code(&self) -> crate::ErrorCode {
        crate::ErrorCode::ScanInvalid
    }
}

fn pow10(n: u32) -> Result<i128, LabelError> {
    10i128.checked_pow(n).ok_or(LabelError::TooLarge)
}

/// `value / 10^from` rescaled to `to` decimals, rounding half away from zero.
fn rescale(value: u64, from: u8, to: u8) -> Result<i128, LabelError> {
    let v = i128::from(value);
    if to >= from {
        v.checked_mul(pow10(u32::from(to - from))?)
            .ok_or(LabelError::TooLarge)
    } else {
        let d = pow10(u32::from(from - to))?;
        Ok((v + d / 2) / d)
    }
}

fn quantity(milli: i128) -> Result<Quantity, LabelError> {
    if milli <= 0 {
        return Err(LabelError::Zero);
    }
    Ok(Quantity::from_milli(
        i64::try_from(milli).map_err(|_| LabelError::TooLarge)?,
    ))
}

/// `round_half_up(a / b)` for positive `b`.
fn div_round(a: i128, b: i128) -> i128 {
    (a * 2 + b) / (b * 2)
}

/// The line for `measure` on an item sold by `unit` at `unit_price`, in a currency with
/// `exponent` decimals.
pub fn label_line(
    measure: LabelMeasure,
    unit: Unit,
    unit_price: Money,
    exponent: u8,
) -> Result<LabelLine, LabelError> {
    match measure {
        LabelMeasure::WeightKg { value, decimals } => {
            let milli = match unit {
                // thousandths of a kilogram
                Unit::Kg => rescale(value, decimals, 3)?,
                // thousandths of a gram
                Unit::G => rescale(value, decimals, 6)?,
                other => return Err(LabelError::NotSoldByWeight(other.code())),
            };
            Ok(LabelLine {
                quantity: quantity(milli)?,
                price_override: None,
            })
        }
        LabelMeasure::Count(n) => {
            if unit.allows_fraction() {
                return Err(LabelError::NotSoldByCount(unit.code()));
            }
            let milli = i128::from(n).checked_mul(1000).ok_or(LabelError::TooLarge)?;
            Ok(LabelLine {
                quantity: quantity(milli)?,
                price_override: None,
            })
        }
        LabelMeasure::Price { value, decimals } => {
            if decimals > exponent
                && value % u64::try_from(pow10(u32::from(decimals - exponent))?).unwrap_or(1) != 0
            {
                return Err(LabelError::PricePrecision);
            }
            let label = rescale(value, decimals, exponent)?;
            if label <= 0 {
                return Err(LabelError::Zero);
            }
            let label_money = Money::from_minor(i64::try_from(label).map_err(|_| LabelError::TooLarge)?);
            if !unit.allows_fraction() {
                return Ok(LabelLine {
                    quantity: Quantity::ONE,
                    price_override: (label_money != unit_price).then_some(label_money),
                });
            }
            let price = i128::from(unit_price.minor());
            if price <= 0 {
                return Err(LabelError::NoUnitPrice);
            }
            // At least one thousandth: a tiny label on a dear item still moves some stock.
            let milli = div_round(label * 1000, price).max(1);
            let q = quantity(milli)?;
            // Does catalogue price × derived quantity round back to the label amount?
            let total = div_round(price * milli, 1000);
            if total == label {
                return Ok(LabelLine {
                    quantity: q,
                    price_override: None,
                });
            }
            // A unit price that makes this quantity cost exactly the label amount.
            let adjusted = div_round(label * 1000, milli);
            if div_round(adjusted * milli, 1000) == label {
                return Ok(LabelLine {
                    quantity: q,
                    price_override: Some(Money::from_minor(
                        i64::try_from(adjusted).map_err(|_| LabelError::TooLarge)?,
                    )),
                });
            }
            // Fall back to one "label" at the label price: the amount is what matters.
            Ok(LabelLine {
                quantity: Quantity::ONE,
                price_override: Some(label_money),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn m(v: i64) -> Money {
        Money::from_minor(v)
    }

    #[test]
    fn weight_sets_the_quantity_of_weighed_items() {
        let w = LabelMeasure::WeightKg {
            value: 1234,
            decimals: 3,
        };
        assert_eq!(
            label_line(w, Unit::Kg, m(499), 2).unwrap(),
            LabelLine {
                quantity: Quantity::from_milli(1234),
                price_override: None
            }
        );
        assert_eq!(
            label_line(w, Unit::G, m(1), 2).unwrap().quantity,
            Quantity::from_milli(1_234_000)
        );
        // 4 decimals round to grams.
        let fine = LabelMeasure::WeightKg {
            value: 12345,
            decimals: 4,
        };
        assert_eq!(
            label_line(fine, Unit::Kg, m(1), 2).unwrap().quantity,
            Quantity::from_milli(1235)
        );
        assert_eq!(
            label_line(w, Unit::Ea, m(1), 2),
            Err(LabelError::NotSoldByWeight("ea"))
        );
        assert_eq!(
            label_line(
                LabelMeasure::WeightKg {
                    value: 0,
                    decimals: 3
                },
                Unit::Kg,
                m(1),
                2
            ),
            Err(LabelError::Zero)
        );
    }

    #[test]
    fn count_sets_whole_quantities() {
        assert_eq!(
            label_line(LabelMeasure::Count(6), Unit::Ea, m(100), 2)
                .unwrap()
                .quantity,
            Quantity::from_milli(6000)
        );
        assert_eq!(
            label_line(LabelMeasure::Count(6), Unit::Kg, m(100), 2),
            Err(LabelError::NotSoldByCount("kg"))
        );
    }

    #[test]
    fn price_labels_cost_exactly_the_label_amount() {
        // 3.99 at 7.98/kg: exactly 0.500 kg at the catalogue price.
        let p = LabelMeasure::Price {
            value: 399,
            decimals: 2,
        };
        assert_eq!(
            label_line(p, Unit::Kg, m(798), 2).unwrap(),
            LabelLine {
                quantity: Quantity::from_milli(500),
                price_override: None
            }
        );
        // 1.00 at 3.00/kg: 0.333 kg, and 0.999 rounds back to 1.00 at the catalogue price.
        let one = LabelMeasure::Price {
            value: 100,
            decimals: 2,
        };
        let line = label_line(one, Unit::Kg, m(300), 2).unwrap();
        assert_eq!(
            line,
            LabelLine {
                quantity: Quantity::from_milli(333),
                price_override: None
            }
        );
        // 0.07 at 9.99/kg: 0.007 kg × 9.99 = 0.07 exactly (0.06993 → 0.07).
        let tiny = label_line(
            LabelMeasure::Price {
                value: 7,
                decimals: 2,
            },
            Unit::Kg,
            m(999),
            2,
        )
        .unwrap();
        assert_eq!(tiny.quantity, Quantity::from_milli(7));
        // Sold by the each: one at the label price.
        assert_eq!(
            label_line(p, Unit::Ea, m(450), 2).unwrap(),
            LabelLine {
                quantity: Quantity::ONE,
                price_override: Some(m(399))
            }
        );
        assert_eq!(label_line(p, Unit::Ea, m(399), 2).unwrap().price_override, None);
        // A 3-decimal label in a 2-decimal currency only if the last digit is 0.
        assert_eq!(
            label_line(
                LabelMeasure::Price {
                    value: 3990,
                    decimals: 3
                },
                Unit::Ea,
                m(1),
                2
            )
            .unwrap()
            .price_override,
            Some(m(399))
        );
        assert_eq!(
            label_line(
                LabelMeasure::Price {
                    value: 3991,
                    decimals: 3
                },
                Unit::Ea,
                m(1),
                2
            ),
            Err(LabelError::PricePrecision)
        );
        // A 0-decimal currency (JPY): 1250 yen.
        assert_eq!(
            label_line(
                LabelMeasure::Price {
                    value: 1250,
                    decimals: 0
                },
                Unit::Ea,
                m(1000),
                0
            )
            .unwrap()
            .price_override,
            Some(m(1250))
        );
        assert_eq!(label_line(p, Unit::Kg, m(0), 2), Err(LabelError::NoUnitPrice));
    }

    proptest! {
        /// Whatever the label and unit price, the line costs exactly the label amount.
        #[test]
        fn a_price_label_line_always_totals_the_label(label in 1u64..1_000_000, unit in 1i64..1_000_000) {
            let line = label_line(LabelMeasure::Price { value: label, decimals: 2 }, Unit::Kg, m(unit), 2).unwrap();
            let price = i128::from(line.price_override.unwrap_or(m(unit)).minor());
            let total = div_round(price * i128::from(line.quantity.milli()), 1000);
            prop_assert_eq!(total, i128::from(label));
            prop_assert!(line.quantity.is_positive());
        }
    }
}
