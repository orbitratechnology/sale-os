//! Rates (ADR-003): tax rates and percentage discounts in parts per million.
//! 7.5% = 75_000 ppm; 100% = 1_000_000 ppm.

use crate::{ErrorCode, HasErrorCode};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub const PPM_PER_UNIT: u32 = 1_000_000;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RatePpm(u32);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("rate must be between 0 and 1,000,000 ppm (0–100%), got {0}")]
pub struct RateError(pub u64);

impl HasErrorCode for RateError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

impl RatePpm {
    pub const ZERO: RatePpm = RatePpm(0);
    pub const FULL: RatePpm = RatePpm(PPM_PER_UNIT);

    pub fn new(ppm: u32) -> Result<Self, RateError> {
        if ppm > PPM_PER_UNIT {
            return Err(RateError(u64::from(ppm)));
        }
        Ok(Self(ppm))
    }
    pub const fn ppm(self) -> u32 {
        self.0
    }
    /// The rate as an exact decimal fraction, e.g. 75_000 ppm → 0.075.
    pub fn as_decimal(self) -> Decimal {
        Decimal::from(self.0) / Decimal::from(PPM_PER_UNIT)
    }
}

impl<'de> Deserialize<'de> for RatePpm {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = u32::deserialize(d)?;
        RatePpm::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn bounds() {
        assert!(RatePpm::new(1_000_001).is_err());
        assert_eq!(RatePpm::new(1_000_000).unwrap(), RatePpm::FULL);
        assert!(serde_json::from_str::<RatePpm>("1000001").is_err());
        assert_eq!(serde_json::from_str::<RatePpm>("75000").unwrap().ppm(), 75_000);
    }

    #[test]
    fn exact_decimal() {
        assert_eq!(
            RatePpm::new(75_000).unwrap().as_decimal(),
            Decimal::from_str("0.075").unwrap()
        );
        assert_eq!(
            RatePpm::new(72_500).unwrap().as_decimal(),
            Decimal::from_str("0.0725").unwrap()
        );
        assert_eq!(
            RatePpm::new(1).unwrap().as_decimal(),
            Decimal::from_str("0.000001").unwrap()
        );
    }
}
