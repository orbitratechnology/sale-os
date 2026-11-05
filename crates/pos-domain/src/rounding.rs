//! Rounding and allocation (ADR-004).

use crate::{ArithmeticError, Money};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundingMode {
    /// "Commercial" rounding: 2.5 → 3, -2.5 → -3. The documented default (ADR-004).
    HalfAwayFromZero,
    /// Banker's rounding: 2.5 → 2, 3.5 → 4.
    HalfEven,
}

/// Rounds a value expressed in minor units (e.g. `249.75`) to whole minor units.
pub fn round_to_minor(value: Decimal, mode: RoundingMode) -> Result<Money, ArithmeticError> {
    let strategy = match mode {
        RoundingMode::HalfAwayFromZero => RoundingStrategy::MidpointAwayFromZero,
        RoundingMode::HalfEven => RoundingStrategy::MidpointNearestEven,
    };
    value
        .round_dp_with_strategy(0, strategy)
        .to_i64()
        .map(Money::from_minor)
        .ok_or(ArithmeticError::Overflow)
}

/// Splits `total` across parts in proportion to `weights` using the largest-remainder
/// method. The result always sums to `total` exactly.
///
/// - Weights must be non-negative. If they are all zero, the split is equal.
/// - Ties on remainder go to the lowest index (deterministic).
/// - A negative `total` is allocated by magnitude and negated, so `allocate(-t, w) == -allocate(t, w)`.
pub fn allocate(total: Money, weights: &[Money]) -> Result<Vec<Money>, ArithmeticError> {
    if weights.is_empty() {
        return if total.is_zero() {
            Ok(Vec::new())
        } else {
            Err(ArithmeticError::EmptyAllocation)
        };
    }
    if weights.iter().any(|w| w.is_negative()) {
        return Err(ArithmeticError::NegativeWeight);
    }
    let weight_sum: i128 = weights.iter().map(|w| i128::from(w.minor())).sum();
    let effective: Vec<i128> = if weight_sum == 0 {
        vec![1; weights.len()]
    } else {
        weights.iter().map(|w| i128::from(w.minor())).collect()
    };
    let denominator: i128 = if weight_sum == 0 {
        weights.len() as i128
    } else {
        weight_sum
    };

    let magnitude = i128::from(total.minor()).abs();
    let mut parts: Vec<i128> = Vec::with_capacity(effective.len());
    let mut remainders: Vec<(i128, usize)> = Vec::with_capacity(effective.len());
    for (i, w) in effective.iter().enumerate() {
        // magnitude ≤ 2^63 and w ≤ 2^63, so the product fits in i128.
        let share = magnitude * w;
        parts.push(share / denominator);
        remainders.push((share % denominator, i));
    }
    let leftover = magnitude - parts.iter().sum::<i128>();
    // Largest remainder first; ties broken by lowest index.
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for &(_, idx) in remainders
        .iter()
        .take(usize::try_from(leftover).map_err(|_| ArithmeticError::Overflow)?)
    {
        parts[idx] += 1;
    }

    let negative = total.is_negative();
    parts
        .into_iter()
        .map(|p| {
            let signed = if negative { -p } else { p };
            i64::try_from(signed)
                .map(Money::from_minor)
                .map_err(|_| ArithmeticError::Overflow)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn m(v: &[i64]) -> Vec<Money> {
        v.iter().copied().map(Money::from_minor).collect()
    }

    #[test]
    fn rounding_modes() {
        let haz = RoundingMode::HalfAwayFromZero;
        assert_eq!(round_to_minor(d("2.5"), haz), Ok(Money::from_minor(3)));
        assert_eq!(round_to_minor(d("-2.5"), haz), Ok(Money::from_minor(-3)));
        assert_eq!(round_to_minor(d("2.4999"), haz), Ok(Money::from_minor(2)));
        assert_eq!(
            round_to_minor(d("2.5"), RoundingMode::HalfEven),
            Ok(Money::from_minor(2))
        );
        assert_eq!(
            round_to_minor(d("3.5"), RoundingMode::HalfEven),
            Ok(Money::from_minor(4))
        );
        assert_eq!(round_to_minor(d("1e20"), haz), Err(ArithmeticError::Overflow));
    }

    #[test]
    fn allocation_examples() {
        assert_eq!(
            allocate(Money::from_minor(100), &m(&[1, 1, 1])),
            Ok(m(&[34, 33, 33]))
        );
        assert_eq!(
            allocate(Money::from_minor(1000), &m(&[333, 333, 334])),
            Ok(m(&[333, 333, 334]))
        );
        assert_eq!(
            allocate(Money::from_minor(-100), &m(&[1, 1, 1])),
            Ok(m(&[-34, -33, -33]))
        );
        assert_eq!(allocate(Money::from_minor(10), &m(&[0, 0])), Ok(m(&[5, 5])));
        assert_eq!(allocate(Money::from_minor(7), &m(&[0, 5])), Ok(m(&[0, 7])));
        assert_eq!(allocate(Money::ZERO, &[]), Ok(vec![]));
        assert_eq!(
            allocate(Money::from_minor(1), &[]),
            Err(ArithmeticError::EmptyAllocation)
        );
        assert_eq!(
            allocate(Money::from_minor(1), &m(&[1, -1])),
            Err(ArithmeticError::NegativeWeight)
        );
    }

    proptest! {
        #[test]
        fn allocation_sums_exactly(
            total in -1_000_000_000_000i64..1_000_000_000_000,
            weights in proptest::collection::vec(0i64..1_000_000_000, 1..50),
        ) {
            let parts = allocate(Money::from_minor(total), &m(&weights)).unwrap();
            prop_assert_eq!(parts.len(), weights.len());
            prop_assert_eq!(Money::checked_sum(parts.iter().copied()).unwrap(), Money::from_minor(total));
            // No part may differ from its exact proportional share by 1 or more.
            let wsum: i128 = weights.iter().map(|&w| i128::from(w)).sum();
            if wsum > 0 {
                for (p, &w) in parts.iter().zip(&weights) {
                    let exact_num = i128::from(total) * i128::from(w);
                    let diff = (i128::from(p.minor()) * wsum - exact_num).abs();
                    prop_assert!(diff < wsum);
                }
            }
        }

        #[test]
        fn allocation_is_sign_symmetric(total in 0i64..1_000_000_000, weights in proptest::collection::vec(0i64..1000, 1..10)) {
            let pos = allocate(Money::from_minor(total), &m(&weights)).unwrap();
            let neg = allocate(Money::from_minor(-total), &m(&weights)).unwrap();
            for (a, b) in pos.iter().zip(neg) {
                prop_assert_eq!(a.minor(), -b.minor());
            }
        }

        #[test]
        fn rounding_is_symmetric(units in -1_000_000_000i64..1_000_000_000, frac in 0u32..10_000) {
            let v = Decimal::from(units) + Decimal::new(i64::from(frac), 4);
            let a = round_to_minor(v, RoundingMode::HalfAwayFromZero).unwrap();
            let b = round_to_minor(-v, RoundingMode::HalfAwayFromZero).unwrap();
            prop_assert_eq!(a.minor(), -b.minor());
        }
    }
}
