//! Scan decoding: what a scanner delivered → ordered catalogue lookup keys (BAR-001..006).
//!
//! A keyboard-wedge or serial scanner delivers a string. Depending on how the scanner is set up
//! and what was printed on the label, the same product can arrive in many shapes: with an AIM
//! symbology identifier (`]E0…`), with an EAN add-on glued on, as a compressed UPC-E, as an
//! ISBN-10, wrapped in Code 39 start/stop characters, inside a GS1 element string or a GS1
//! Digital Link URL, with a price or weight embedded by an in-store scale, or typed through the
//! wrong keyboard layout. [`decode_scan`] turns the string into:
//!
//! - an ordered, de-duplicated list of [`LookupCandidate`]s. Each `value` equals
//!   `normalize_barcode(<the form a shop would register>, None).value`, so lookup can try them
//!   in order against `product_barcodes.value_normalized` and stop at the first hit. The input
//!   as delivered always comes first (when it is a valid barcode at all), then the forms
//!   derived from it, most specific first;
//! - what the barcode itself carries: GS1 data ([`Gs1Data`]), an embedded price/weight/count
//!   ([`EmbeddedMeasure`]), an EAN add-on;
//! - [`ScanNote`]s for the operator (wrong keyboard layout, failed check digit, …).
//!
//! Everything here is pure and float-free. Variable-measure layouts differ by country and chain,
//! so they are configured as text rules ([`VariableMeasureRule`]).

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use super::barcode::{
    BarcodeError, Symbology, digits, gtin_check_digit, gtin_valid, normalize_barcode, text, upce_to_upca,
};
use crate::{ErrorCode, HasErrorCode};

/// Longest raw input accepted (2D codes carry a few kilobytes at most).
pub const MAX_SCAN_INPUT_LEN: usize = 4096;
/// Most decimals a variable-measure rule may declare.
pub const MAX_RULE_DECIMALS: u8 = 6;
/// Most value digits (V) in a variable-measure pattern; the value then always fits in a `u32`.
pub const MAX_RULE_VALUE_DIGITS: usize = 9;
/// Most prefix digits (P) in a variable-measure pattern.
pub const MAX_RULE_PREFIX_DIGITS: usize = 6;

/// ASCII group separator: how scanners transmit the GS1 FNC1 separator.
const GS: char = '\u{1d}';
const GS_BYTE: u8 = 0x1d;

// ---------------------------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------------------------

/// The symbology the scan most likely came from. Separate from [`Symbology`], which is the
/// stored (DB-constrained) classification of a registered barcode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ScannedSymbology {
    Ean13,
    Ean8,
    UpcA,
    UpcE,
    /// EAN-13/UPC with a 2- or 5-digit add-on.
    Ean13AddOn,
    Itf14,
    Itf,
    Code39,
    Code93,
    Code11,
    Codabar,
    Msi,
    Code128,
    Gs1_128,
    Gs1DataBar,
    Gs1DataMatrix,
    Gs1Qr,
    Gs1DigitalLink,
    Qr,
    DataMatrix,
    Pdf417,
    Aztec,
    Isbn10,
    Issn,
    Internal,
    Unknown,
}

impl From<Symbology> for ScannedSymbology {
    fn from(s: Symbology) -> Self {
        match s {
            Symbology::Ean13 => Self::Ean13,
            Symbology::Ean8 => Self::Ean8,
            Symbology::UpcA => Self::UpcA,
            Symbology::UpcE => Self::UpcE,
            Symbology::Code128 => Self::Code128,
            Symbology::Code39 => Self::Code39,
            Symbology::Itf14 => Self::Itf14,
            Symbology::Qr => Self::Qr,
            Symbology::DataMatrix => Self::DataMatrix,
            Symbology::Internal => Self::Internal,
            Symbology::Unknown => Self::Unknown,
        }
    }
}

/// Why a candidate was produced.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateSource {
    /// The input as delivered (cleaned the way `normalize_barcode` cleans).
    Exact,
    /// The input without its AIM symbology identifier (`]E0`, `]C1`, …).
    AimStripped,
    /// The input re-read as if typed on the US layout the scanner assumed.
    KeyboardLayout { layout: &'static str },
    /// The GTIN (AI 01) of a GS1 element string or Digital Link.
    Gs1Gtin,
    /// The EAN-13/UPC-A without its add-on.
    AddOnBase,
    /// The UPC-A that a UPC-E expands to (or the full UPC-E of a stripped body).
    UpcEExpanded,
    /// The UPC-E that a UPC-A compresses to.
    UpcECompressed,
    /// The Bookland EAN-13 of an ISBN-10.
    Isbn10,
    /// The ISBN-10 of a `978` Bookland EAN-13.
    Isbn10Form,
    /// The `977` EAN-13 of an ISSN.
    Issn,
    /// The ISSN of a `977` EAN-13.
    IssnForm,
    /// A hyphenated or spaced ISBN-13 without its separators.
    SeparatorsStripped,
    /// Code 39 `*…*` or Codabar `A…B` start/stop characters removed.
    StartStopStripped,
    /// A Code 39 mod-43 check character removed.
    CheckCharStripped,
    /// The variable-measure code with its value zeroed (the shop's PLU barcode).
    VariableMeasurePlu,
    /// The bare item digits of a variable-measure code (an internal code).
    VariableMeasureItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct LookupCandidate {
    /// A `normalize_barcode(_, None).value`: compare with `product_barcodes.value_normalized`.
    pub value: String,
    pub via: CandidateSource,
}

/// Something about the scan the operator may need to know.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScanNote {
    /// The scanner types for a US layout but the computer uses another one. The UI can show
    /// how to set the scanner (or Windows) up.
    KeyboardLayoutMismatch { layout: &'static str },
    /// The scanner sent an AIM symbology identifier.
    AimIdentifier { symbology: ScannedSymbology },
    /// A retail-length numeric code failed its GS1 check digit: a mis-scan, a typo, or the
    /// shop's own number.
    CheckDigitFailed,
    /// An EAN/UPC add-on was separated from the main code.
    AddOnSeparated,
    /// GS1 data was read (see [`DecodedScan::gs1`]).
    Gs1Parsed,
    /// A GS1 key (00, 01, 02, 03, 41x) failed its check digit and was not used.
    Gs1CheckDigitFailed { ai: String },
    /// The GS1 data is malformed from this AI on; the elements before it were kept.
    Gs1Invalid { ai: String, problem: Gs1Problem },
    /// A variable-measure rule matched (see [`DecodedScan::measure`]).
    VariableMeasure,
    /// The UPC-A price check digit (X) does not match the embedded price.
    PriceCheckDigitFailed,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Gs1Problem {
    /// The application identifier is not one GS1 defines a length for.
    UnknownAi,
    /// A fixed-length value is cut short.
    Truncated,
    TooLong,
    Empty,
    NotNumeric,
    BadCharacter,
    BadDate,
    BadDecimals,
}

/// One GS1 element: application identifier and value, in scan order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct Gs1Element {
    pub ai: String,
    pub value: String,
    /// GS1 data title (e.g. `BATCH/LOT`); `None` for an AI this table does not know.
    pub title: Option<&'static str>,
}

/// An integer with an implied decimal point: `value / 10^decimals`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ScaledNumber {
    pub value: u32,
    pub decimals: u8,
}

/// A GS1 price (392n / 393n). The amount can be 15 digits, so it stays a digit string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct Gs1Price {
    pub amount: String,
    pub decimals: u8,
    /// ISO 4217 numeric currency code (393n only).
    pub currency: Option<String>,
}

/// GS1 data from an element string (GS1-128, DataBar, DataMatrix, QR) or a Digital Link.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct Gs1Data {
    /// Every element read, known or not, in order.
    pub elements: Vec<Gs1Element>,
    /// `false` when parsing stopped at a malformed element (see the `Gs1Invalid` note).
    pub complete: bool,
    /// AI 00, check digit valid.
    pub sscc: Option<String>,
    /// AI 01 as encoded (14 digits), check digit valid.
    pub gtin: Option<String>,
    /// AI 02, check digit valid.
    pub content_gtin: Option<String>,
    /// AI 10.
    pub batch: Option<String>,
    /// AI 17, `YYMMDD` (`DD` may be `00`: end of month).
    pub expiry: Option<String>,
    /// AI 15, `YYMMDD`.
    pub best_before: Option<String>,
    /// AI 21.
    pub serial: Option<String>,
    /// AI 30 or 37.
    pub count: Option<u32>,
    /// AI 310n.
    pub net_weight_kg: Option<ScaledNumber>,
    /// AI 392n or 393n.
    pub price: Option<Gs1Price>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum MeasureKind {
    /// In the currency's units: `value / 10^decimals`.
    Price,
    /// In kilograms.
    Weight,
    Count,
}

/// A price, weight or count embedded in an in-store (variable-measure) barcode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedMeasure {
    pub kind: MeasureKind,
    pub value: u32,
    pub decimals: u8,
    /// The item digits (I) of the code.
    pub item: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DecodedScan {
    pub symbology: ScannedSymbology,
    /// Lookup keys, most specific first, de-duplicated. Never empty.
    pub candidates: Vec<LookupCandidate>,
    pub gs1: Option<Gs1Data>,
    pub measure: Option<EmbeddedMeasure>,
    /// The 2- or 5-digit EAN/UPC add-on (periodical issue, book price).
    pub add_on: Option<String>,
    pub notes: Vec<ScanNote>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanDecodeConfig {
    /// Tried in order; the first rule that matches wins.
    pub variable_measure: Vec<VariableMeasureRule>,
}

// ---------------------------------------------------------------------------------------------
// Variable-measure rules
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScanRuleError {
    #[error("a rule has four parts separated by spaces: prefixes, pattern, kind and decimals (found {0})")]
    FieldCount(usize),
    #[error("{0:?} is not a prefix: use digits, a range like 22-24, or a comma-separated list of them")]
    BadPrefix(String),
    #[error("prefix {prefix:?} must have {digits} digit(s), one for each P in the pattern")]
    PrefixLength { prefix: String, digits: usize },
    #[error("the prefix range {0:?} goes backwards")]
    PrefixRange(String),
    #[error("the pattern must be 8, 12 or 13 characters long, like the barcode (found {0})")]
    PatternLength(usize),
    #[error(
        "{0:?} is not a pattern letter: use P (prefix), I (item), V (value), X (price check) and C (check)"
    )]
    PatternLetter(char),
    #[error("in the pattern, {0}")]
    PatternLayout(&'static str),
    #[error("{0:?} is not a kind: use price, weight or count")]
    Kind(String),
    #[error("{0:?} is not a number of decimals from 0 to {MAX_RULE_DECIMALS}")]
    Decimals(String),
    #[error("a count rule takes 0 decimals")]
    CountDecimals,
    #[error("line {line}: {error}")]
    Line { line: usize, error: Box<ScanRuleError> },
}

impl HasErrorCode for ScanRuleError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Slot {
    Prefix,
    Item,
    Value,
    PriceCheck,
    Check,
}

impl Slot {
    fn letter(self) -> char {
        match self {
            Slot::Prefix => 'P',
            Slot::Item => 'I',
            Slot::Value => 'V',
            Slot::PriceCheck => 'X',
            Slot::Check => 'C',
        }
    }
}

/// One variable-measure layout, parsed from `<prefixes> <pattern> <kind> <decimals>`:
///
/// - `prefixes`: comma-separated values or ranges, each with as many digits as the pattern has
///   `P`s (`20,22-24`);
/// - `pattern`: one letter per barcode digit (8, 12 or 13): `P` prefix, `I` item, `V` value,
///   `X` UPC price check digit (optional), `C` check digit (last);
/// - `kind`: `price`, `weight` (kg) or `count`;
/// - `decimals`: implied decimals of the value (0 for a count).
///
/// Examples: `21 PPIIIIIVVVVVC price 2`, `20,22-24 PPIIIIIVVVVVC weight 3`,
/// `2 PIIIIIXVVVVC price 2` (US UPC-A type 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariableMeasureRule {
    prefixes: Vec<(u32, u32)>,
    pattern: Vec<Slot>,
    kind: MeasureKind,
    decimals: u8,
}

struct MeasureMatch {
    measure: EmbeddedMeasure,
    plu: Vec<u8>,
    price_check_ok: bool,
}

enum RuleOutcome {
    NoMatch,
    BadCheck,
    Match(MeasureMatch),
}

impl VariableMeasureRule {
    pub fn parse(line: &str) -> Result<Self, ScanRuleError> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [prefixes, pattern, kind, decimals] = fields[..] else {
            return Err(ScanRuleError::FieldCount(fields.len()));
        };
        let pattern = parse_pattern(pattern)?;
        let prefix_len = pattern.iter().take_while(|s| **s == Slot::Prefix).count();
        let prefixes = parse_prefixes(prefixes, prefix_len)?;
        let kind = match kind.to_ascii_lowercase().as_str() {
            "price" => MeasureKind::Price,
            "weight" => MeasureKind::Weight,
            "count" => MeasureKind::Count,
            _ => return Err(ScanRuleError::Kind(kind.to_owned())),
        };
        let decimals = decimals
            .parse::<u8>()
            .ok()
            .filter(|d| *d <= MAX_RULE_DECIMALS && decimals.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| ScanRuleError::Decimals(decimals.to_owned()))?;
        if kind == MeasureKind::Count && decimals != 0 {
            return Err(ScanRuleError::CountDecimals);
        }
        Ok(VariableMeasureRule {
            prefixes,
            pattern,
            kind,
            decimals,
        })
    }

    pub fn kind(&self) -> MeasureKind {
        self.kind
    }

    pub fn decimals(&self) -> u8 {
        self.decimals
    }

    /// Length of the barcodes this rule applies to (8, 12 or 13).
    pub fn code_len(&self) -> usize {
        self.pattern.len()
    }

    fn prefix_len(&self) -> usize {
        self.pattern.iter().take_while(|s| **s == Slot::Prefix).count()
    }

    fn pick(&self, d: &[u8], slot: Slot) -> Vec<u8> {
        self.pattern
            .iter()
            .zip(d)
            .filter(|(s, _)| **s == slot)
            .map(|(_, x)| *x)
            .collect()
    }

    fn try_match(&self, d: &[u8]) -> RuleOutcome {
        if d.len() != self.pattern.len() {
            return RuleOutcome::NoMatch;
        }
        let prefix = number(&d[..self.prefix_len()]);
        if !self.prefixes.iter().any(|(lo, hi)| (*lo..=*hi).contains(&prefix)) {
            return RuleOutcome::NoMatch;
        }
        if !gtin_valid(d) {
            return RuleOutcome::BadCheck;
        }
        let value = self.pick(d, Slot::Value);
        let mut plu = d.to_vec();
        for (digit, slot) in plu.iter_mut().zip(&self.pattern) {
            if matches!(slot, Slot::Value | Slot::PriceCheck) {
                *digit = 0;
            }
        }
        let last = plu.len() - 1;
        plu[last] = gtin_check_digit(&plu[..last]);
        let price_check_ok = match (self.pick(d, Slot::PriceCheck).first(), value.len()) {
            (Some(x), 4) => price_check_4(&value) == *x,
            (Some(x), 5) => price_check_5(&value) == Some(*x),
            _ => true,
        };
        RuleOutcome::Match(MeasureMatch {
            measure: EmbeddedMeasure {
                kind: self.kind,
                value: number(&value),
                decimals: self.decimals,
                item: text(&self.pick(d, Slot::Item)),
            },
            plu,
            price_check_ok,
        })
    }
}

impl FromStr for VariableMeasureRule {
    type Err = ScanRuleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// The rule in its canonical text form (it parses back to the same rule).
impl fmt::Display for VariableMeasureRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self.prefix_len();
        let prefixes: Vec<String> = self
            .prefixes
            .iter()
            .map(|(lo, hi)| {
                if lo == hi {
                    format!("{lo:0width$}")
                } else {
                    format!("{lo:0width$}-{hi:0width$}")
                }
            })
            .collect();
        let pattern: String = self.pattern.iter().map(|s| s.letter()).collect();
        let kind = match self.kind {
            MeasureKind::Price => "price",
            MeasureKind::Weight => "weight",
            MeasureKind::Count => "count",
        };
        write!(f, "{} {pattern} {kind} {}", prefixes.join(","), self.decimals)
    }
}

/// Parses rule lines (from settings). Blank lines and lines starting with `#` are skipped.
/// Errors name the 1-based line.
pub fn variable_measure_rules(lines: &[String]) -> Result<Vec<VariableMeasureRule>, ScanRuleError> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let l = l.trim();
            !l.is_empty() && !l.starts_with('#')
        })
        .map(|(i, l)| {
            VariableMeasureRule::parse(l).map_err(|e| ScanRuleError::Line {
                line: i + 1,
                error: Box::new(e),
            })
        })
        .collect()
}

fn parse_pattern(s: &str) -> Result<Vec<Slot>, ScanRuleError> {
    let len = s.chars().count();
    if !matches!(len, 8 | 12 | 13) {
        return Err(ScanRuleError::PatternLength(len));
    }
    let pattern = s
        .chars()
        .map(|c| match c.to_ascii_uppercase() {
            'P' => Ok(Slot::Prefix),
            'I' => Ok(Slot::Item),
            'V' => Ok(Slot::Value),
            'X' => Ok(Slot::PriceCheck),
            'C' => Ok(Slot::Check),
            _ => Err(ScanRuleError::PatternLetter(c)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let count = |slot| pattern.iter().filter(|s| **s == slot).count();
    let together = |slot| {
        let first = pattern.iter().position(|s| *s == slot);
        let last = pattern.iter().rposition(|s| *s == slot);
        match (first, last) {
            (Some(a), Some(b)) => b - a + 1 == count(slot),
            _ => true,
        }
    };
    let layout = |msg| Err(ScanRuleError::PatternLayout(msg));
    let p = pattern.iter().take_while(|s| **s == Slot::Prefix).count();
    if p == 0 {
        return layout("it must start with the prefix digits (P)");
    }
    if count(Slot::Prefix) != p {
        return layout("all prefix digits (P) must be at the start");
    }
    if p > MAX_RULE_PREFIX_DIGITS {
        return layout("at most 6 prefix digits (P)");
    }
    if count(Slot::Check) != 1 || pattern.last() != Some(&Slot::Check) {
        return layout("it must end with a single check digit (C)");
    }
    if count(Slot::Item) == 0 {
        return layout("it needs item digits (I)");
    }
    if !together(Slot::Item) {
        return layout("the item digits (I) must be together");
    }
    if count(Slot::Value) == 0 {
        return layout("it needs value digits (V)");
    }
    if count(Slot::Value) > MAX_RULE_VALUE_DIGITS {
        return layout("at most 9 value digits (V)");
    }
    if !together(Slot::Value) {
        return layout("the value digits (V) must be together");
    }
    if count(Slot::PriceCheck) > 1 {
        return layout("at most one price check digit (X)");
    }
    Ok(pattern)
}

fn parse_prefixes(s: &str, digits_len: usize) -> Result<Vec<(u32, u32)>, ScanRuleError> {
    let one = |p: &str| -> Result<u32, ScanRuleError> {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ScanRuleError::BadPrefix(p.to_owned()));
        }
        if p.len() != digits_len {
            return Err(ScanRuleError::PrefixLength {
                prefix: p.to_owned(),
                digits: digits_len,
            });
        }
        p.parse().map_err(|_| ScanRuleError::BadPrefix(p.to_owned()))
    };
    s.split(',')
        .map(|item| match item.split_once('-') {
            Some((lo, hi)) => {
                let (lo, hi) = (one(lo)?, one(hi)?);
                if lo > hi {
                    return Err(ScanRuleError::PrefixRange(item.to_owned()));
                }
                Ok((lo, hi))
            }
            None => one(item).map(|v| (v, v)),
        })
        .collect()
}

/// Digits as a number. Callers pass at most 9 digits, which always fit in a `u32`.
fn number(d: &[u8]) -> u32 {
    d.iter().fold(0, |acc, x| acc * 10 + u32::from(*x))
}

// UPC price check digit (GS1 General Specifications, "check digit calculation for price/weight
// fields"): weighting factors 2-, 3, 5+ and 5- applied to each digit.
fn w2_minus(d: u8) -> u8 {
    let p = 2 * d;
    (p % 10 + 10 - p / 10) % 10
}

fn w3(d: u8) -> u8 {
    (3 * d) % 10
}

fn w5_plus(d: u8) -> u8 {
    let p = 5 * d;
    (p % 10 + p / 10) % 10
}

fn w5_minus(d: u8) -> u8 {
    let p = 5 * d;
    (p % 10 + 10 - p / 10) % 10
}

/// Four-digit price: weights 2-, 2-, 3, 5-; the units digit of 3 × the sum.
fn price_check_4(v: &[u8]) -> u8 {
    let sum = u32::from(w2_minus(v[0]))
        + u32::from(w2_minus(v[1]))
        + u32::from(w3(v[2]))
        + u32::from(w5_minus(v[3]));
    ((sum * 3) % 10) as u8
}

/// Five-digit price: weights 5+, 2-, 5-, 5+, 2-; the check digit is the digit whose 5- product
/// takes the sum up to a multiple of ten.
fn price_check_5(v: &[u8]) -> Option<u8> {
    let sum = u32::from(w5_plus(v[0]))
        + u32::from(w2_minus(v[1]))
        + u32::from(w5_minus(v[2]))
        + u32::from(w5_plus(v[3]))
        + u32::from(w2_minus(v[4]));
    let target = ((10 - sum % 10) % 10) as u8;
    (0..10).find(|d| w5_minus(*d) == target)
}

// ---------------------------------------------------------------------------------------------
// GTIN family helpers: UPC-E, ISBN, ISSN
// ---------------------------------------------------------------------------------------------

/// The UPC-A of an 8-digit UPC-E, when the expansion's check digit holds.
fn upce_expansion(e: &[u8]) -> Option<Vec<u8>> {
    if e.len() != 8 {
        return None;
    }
    upce_to_upca(e).filter(|a| gtin_valid(a))
}

/// The 8-digit UPC-E of a 12-digit UPC-A, when it has one.
fn upca_to_upce(a: &[u8]) -> Option<Vec<u8>> {
    if a.len() != 12 || a[0] > 1 {
        return None;
    }
    let bodies = [
        [a[1], a[2], a[8], a[9], a[10], a[3]],
        [a[1], a[2], a[3], a[9], a[10], 3],
        [a[1], a[2], a[3], a[4], a[10], 4],
        [a[1], a[2], a[3], a[4], a[5], a[10]],
    ];
    bodies.into_iter().find_map(|body| {
        let mut e = vec![a[0]];
        e.extend(body);
        e.push(a[11]);
        (upce_to_upca(&e).as_deref() == Some(a)).then_some(e)
    })
}

/// A UPC-E with a computed check digit, from its number system and 6-digit body.
fn upce_with_check(ns: u8, body: &[u8]) -> Vec<u8> {
    let mut e = vec![ns];
    e.extend(body);
    e.push(0);
    if let Some(a) = upce_to_upca(&e) {
        e[7] = gtin_check_digit(&a[..11]);
    }
    e
}

/// Mod-11 check (ISBN-10, ISSN) of `body` with weights `top, top-1, …`; 10 stands for `X`.
fn mod11_check(body: &[u8], top: u32) -> u8 {
    let sum: u32 = body
        .iter()
        .zip((2..=top).rev())
        .map(|(d, w)| u32::from(*d) * w)
        .sum();
    ((11 - sum % 11) % 11) as u8
}

/// Digit values of an ISBN/ISSN string; a final `X` is 10.
fn mod11_values(s: &[u8]) -> Option<Vec<u8>> {
    let last = s.len().checked_sub(1)?;
    s.iter()
        .enumerate()
        .map(|(i, b)| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'X' | b'x' if i == last => Some(10),
            _ => None,
        })
        .collect()
}

fn isbn10_valid(v: &[u8]) -> bool {
    v.len() == 10 && v[..9].iter().all(|d| *d < 10) && mod11_check(&v[..9], 10) == v[9]
}

fn issn_valid(v: &[u8]) -> bool {
    v.len() == 8 && v[..7].iter().all(|d| *d < 10) && mod11_check(&v[..7], 8) == v[7]
}

fn with_gtin_check(mut body: Vec<u8>) -> Vec<u8> {
    body.push(gtin_check_digit(&body));
    body
}

fn isbn10_to_ean(v: &[u8]) -> Vec<u8> {
    let mut e = vec![9, 7, 8];
    e.extend(&v[..9]);
    with_gtin_check(e)
}

fn issn_to_ean(v: &[u8]) -> Vec<u8> {
    let mut e = vec![9, 7, 7];
    e.extend(&v[..7]);
    e.extend([0, 0]);
    with_gtin_check(e)
}

fn mod11_text(body: &[u8], check: u8) -> String {
    let mut s = text(body);
    s.push(if check == 10 {
        'X'
    } else {
        char::from(b'0' + check)
    });
    s
}

// ---------------------------------------------------------------------------------------------
// GS1
// ---------------------------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum AiLen {
    Fixed(usize),
    Var(usize),
}

struct AiDef {
    /// The AI; a trailing `n` (or `nn`) matches any digit.
    ai: &'static str,
    len: AiLen,
    numeric: bool,
    title: &'static str,
}

const fn ai(ai: &'static str, len: AiLen, numeric: bool, title: &'static str) -> AiDef {
    AiDef {
        ai,
        len,
        numeric,
        title,
    }
}

use AiLen::{Fixed, Var};

/// GS1 application identifiers (General Specifications §3.2). First match wins, so specific
/// entries come before wildcard families.
const AI_TABLE: &[AiDef] = &[
    ai("00", Fixed(18), true, "SSCC"),
    ai("01", Fixed(14), true, "GTIN"),
    ai("02", Fixed(14), true, "CONTENT"),
    ai("03", Fixed(14), true, "MTO GTIN"),
    ai("04", Fixed(16), true, "SRIN"),
    ai("10", Var(20), false, "BATCH/LOT"),
    ai("11", Fixed(6), true, "PROD DATE"),
    ai("12", Fixed(6), true, "DUE DATE"),
    ai("13", Fixed(6), true, "PACK DATE"),
    ai("15", Fixed(6), true, "BEST BEFORE or BEST BY"),
    ai("16", Fixed(6), true, "SELL BY"),
    ai("17", Fixed(6), true, "USE BY OR EXPIRY"),
    ai("20", Fixed(2), true, "VARIANT"),
    ai("21", Var(20), false, "SERIAL"),
    ai("22", Var(20), false, "CPV"),
    ai("235", Var(28), false, "TPX"),
    ai("240", Var(30), false, "ADDITIONAL ID"),
    ai("241", Var(30), false, "CUST. PART No."),
    ai("242", Var(6), true, "MTO VARIANT"),
    ai("243", Var(20), false, "PCN"),
    ai("250", Var(30), false, "SECONDARY SERIAL"),
    ai("251", Var(30), false, "REF. TO SOURCE"),
    ai("253", Var(30), false, "GDTI"),
    ai("254", Var(20), false, "GLN EXTENSION COMPONENT"),
    ai("255", Var(25), true, "GCN"),
    ai("30", Var(8), true, "VAR. COUNT"),
    ai("310n", Fixed(6), true, "NET WEIGHT (kg)"),
    ai("311n", Fixed(6), true, "LENGTH (m)"),
    ai("312n", Fixed(6), true, "WIDTH (m)"),
    ai("313n", Fixed(6), true, "HEIGHT (m)"),
    ai("314n", Fixed(6), true, "AREA (m2)"),
    ai("315n", Fixed(6), true, "NET VOLUME (l)"),
    ai("316n", Fixed(6), true, "NET VOLUME (m3)"),
    ai("32nn", Fixed(6), true, "MEASURE (imperial)"),
    ai("33nn", Fixed(6), true, "LOGISTIC MEASURE"),
    ai("34nn", Fixed(6), true, "LOGISTIC MEASURE"),
    ai("35nn", Fixed(6), true, "MEASURE"),
    ai("36nn", Fixed(6), true, "MEASURE"),
    ai("37", Var(8), true, "COUNT"),
    ai("390n", Var(15), true, "AMOUNT"),
    ai("391n", Var(18), true, "AMOUNT"),
    ai("392n", Var(15), true, "PRICE"),
    ai("393n", Var(18), true, "PRICE"),
    ai("394n", Var(4), true, "PRCNT OFF"),
    ai("395n", Var(6), true, "PRICE/UoM"),
    ai("400", Var(30), false, "ORDER NUMBER"),
    ai("401", Var(30), false, "GINC"),
    ai("402", Var(17), true, "GSIN"),
    ai("403", Var(30), false, "ROUTE"),
    ai("41n", Fixed(13), true, "GLN"),
    ai("420", Var(20), false, "SHIP TO POST"),
    ai("421", Var(12), false, "SHIP TO POST"),
    ai("422", Var(3), true, "ORIGIN"),
    ai("423", Var(15), true, "COUNTRY - INITIAL PROCESS."),
    ai("424", Var(3), true, "COUNTRY - PROCESS."),
    ai("425", Var(15), true, "COUNTRY - DISASSEMBLY"),
    ai("426", Var(3), true, "COUNTRY - FULL PROCESS"),
    ai("427", Var(3), false, "ORIGIN SUBDIVISION"),
    ai("7001", Var(13), true, "NSN"),
    ai("7002", Var(30), false, "MEAT CUT"),
    ai("7003", Var(10), true, "EXPIRY TIME"),
    ai("7004", Var(4), true, "ACTIVE POTENCY"),
    ai("7005", Var(12), false, "CATCH AREA"),
    ai("7006", Var(6), true, "FIRST FREEZE DATE"),
    ai("7007", Var(12), true, "HARVEST DATE"),
    ai("7008", Var(3), false, "AQUATIC SPECIES"),
    ai("7009", Var(10), false, "FISHING GEAR TYPE"),
    ai("7010", Var(2), false, "PROD METHOD"),
    ai("8001", Var(14), true, "DIMENSIONS"),
    ai("8002", Var(20), false, "CMT No."),
    ai("8003", Var(30), false, "GRAI"),
    ai("8004", Var(30), false, "GIAI"),
    ai("8005", Var(6), true, "PRICE PER UNIT"),
    ai("8006", Var(18), true, "ITIP"),
    ai("8007", Var(34), false, "IBAN"),
    ai("8008", Var(12), true, "PROD TIME"),
    ai("8012", Var(20), false, "VERSION"),
    ai("8013", Var(25), false, "GMN"),
    ai("8017", Var(18), true, "GSRN - PROVIDER"),
    ai("8018", Var(18), true, "GSRN - RECIPIENT"),
    ai("8020", Var(25), false, "REF No."),
    ai("8200", Var(70), false, "PRODUCT URL"),
    ai("90", Var(30), false, "INTERNAL"),
    ai("9n", Var(90), false, "INTERNAL"),
];

/// Primary keys that may start the path of a GS1 Digital Link URI.
const DL_KEYS: &[&str] = &[
    "00", "01", "253", "255", "401", "402", "414", "417", "8003", "8004", "8006", "8010", "8013", "8017",
    "8018",
];

/// AI length and predefined data length, by the AI's first two digits. GS1 fixes these for
/// every AI, assigned or future, which is what makes element strings parseable.
fn ai_family(ai: &str) -> Option<(usize, Option<usize>)> {
    let b = ai.as_bytes();
    if b.len() < 2 || !b[..2].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n = (b[0] - b'0') * 10 + (b[1] - b'0');
    Some(match n {
        0 => (2, Some(18)),
        1..=3 => (2, Some(14)),
        4 => (2, Some(16)),
        11..=19 => (2, Some(6)),
        20 => (2, Some(2)),
        10 | 21 | 22 | 30 | 37 | 90..=99 => (2, None),
        23..=25 | 40 | 42 | 71 => (3, None),
        41 => (3, Some(13)),
        31..=36 => (4, Some(6)),
        39 | 43 | 70 | 72 | 80..=82 => (4, None),
        _ => return None,
    })
}

struct AiSpec {
    len: AiLen,
    numeric: bool,
    title: Option<&'static str>,
}

fn spec_for(ai: &str) -> AiSpec {
    let known = AI_TABLE
        .iter()
        .find(|d| d.ai.len() == ai.len() && d.ai.bytes().zip(ai.bytes()).all(|(p, c)| p == c || p == b'n'));
    if let Some(d) = known {
        return AiSpec {
            len: d.len,
            numeric: d.numeric,
            title: Some(d.title),
        };
    }
    // Not in the table: the predefined length when its family has one (all numeric), else
    // variable up to the longest GS1 value.
    let fixed = ai_family(ai).and_then(|(len, fixed)| if len == ai.len() { fixed } else { None });
    AiSpec {
        len: fixed.map_or(Var(90), Fixed),
        numeric: fixed.is_some(),
        title: None,
    }
}

fn valid_yymmdd(v: &str) -> bool {
    let b = v.as_bytes();
    let two = |i: usize| (b[i] - b'0') * 10 + (b[i + 1] - b'0');
    b.len() == 6 && b.iter().all(u8::is_ascii_digit) && (1..=12).contains(&two(2)) && two(4) <= 31
}

fn check_value(ai: &str, spec: &AiSpec, v: &str) -> Result<(), Gs1Problem> {
    match spec.len {
        Fixed(n) if v.len() < n => return Err(Gs1Problem::Truncated),
        Var(_) if v.is_empty() => return Err(Gs1Problem::Empty),
        Fixed(n) | Var(n) if v.len() > n => return Err(Gs1Problem::TooLong),
        _ => {}
    }
    if spec.numeric && !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Gs1Problem::NotNumeric);
    }
    if !v.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Gs1Problem::BadCharacter);
    }
    if matches!(ai, "11" | "12" | "13" | "15" | "16" | "17") && !valid_yymmdd(v) {
        return Err(Gs1Problem::BadDate);
    }
    let b = ai.as_bytes();
    if b.len() == 4 && (b'1'..=b'6').contains(&b[1]) && b[0] == b'3' && b[3] > b'5' {
        return Err(Gs1Problem::BadDecimals);
    }
    if matches!(ai.get(..3), Some("391" | "393")) && ai.len() == 4 && v.len() < 4 {
        return Err(Gs1Problem::Truncated);
    }
    Ok(())
}

#[derive(Default)]
struct Gs1Parse {
    elements: Vec<Gs1Element>,
    /// Where and why parsing stopped.
    error: Option<(String, Gs1Problem)>,
}

impl Gs1Parse {
    fn add(&mut self, ai: &str, value: &str) -> bool {
        let spec = spec_for(ai);
        match check_value(ai, &spec, value) {
            Ok(()) => {
                self.elements.push(Gs1Element {
                    ai: ai.to_owned(),
                    value: value.to_owned(),
                    title: spec.title,
                });
                true
            }
            Err(problem) => {
                self.error = Some((ai.to_owned(), problem));
                false
            }
        }
    }
}

/// An element string: AIs and values back to back, variable-length values ended by GS.
fn parse_element_string(s: &str) -> Gs1Parse {
    let mut out = Gs1Parse::default();
    if !s.is_ascii() {
        out.error = Some((String::new(), Gs1Problem::BadCharacter));
        return out;
    }
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == GS_BYTE {
            i += 1;
            continue;
        }
        let head = &s[i..(i + 4).min(b.len())];
        let Some((ai_len, _)) = ai_family(head) else {
            out.error = Some((head[..2.min(head.len())].to_owned(), Gs1Problem::UnknownAi));
            break;
        };
        if i + ai_len > b.len() || !b[i..i + ai_len].iter().all(u8::is_ascii_digit) {
            out.error = Some((head.to_owned(), Gs1Problem::Truncated));
            break;
        }
        let ai = &s[i..i + ai_len];
        i += ai_len;
        let end = match spec_for(ai).len {
            Fixed(n) => (i + n).min(b.len()),
            Var(_) => b.len(),
        };
        let value = s[i..end].split(GS).next().unwrap_or("");
        i += value.len();
        if !out.add(ai, value) {
            break;
        }
    }
    out
}

/// `(ai)` at byte `i`: the AI's byte range.
fn paren_ai(b: &[u8], i: usize) -> Option<(usize, usize)> {
    if b.get(i) != Some(&b'(') {
        return None;
    }
    let n = b[i + 1..]
        .iter()
        .take(5)
        .take_while(|c| c.is_ascii_digit())
        .count();
    ((2..=4).contains(&n) && b.get(i + 1 + n) == Some(&b')')).then_some((i + 1, i + 1 + n))
}

/// The human-readable form: `(01)09501101530003(17)270101(10)AB12`.
fn parse_parenthesised(t: &str) -> Option<Gs1Parse> {
    if !t.is_ascii() {
        return None;
    }
    let s: String = t.chars().filter(|c| *c != GS).collect();
    let b = s.as_bytes();
    let (mut a0, mut a1) = paren_ai(b, 0)?;
    let mut out = Gs1Parse::default();
    loop {
        let start = a1 + 1;
        let next = (start..b.len()).find_map(|k| paren_ai(b, k).map(|r| (k, r)));
        let end = next.map_or(b.len(), |(k, _)| k);
        if !out.add(&s[a0..a1], &s[start..end]) {
            break;
        }
        match next {
            Some((_, r)) => (a0, a1) = r,
            None => break,
        }
    }
    Some(out)
}

fn hex(b: u8) -> Option<u8> {
    char::from(b).to_digit(16).map(|d| d as u8)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|x| hex(*x)),
                b.get(i + 2).and_then(|x| hex(*x)),
            )
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A GS1 Digital Link URI on any domain: `https://example.com/any/prefix/01/<gtin>/10/<lot>?17=…`.
fn parse_digital_link(t: &str) -> Option<Gs1Parse> {
    if !t.is_ascii() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let skip = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return None;
    };
    let rest = t[skip..].split('#').next().unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (_host, path) = path.split_once('/')?;
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let is_ai = |s: &str| (2..=4).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    let start = (0..segs.len()).find(|&i| {
        DL_KEYS.contains(&segs[i])
            && (segs.len() - i).is_multiple_of(2)
            && segs[i..].iter().step_by(2).all(|s| is_ai(s))
    })?;
    let mut pairs: Vec<(String, String)> = segs[start..]
        .chunks(2)
        .map(|c| (c[0].to_owned(), percent_decode(c[1])))
        .collect();
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=')
            && is_ai(k)
        {
            pairs.push((k.to_owned(), percent_decode(v)));
        }
    }
    let mut out = Gs1Parse::default();
    for (ai, mut v) in pairs {
        // Digital Link allows GTIN-8/12/13 in the 01 slot, padded to 14 by resolvers.
        if ai == "01" && matches!(v.len(), 8 | 12 | 13) && v.bytes().all(|b| b.is_ascii_digit()) {
            v = format!("{v:0>14}");
        }
        if !out.add(&ai, &v) {
            break;
        }
    }
    Some(out)
}

/// Without any GS1 signal (no AIM id, no GS, keyboard wedges drop both), a string is read
/// as an element string only when it starts with a check-digit-valid AI 01 and parses
/// completely. What follows the GTIN may be alphanumeric (batch, serial), as on most
/// GS1-128 labels.
fn looks_like_gs1(t: &str) -> bool {
    t.len() >= 16
        && t.is_ascii()
        && t.starts_with("01")
        && digits(&t[..16]).is_some_and(|d| gtin_valid(&d[2..16]))
}

// ---------------------------------------------------------------------------------------------
// Keyboard layouts
// ---------------------------------------------------------------------------------------------

/// What a host layout types for each key a US-layout scanner presses: `(US chars, host chars)`
/// rows of equal length. Dead keys are left out.
struct Layout {
    id: &'static str,
    rows: &'static [(&'static str, &'static str)],
}

const LAYOUTS: &[Layout] = &[
    Layout {
        id: "azerty_fr",
        rows: &[
            ("1234567890-=", "&é\"'(-è_çà)="),
            ("!@#$%^&*()_+", "1234567890°+"),
            ("qwertyuiop]", "azertyuiop$"),
            ("QWERTYUIOP}", "AZERTYUIOP£"),
            ("asdfghjkl;'\\", "qsdfghjklmù*"),
            ("ASDFGHJKL:\"|", "QSDFGHJKLM%µ"),
            ("zxcvbnm,./", "wxcvbn,;:!"),
            ("ZXCVBNM<>?", "WXCVBN?./§"),
            ("` ", "² "),
        ],
    },
    Layout {
        id: "azerty_be",
        rows: &[
            ("1234567890-=", "&é\"'(§è!çà)-"),
            ("!@#$%^&*()_+", "1234567890°_"),
            ("qwertyuiop]", "azertyuiop$"),
            ("QWERTYUIOP}", "AZERTYUIOP*"),
            ("asdfghjkl;'\\", "qsdfghjklmùµ"),
            ("ASDFGHJKL:\"|", "QSDFGHJKLM%£"),
            ("zxcvbnm,./", "wxcvbn,;:="),
            ("ZXCVBNM<>?", "WXCVBN?./+"),
            ("`~ ", "²³ "),
        ],
    },
    Layout {
        id: "qwertz_de",
        rows: &[
            ("1234567890-", "1234567890ß"),
            ("!@#$%^&*()_", "!\"§$%&/()=?"),
            ("qwertyuiop[]", "qwertzuiopü+"),
            ("QWERTYUIOP{}", "QWERTZUIOPÜ*"),
            ("asdfghjkl;'\\", "asdfghjklöä#"),
            ("ASDFGHJKL:\"|", "ASDFGHJKLÖÄ'"),
            ("zxcvbnm,./", "yxcvbnm,.-"),
            ("ZXCVBNM<>?", "YXCVBNM;:_"),
            ("~ ", "° "),
        ],
    },
];

fn unmap(layout: &Layout, c: char) -> Option<char> {
    if c == GS {
        return Some(GS);
    }
    layout.rows.iter().find_map(|(us, host)| {
        let i = host.chars().position(|h| h == c)?;
        us.chars().nth(i)
    })
}

/// For input with non-ASCII characters: every layout that could have typed all of it, with the
/// US text the scanner meant, the most barcode-like reading first.
fn keyboard_remaps(t: &str) -> Vec<(&'static str, String)> {
    if t.is_ascii() {
        return Vec::new();
    }
    let mut found: Vec<(&'static str, String)> = LAYOUTS
        .iter()
        .filter_map(|l| {
            let us: Option<String> = t.chars().map(|c| unmap(l, c)).collect();
            us.map(|us| (l.id, us))
        })
        .collect();
    found.sort_by_key(|(_, us)| std::cmp::Reverse(us.bytes().filter(u8::is_ascii_alphanumeric).count()));
    found
}

// ---------------------------------------------------------------------------------------------
// AIM identifiers, Code 39, Codabar
// ---------------------------------------------------------------------------------------------

/// An ISO/IEC 15424 symbology identifier `]cm` at the start: the symbology and the rest.
fn strip_aim(s: &str) -> Option<(ScannedSymbology, &str)> {
    use ScannedSymbology::*;
    let b = s.as_bytes();
    if b.len() < 3 || b[0] != b']' || !b[2].is_ascii_alphanumeric() {
        return None;
    }
    let sym = match (b[1], b[2]) {
        (b'A', _) => Code39,
        (b'C', b'1') => Gs1_128,
        (b'C', _) => Code128,
        (b'E', b'4') => Ean8,
        (b'E', b'1'..=b'3') => Ean13AddOn,
        (b'E', _) => Ean13,
        (b'F', _) => Codabar,
        (b'G', _) => Code93,
        (b'H', _) => Code11,
        (b'I', _) => Itf,
        (b'M', _) => Msi,
        (b'L', _) => Pdf417,
        (b'Q', b'3') => Gs1Qr,
        (b'Q', _) => Qr,
        (b'd', b'2') => Gs1DataMatrix,
        (b'd', _) => DataMatrix,
        (b'e', _) => Gs1DataBar,
        (b'z', _) => Aztec,
        (b'X', _) => Unknown,
        _ => return None,
    };
    Some((sym, &s[3..]))
}

/// `]E0` covers EAN-13, UPC-A and UPC-E; `]I` covers ITF-14 and other ITF lengths.
fn refine_aim(sym: ScannedSymbology, body: &str) -> ScannedSymbology {
    use ScannedSymbology::*;
    let n = if body.bytes().all(|b| b.is_ascii_digit()) {
        body.len()
    } else {
        0
    };
    match (sym, n) {
        (Ean13, 6..=8) => UpcE,
        (Ean13, 12) => UpcA,
        (Ean13, 13) if body.starts_with('0') => UpcA,
        (Itf, 14) => Itf14,
        (s, _) => s,
    }
}

const CODE39: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-. $/+%";
const CODABAR_DATA: &[u8] = b"0123456789-$:/.+";

fn code39_check(data: &[u8]) -> Option<u8> {
    let sum = data.iter().try_fold(0usize, |acc, c| {
        CODE39.iter().position(|x| x == c).map(|p| acc + p)
    })?;
    Some(CODE39[sum % CODE39.len()])
}

// ---------------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------------

struct Decoder<'a> {
    cfg: &'a ScanDecodeConfig,
    declared: Option<ScannedSymbology>,
    candidates: Vec<LookupCandidate>,
    notes: Vec<ScanNote>,
    gs1: Option<Gs1Data>,
    measure: Option<EmbeddedMeasure>,
    add_on: Option<String>,
    detected: Option<ScannedSymbology>,
}

impl Decoder<'_> {
    /// Adds the stored form of `form`, unless it is invalid or already there.
    fn push(&mut self, form: &str, via: CandidateSource) {
        if let Ok(n) = normalize_barcode(form, None)
            && !self.candidates.iter().any(|c| c.value == n.value)
        {
            self.candidates.push(LookupCandidate { value: n.value, via });
        }
    }

    fn push_digits(&mut self, d: &[u8], via: CandidateSource) {
        self.push(&text(d), via);
    }

    fn note(&mut self, note: ScanNote) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    fn detect(&mut self, sym: ScannedSymbology) {
        self.detected.get_or_insert(sym);
    }

    /// A numeric code and the other registrable forms of the same trade item.
    fn gtin_family(&mut self, d: &[u8], via: CandidateSource) {
        self.push_digits(d, via);
        if d.len() == 8 {
            if self.declared != Some(ScannedSymbology::Ean8)
                && let Some(a) = upce_expansion(d)
            {
                self.push_digits(&a, CandidateSource::UpcEExpanded);
            }
            return;
        }
        if !matches!(d.len(), 12..=14) || !gtin_valid(d) {
            return;
        }
        let Some(c) = normalize_barcode(&text(d), None)
            .ok()
            .and_then(|n| digits(&n.value))
        else {
            return;
        };
        if c.len() != 13 {
            return;
        }
        if c[..5] == [0; 5] {
            // A GTIN-8 written as GTIN-13/14 (as in AI 01): the shop registered the EAN-8.
            self.push_digits(&c[5..], via);
        }
        if c[0] == 0
            && let Some(e) = upca_to_upce(&c[1..])
        {
            self.push_digits(&e, CandidateSource::UpcECompressed);
        }
        if c[..3] == [9, 7, 8] {
            self.push(
                &mod11_text(&c[3..12], mod11_check(&c[3..12], 10)),
                CandidateSource::Isbn10Form,
            );
        }
        if c[..3] == [9, 7, 7] {
            self.push(
                &mod11_text(&c[3..10], mod11_check(&c[3..10], 8)),
                CandidateSource::IssnForm,
            );
        }
    }

    fn derive(&mut self, t: &str) {
        use ScannedSymbology::*;
        if self.try_gs1(t) {
            return;
        }
        let open = matches!(self.declared, None | Some(Unknown | Internal));
        let retail = open
            || matches!(
                self.declared,
                Some(Ean13 | Ean8 | UpcA | UpcE | Ean13AddOn | Itf14 | Itf | Isbn10 | Issn)
            );
        let compact: String = t.chars().filter(|c| *c != GS).collect();
        match digits(&compact) {
            Some(d) if !d.is_empty() => {
                if retail {
                    self.numeric(&d);
                }
            }
            _ if retail => self.retail_text(&compact),
            _ => {}
        }
        let up = compact.to_ascii_uppercase();
        if open || self.declared == Some(Code39) {
            self.code39(&up);
        }
        if open || self.declared == Some(Codabar) {
            self.codabar(&up);
        }
    }

    fn try_gs1(&mut self, t: &str) -> bool {
        use ScannedSymbology::*;
        let declared = self.declared;
        let gs1_declared = matches!(declared, Some(Gs1_128 | Gs1DataBar | Gs1DataMatrix | Gs1Qr));
        let carrier = match declared {
            Some(Qr | Gs1Qr) => Gs1Qr,
            Some(DataMatrix | Gs1DataMatrix) => Gs1DataMatrix,
            Some(Gs1DataBar) => Gs1DataBar,
            _ => Gs1_128,
        };
        let heuristic_ok = gs1_declared
            || matches!(
                declared,
                None | Some(Unknown | Internal | Code128 | Qr | DataMatrix)
            );
        let lower = t.get(..8).map(str::to_ascii_lowercase).unwrap_or_default();
        let (parse, sym) = if lower.starts_with("http://") || lower.starts_with("https://") {
            (parse_digital_link(t), Gs1DigitalLink)
        } else if t.starts_with('(') {
            (parse_parenthesised(t), carrier)
        } else if t.contains(GS) || gs1_declared {
            (Some(parse_element_string(t)), carrier)
        } else if heuristic_ok && looks_like_gs1(t) {
            let p = parse_element_string(t);
            (p.error.is_none().then_some(p), carrier)
        } else {
            (None, carrier)
        };
        let Some(p) = parse else {
            return false;
        };
        if let Some((ai, problem)) = p.error.clone() {
            self.note(ScanNote::Gs1Invalid { ai, problem });
        }
        if p.elements.is_empty() {
            return false;
        }
        self.apply_gs1(p, sym);
        true
    }

    fn apply_gs1(&mut self, p: Gs1Parse, sym: ScannedSymbology) {
        let mut data = Gs1Data {
            complete: p.error.is_none(),
            ..Gs1Data::default()
        };
        for e in &p.elements {
            let (ai, v) = (e.ai.as_str(), e.value.as_str());
            let set = |slot: &mut Option<String>| {
                slot.get_or_insert_with(|| v.to_owned());
            };
            let decimals = ai.as_bytes().get(3).map_or(0, |b| b - b'0');
            let is_key = matches!(ai, "00" | "01" | "02" | "03") || (ai.len() == 3 && ai.starts_with("41"));
            if is_key && !digits(v).is_some_and(|d| gtin_valid(&d)) {
                self.note(ScanNote::Gs1CheckDigitFailed { ai: ai.to_owned() });
                continue;
            }
            match ai {
                "00" => set(&mut data.sscc),
                "01" => set(&mut data.gtin),
                "02" => set(&mut data.content_gtin),
                "10" => set(&mut data.batch),
                "15" => set(&mut data.best_before),
                "17" => set(&mut data.expiry),
                "21" => set(&mut data.serial),
                "30" | "37" => {
                    if data.count.is_none() {
                        data.count = v.parse().ok();
                    }
                }
                _ if ai.len() == 4 && ai.starts_with("310") => {
                    if data.net_weight_kg.is_none() {
                        data.net_weight_kg = v.parse().ok().map(|value| ScaledNumber { value, decimals });
                    }
                }
                _ if ai.len() == 4 && ai.starts_with("392") => {
                    data.price.get_or_insert_with(|| Gs1Price {
                        amount: v.to_owned(),
                        decimals,
                        currency: None,
                    });
                }
                _ if ai.len() == 4 && ai.starts_with("393") => {
                    data.price.get_or_insert_with(|| Gs1Price {
                        amount: v[3..].to_owned(),
                        decimals,
                        currency: Some(v[..3].to_owned()),
                    });
                }
                _ => {}
            }
        }
        data.elements = p.elements;
        self.note(ScanNote::Gs1Parsed);
        if let Some(d) = data.gtin.as_deref().and_then(digits) {
            self.gtin_family(&d, CandidateSource::Gs1Gtin);
        }
        self.gs1 = Some(data);
        self.detect(sym);
    }

    fn numeric(&mut self, d: &[u8]) {
        use ScannedSymbology::*;
        let mut explained = false;
        if let Some((base, add)) = split_add_on(d, self.declared) {
            self.add_on = Some(text(add));
            self.note(ScanNote::AddOnSeparated);
            self.detect(Ean13AddOn);
            self.gtin_family(base, CandidateSource::AddOnBase);
            explained = true;
        }
        self.variable_measure(d);
        self.gtin_family(d, CandidateSource::Exact);
        if self.declared == Some(UpcE) && matches!(d.len(), 6 | 7) {
            self.upce_body(d);
            explained = true;
        }
        if d.len() == 8 && upce_expansion(d).is_some() {
            explained = true;
        }
        if d.len() == 8 && issn_valid(d) {
            self.push_digits(&issn_to_ean(d), CandidateSource::Issn);
            if !explained && !gtin_valid(d) {
                self.detect(Issn);
            }
            explained = true;
        }
        if d.len() == 10 && isbn10_valid(d) {
            self.gtin_family(&isbn10_to_ean(d), CandidateSource::Isbn10);
            self.detect(Isbn10);
            explained = true;
        }
        if matches!(d.len(), 8 | 12 | 13 | 14) && !gtin_valid(d) && !explained {
            self.note(ScanNote::CheckDigitFailed);
        }
    }

    /// A UPC-E sent without number system and/or check digit (6 or 7 digits).
    fn upce_body(&mut self, d: &[u8]) {
        let mut forms = Vec::new();
        if d.len() == 7 {
            let mut e = vec![0];
            e.extend(d);
            if upce_expansion(&e).is_some() {
                forms.push(e);
            }
            if d[0] <= 1 {
                forms.push(upce_with_check(d[0], &d[1..]));
            }
        } else {
            forms.push(upce_with_check(0, d));
        }
        for e in forms {
            if let Some(a) = upce_expansion(&e) {
                self.push_digits(&a, CandidateSource::UpcEExpanded);
                self.push_digits(&e, CandidateSource::UpcEExpanded);
            }
        }
    }

    fn variable_measure(&mut self, d: &[u8]) {
        let cfg = self.cfg;
        // A UPC-A may arrive as 13 digits with a leading 0.
        let mut forms = vec![d];
        if d.len() == 13 && d[0] == 0 {
            forms.push(&d[1..]);
        }
        for rule in &cfg.variable_measure {
            for f in &forms {
                if let RuleOutcome::Match(m) = rule.try_match(f) {
                    self.note(ScanNote::VariableMeasure);
                    if !m.price_check_ok {
                        self.note(ScanNote::PriceCheckDigitFailed);
                    }
                    self.push_digits(&m.plu, CandidateSource::VariableMeasurePlu);
                    let item = m.measure.item.clone();
                    self.push(&item, CandidateSource::VariableMeasureItem);
                    let trimmed = item.trim_start_matches('0');
                    if !trimmed.is_empty() {
                        self.push(trimmed, CandidateSource::VariableMeasureItem);
                    }
                    self.measure = Some(m.measure);
                    return;
                }
            }
        }
    }

    /// Non-numeric retail text: an ISBN-10/ISSN with `X` or hyphens, a hyphenated ISBN-13, or
    /// an EAN/UPC with its add-on after a space.
    fn retail_text(&mut self, t: &str) {
        if !t.is_ascii() {
            return;
        }
        let up = t.to_ascii_uppercase();
        if up
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'X' | b'-' | b' '))
        {
            let compact: Vec<u8> = up.bytes().filter(|b| !matches!(b, b'-' | b' ')).collect();
            if let Some(v) = mod11_values(&compact) {
                if isbn10_valid(&v) {
                    self.gtin_family(&isbn10_to_ean(&v), CandidateSource::Isbn10);
                    self.push(&mod11_text(&v[..9], v[9]), CandidateSource::SeparatorsStripped);
                    self.detect(ScannedSymbology::Isbn10);
                } else if issn_valid(&v) {
                    self.push_digits(&issn_to_ean(&v), CandidateSource::Issn);
                    self.push(&mod11_text(&v[..7], v[7]), CandidateSource::SeparatorsStripped);
                    self.detect(ScannedSymbology::Issn);
                } else if v.len() == 13
                    && v.iter().all(|x| *x < 10)
                    && matches!(v[..3], [9, 7, 8] | [9, 7, 9])
                {
                    self.gtin_family(&v, CandidateSource::SeparatorsStripped);
                }
            }
        }
        if let Some((main, add)) = up.split_once(' ')
            && let (Some(m), Some(a)) = (digits(main), digits(add))
            && matches!(m.len(), 12 | 13)
            && matches!(a.len(), 2 | 5)
            && gtin_valid(&m)
        {
            self.add_on = Some(text(&a));
            self.note(ScanNote::AddOnSeparated);
            self.detect(ScannedSymbology::Ean13AddOn);
            self.gtin_family(&m, CandidateSource::AddOnBase);
        }
    }

    /// Code 39 start/stop `*` and the optional mod-43 check character.
    fn code39(&mut self, up: &str) {
        if !up.is_ascii() {
            return;
        }
        let declared = self.declared == Some(ScannedSymbology::Code39);
        let mut inner = up;
        if up.len() >= 3 && up.starts_with('*') && up.ends_with('*') {
            inner = &up[1..up.len() - 1];
            if !inner.bytes().all(|b| CODE39.contains(&b)) {
                return;
            }
            self.push(inner, CandidateSource::StartStopStripped);
            self.detect(ScannedSymbology::Code39);
        }
        let b = inner.as_bytes();
        // Every code's last character matches its mod-43 check one time in 43, so a purely
        // numeric code is only read this way when the scanner says it is Code 39.
        if b.len() >= 2
            && b.iter().all(|c| CODE39.contains(c))
            && (declared || b.iter().any(|c| !c.is_ascii_digit()))
            && code39_check(&b[..b.len() - 1]) == Some(b[b.len() - 1])
        {
            self.push(&inner[..inner.len() - 1], CandidateSource::CheckCharStripped);
            self.detect(ScannedSymbology::Code39);
        }
    }

    /// Codabar start/stop characters (A–D; also T, N, *, E when the scanner says Codabar).
    fn codabar(&mut self, up: &str) {
        let b = up.as_bytes();
        if b.len() < 3 || !up.is_ascii() {
            return;
        }
        let stops: &[u8] = if self.declared == Some(ScannedSymbology::Codabar) {
            b"ABCDTN*E"
        } else {
            b"ABCD"
        };
        let last = b.len() - 1;
        if stops.contains(&b[0])
            && stops.contains(&b[last])
            && b[1..last].iter().all(|c| CODABAR_DATA.contains(c))
        {
            self.push(&up[1..last], CandidateSource::StartStopStripped);
            self.detect(ScannedSymbology::Codabar);
        }
    }
}

/// An EAN-13/UPC-A (or, with `]E3`, a UPC-E/EAN-8) followed by a 2- or 5-digit add-on.
fn split_add_on(d: &[u8], declared: Option<ScannedSymbology>) -> Option<(&[u8], &[u8])> {
    let e3 = declared == Some(ScannedSymbology::Ean13AddOn);
    let base_len = match d.len() {
        15 | 18 => 13,
        17 => 12,
        14 if e3 || !gtin_valid(d) => 12,
        10 | 13 if e3 => 8,
        _ => return None,
    };
    let (base, add) = d.split_at(base_len);
    let ok = gtin_valid(base) || (base_len == 8 && upce_expansion(base).is_some());
    ok.then_some((base, add))
}

/// The symbology of a scan nothing more specific was found for.
fn plain_symbology(t: &str) -> ScannedSymbology {
    match normalize_barcode(t, None) {
        Ok(n)
            if n.symbology == Symbology::Internal
                && digits(&n.value).is_some_and(|d| upce_expansion(&d).is_some()) =>
        {
            ScannedSymbology::UpcE
        }
        Ok(n) => n.symbology.into(),
        Err(_) => ScannedSymbology::Unknown,
    }
}

/// Decodes what a scanner (or the keyboard) delivered into ordered lookup keys plus whatever
/// the barcode carries. `hint` is the symbology when the source knows it; an AIM identifier
/// in the data takes precedence.
///
/// Errors only when nothing usable remains, with the error `normalize_barcode` gives for the
/// input (`SCAN_INVALID`).
pub fn decode_scan(
    raw: &str,
    hint: Option<Symbology>,
    cfg: &ScanDecodeConfig,
) -> Result<DecodedScan, BarcodeError> {
    let cleaned: String = raw.chars().filter(|c| *c == GS || !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Err(BarcodeError::Empty);
    }
    if cleaned.chars().count() > MAX_SCAN_INPUT_LEN {
        return Err(BarcodeError::TooLong);
    }
    let mut dec = Decoder {
        cfg,
        declared: None,
        candidates: Vec::new(),
        notes: Vec::new(),
        gs1: None,
        measure: None,
        add_on: None,
        detected: None,
    };
    dec.push(cleaned, CandidateSource::Exact);

    let mut work = cleaned.to_owned();
    let mut remaps = keyboard_remaps(&work).into_iter();
    if let Some((layout, us)) = remaps.next() {
        dec.note(ScanNote::KeyboardLayoutMismatch { layout });
        dec.push(&us, CandidateSource::KeyboardLayout { layout });
        work = us;
    }

    let mut aim = None;
    if let Some((sym, body)) = strip_aim(&work) {
        let sym = refine_aim(sym, body);
        aim = Some(sym);
        dec.note(ScanNote::AimIdentifier { symbology: sym });
        dec.push(body, CandidateSource::AimStripped);
        work = body.to_owned();
    }
    dec.declared = aim.or(hint.map(ScannedSymbology::from));
    dec.derive(&work);
    for (layout, us) in remaps {
        dec.push(&us, CandidateSource::KeyboardLayout { layout });
    }

    if dec.candidates.is_empty() {
        return Err(normalize_barcode(cleaned, None)
            .err()
            .unwrap_or(BarcodeError::Empty));
    }
    let symbology = dec
        .detected
        .or(dec.declared)
        .unwrap_or_else(|| plain_symbology(&work));
    Ok(DecodedScan {
        symbology,
        candidates: dec.candidates,
        gs1: dec.gs1,
        measure: dec.measure,
        add_on: dec.add_on,
        notes: dec.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn none() -> ScanDecodeConfig {
        ScanDecodeConfig::default()
    }

    fn rules(lines: &[&str]) -> ScanDecodeConfig {
        let lines: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
        ScanDecodeConfig {
            variable_measure: variable_measure_rules(&lines).unwrap(),
        }
    }

    fn decode_with(raw: &str, hint: Option<Symbology>, cfg: &ScanDecodeConfig) -> DecodedScan {
        let d = decode_scan(raw, hint, cfg).unwrap();
        assert_invariants(raw, &d);
        d
    }

    fn decode(raw: &str) -> DecodedScan {
        decode_with(raw, None, &none())
    }

    /// The stored form of what a shop registered (product editor / CSV import pass `None`).
    fn reg(form: &str) -> String {
        normalize_barcode(form, None).unwrap().value
    }

    fn values(d: &DecodedScan) -> Vec<&str> {
        d.candidates.iter().map(|c| c.value.as_str()).collect()
    }

    fn via(d: &DecodedScan, registered: &str) -> CandidateSource {
        let v = reg(registered);
        d.candidates
            .iter()
            .find(|c| c.value == v)
            .unwrap_or_else(|| panic!("{v} not among {:?}", values(d)))
            .via
    }

    /// The input as delivered comes first; candidates are unique fixed points of normalisation.
    fn assert_invariants(raw: &str, d: &DecodedScan) {
        assert!(!d.candidates.is_empty());
        if let Ok(n) = normalize_barcode(raw, None) {
            assert_eq!(d.candidates[0].value, n.value, "{raw:?}");
            assert_eq!(d.candidates[0].via, CandidateSource::Exact, "{raw:?}");
        }
        for (i, c) in d.candidates.iter().enumerate() {
            assert_eq!(normalize_barcode(&c.value, None).unwrap().value, c.value);
            assert!(
                !d.candidates[..i].iter().any(|o| o.value == c.value),
                "duplicate {}",
                c.value
            );
        }
    }

    /// Scanning `raw` finds the product registered as `registered`.
    fn finds(raw: &str, registered: &str) -> DecodedScan {
        let d = decode(raw);
        assert!(
            values(&d).contains(&reg(registered).as_str()),
            "{raw:?} → {:?}",
            values(&d)
        );
        d
    }

    fn ean(body: &str) -> String {
        let d = digits(body).unwrap();
        format!("{body}{}", gtin_check_digit(&d))
    }

    // --- UPC-E ------------------------------------------------------------------------------

    #[test]
    fn upc_e_finds_the_registered_upc_a_and_vice_versa() {
        let d = finds("04252614", "042100005264");
        assert_eq!(via(&d, "042100005264"), CandidateSource::UpcEExpanded);
        assert_eq!(d.symbology, ScannedSymbology::UpcE);
        let d = finds("042100005264", "04252614");
        assert_eq!(via(&d, "04252614"), CandidateSource::UpcECompressed);
        assert_eq!(d.symbology, ScannedSymbology::UpcA);
        // The 13- and 14-digit forms of the UPC-A compress too.
        finds("0042100005264", "04252614");
        finds("00042100005264", "04252614");
        // All four zero-suppression forms round-trip.
        for (upce, upca) in [
            ("0123450", "01200000345"),
            ("0123453", "01230000045"),
            ("0123454", "01234000005"),
            ("0123455", "01234500005"),
        ] {
            let check = gtin_check_digit(&digits(upca).unwrap());
            let (upce, upca) = (format!("{upce}{check}"), format!("{upca}{check}"));
            assert_eq!(via(&finds(&upce, &upca), &upca), CandidateSource::UpcEExpanded);
            assert_eq!(via(&finds(&upca, &upce), &upce), CandidateSource::UpcECompressed);
        }
    }

    #[test]
    fn a_valid_ean_8_stays_ean_8_first_with_its_upc_e_reading_after() {
        // 01234565 is both a valid EAN-8 and a valid UPC-E.
        assert!(gtin_valid(&digits("01234565").unwrap()));
        let d = decode("01234565");
        assert_eq!(d.symbology, ScannedSymbology::Ean8);
        assert_eq!(values(&d), ["01234565", "0012345000065"]);
        // With ]E4 (EAN-8) the UPC-E reading is not offered.
        assert_eq!(values(&decode("]E401234565")), ["]E401234565", "01234565"]);
        // An EAN-8 whose UPC-E reading fails its check digit: no expansion.
        assert_eq!(values(&decode("96385074")), ["96385074"]);
    }

    #[test]
    fn upc_e_bodies_without_number_system_or_check_need_the_scanner_to_say_upc_e() {
        let hint = Some(Symbology::UpcE);
        for raw in ["425261", "0425261", "4252614"] {
            let d = decode_with(raw, hint, &none());
            assert!(values(&d).contains(&reg("042100005264").as_str()), "{raw}");
            assert!(values(&d).contains(&"04252614"), "{raw}");
            assert_eq!(d.symbology, ScannedSymbology::UpcE);
            // Without the hint a 6/7-digit number is just an internal code.
            assert_eq!(values(&decode(raw)), [raw]);
        }
        // ]E0 with 7 digits can only be a UPC-E.
        let d = decode("]E04252614");
        assert!(values(&d).contains(&reg("042100005264").as_str()));
        assert_eq!(d.symbology, ScannedSymbology::UpcE);
    }

    // --- add-ons, ISBN, ISSN ----------------------------------------------------------------

    #[test]
    fn ean_add_ons_are_separated() {
        let d = finds("977123456700303", "9771234567003");
        assert_eq!(values(&d)[..2], ["977123456700303", "9771234567003"]);
        assert_eq!(via(&d, "9771234567003"), CandidateSource::AddOnBase);
        assert_eq!(d.add_on.as_deref(), Some("03"));
        assert_eq!(d.symbology, ScannedSymbology::Ean13AddOn);
        assert!(d.notes.contains(&ScanNote::AddOnSeparated));
        assert!(!d.notes.contains(&ScanNote::CheckDigitFailed));
        // The ISSN of the periodical is offered too.
        finds("977123456700303", "12345679");

        let d = finds("978020137962452499", "9780201379624");
        assert_eq!(d.add_on.as_deref(), Some("52499"));
        finds("978020137962452499", "0201379627");

        // UPC-A + 5 (17 digits), UPC-A + 2 (14 digits that are not a valid GTIN-14).
        let d = finds("03600029145212345", "036000291452");
        assert_eq!(d.add_on.as_deref(), Some("12345"));
        let d = finds("03600029145212", "036000291452");
        assert_eq!(d.add_on.as_deref(), Some("12"));
        // A valid GTIN-14 is not split.
        let d = decode("10012345678902");
        assert_eq!((d.add_on, d.symbology), (None, ScannedSymbology::Itf14));
        // Add-on after a space, and UPC-E + add-on announced by ]E3.
        assert_eq!(
            finds("9771234567003 03", "9771234567003").add_on.as_deref(),
            Some("03")
        );
        let d = decode("]E30425261412");
        assert!(values(&d).contains(&reg("042100005264").as_str()));
        assert_eq!(d.add_on.as_deref(), Some("12"));
        // A 15-digit number whose first 13 digits are no EAN-13 is left alone.
        assert_eq!(decode("400638133393212").add_on, None);
    }

    #[test]
    fn isbn_10_finds_the_bookland_ean_and_vice_versa() {
        let d = finds("0201379627", "9780201379624");
        assert_eq!(via(&d, "9780201379624"), CandidateSource::Isbn10);
        assert_eq!(d.symbology, ScannedSymbology::Isbn10);
        let d = finds("9780201379624", "0201379627");
        assert_eq!(via(&d, "0201379627"), CandidateSource::Isbn10Form);
        // X check digit, lower-case x, hyphens.
        finds("080442957X", &ean("978080442957"));
        finds("080442957x", &ean("978080442957"));
        finds(&ean("978080442957"), "080442957X");
        finds("0-201-37962-7", "9780201379624");
        finds("0-201-37962-7", "0201379627");
        finds("978-0-201-37962-4", "9780201379624");
        // An invalid ISBN-10 is just a number.
        assert_eq!(values(&decode("0201379628")), ["0201379628"]);
    }

    #[test]
    fn issn_finds_the_977_ean() {
        let d = finds("03178471", "9770317847001");
        assert_eq!(via(&d, "9770317847001"), CandidateSource::Issn);
        assert_eq!(d.symbology, ScannedSymbology::Issn);
        finds("0317-8471", "9770317847001");
        let d = finds("2434561X", &ean("977243456100"));
        assert_eq!(d.symbology, ScannedSymbology::Issn);
        finds("2434-561x", &ean("977243456100"));
        finds(&ean("977243456100"), "2434561X");
        finds("9770317847001", "03178471");
    }

    // --- Code 39, Codabar -------------------------------------------------------------------

    #[test]
    fn code_39_start_stop_and_check_character() {
        let d = finds("*ABC-1234*", "ABC-1234");
        assert_eq!(via(&d, "ABC-1234"), CandidateSource::StartStopStripped);
        assert_eq!(d.symbology, ScannedSymbology::Code39);
        finds("*abc-1234*", "ABC-1234");
        let d = finds("ABC-1234-", "ABC-1234");
        assert_eq!(via(&d, "ABC-1234"), CandidateSource::CheckCharStripped);
        assert_eq!(d.symbology, ScannedSymbology::Code39);
        // Both at once.
        finds("*ABC-1234-*", "ABC-1234");
        // A wrong check character is not stripped.
        assert_eq!(values(&decode("ABC-1234A")), ["ABC-1234A"]);
        // A numeric code is only read with a check character when the scanner says Code 39.
        assert_eq!(code39_check(b"00005"), Some(b'5'));
        assert_eq!(values(&decode("000055")), ["000055"]);
        let d = decode_with("000055", Some(Symbology::Code39), &none());
        assert_eq!(values(&d), ["000055", "00005"]);
        // An AIM identifier for another symbology turns the Code 39 reading off.
        assert_eq!(values(&decode("]C0ABC-1234-")), ["]C0ABC-1234-", "ABC-1234-"]);
        assert_eq!(values(&decode("]A0ABC-1234-"))[2], "ABC-1234");
    }

    #[test]
    fn codabar_start_stop_characters_are_stripped() {
        let d = finds("A40156B", "40156");
        assert_eq!(via(&d, "40156"), CandidateSource::StartStopStripped);
        assert_eq!(d.symbology, ScannedSymbology::Codabar);
        finds("c40-15:6d", "40-15:6");
        finds("]F0A40156B", "40156");
        // T/N/*/E start/stop only when the scanner says Codabar.
        assert_eq!(values(&decode("T40156N")), ["T40156N"]);
        assert!(values(&decode("]F0T40156N")).contains(&"40156"));
        // Not Codabar: letters inside.
        assert_eq!(values(&decode("ABAD")), ["ABAD"]);
    }

    // --- AIM identifiers --------------------------------------------------------------------

    #[test]
    fn aim_identifiers_are_stripped_and_name_the_symbology() {
        let d = finds("]E04006381333931", "4006381333931");
        assert_eq!(values(&d), ["]E04006381333931", "4006381333931"]);
        assert_eq!(via(&d, "4006381333931"), CandidateSource::AimStripped);
        assert_eq!(d.symbology, ScannedSymbology::Ean13);
        assert!(d.notes.contains(&ScanNote::AimIdentifier {
            symbology: ScannedSymbology::Ean13
        }));
        use ScannedSymbology::*;
        for (raw, sym) in [
            ("]A0ABC", Code39),
            ("]C0ABC", Code128),
            ("]E0036000291452", UpcA),
            ("]E00036000291452", UpcA),
            ("]E496385074", Ean8),
            ("]E3977123456700303", Ean13AddOn),
            ("]F0A123B", Codabar),
            ("]G0ABC", Code93),
            ("]H0123", Code11),
            ("]I010012345678902", Itf14),
            ("]I01234567890", Itf),
            ("]M0123", Msi),
            ("]L0ABC", Pdf417),
            ("]Q1ABC", Qr),
            ("]d1ABC", DataMatrix),
            ("]z0ABC", Aztec),
            ("]X0ABC", Unknown),
            ("]C10109501101530003", Gs1_128),
            ("]e00109501101530003", Gs1DataBar),
            ("]d20109501101530003", Gs1DataMatrix),
            ("]Q30109501101530003", Gs1Qr),
        ] {
            let d = decode(raw);
            assert_eq!(d.symbology, sym, "{raw}");
            assert_eq!(d.candidates[1].value, reg(&raw[3..]), "{raw}");
        }
        // Not an AIM identifier: left as it is.
        assert_eq!(values(&decode("]K0ABC")), ["]K0ABC"]);
        // A hint is used when there is no AIM identifier; the AIM identifier wins otherwise.
        assert_eq!(
            decode_with("ABC", Some(Symbology::Code128), &none()).symbology,
            Code128
        );
        assert_eq!(
            decode_with("]A0ABC", Some(Symbology::Code128), &none()).symbology,
            Code39
        );
    }

    // --- GS1 --------------------------------------------------------------------------------

    const GTIN: &str = "09501101530003";
    const GTIN13: &str = "9501101530003";

    #[test]
    fn gs1_element_string_with_gs_separators() {
        let raw = "01095011015300031727010110AB12\u{1d}21SER";
        let d = finds(raw, GTIN13);
        assert_eq!(via(&d, GTIN13), CandidateSource::Gs1Gtin);
        assert_eq!(d.symbology, ScannedSymbology::Gs1_128);
        assert!(d.notes.contains(&ScanNote::Gs1Parsed));
        let g = d.gs1.unwrap();
        assert!(g.complete);
        assert_eq!(g.gtin.as_deref(), Some(GTIN));
        assert_eq!(g.expiry.as_deref(), Some("270101"));
        assert_eq!(g.batch.as_deref(), Some("AB12"));
        assert_eq!(g.serial.as_deref(), Some("SER"));
        let ais: Vec<&str> = g.elements.iter().map(|e| e.ai.as_str()).collect();
        assert_eq!(ais, ["01", "17", "10", "21"]);
        assert_eq!(g.elements[2].title, Some("BATCH/LOT"));

        // With ]C1, a leading GS (FNC1 sent as GS), and trailing CR/LF.
        let d = finds("]C1\u{1d}0109501101530003\u{1d}10AB12\u{1d}\r\n", GTIN13);
        assert_eq!(d.gs1.unwrap().batch.as_deref(), Some("AB12"));
        // Fixed-length AIs chain without separators; no GS1 signal needed for 01 + fixed.
        let d = finds("010950110153000317270101", GTIN13);
        assert_eq!(d.gs1.unwrap().expiry.as_deref(), Some("270101"));
        assert!(decode("0109501101530003").gs1.is_some());
        // Without a GS1 signal a number that does not parse completely is not GS1.
        assert!(decode("01095011015300031").gs1.is_none());
    }

    #[test]
    fn gs1_parenthesised_and_digital_link() {
        let d = finds("(01)09501101530003(17)270101(10)AB12", GTIN13);
        let g = d.gs1.unwrap();
        assert_eq!(
            (g.expiry.as_deref(), g.batch.as_deref(), g.complete),
            (Some("270101"), Some("AB12"), true)
        );

        let url = "https://id.gs1.org/01/09501101530003/10/AB12/21/12345?17=270101";
        let d = finds(url, GTIN13);
        assert_eq!(via(&d, GTIN13), CandidateSource::Gs1Gtin);
        assert_eq!(d.symbology, ScannedSymbology::Gs1DigitalLink);
        let g = d.gs1.unwrap();
        assert_eq!(g.gtin.as_deref(), Some(GTIN));
        assert_eq!(g.batch.as_deref(), Some("AB12"));
        assert_eq!(g.serial.as_deref(), Some("12345"));
        assert_eq!(g.expiry.as_deref(), Some("270101"));
        // Any domain and path prefix, percent-encoding, a short GTIN, other query keys.
        let d = finds(
            "HTTPS://shop.example/p/x/01/9501101530003/10/A%2FB?linkType=all#top",
            GTIN13,
        );
        let g = d.gs1.unwrap();
        assert_eq!(g.gtin.as_deref(), Some(GTIN));
        assert_eq!(g.batch.as_deref(), Some("A/B"));
        // A URL longer than a barcode can be: the GTIN comes first.
        let d = finds(
            "https://example.com/products/some/long/path/01/09501101530003/10/AB12",
            GTIN13,
        );
        assert_eq!(d.candidates[0].via, CandidateSource::Gs1Gtin);
        // A URL without a GS1 key path is not a Digital Link.
        assert!(decode("https://example.com/").gs1.is_none());
        // With ]Q1 (QR) it is still a Digital Link.
        assert_eq!(
            decode("]Q1https://id.gs1.org/01/09501101530003").symbology,
            ScannedSymbology::Gs1DigitalLink
        );
    }

    #[test]
    fn gs1_bad_check_digit_truncation_and_unknown_ais() {
        // Bad check digit in AI 01: kept as an element, not a GTIN candidate, noted.
        let d = decode("0109501101530004\u{1d}10AB");
        let g = d.gs1.clone().unwrap();
        assert_eq!(g.gtin, None);
        assert_eq!(g.batch.as_deref(), Some("AB"));
        assert!(
            d.notes
                .contains(&ScanNote::Gs1CheckDigitFailed { ai: "01".into() })
        );
        assert!(!values(&d).contains(&"9501101530004"));
        assert!(d.candidates.iter().all(|c| c.via != CandidateSource::Gs1Gtin));

        // A fixed-length AI cut short: the elements before it are kept.
        let d = decode("]C1010950110153000317270");
        let g = d.gs1.clone().unwrap();
        assert!(!g.complete);
        assert_eq!(g.elements.len(), 1);
        assert!(d.notes.contains(&ScanNote::Gs1Invalid {
            ai: "17".into(),
            problem: Gs1Problem::Truncated
        }));
        assert!(values(&d).contains(&GTIN13));
        // A fixed-length AI running into a GS is truncated too.
        assert!(
            decode("]C10109501101530003\u{1d}17\u{1d}")
                .notes
                .contains(&ScanNote::Gs1Invalid {
                    ai: "17".into(),
                    problem: Gs1Problem::Truncated
                })
        );

        // Unknown AIs: 14 (predefined length 6), 4321 (4-digit family, variable).
        let d = decode("]C10109501101530003141231014321XYZ\u{1d}10L1");
        let g = d.gs1.unwrap();
        assert!(g.complete);
        let el: Vec<(&str, &str, Option<&str>)> = g
            .elements
            .iter()
            .map(|e| (e.ai.as_str(), e.value.as_str(), e.title))
            .collect();
        assert_eq!(
            el,
            [
                ("01", GTIN, Some("GTIN")),
                ("14", "123101", None),
                ("4321", "XYZ", None),
                ("10", "L1", Some("BATCH/LOT"))
            ]
        );
        // In the parenthesised form any AI is accepted.
        let g = decode("(01)09501101530003(89)foo(99)ABC").gs1.unwrap();
        assert_eq!(g.elements[1].title, None);
        assert_eq!(g.elements[2].title, Some("INTERNAL"));
        // An AI from no GS1 family stops an element string.
        let d = decode("]C10109501101530003\u{1d}55X");
        assert!(d.notes.contains(&ScanNote::Gs1Invalid {
            ai: "55".into(),
            problem: Gs1Problem::UnknownAi
        }));
        // Malformed values.
        for (raw, ai, problem) in [
            ("(01)0950110153000", "01", Gs1Problem::Truncated),
            ("(01)095011015300031", "01", Gs1Problem::TooLong),
            ("(17)271301", "17", Gs1Problem::BadDate),
            ("(10)", "10", Gs1Problem::Empty),
            ("(3106)001250", "3106", Gs1Problem::BadDecimals),
            ("(30)12A", "30", Gs1Problem::NotNumeric),
            ("(10)ABCDEFGHIJKLMNOPQRSTU", "10", Gs1Problem::TooLong),
        ] {
            let d = decode(raw);
            let note = ScanNote::Gs1Invalid {
                ai: ai.into(),
                problem,
            };
            assert!(d.notes.contains(&note), "{raw}: {:?}", d.notes);
        }
    }

    #[test]
    fn gs1_measures_prices_and_counts_keep_their_decimals() {
        let g = decode("(01)09501101530003(3103)001250(3922)1999(37)12")
            .gs1
            .unwrap();
        assert_eq!(
            g.net_weight_kg,
            Some(ScaledNumber {
                value: 1250,
                decimals: 3
            })
        );
        assert_eq!(
            g.price,
            Some(Gs1Price {
                amount: "1999".into(),
                decimals: 2,
                currency: None
            })
        );
        assert_eq!(g.count, Some(12));
        let g = decode("]C10109501101530003393297812345\u{1d}3012").gs1.unwrap();
        assert_eq!(
            g.price,
            Some(Gs1Price {
                amount: "12345".into(),
                decimals: 2,
                currency: Some("978".into())
            })
        );
        assert_eq!(g.count, Some(12));
        let g = decode("(00)106141411234567897(15)261231(02)09501101530003")
            .gs1
            .unwrap();
        assert_eq!(g.sscc.as_deref(), Some("106141411234567897"));
        assert_eq!(g.best_before.as_deref(), Some("261231"));
        assert_eq!(g.content_gtin.as_deref(), Some(GTIN));
    }

    #[test]
    fn gs1_gtin_folds_like_normalize_barcode() {
        // Indicator 0: the EAN-13 (and an EAN-8 written as GTIN-14 is the EAN-8).
        finds("(01)00000096385074", "96385074");
        finds("(01)00036000291452", "036000291452");
        finds("(01)00042100005264", "04252614");
        // Another indicator is a different trade item (a case): kept as GTIN-14.
        let d = decode("(01)10012345678902");
        assert!(values(&d).contains(&"10012345678902"));
        assert!(!values(&d).contains(&reg("012345678905").as_str()));
        // DataMatrix/QR hints name the GS1 carrier.
        assert_eq!(
            decode_with("0109501101530003\u{1d}10A", Some(Symbology::DataMatrix), &none()).symbology,
            ScannedSymbology::Gs1DataMatrix
        );
    }

    // --- variable measure -------------------------------------------------------------------

    #[test]
    fn variable_measure_weight_and_price() {
        let cfg = rules(&["21 PPIIIIIVVVVVC price 2", "20,22-24 PPIIIIIVVVVVC weight 3"]);
        let d = decode_with("2012345012349", None, &cfg);
        assert_eq!(
            d.measure,
            Some(EmbeddedMeasure {
                kind: MeasureKind::Weight,
                value: 1234,
                decimals: 3,
                item: "12345".into()
            })
        );
        assert_eq!(values(&d), ["2012345012349", "2012345000001", "12345"]);
        let vias: Vec<CandidateSource> = d.candidates.iter().map(|c| c.via).collect();
        assert_eq!(
            vias,
            [
                CandidateSource::Exact,
                CandidateSource::VariableMeasurePlu,
                CandidateSource::VariableMeasureItem
            ]
        );
        assert!(d.notes.contains(&ScanNote::VariableMeasure));
        assert_eq!(d.symbology, ScannedSymbology::Ean13);

        let code = ean("211234500199");
        let d = decode_with(&code, None, &cfg);
        let m = d.measure.clone().unwrap();
        assert_eq!((m.kind, m.value, m.decimals), (MeasureKind::Price, 199, 2));
        assert_eq!(values(&d)[1], ean("211234500000"));

        // Prefix ranges: 22–24 match, 25 and 21-as-weight do not.
        for p in ["22", "23", "24"] {
            let d = decode_with(&ean(&format!("{p}0004201500")), None, &cfg);
            assert_eq!(d.measure.map(|m| m.kind), Some(MeasureKind::Weight), "{p}");
        }
        assert!(decode_with(&ean("250004201500"), None, &cfg).measure.is_none());
        // Without rules the code is just an EAN-13.
        assert!(decode("2012345012349").measure.is_none());
        // Leading zeros of the item are also offered stripped (scale PLU 42).
        let d = decode_with(&ean("200004201500"), None, &cfg);
        assert_eq!(values(&d)[2..], ["00042", "42"]);
    }

    #[test]
    fn variable_measure_bad_check_digit_gives_no_measure() {
        let cfg = rules(&["20 PPIIIIIVVVVVC weight 3"]);
        let d = decode_with("2012345012340", None, &cfg);
        assert!(d.measure.is_none());
        assert!(d.notes.contains(&ScanNote::CheckDigitFailed));
        assert_eq!(values(&d), ["2012345012340"]);
    }

    #[test]
    fn variable_measure_us_upc_a_with_price_check_digit() {
        // GS1 General Specifications worked examples.
        assert_eq!(price_check_4(&[2, 8, 7, 5]), 9);
        assert_eq!(price_check_5(&[1, 4, 6, 8, 5]), Some(6));
        let cfg = rules(&["2 PIIIIIXVVVVC price 2"]);
        let code = ean("21234592875");
        let d = decode_with(&code, None, &cfg);
        let m = d.measure.clone().unwrap();
        assert_eq!((m.value, m.decimals, m.item.as_str()), (2875, 2, "12345"));
        assert!(!d.notes.contains(&ScanNote::PriceCheckDigitFailed));
        assert_eq!(d.symbology, ScannedSymbology::UpcA);
        // The PLU has the price and X zeroed.
        assert_eq!(values(&d)[1], reg(&ean("21234500000")));
        // Sent as 13 digits with a leading 0.
        assert!(decode_with(&format!("0{code}"), None, &cfg).measure.is_some());
        // A wrong X is reported; the measure is still read (the code's check digit holds).
        let d = decode_with(&ean("21234512875"), None, &cfg);
        assert!(d.measure.is_some());
        assert!(d.notes.contains(&ScanNote::PriceCheckDigitFailed));
    }

    #[test]
    fn variable_measure_rules_parse_strictly() {
        let ok = VariableMeasureRule::parse("20,22-24 PPIIIIIVVVVVC weight 3").unwrap();
        assert_eq!(ok.to_string(), "20,22-24 PPIIIIIVVVVVC weight 3");
        assert_eq!(
            (ok.kind(), ok.decimals(), ok.code_len()),
            (MeasureKind::Weight, 3, 13)
        );
        let loose: VariableMeasureRule = " 2  piiiiixvvvvc  PRICE 2 ".parse().unwrap();
        assert_eq!(loose.to_string(), "2 PIIIIIXVVVVC price 2");
        let ean8 = VariableMeasureRule::parse("02 PPIIIVVC count 0").unwrap();
        assert_eq!(ean8.to_string(), "02 PPIIIVVC count 0");
        use ScanRuleError::*;
        let layout = PatternLayout;
        for (line, err) in [
            ("21 PPIIIIIVVVVVC price", FieldCount(3)),
            ("21 PPIIIIIVVVVVC price 2 x", FieldCount(5)),
            ("", FieldCount(0)),
            ("2a PPIIIIIVVVVVC price 2", BadPrefix("2a".into())),
            ("21, PPIIIIIVVVVVC price 2", BadPrefix(String::new())),
            (
                "2 PPIIIIIVVVVVC price 2",
                PrefixLength {
                    prefix: "2".into(),
                    digits: 2,
                },
            ),
            ("24-22 PPIIIIIVVVVVC price 2", PrefixRange("24-22".into())),
            ("21 PPIIIIIVVVVVVC price 2", PatternLength(14)),
            ("21 PPIIIIVVVVC price 2", PatternLength(11)),
            ("21 PPIIIIIVVVVVQ price 2", PatternLetter('Q')),
            (
                "21 IPPIIIIVVVVVC price 2",
                layout("it must start with the prefix digits (P)"),
            ),
            (
                "21 PPIIPIIVVVVVC price 2",
                layout("all prefix digits (P) must be at the start"),
            ),
            (
                "21 PPIIIIIVVVVCV price 2",
                layout("it must end with a single check digit (C)"),
            ),
            (
                "21 PPIIIIICVVVVC price 2",
                layout("it must end with a single check digit (C)"),
            ),
            ("21 PPVVVVVVVVVVC price 2", layout("it needs item digits (I)")),
            ("21 PPIIIIIIIIIIC price 2", layout("it needs value digits (V)")),
            (
                "21 PPIIVIIVVVVVC price 2",
                layout("the item digits (I) must be together"),
            ),
            (
                "21 PPIVVVVVIIIIC price 2",
                layout("the item digits (I) must be together"),
            ),
            (
                "21 PPIIIVVIVVVVC price 2",
                layout("the item digits (I) must be together"),
            ),
            (
                "21 PPIIIIXXVVVVC price 2",
                layout("at most one price check digit (X)"),
            ),
            ("2 PIVVVVVVVVVVC price 2", layout("at most 9 value digits (V)")),
            (
                "1234567 PPPPPPPIVVVVC price 2",
                layout("at most 6 prefix digits (P)"),
            ),
            ("21 PPIIIIIVVVVVC money 2", Kind("money".into())),
            ("21 PPIIIIIVVVVVC price 7", Decimals("7".into())),
            ("21 PPIIIIIVVVVVC price -1", Decimals("-1".into())),
            ("21 PPIIIIIVVVVVC price +1", Decimals("+1".into())),
            ("21 PPIIIIIVVVVVC count 1", CountDecimals),
        ] {
            assert_eq!(VariableMeasureRule::parse(line), Err(err), "{line}");
        }
        let lines: Vec<String> = [
            "# weights",
            "",
            "20 PPIIIIIVVVVVC weight 3",
            "21 PPIIIIIVVVVVC cost 2",
        ]
        .map(String::from)
        .to_vec();
        let err = variable_measure_rules(&lines).unwrap_err();
        assert_eq!(
            err,
            Line {
                line: 4,
                error: Box::new(Kind("cost".into()))
            }
        );
        assert_eq!(
            err.to_string(),
            "line 4: \"cost\" is not a kind: use price, weight or count"
        );
        assert_eq!(err.error_code(), ErrorCode::ValidationFailed);
        assert_eq!(variable_measure_rules(&lines[..3]).unwrap().len(), 1);
    }

    // --- keyboard layouts -------------------------------------------------------------------

    #[test]
    fn layout_tables_are_consistent() {
        for l in LAYOUTS {
            let mut seen = Vec::new();
            for (us, host) in l.rows {
                assert_eq!(us.chars().count(), host.chars().count(), "{} {us}", l.id);
                assert!(us.is_ascii());
                seen.extend(host.chars());
            }
            let n = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), n, "{}: a host character appears twice", l.id);
        }
        let typed = |id: &str, us: &str| -> String {
            let l = LAYOUTS.iter().find(|l| l.id == id).unwrap();
            us.chars()
                .map(|c| {
                    l.rows
                        .iter()
                        .find_map(|(u, h)| u.chars().position(|x| x == c).and_then(|i| h.chars().nth(i)))
                        .unwrap()
                })
                .collect()
        };
        assert_eq!(typed("azerty_fr", "1234567890"), "&é\"'(-è_çà");
        assert_eq!(typed("azerty_be", "1234567890"), "&é\"'(§è!çà");
        assert_eq!(typed("qwertz_de", "ABC-123yz"), "ABCß123zy");
    }

    #[test]
    fn keyboard_layout_mismatch_is_remapped() {
        let d = finds("&é\"'(-è_çà", "1234567890");
        assert_eq!(
            d.candidates[0].via,
            CandidateSource::KeyboardLayout { layout: "azerty_fr" }
        );
        assert!(
            d.notes
                .contains(&ScanNote::KeyboardLayoutMismatch { layout: "azerty_fr" })
        );

        let d = finds("&é\"'(§è!çà", "1234567890");
        assert_eq!(
            d.candidates[0].via,
            CandidateSource::KeyboardLayout { layout: "azerty_be" }
        );
        assert!(
            d.notes
                .contains(&ScanNote::KeyboardLayoutMismatch { layout: "azerty_be" })
        );

        // A whole EAN-13 typed on AZERTY.
        let d = finds("'àà-\"_&\"\"\"ç\"&", "4006381333931");
        assert_eq!(d.symbology, ScannedSymbology::Ean13);
        // Code 39 with letters on AZERTY: Q is A, ')' is '-'.
        finds("QBC)&é\"'", "ABC-1234");
        // An AIM identifier typed on AZERTY (']' is '$', '0' is 'à') is decoded after remapping.
        let d = finds("$Eà'àà-\"_&\"\"\"ç\"&", "4006381333931");
        assert_eq!(d.symbology, ScannedSymbology::Ean13);

        let d = finds("ABCß123", "ABC-123");
        assert!(
            d.notes
                .contains(&ScanNote::KeyboardLayoutMismatch { layout: "qwertz_de" })
        );
        finds("ZYXß1", "YZX-1");

        // Pure ASCII is never remapped, even when it looks like AZERTY output.
        for raw in ["&\"'(", "QBC)123", "ZYX-1"] {
            let d = decode(raw);
            assert_eq!(d.candidates.len(), 1, "{raw}");
            assert!(d.notes.is_empty(), "{raw}");
        }
        // Non-ASCII that no layout produces is still an error.
        assert_eq!(
            decode_scan("ABC€", None, &none()),
            Err(BarcodeError::InvalidCharacters)
        );
    }

    // --- cleaning and errors ----------------------------------------------------------------

    #[test]
    fn input_is_cleaned_and_bounded() {
        let d = finds("\u{2}4006381333931\r\n", "4006381333931");
        assert_eq!(values(&d), ["4006381333931"]);
        assert_eq!(finds("  abc-123 \t", "ABC-123").candidates.len(), 1);
        assert_eq!(
            decode_scan(" \r\n\u{1d} ", None, &none()),
            Err(BarcodeError::Empty)
        );
        assert_eq!(decode_scan("", None, &none()), Err(BarcodeError::Empty));
        // Up to 4096 characters are decoded; longer input is refused.
        let gs1 = format!("]C10109501101530003\u{1d}10{}", "A".repeat(20));
        let long = format!("{gs1}\u{1d}91{}", "9".repeat(MAX_SCAN_INPUT_LEN - gs1.len() - 3));
        assert_eq!(long.chars().count(), MAX_SCAN_INPUT_LEN);
        let d = decode_scan(&long, None, &none()).unwrap();
        assert!(values(&d).contains(&GTIN13));
        assert_eq!(
            decode_scan(&format!("{long}9"), None, &none()),
            Err(BarcodeError::TooLong)
        );
        // Nothing usable: the error normalize_barcode gives.
        assert_eq!(
            decode_scan(&"Z".repeat(65), None, &none()),
            Err(BarcodeError::TooLong)
        );
        let err = decode_scan("\u{1f600}", None, &none()).unwrap_err();
        assert_eq!(err, BarcodeError::InvalidCharacters);
        assert_eq!(err.error_code(), ErrorCode::ScanInvalid);
        assert_eq!(decode("ABC").symbology, ScannedSymbology::Unknown);
        assert_eq!(decode("12345").symbology, ScannedSymbology::Internal);
    }

    #[test]
    fn retail_codes_with_a_bad_check_digit_are_noted() {
        let d = decode("4006381333932");
        assert_eq!(d.symbology, ScannedSymbology::Internal);
        assert!(d.notes.contains(&ScanNote::CheckDigitFailed));
        assert!(decode("4006381333931").notes.is_empty());
        assert!(decode("12345").notes.is_empty());
        let d = decode_with("4006381333932", Some(Symbology::Ean13), &none());
        assert_eq!(d.symbology, ScannedSymbology::Ean13);
        assert!(d.notes.contains(&ScanNote::CheckDigitFailed));
    }

    #[test]
    fn output_serialises_for_the_ui() {
        let json = serde_json::to_value(decode("]E04006381333931")).unwrap();
        assert_eq!(json["symbology"], "ean13");
        assert_eq!(json["candidates"][1]["via"]["kind"], "aim_stripped");
        assert_eq!(json["notes"][0]["kind"], "aim_identifier");
        assert_eq!(json["notes"][0]["symbology"], "ean13");
        assert_eq!(json["addOn"], serde_json::Value::Null);
        let json = serde_json::to_value(decode("&é\"'(-è_çà")).unwrap();
        assert_eq!(json["candidates"][0]["via"]["layout"], "azerty_fr");
        let json = serde_json::to_value(decode("(01)09501101530003(3103)001250")).unwrap();
        assert_eq!(json["gs1"]["netWeightKg"]["decimals"], 3);
        assert_eq!(json["symbology"], "gs1_128");
    }

    proptest! {
        #[test]
        fn never_panics_and_candidates_are_unique_fixed_points(raw in "(?s).{0,300}") {
            if let Ok(d) = decode_scan(&raw, None, &none()) {
                assert_invariants(&raw, &d);
            }
        }

        #[test]
        fn scanner_like_input_never_panics(
            raw in "(\\][A-Za-z][0-9])?[0-9A-Za-z()*%/+.$: \u{1d}-]{0,80}",
            hint in proptest::option::of(proptest::sample::select(vec![
                Symbology::Ean13, Symbology::UpcE, Symbology::Code39, Symbology::Code128, Symbology::Qr,
            ])),
        ) {
            let cfg = rules(&["2 PIIIIIXVVVVC price 2", "20-29 PPIIIIIVVVVVC weight 3", "2 PIIIIVVC count 0"]);
            if let Ok(d) = decode_scan(&raw, hint, &cfg) {
                assert_invariants(&raw, &d);
            }
        }

        #[test]
        fn digit_strings_always_decode(raw in "[0-9]{1,24}", hint in proptest::option::of(Just(Symbology::UpcE))) {
            let cfg = rules(&["2 PIIIIIXVVVVC price 2", "20-29 PPIIIIIVVVVVC weight 3"]);
            let d = decode_scan(&raw, hint, &cfg).unwrap();
            assert_invariants(&raw, &d);
        }

        /// Every UPC-A that compresses expands back to itself.
        #[test]
        fn upc_e_round_trips(body in proptest::collection::vec(0u8..10, 11), ns in 0u8..2) {
            let mut a = body;
            a[0] = ns;
            a.push(gtin_check_digit(&a));
            if let Some(e) = upca_to_upce(&a) {
                prop_assert_eq!(upce_expansion(&e), Some(a));
            }
        }
    }
}
