//! Barcode normalisation and GTIN check digits (E06-T03, CAT-003/004, BAR-004).
//!
//! Every barcode is stored and looked up in one canonical form, so the same product is found
//! however it was scanned or typed:
//!
//! - ASCII control characters (scanner suffixes such as CR/LF/TAB, stray prefixes) are
//!   removed and surrounding spaces trimmed.
//! - Letters are upper-cased. Wedge scanners invert case when Caps Lock is on, and treating
//!   `abc123` and `ABC123` as different products would be a support nightmare. The cost is
//!   that two internal Code 128 codes differing only in case collide; such duplicates are
//!   reported by the uniqueness check rather than silently merged.
//! - The GTIN family (UPC-A, UPC-E, EAN-8, EAN-13, GTIN-14) is validated with the GS1 check
//!   digit. UPC-A, UPC-E and GTIN-14 with indicator `0` are canonicalised to their EAN-13 form,
//!   so `012345678905`, `0012345678905` and `00012345678905` are the same item.
//! - A numeric code that fails its check digit is rejected when the caller says what
//!   symbology it is. Without a hint it is accepted as an `Internal` code with
//!   `check_digit_failed` set, because shops do label goods with their own numbers.

use crate::{ErrorCode, HasErrorCode};
use serde::{Deserialize, Serialize};

pub const MAX_BARCODE_LEN: usize = 64;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Symbology {
    Ean13,
    Ean8,
    UpcA,
    UpcE,
    Code128,
    Code39,
    Itf14,
    Qr,
    DataMatrix,
    /// The shop's own numbering, or a numeric code that failed its GTIN check digit.
    Internal,
    Unknown,
}

impl Symbology {
    pub fn from_code(code: &str) -> Option<Symbology> {
        use Symbology::*;
        [
            Ean13, Ean8, UpcA, UpcE, Code128, Code39, Itf14, Qr, DataMatrix, Internal, Unknown,
        ]
        .into_iter()
        .find(|s| s.as_str() == code)
    }

    /// The value stored in `product_barcodes.symbology` (docs/backlog/04, 0003).
    pub const fn as_str(self) -> &'static str {
        match self {
            Symbology::Ean13 => "ean13",
            Symbology::Ean8 => "ean8",
            Symbology::UpcA => "upca",
            Symbology::UpcE => "upce",
            Symbology::Code128 => "code128",
            Symbology::Code39 => "code39",
            Symbology::Itf14 => "itf14",
            Symbology::Qr => "qr",
            Symbology::DataMatrix => "datamatrix",
            Symbology::Internal => "internal",
            Symbology::Unknown => "unknown",
        }
    }
}

/// A barcode in canonical form, ready for storage and lookup.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct NormalizedBarcode {
    pub value: String,
    pub symbology: Symbology,
    /// Numeric code accepted as `Internal` although it failed the GTIN check digit (no hint
    /// given). The UI can warn: it may be a mis-scan or a mistyped retail barcode.
    pub check_digit_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BarcodeError {
    #[error("the barcode is empty")]
    Empty,
    #[error("the barcode is longer than {MAX_BARCODE_LEN} characters")]
    TooLong,
    #[error("the barcode contains characters that are not printable ASCII")]
    InvalidCharacters,
    #[error("the check digit does not match for a {0:?} barcode (mis-scan or typo)")]
    CheckDigit(Symbology),
    #[error("a {symbology:?} barcode must be {expected} digits")]
    WrongLength {
        symbology: Symbology,
        expected: &'static str,
    },
}

impl HasErrorCode for BarcodeError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ScanInvalid
    }
}

/// GS1 check digit for the given digits (without the check digit). Works for GTIN-8, -12,
/// -13 and -14: weights 3,1,3,1… from the right.
pub fn gtin_check_digit(body: &[u8]) -> u8 {
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| u32::from(*d) * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    ((10 - sum % 10) % 10) as u8
}

fn digits(s: &str) -> Option<Vec<u8>> {
    s.bytes().map(|b| b.is_ascii_digit().then(|| b - b'0')).collect()
}

fn gtin_valid(d: &[u8]) -> bool {
    d.len() >= 2 && gtin_check_digit(&d[..d.len() - 1]) == d[d.len() - 1]
}

/// UPC-E (number system, 6 digits, check) → UPC-A (12 digits), per the GS1 zero-suppression
/// rules. Returns `None` for a number system other than 0 or 1.
fn upce_to_upca(d: &[u8]) -> Option<Vec<u8>> {
    let (ns, x, check) = (d[0], &d[1..7], d[7]);
    if ns > 1 {
        return None;
    }
    let body: Vec<u8> = match x[5] {
        0..=2 => vec![ns, x[0], x[1], x[5], 0, 0, 0, 0, x[2], x[3], x[4]],
        3 => vec![ns, x[0], x[1], x[2], 0, 0, 0, 0, 0, x[3], x[4]],
        4 => vec![ns, x[0], x[1], x[2], x[3], 0, 0, 0, 0, 0, x[4]],
        _ => vec![ns, x[0], x[1], x[2], x[3], x[4], 0, 0, 0, 0, x[5]],
    };
    let mut upca = body;
    upca.push(check);
    Some(upca)
}

fn text(d: &[u8]) -> String {
    d.iter().map(|x| char::from(b'0' + x)).collect()
}

/// Normalises a scanned or typed barcode. `hint` is the symbology when the source knows it
/// (a scanner that reports it, or the barcode type chosen in the product editor).
pub fn normalize_barcode(raw: &str, hint: Option<Symbology>) -> Result<NormalizedBarcode, BarcodeError> {
    let cleaned: String = raw.chars().filter(|c| !c.is_ascii_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Err(BarcodeError::Empty);
    }
    if cleaned.len() > MAX_BARCODE_LEN {
        return Err(BarcodeError::TooLong);
    }
    if !cleaned.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return Err(BarcodeError::InvalidCharacters);
    }
    let value = cleaned.to_ascii_uppercase();

    let Some(d) = digits(&value) else {
        let symbology = match hint {
            Some(
                h @ (Symbology::Code128
                | Symbology::Code39
                | Symbology::Qr
                | Symbology::DataMatrix
                | Symbology::Internal),
            ) => h,
            Some(
                h @ (Symbology::Ean13
                | Symbology::Ean8
                | Symbology::UpcA
                | Symbology::UpcE
                | Symbology::Itf14),
            ) => {
                return Err(BarcodeError::WrongLength {
                    symbology: h,
                    expected: "all",
                });
            }
            _ => Symbology::Unknown,
        };
        return Ok(NormalizedBarcode {
            value,
            symbology,
            check_digit_failed: false,
        });
    };

    let gtin_hint = matches!(
        hint,
        Some(Symbology::Ean13 | Symbology::Ean8 | Symbology::UpcA | Symbology::UpcE | Symbology::Itf14)
    );
    let ean13 = |d: &[u8], symbology| NormalizedBarcode {
        value: text(d),
        symbology,
        check_digit_failed: false,
    };
    let failed = |symbology: Symbology| -> Result<NormalizedBarcode, BarcodeError> {
        if gtin_hint {
            Err(BarcodeError::CheckDigit(hint.unwrap_or(symbology)))
        } else {
            Ok(NormalizedBarcode {
                value: value.clone(),
                symbology: Symbology::Internal,
                check_digit_failed: true,
            })
        }
    };

    match (d.len(), hint) {
        (8, Some(Symbology::UpcE)) => match upce_to_upca(&d) {
            Some(upca) if gtin_valid(&upca) => {
                let mut e = vec![0];
                e.extend(upca);
                Ok(ean13(&e, Symbology::UpcE))
            }
            _ => Err(BarcodeError::CheckDigit(Symbology::UpcE)),
        },
        (8, _) if gtin_valid(&d) => Ok(ean13(&d, Symbology::Ean8)),
        (8, _) => failed(Symbology::Ean8),
        (12, _) if gtin_valid(&d) => {
            let mut e = vec![0];
            e.extend(&d);
            Ok(ean13(&e, Symbology::UpcA))
        }
        (12, _) => failed(Symbology::UpcA),
        (13, _) if gtin_valid(&d) => {
            let symbology = if d[0] == 0 {
                Symbology::UpcA
            } else {
                Symbology::Ean13
            };
            Ok(ean13(&d, symbology))
        }
        (13, _) => failed(Symbology::Ean13),
        (14, _) if gtin_valid(&d) => {
            if d[0] == 0 {
                // GTIN-14 with indicator 0 is the same trade item as its EAN-13.
                let symbology = if d[1] == 0 {
                    Symbology::UpcA
                } else {
                    Symbology::Ean13
                };
                Ok(ean13(&d[1..], symbology))
            } else {
                Ok(ean13(&d, Symbology::Itf14))
            }
        }
        (14, _) => failed(Symbology::Itf14),
        (_, Some(h)) if gtin_hint => Err(BarcodeError::WrongLength {
            symbology: h,
            expected: match h {
                Symbology::Ean8 | Symbology::UpcE => "8",
                Symbology::UpcA => "12",
                Symbology::Ean13 => "13",
                _ => "14",
            },
        }),
        (_, Some(h)) => Ok(NormalizedBarcode {
            value,
            symbology: h,
            check_digit_failed: false,
        }),
        _ => Ok(NormalizedBarcode {
            value,
            symbology: Symbology::Internal,
            check_digit_failed: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn norm(raw: &str) -> String {
        normalize_barcode(raw, None).unwrap().value
    }

    #[test]
    fn the_same_upc_a_item_resolves_identically_however_it_arrives() {
        let canonical = "0012345678905";
        for raw in [
            "012345678905",
            "0012345678905",
            " 012345678905\r",
            "\t012345678905\n",
            "00012345678905",
        ] {
            assert_eq!(norm(raw), canonical, "{raw:?}");
        }
        assert_eq!(
            normalize_barcode("012345678905", None).unwrap().symbology,
            Symbology::UpcA
        );
    }

    #[test]
    fn known_valid_codes() {
        let cases = [
            ("4006381333931", Symbology::Ean13, "4006381333931"),
            ("96385074", Symbology::Ean8, "96385074"),
            ("036000291452", Symbology::UpcA, "0036000291452"),
            ("10012345678902", Symbology::Itf14, "10012345678902"),
        ];
        for (raw, sym, value) in cases {
            let n = normalize_barcode(raw, None).unwrap();
            assert_eq!(
                (n.symbology, n.value.as_str(), n.check_digit_failed),
                (sym, value, false),
                "{raw}"
            );
        }
    }

    #[test]
    fn upc_e_expands_to_the_same_item_as_its_upc_a() {
        let e = normalize_barcode("04252614", Some(Symbology::UpcE)).unwrap();
        let a = normalize_barcode("042100005264", None).unwrap();
        assert_eq!(e.value, a.value);
        assert_eq!(e.value, "0042100005264");
        assert_eq!(
            normalize_barcode("04252615", Some(Symbology::UpcE)),
            Err(BarcodeError::CheckDigit(Symbology::UpcE))
        );
    }

    #[test]
    fn bad_check_digit_rejected_with_hint_flagged_without() {
        assert_eq!(
            normalize_barcode("4006381333932", Some(Symbology::Ean13)),
            Err(BarcodeError::CheckDigit(Symbology::Ean13))
        );
        let n = normalize_barcode("4006381333932", None).unwrap();
        assert_eq!((n.symbology, n.check_digit_failed), (Symbology::Internal, true));
        assert_eq!(
            BarcodeError::CheckDigit(Symbology::Ean13).error_code(),
            ErrorCode::ScanInvalid
        );
    }

    #[test]
    fn text_codes_are_trimmed_and_upper_cased() {
        let n = normalize_barcode("  abc-123x\r\n", Some(Symbology::Code128)).unwrap();
        assert_eq!((n.value.as_str(), n.symbology), ("ABC-123X", Symbology::Code128));
        assert_eq!(
            normalize_barcode("SKU 42", None).unwrap().symbology,
            Symbology::Unknown
        );
        assert_eq!(norm("12345"), "12345");
        assert_eq!(
            normalize_barcode("12345", None).unwrap().symbology,
            Symbology::Internal
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(normalize_barcode(" \r\n", None), Err(BarcodeError::Empty));
        assert_eq!(
            normalize_barcode(&"9".repeat(65), None),
            Err(BarcodeError::TooLong)
        );
        assert_eq!(
            normalize_barcode("café", None),
            Err(BarcodeError::InvalidCharacters)
        );
        assert!(matches!(
            normalize_barcode("ABC", Some(Symbology::Ean13)),
            Err(BarcodeError::WrongLength { .. })
        ));
        assert!(matches!(
            normalize_barcode("123", Some(Symbology::Ean13)),
            Err(BarcodeError::WrongLength { .. })
        ));
    }

    proptest! {
        /// The GS1 check digit catches every single-digit error.
        #[test]
        fn check_digit_detects_any_single_digit_error(body in proptest::collection::vec(0u8..10, 12), pos in 0usize..13, delta in 1u8..10) {
            let mut code = body.clone();
            code.push(gtin_check_digit(&body));
            prop_assert!(gtin_valid(&code));
            code[pos] = (code[pos] + delta) % 10;
            prop_assert!(!gtin_valid(&code));
        }

        /// Normalising is idempotent: a stored value normalises to itself.
        #[test]
        fn normalisation_is_idempotent(raw in "[ -~]{1,30}") {
            if let Ok(n) = normalize_barcode(&raw, None) {
                prop_assert_eq!(normalize_barcode(&n.value, None).unwrap().value, n.value);
            }
        }
    }
}
