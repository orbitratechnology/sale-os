//! Code 128 (ISO/IEC 15417) for the receipt-number barcode in PDF receipts (E13-T04).
//!
//! Thermal printers draw the barcode themselves (`GS k`, see [`crate::escpos`]); a PDF has
//! to draw the bars, so the symbol is encoded here. Code sets B (printable ASCII) and C
//! (digit pairs) are used, switching as ISO/IEC 15417 Annex E suggests to keep the symbol
//! short: start in C when the data begins with four or more digits (or is exactly two),
//! change to C for a run of at least four digits at the end or six in the middle (an odd
//! run leaves its first digit in B), and back to B when fewer than two digits remain. Code
//! set A (control characters) is never needed: receipt numbers are printable ASCII.
//!
//! The pattern table and the encodings in the tests were checked against python-barcode
//! 0.16.1.

/// Bar and space widths, in modules, of symbol values 0–105 (bar first). The stop symbol
/// is [`STOP`].
const PATTERNS: [&str; 106] = [
    "212222", "222122", "222221", "121223", "121322", "131222", "122213", "122312", "132212", "221213",
    "221312", "231212", "112232", "122132", "122231", "113222", "123122", "123221", "223211", "221132",
    "221231", "213212", "223112", "312131", "311222", "321122", "321221", "312212", "322112", "322211",
    "212123", "212321", "232121", "111323", "131123", "131321", "112313", "132113", "132311", "211313",
    "231113", "231311", "112133", "112331", "132131", "113123", "113321", "133121", "313121", "211331",
    "231131", "213113", "213311", "213131", "311123", "311321", "331121", "312113", "312311", "332111",
    "314111", "221411", "431111", "111224", "111422", "121124", "121421", "141122", "141221", "112214",
    "112412", "122114", "122411", "142112", "142211", "241211", "221114", "413111", "241112", "134111",
    "111242", "121142", "121241", "114212", "124112", "124211", "411212", "421112", "421211", "212141",
    "214121", "412121", "111143", "111341", "131141", "114113", "114311", "411113", "411311", "113141",
    "114131", "311141", "411131", "211412", "211214", "211232",
];

/// The stop symbol with its final bar (13 modules).
const STOP: &str = "2331112";

pub const START_B: u8 = 104;
pub const START_C: u8 = 105;
/// In code set B: change to C. (In C the same value is a pair of digits, "99".)
pub const CODE_C: u8 = 99;
/// In code set C: change to B.
pub const CODE_B: u8 = 100;
pub const STOP_VALUE: u8 = 106;

/// Modules of light space required on each side of the symbol.
pub const QUIET_ZONE: usize = 10;

/// The symbol values for `data`: start, data, check symbol and stop. `None` when `data` is
/// empty or has a character outside printable ASCII (space to `~`).
pub fn symbols(data: &str) -> Option<Vec<u8>> {
    let bytes = data.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(|b| (0x20..=0x7E).contains(b)) {
        return None;
    }
    let digits_from = |i: usize| bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    let mut out = Vec::with_capacity(bytes.len() + 4);
    let lead = digits_from(0);
    let mut in_c = lead >= 4 || (lead == 2 && bytes.len() == 2);
    out.push(if in_c { START_C } else { START_B });
    let mut i = 0;
    while i < bytes.len() {
        let run = digits_from(i);
        if in_c {
            if run >= 2 {
                out.push((bytes[i] - b'0') * 10 + (bytes[i + 1] - b'0'));
                i += 2;
                continue;
            }
            out.push(CODE_B);
            in_c = false;
        }
        let at_end = i + run == bytes.len();
        if run >= 6 || (run >= 4 && at_end) {
            if run % 2 == 1 {
                out.push(bytes[i] - 0x20);
                i += 1;
            }
            out.push(CODE_C);
            in_c = true;
            continue;
        }
        out.push(bytes[i] - 0x20);
        i += 1;
    }
    out.push(check(&out));
    out.push(STOP_VALUE);
    Some(out)
}

/// The check symbol: the start value plus each following value times its position, mod 103.
fn check(values: &[u8]) -> u8 {
    let sum = values
        .iter()
        .enumerate()
        .map(|(i, v)| usize::from(*v) * i.max(1))
        .sum::<usize>();
    u8::try_from(sum % 103).unwrap_or(0)
}

/// The symbol as modules, left to right, `true` for a bar (no quiet zones).
pub fn modules(data: &str) -> Option<Vec<bool>> {
    let values = symbols(data)?;
    let mut out = Vec::with_capacity(values.len() * 11 + 2);
    for v in values {
        let widths = if v == STOP_VALUE {
            STOP
        } else {
            PATTERNS[usize::from(v)]
        };
        for (k, w) in widths.bytes().enumerate() {
            let bar = k % 2 == 0;
            out.extend(std::iter::repeat_n(bar, usize::from(w - b'0')));
        }
    }
    Some(out)
}

/// The bars of the symbol as (first module, width in modules), left to right.
pub fn bars(data: &str) -> Option<Vec<(usize, usize)>> {
    let m = modules(data)?;
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, bar) in m.iter().enumerate() {
        if !bar {
            continue;
        }
        match out.last_mut() {
            Some((start, w)) if *start + *w == i => *w += 1,
            _ => out.push((i, 1)),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(data: &str) -> String {
        modules(data)
            .unwrap()
            .iter()
            .map(|b| if *b { '1' } else { '0' })
            .collect()
    }

    /// Every symbol is 11 modules with three bars and three spaces; the stop is 13 with four
    /// bars. No two values share a pattern.
    #[test]
    fn the_pattern_table_is_well_formed() {
        let mut seen = std::collections::BTreeSet::new();
        for p in PATTERNS {
            assert_eq!(p.len(), 6);
            assert_eq!(p.bytes().map(|b| u32::from(b - b'0')).sum::<u32>(), 11, "{p}");
            assert!(seen.insert(p), "{p} twice");
        }
        assert_eq!(STOP.bytes().map(|b| u32::from(b - b'0')).sum::<u32>(), 13);
    }

    /// Reference encodings from python-barcode 0.16.1 (`Code128(data).build()`), which picks
    /// the same code sets for these inputs.
    #[test]
    fn known_inputs_match_reference_encodings() {
        for (data, want) in [
            (
                "R01-000001",
                "110100100001100010111010011101100100111001101001101110010111011110110110011001101100110011001101100100110100001100011101011",
            ),
            (
                "R01-Z-0007",
                "11010010000110001011101001110110010011100110100110111001110110001010011011100101110111101101100110010011000100100001100101100011101011",
            ),
            (
                "Wikipedia",
                "11010010000111010001101000011010011000010010100001101001010011110010110010000100001001101000011010010010110000111100100101100011101011",
            ),
            (
                "123456",
                "11010011100101100111001000101100011100010110100011011101100011101011",
            ),
            (
                "1234567",
                "110100111001011001110010001011000111000101101011110111011101101110100011011101100011101011",
            ),
            (
                "AB12",
                "1101001000010100011000100010110001001110011011001110010110010111001100011101011",
            ),
            ("00", "1101001110011011001100110011001101100011101011"),
            (
                "R01-R-000123",
                "1101001000011000101110100111011001001110011010011011100110001011101001101110010111011110110110011001100110110011101101110100100011001100011101011",
            ),
        ] {
            assert_eq!(pattern(data), want, "{data}");
        }
    }

    #[test]
    fn code_sets_and_check_symbol() {
        // B for the prefix, C for the six digits: start B, R 0 1 -, Code C, 00 00 01,
        // check (104 + 50·1 + 16·2 + 17·3 + 13·4 + 99·5 + 0·6 + 0·7 + 1·8) mod 103 = 71.
        assert_eq!(
            symbols("R01-000001").unwrap(),
            [104, 50, 16, 17, 13, 99, 0, 0, 1, 71, 106]
        );
        // Four digits in the middle stay in B (switching would not save a symbol).
        assert_eq!(symbols("X1234Y").unwrap()[..7], [104, 56, 17, 18, 19, 20, 57]);
        // Six in the middle: C, then back to B.
        assert_eq!(symbols("X123456Y").unwrap()[..7], [104, 56, 99, 12, 34, 56, 100]);
        // An odd run at the end leaves its first digit in B.
        assert_eq!(symbols("A12345").unwrap()[..5], [104, 33, 17, 99, 23]);
        // Lowercase, space and the characters around them are code set B.
        assert_eq!(symbols("a ~").unwrap()[..4], [104, 65, 0, 94]);
    }

    #[test]
    fn what_cannot_be_encoded_is_refused() {
        assert_eq!(symbols(""), None);
        assert_eq!(symbols("R01-Ž"), None);
        assert_eq!(symbols("tab\there"), None);
    }

    #[test]
    fn bars_are_runs_of_dark_modules() {
        let b = bars("00").unwrap();
        // Start C begins with a 2-module bar; the stop ends with a 2-module bar.
        assert_eq!(b[0], (0, 2));
        let total = modules("00").unwrap().len();
        assert_eq!(total, 11 * 3 + 13);
        let last = b.last().unwrap();
        assert_eq!(last.0 + last.1, total);
        // Three bars per symbol, four in the stop.
        assert_eq!(b.len(), 3 * 3 + 4);
    }
}
