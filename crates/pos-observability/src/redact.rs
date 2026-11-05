/// Replacement written for secret values.
pub const REDACTED: &str = "[REDACTED]";

/// Field names whose values are never logged. Matching is case-insensitive on the whole
/// name and on `_`-separated parts, so `refresh_token`, `userPin` → `userpin` (whole name
/// contains `pin` as a suffix) and `Authorization` are all caught.
const SECRET_WORDS: &[&str] = &[
    "pin",
    "password",
    "passcode",
    "passwd",
    "token",
    "secret",
    "authorization",
    "cookie",
    "apikey",
    "api_key",
    "cvv",
    "pan",
    "card_number",
];

pub fn is_secret_field(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if SECRET_WORDS.iter().any(|w| lower == *w) {
        return true;
    }
    lower
        .split(['_', '-', '.'])
        .any(|part| SECRET_WORDS.contains(&part))
        || ["pin", "token", "password", "secret"]
            .iter()
            .any(|w| lower.ends_with(w))
}

/// Masks digit runs of 14–19 digits (spaces and dashes allowed between digits) that pass
/// the Luhn check, keeping the last four digits: `4111 1111 1111 1111` → `[CARD …1111]`.
///
/// 13-digit runs are deliberately not masked: legacy 13-digit PANs are out of circulation,
/// while EAN-13 barcodes and millisecond timestamps are 13 digits and would otherwise be
/// masked about one time in ten.
pub fn mask_card_numbers(input: &str) -> std::borrow::Cow<'_, str> {
    let bytes = input.as_bytes();
    let mut out: Option<String> = None;
    let mut i = 0;
    let mut copied_to = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Collect a run of digits allowing single separators between them.
        let start = i;
        let mut digits = Vec::new();
        let mut end = i;
        let mut j = i;
        while j < bytes.len() {
            let b = bytes[j];
            if b.is_ascii_digit() {
                digits.push(b - b'0');
                end = j + 1;
                j += 1;
            } else if (b == b' ' || b == b'-')
                && j + 1 < bytes.len()
                && bytes[j + 1].is_ascii_digit()
                && !digits.is_empty()
            {
                j += 1;
            } else {
                break;
            }
        }
        if (14..=19).contains(&digits.len()) && luhn_valid(&digits) {
            let s = out.get_or_insert_with(|| String::with_capacity(input.len()));
            s.push_str(&input[copied_to..start]);
            let last4: String = digits[digits.len() - 4..]
                .iter()
                .map(|d| char::from(b'0' + d))
                .collect();
            s.push_str(&format!("[CARD …{last4}]"));
            copied_to = end;
        }
        i = end.max(start + 1);
    }
    match out {
        Some(mut s) => {
            s.push_str(&input[copied_to..]);
            std::borrow::Cow::Owned(s)
        }
        None => std::borrow::Cow::Borrowed(input),
    }
}

fn luhn_valid(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = u32::from(d);
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_field_names() {
        for name in [
            "pin",
            "PIN",
            "staff_pin",
            "password",
            "refresh_token",
            "access_token",
            "Authorization",
            "api_key",
            "x.secret",
            "newPin",
        ] {
            assert!(is_secret_field(name), "{name}");
        }
        for name in [
            "pinned",
            "spinner",
            "message",
            "register_id",
            "tokens_count",
            "correlation_id",
        ] {
            assert!(!is_secret_field(name), "{name}");
        }
    }

    #[test]
    fn masks_luhn_valid_card_numbers_only() {
        assert_eq!(
            mask_card_numbers("ref 4111111111111111 ok"),
            "ref [CARD …1111] ok"
        );
        assert_eq!(mask_card_numbers("4111 1111 1111 1111"), "[CARD …1111]");
        assert_eq!(
            mask_card_numbers("5500-0000-0000-0004 and 4111111111111111"),
            "[CARD …0004] and [CARD …1111]"
        );
        // Not Luhn-valid, or too short/long: left alone (receipt numbers, barcodes, timestamps).
        assert_eq!(mask_card_numbers("4111111111111112"), "4111111111111112");
        assert_eq!(mask_card_numbers("5012345678900"), "5012345678900");
        assert_eq!(
            mask_card_numbers("R01-000042 1767225600000"),
            "R01-000042 1767225600000"
        );
        assert!(matches!(
            mask_card_numbers("nothing here"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
