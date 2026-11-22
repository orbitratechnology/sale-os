//! Scales (E20-T06, PRD §12.6 (5), CAT-007): items sold by weight take their quantity from
//! the scale on the counter.
//!
//! A weight is exact: a whole number of milligrams, parsed from the scale's decimal text
//! without floating point, so `1.234 kg` is 1,234,000 mg and `0.5 g` is 500 mg. Only a
//! *stable* reading may be sold; a reading still moving, over or under the scale's range is
//! reported as such, and the application refuses it.

use crate::{DeviceError, DeviceHealth};

/// A weight in milligrams (negative when the pan reads below zero, e.g. after a tare).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Weight(i64);

/// The unit a scale reports in. Pounds and ounces are not accepted: they have no exact
/// gram value at the scale's resolution, and shops here weigh in kilograms.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WeightUnit {
    Kg,
    G,
}

impl WeightUnit {
    const fn milligrams(self) -> i64 {
        match self {
            WeightUnit::Kg => 1_000_000,
            WeightUnit::G => 1_000,
        }
    }

    /// Decimals finer than a milligram cannot be held.
    const fn max_decimals(self) -> usize {
        match self {
            WeightUnit::Kg => 6,
            WeightUnit::G => 3,
        }
    }

    pub fn from_symbol(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "kg" => Some(WeightUnit::Kg),
            "g" => Some(WeightUnit::G),
            _ => None,
        }
    }
}

impl Weight {
    pub const ZERO: Weight = Weight(0);

    pub const fn from_milligrams(mg: i64) -> Self {
        Self(mg)
    }

    pub const fn milligrams(self) -> i64 {
        self.0
    }

    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// Parses a decimal such as `1.234`, ` -0.012`, `+0001.230` or `250` in `unit`,
    /// exactly. `None` for anything else (a second point, letters, too many decimals,
    /// out of range).
    pub fn parse(text: &str, unit: WeightUnit) -> Option<Self> {
        let t = text.trim();
        let (negative, digits) = match t.as_bytes().first()? {
            b'-' => (true, t[1..].trim_start()),
            b'+' => (false, t[1..].trim_start()),
            _ => (false, t),
        };
        let (whole, frac) = match digits.split_once('.') {
            Some((w, f)) => (w, f),
            None => (digits, ""),
        };
        if (whole.is_empty() && frac.is_empty())
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
            || frac.len() > unit.max_decimals()
        {
            return None;
        }
        let whole: i64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
        let mut frac_mg: i64 = 0;
        let mut scale = unit.milligrams();
        for b in frac.bytes() {
            scale /= 10;
            frac_mg = frac_mg.checked_add(i64::from(b - b'0').checked_mul(scale)?)?;
        }
        let mg = whole.checked_mul(unit.milligrams())?.checked_add(frac_mg)?;
        Some(Self(if negative { -mg } else { mg }))
    }

    /// The weight in grams as exact decimal text: `1234`, `1234.5`, `-12.25`, `0.001`.
    pub fn grams_text(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let mg = self.0.unsigned_abs();
        let (g, rest) = (mg / 1000, mg % 1000);
        if rest == 0 {
            format!("{sign}{g}")
        } else {
            let frac = format!("{rest:03}");
            format!("{sign}{g}.{}", frac.trim_end_matches('0'))
        }
    }
}

impl std::fmt::Display for Weight {
    /// `1.234 kg` from a kilogram up, else `250 g`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.unsigned_abs() >= 1_000_000 {
            let sign = if self.0 < 0 { "-" } else { "" };
            let mg = self.0.unsigned_abs();
            let kg = format!("{}.{:06}", mg / 1_000_000, mg % 1_000_000);
            let kg = kg.trim_end_matches('0');
            let kg = if kg.ends_with('.') {
                format!("{kg}000")
            } else {
                kg.to_owned()
            };
            // At least three decimals: "1.200 kg".
            let decimals = kg.split_once('.').map_or(0, |(_, d)| d.len());
            let kg = format!("{kg}{}", "0".repeat(3usize.saturating_sub(decimals)));
            write!(f, "{sign}{kg} kg")
        } else {
            write!(f, "{} g", self.grams_text())
        }
    }
}

/// What the scale says now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScaleReading {
    /// Settled: this weight may be sold (if above zero).
    Stable(Weight),
    /// Still moving; the weight when the scale gave one.
    Moving(Option<Weight>),
    /// More than the scale can weigh.
    Overload,
    /// Below the scale's range (e.g. the pan lifted, or a negative weight past the limit).
    Underload,
}

#[async_trait::async_trait]
pub trait ScalePort: Send + Sync {
    /// Which scale, in words ("COM3 @ 9600, Toledo 8217"), for the settings screen.
    fn describe(&self) -> String;

    /// Asks the scale for its current reading (or takes the next one it sends). Bounded in
    /// time: a scale that does not answer is an error, never a wait.
    async fn read(&self) -> Result<ScaleReading, DeviceError>;

    /// `Unknown` until the first reading, then `Ready` or `Offline` with the reason.
    fn health(&self) -> tokio::sync::watch::Receiver<DeviceHealth>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_parse_exactly() {
        let kg = |t: &str| Weight::parse(t, WeightUnit::Kg).map(Weight::milligrams);
        assert_eq!(kg("1.234"), Some(1_234_000));
        assert_eq!(kg("  +0001.230"), Some(1_230_000));
        assert_eq!(kg("-0.012"), Some(-12_000));
        assert_eq!(kg("- 0.012"), Some(-12_000));
        assert_eq!(kg("0.0005"), Some(500));
        assert_eq!(kg(".5"), Some(500_000));
        assert_eq!(kg("2"), Some(2_000_000));
        assert_eq!(kg("0.1234567"), None, "finer than a milligram");
        for bad in [
            "",
            ".",
            "1.2.3",
            "1,234",
            "1.2kg",
            "abc",
            "+",
            "99999999999999999",
        ] {
            assert_eq!(kg(bad), None, "{bad:?}");
        }
        let g = |t: &str| Weight::parse(t, WeightUnit::G).map(Weight::milligrams);
        assert_eq!(g("250"), Some(250_000));
        assert_eq!(g("0.5"), Some(500));
        assert_eq!(g("12.0005"), None);
    }

    #[test]
    fn weights_print_in_grams_and_kilograms() {
        let w = Weight::from_milligrams;
        assert_eq!(w(1_234_000).grams_text(), "1234");
        assert_eq!(w(1_234_500).grams_text(), "1234.5");
        assert_eq!(w(-12_250).grams_text(), "-12.25");
        assert_eq!(w(1).grams_text(), "0.001");
        assert_eq!(w(0).grams_text(), "0");
        assert_eq!(w(1_234_000).to_string(), "1.234 kg");
        assert_eq!(w(1_200_000).to_string(), "1.200 kg");
        assert_eq!(w(2_000_000).to_string(), "2.000 kg");
        assert_eq!(w(1_234_500).to_string(), "1.2345 kg");
        assert_eq!(w(250_000).to_string(), "250 g");
        assert_eq!(w(-12_000).to_string(), "-12 g");
    }
}
