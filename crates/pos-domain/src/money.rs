//! Money (ADR-001): an `i64` count of minor units. The currency is business-scoped context
//! (one currency per business in the MVP), so `Money` does not carry it.
//! All arithmetic is checked and never panics.

use crate::{ArithmeticError, ErrorCode, HasErrorCode};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Money = Money(0);

    pub const fn from_minor(minor: i64) -> Self {
        Self(minor)
    }
    pub const fn minor(self) -> i64 {
        self.0
    }
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }
    pub fn checked_add(self, rhs: Money) -> Result<Money, ArithmeticError> {
        self.0
            .checked_add(rhs.0)
            .map(Money)
            .ok_or(ArithmeticError::Overflow)
    }
    pub fn checked_sub(self, rhs: Money) -> Result<Money, ArithmeticError> {
        self.0
            .checked_sub(rhs.0)
            .map(Money)
            .ok_or(ArithmeticError::Overflow)
    }
    pub fn checked_neg(self) -> Result<Money, ArithmeticError> {
        self.0.checked_neg().map(Money).ok_or(ArithmeticError::Overflow)
    }
    /// Checked sum of an iterator of amounts.
    pub fn checked_sum<I: IntoIterator<Item = Money>>(items: I) -> Result<Money, ArithmeticError> {
        items.into_iter().try_fold(Money::ZERO, Money::checked_add)
    }
}

/// ISO-4217 alphabetic code, upper-case ASCII.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CurrencyCode([u8; 3]);

impl CurrencyCode {
    pub fn new(code: &str) -> Result<Self, CurrencyError> {
        let b = code.as_bytes();
        if b.len() != 3 || !b.iter().all(u8::is_ascii_uppercase) {
            return Err(CurrencyError::InvalidCode(code.to_owned()));
        }
        Ok(Self([b[0], b[1], b[2]]))
    }
    pub fn as_str(&self) -> &str {
        // Invariant: always three ASCII upper-case letters.
        std::str::from_utf8(&self.0).unwrap_or("XXX")
    }
}

impl std::fmt::Display for CurrencyCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for CurrencyCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CurrencyCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        CurrencyCode::new(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CurrencyError {
    #[error("invalid ISO-4217 currency code: {0:?}")]
    InvalidCode(String),
    #[error("unsupported currency: {0}")]
    Unsupported(String),
    #[error("currency exponent must be 0..=4, got {0}")]
    InvalidExponent(u8),
}

impl HasErrorCode for CurrencyError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

/// ISO-4217 minor-unit exponents. Currencies absent from this table are rejected at setup
/// rather than guessed (E04-T01). Extend the table, never accept user-entered exponents.
const ISO_4217: &[(&str, u8)] = &[
    ("AED", 2),
    ("AFN", 2),
    ("ALL", 2),
    ("AMD", 2),
    ("ARS", 2),
    ("AUD", 2),
    ("AZN", 2),
    ("BAM", 2),
    ("BDT", 2),
    ("BGN", 2),
    ("BHD", 3),
    ("BIF", 0),
    ("BND", 2),
    ("BOB", 2),
    ("BRL", 2),
    ("BWP", 2),
    ("BYN", 2),
    ("CAD", 2),
    ("CDF", 2),
    ("CHF", 2),
    ("CLP", 0),
    ("CNY", 2),
    ("COP", 2),
    ("CRC", 2),
    ("CZK", 2),
    ("DJF", 0),
    ("DKK", 2),
    ("DOP", 2),
    ("DZD", 2),
    ("EGP", 2),
    ("ETB", 2),
    ("EUR", 2),
    ("GBP", 2),
    ("GEL", 2),
    ("GHS", 2),
    ("GNF", 0),
    ("GTQ", 2),
    ("HKD", 2),
    ("HNL", 2),
    ("HUF", 2),
    ("IDR", 2),
    ("ILS", 2),
    ("INR", 2),
    ("IQD", 3),
    ("ISK", 0),
    ("JMD", 2),
    ("JOD", 3),
    ("JPY", 0),
    ("KES", 2),
    ("KGS", 2),
    ("KHR", 2),
    ("KMF", 0),
    ("KRW", 0),
    ("KWD", 3),
    ("KZT", 2),
    ("LAK", 2),
    ("LBP", 2),
    ("LKR", 2),
    ("LYD", 3),
    ("MAD", 2),
    ("MDL", 2),
    ("MGA", 2),
    ("MKD", 2),
    ("MMK", 2),
    ("MNT", 2),
    ("MUR", 2),
    ("MWK", 2),
    ("MXN", 2),
    ("MYR", 2),
    ("MZN", 2),
    ("NAD", 2),
    ("NGN", 2),
    ("NOK", 2),
    ("NPR", 2),
    ("NZD", 2),
    ("OMR", 3),
    ("PAB", 2),
    ("PEN", 2),
    ("PGK", 2),
    ("PHP", 2),
    ("PKR", 2),
    ("PLN", 2),
    ("PYG", 0),
    ("QAR", 2),
    ("RON", 2),
    ("RSD", 2),
    ("RWF", 0),
    ("SAR", 2),
    ("SEK", 2),
    ("SGD", 2),
    ("SLE", 2),
    ("SOS", 2),
    ("SYP", 2),
    ("THB", 2),
    ("TND", 3),
    ("TRY", 2),
    ("TTD", 2),
    ("TWD", 2),
    ("TZS", 2),
    ("UAH", 2),
    ("UGX", 0),
    ("USD", 2),
    ("UYU", 2),
    ("UZS", 2),
    ("VES", 2),
    ("VND", 0),
    ("VUV", 0),
    ("XAF", 0),
    ("XCD", 2),
    ("XOF", 0),
    ("XPF", 0),
    ("YER", 2),
    ("ZAR", 2),
    ("ZMW", 2),
    ("ZWG", 2),
];

/// A currency with its minor-unit exponent (0..=4).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Currency {
    pub code: CurrencyCode,
    pub exponent: u8,
}

/// Locale-specific number punctuation. The UI adds currency symbols; the domain only
/// formats the number.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NumberFormat {
    pub decimal: char,
    pub group: Option<char>,
}

impl NumberFormat {
    pub const EN_US: NumberFormat = NumberFormat {
        decimal: '.',
        group: Some(','),
    };
    pub const DE_DE: NumberFormat = NumberFormat {
        decimal: ',',
        group: Some('.'),
    };
    /// Machine format used in CSV exports and IPC display strings: `1234.56`.
    pub const PLAIN: NumberFormat = NumberFormat {
        decimal: '.',
        group: None,
    };
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoneyParseError {
    #[error("not a valid amount: {0:?}")]
    Invalid(String),
    #[error("too many decimal places for this currency: {0:?}")]
    TooManyDecimals(String),
    #[error("amount out of range: {0:?}")]
    OutOfRange(String),
}

impl HasErrorCode for MoneyParseError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

impl Currency {
    pub fn new(code: CurrencyCode, exponent: u8) -> Result<Self, CurrencyError> {
        if exponent > 4 {
            return Err(CurrencyError::InvalidExponent(exponent));
        }
        Ok(Self { code, exponent })
    }

    /// Looks a currency up in the embedded ISO-4217 table.
    pub fn from_iso(code: &str) -> Result<Self, CurrencyError> {
        let c = CurrencyCode::new(code)?;
        ISO_4217
            .iter()
            .find(|(k, _)| *k == c.as_str())
            .map(|&(_, exponent)| Currency { code: c, exponent })
            .ok_or_else(|| CurrencyError::Unsupported(code.to_owned()))
    }

    /// Formats minor units as a localised decimal number, e.g. `-1,234.56`.
    pub fn format(&self, m: Money, nf: NumberFormat) -> String {
        format_minor(m.minor(), self.exponent, nf)
    }

    /// Parses a localised decimal number into minor units. Accepts an optional leading `-`,
    /// optional group separators in the integer part, and at most `exponent` decimals.
    pub fn parse(&self, input: &str, nf: NumberFormat) -> Result<Money, MoneyParseError> {
        parse_minor(input, self.exponent, nf).map(Money::from_minor)
    }
}

fn format_minor(minor: i64, exponent: u8, nf: NumberFormat) -> String {
    let negative = minor < 0;
    // unsigned_abs avoids overflow on i64::MIN.
    let digits = minor.unsigned_abs().to_string();
    let exp = usize::from(exponent);
    let padded = if digits.len() <= exp {
        format!("{}{}", "0".repeat(exp + 1 - digits.len()), digits)
    } else {
        digits
    };
    let (int_part, frac_part) = padded.split_at(padded.len() - exp);

    let mut out = String::with_capacity(padded.len() + padded.len() / 3 + 2);
    if negative {
        out.push('-');
    }
    match nf.group {
        Some(g) => {
            for (i, ch) in int_part.chars().enumerate() {
                if i > 0 && (int_part.len() - i) % 3 == 0 {
                    out.push(g);
                }
                out.push(ch);
            }
        }
        None => out.push_str(int_part),
    }
    if exp > 0 {
        out.push(nf.decimal);
        out.push_str(frac_part);
    }
    out
}

fn parse_minor(input: &str, exponent: u8, nf: NumberFormat) -> Result<i64, MoneyParseError> {
    let invalid = || MoneyParseError::Invalid(input.to_owned());
    let s = input.trim();
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    if body.is_empty() {
        return Err(invalid());
    }
    let (int_raw, frac) = match body.split_once(nf.decimal) {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    if int_raw.is_empty() && frac.is_empty() {
        return Err(invalid());
    }
    // Group separators are only allowed between digits in the integer part.
    let int_digits: String = match nf.group {
        Some(g) => {
            if int_raw.starts_with(g) || int_raw.ends_with(g) || int_raw.contains(&format!("{g}{g}")) {
                return Err(invalid());
            }
            int_raw.chars().filter(|c| *c != g).collect()
        }
        None => int_raw.to_owned(),
    };
    if !int_digits.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(invalid());
    }
    if frac.len() > usize::from(exponent) {
        return Err(MoneyParseError::TooManyDecimals(input.to_owned()));
    }
    let frac_padded = format!("{frac:0<width$}", width = usize::from(exponent));
    let all = format!("{int_digits}{frac_padded}");
    let all = all.trim_start_matches('0');
    let magnitude: i128 = if all.is_empty() {
        0
    } else {
        if all.len() > 20 {
            return Err(MoneyParseError::OutOfRange(input.to_owned()));
        }
        all.parse::<i128>().map_err(|_| invalid())?
    };
    let signed = if negative { -magnitude } else { magnitude };
    i64::try_from(signed).map_err(|_| MoneyParseError::OutOfRange(input.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn usd() -> Currency {
        Currency::from_iso("USD").unwrap()
    }

    #[test]
    fn iso_table_exponents() {
        assert_eq!(Currency::from_iso("JPY").unwrap().exponent, 0);
        assert_eq!(Currency::from_iso("KWD").unwrap().exponent, 3);
        assert_eq!(Currency::from_iso("EUR").unwrap().exponent, 2);
        assert!(matches!(
            Currency::from_iso("usd"),
            Err(CurrencyError::InvalidCode(_))
        ));
        assert!(matches!(
            Currency::from_iso("XYZ"),
            Err(CurrencyError::Unsupported(_))
        ));
        assert!(Currency::new(CurrencyCode::new("USD").unwrap(), 5).is_err());
    }

    #[test]
    fn format_examples() {
        let eur = Currency::from_iso("EUR").unwrap();
        let jpy = Currency::from_iso("JPY").unwrap();
        let kwd = Currency::from_iso("KWD").unwrap();
        assert_eq!(
            usd().format(Money::from_minor(123_456), NumberFormat::EN_US),
            "1,234.56"
        );
        assert_eq!(
            eur.format(Money::from_minor(123_456), NumberFormat::DE_DE),
            "1.234,56"
        );
        assert_eq!(usd().format(Money::from_minor(-5), NumberFormat::EN_US), "-0.05");
        assert_eq!(usd().format(Money::ZERO, NumberFormat::EN_US), "0.00");
        assert_eq!(
            jpy.format(Money::from_minor(1_000_000), NumberFormat::EN_US),
            "1,000,000"
        );
        assert_eq!(kwd.format(Money::from_minor(1_500), NumberFormat::EN_US), "1.500");
        assert_eq!(
            usd().format(Money::from_minor(i64::MIN), NumberFormat::PLAIN),
            "-92233720368547758.08"
        );
    }

    #[test]
    fn parse_examples() {
        let eur = Currency::from_iso("EUR").unwrap();
        assert_eq!(
            usd().parse("1,234.56", NumberFormat::EN_US),
            Ok(Money::from_minor(123_456))
        );
        assert_eq!(
            usd().parse("1234.5", NumberFormat::EN_US),
            Ok(Money::from_minor(123_450))
        );
        assert_eq!(
            usd().parse("-0.05", NumberFormat::EN_US),
            Ok(Money::from_minor(-5))
        );
        assert_eq!(usd().parse(".5", NumberFormat::EN_US), Ok(Money::from_minor(50)));
        assert_eq!(
            eur.parse("1.234,56", NumberFormat::DE_DE),
            Ok(Money::from_minor(123_456))
        );
        assert!(matches!(
            usd().parse("1.234", NumberFormat::EN_US),
            Err(MoneyParseError::TooManyDecimals(_))
        ));
        for bad in ["", "-", "abc", "1,,234", ",123", "12a", "1.2.3", "1e5", "+1"] {
            assert!(
                usd().parse(bad, NumberFormat::EN_US).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(matches!(
            usd().parse("99999999999999999999", NumberFormat::EN_US),
            Err(MoneyParseError::OutOfRange(_))
        ));
    }

    #[test]
    fn checked_arithmetic_reports_overflow() {
        assert_eq!(
            Money::from_minor(i64::MAX).checked_add(Money::from_minor(1)),
            Err(ArithmeticError::Overflow)
        );
        assert_eq!(
            Money::from_minor(i64::MIN).checked_neg(),
            Err(ArithmeticError::Overflow)
        );
        assert_eq!(
            Money::checked_sum([1, 2, 3].map(Money::from_minor)),
            Ok(Money::from_minor(6))
        );
    }

    #[test]
    fn currency_code_serde() {
        let c = Currency::from_iso("GBP").unwrap();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"code":"GBP","exponent":2}"#);
        assert!(serde_json::from_str::<CurrencyCode>("\"gbp\"").is_err());
    }

    proptest! {
        #[test]
        fn add_sub_never_panic(a in any::<i64>(), b in any::<i64>()) {
            let (x, y) = (Money::from_minor(a), Money::from_minor(b));
            let _ = x.checked_add(y);
            let _ = x.checked_sub(y);
            let _ = x.checked_neg();
        }

        #[test]
        fn format_parse_round_trip(minor in any::<i64>(), exponent in 0u8..=4, de in any::<bool>(), grouped in any::<bool>()) {
            let cur = Currency::new(CurrencyCode::new("TST").unwrap(), exponent).unwrap();
            let nf = match (de, grouped) {
                (true, true) => NumberFormat::DE_DE,
                (false, true) => NumberFormat::EN_US,
                (_, false) => NumberFormat::PLAIN,
            };
            let text = cur.format(Money::from_minor(minor), nf);
            prop_assert_eq!(cur.parse(&text, nf), Ok(Money::from_minor(minor)));
        }
    }
}
