//! Quantity (ADR-002): an `i64` count of milli-units (fixed scale 3), so `1.250 kg` is `1250`.

use crate::{ArithmeticError, ErrorCode, HasErrorCode};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Quantity(i64);

pub const QUANTITY_SCALE: u32 = 3;
const MILLI: i64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuantityError {
    #[error("quantity must be greater than zero")]
    NotPositive,
    #[error("this unit does not allow fractional quantities")]
    FractionNotAllowed,
    #[error("not a valid quantity: {0:?}")]
    Invalid(String),
    #[error("at most 3 decimal places are allowed: {0:?}")]
    TooManyDecimals(String),
    #[error("quantity out of range")]
    OutOfRange,
}

impl HasErrorCode for QuantityError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

impl Quantity {
    pub const ZERO: Quantity = Quantity(0);
    pub const ONE: Quantity = Quantity(MILLI);

    pub const fn from_milli(milli: i64) -> Self {
        Self(milli)
    }
    pub fn from_whole(units: i64) -> Result<Self, QuantityError> {
        units
            .checked_mul(MILLI)
            .map(Self)
            .ok_or(QuantityError::OutOfRange)
    }
    pub const fn milli(self) -> i64 {
        self.0
    }
    pub const fn is_whole(self) -> bool {
        self.0 % MILLI == 0
    }
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }
    pub fn checked_add(self, rhs: Quantity) -> Result<Quantity, ArithmeticError> {
        self.0
            .checked_add(rhs.0)
            .map(Quantity)
            .ok_or(ArithmeticError::Overflow)
    }
    pub fn checked_sub(self, rhs: Quantity) -> Result<Quantity, ArithmeticError> {
        self.0
            .checked_sub(rhs.0)
            .map(Quantity)
            .ok_or(ArithmeticError::Overflow)
    }
    pub fn checked_neg(self) -> Result<Quantity, ArithmeticError> {
        self.0
            .checked_neg()
            .map(Quantity)
            .ok_or(ArithmeticError::Overflow)
    }

    /// A sellable/receivable quantity must be positive, and whole for units such as `ea`.
    pub fn validate_for_unit(self, allows_fraction: bool) -> Result<(), QuantityError> {
        if !self.is_positive() {
            return Err(QuantityError::NotPositive);
        }
        if !allows_fraction && !self.is_whole() {
            return Err(QuantityError::FractionNotAllowed);
        }
        Ok(())
    }

    /// Parses a plain decimal such as `"1.25"` or `"-3"` (at most 3 decimals).
    pub fn parse_decimal(input: &str) -> Result<Self, QuantityError> {
        let invalid = || QuantityError::Invalid(input.to_owned());
        let s = input.trim();
        let (negative, body) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (int_part, frac) = body.split_once('.').unwrap_or((body, ""));
        if (int_part.is_empty() && frac.is_empty())
            || !int_part.chars().all(|c| c.is_ascii_digit())
            || !frac.chars().all(|c| c.is_ascii_digit())
        {
            return Err(invalid());
        }
        if frac.len() > QUANTITY_SCALE as usize {
            return Err(QuantityError::TooManyDecimals(input.to_owned()));
        }
        let int_val: i128 = if int_part.is_empty() {
            0
        } else {
            if int_part.trim_start_matches('0').len() > 19 {
                return Err(QuantityError::OutOfRange);
            }
            int_part.parse().map_err(|_| invalid())?
        };
        let frac_val: i128 = format!("{frac:0<3}").parse().map_err(|_| invalid())?;
        let magnitude = int_val * i128::from(MILLI) + frac_val;
        let signed = if negative { -magnitude } else { magnitude };
        i64::try_from(signed)
            .map(Self)
            .map_err(|_| QuantityError::OutOfRange)
    }

    /// Formats as a plain decimal with trailing zeros trimmed: `1250` → `"1.25"`, `2000` → `"2"`.
    pub fn to_decimal_string(self) -> String {
        let negative = self.0 < 0;
        let abs = self.0.unsigned_abs();
        let int_part = abs / MILLI.unsigned_abs();
        let frac = abs % MILLI.unsigned_abs();
        let sign = if negative { "-" } else { "" };
        if frac == 0 {
            format!("{sign}{int_part}")
        } else {
            let f = format!("{frac:03}");
            format!("{sign}{int_part}.{}", f.trim_end_matches('0'))
        }
    }
}

impl std::fmt::Display for Quantity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_decimal_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn unit_validation() {
        let one_and_half = Quantity::parse_decimal("1.5").unwrap();
        assert_eq!(
            one_and_half.validate_for_unit(false),
            Err(QuantityError::FractionNotAllowed)
        );
        assert_eq!(one_and_half.validate_for_unit(true), Ok(()));
        assert_eq!(
            Quantity::ZERO.validate_for_unit(true),
            Err(QuantityError::NotPositive)
        );
        assert_eq!(
            Quantity::from_milli(-1000).validate_for_unit(false),
            Err(QuantityError::NotPositive)
        );
        assert_eq!(Quantity::from_whole(3).unwrap().validate_for_unit(false), Ok(()));
    }

    #[test]
    fn parse_and_format() {
        assert_eq!(Quantity::parse_decimal("0.250").unwrap().milli(), 250);
        assert_eq!(Quantity::parse_decimal("12").unwrap().milli(), 12_000);
        assert_eq!(Quantity::parse_decimal("-1.5").unwrap().milli(), -1_500);
        assert_eq!(Quantity::from_milli(1_250).to_string(), "1.25");
        assert_eq!(Quantity::from_milli(2_000).to_string(), "2");
        assert_eq!(Quantity::from_milli(-5).to_string(), "-0.005");
        assert!(matches!(
            Quantity::parse_decimal("1.2345"),
            Err(QuantityError::TooManyDecimals(_))
        ));
        for bad in ["", ".", "abc", "1,5", "1.2.3", "--1"] {
            assert!(Quantity::parse_decimal(bad).is_err(), "{bad:?}");
        }
    }

    proptest! {
        #[test]
        fn decimal_round_trip(m in any::<i64>()) {
            let q = Quantity::from_milli(m);
            prop_assert_eq!(Quantity::parse_decimal(&q.to_decimal_string()), Ok(q));
        }

        #[test]
        fn arithmetic_never_panics(a in any::<i64>(), b in any::<i64>()) {
            let _ = Quantity::from_milli(a).checked_add(Quantity::from_milli(b));
            let _ = Quantity::from_milli(a).checked_sub(Quantity::from_milli(b));
            let _ = Quantity::from_milli(a).checked_neg();
        }
    }
}
