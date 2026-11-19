//! Variable-measure barcodes (BAR-009, grocery): labels printed by a shop scale that carry an
//! item code and a weight or a price in an EAN-13 (or UPC-A) from the restricted-circulation
//! range. What the digits mean is the shop's choice (GS1 leaves prefixes 20–29, and 02 for
//! UPC, to each country or shop), so each layout is a [`LabelRule`] in the settings.
//!
//! A label `PP IIIII VVVVV C` (prefix, item, value, check digit) is sold as the product whose
//! barcode is the same code with the value set to zeros (`PP IIIII 00000 C'`): that is the
//! barcode the owner gives the product, as scale software prints it for a zero value.

use super::barcode::gtin_check_digit;
use crate::{Money, Quantity};
use serde::{Deserialize, Serialize};

/// What the value digits hold.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum LabelKind {
    /// Grams (0.001 kg).
    Weight,
    /// The price in minor units of the shop's currency (pence, cents).
    Price,
}

/// One label layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct LabelRule {
    /// The leading digits that mark this layout (`"21"`; `"02"` for a UPC-A label).
    pub prefix: String,
    pub kind: LabelKind,
    /// Digits of item code after the prefix.
    pub item_digits: u8,
    /// A price check digit sits between the item code and the value (some scales print one).
    #[serde(default)]
    pub value_check_digit: bool,
}

/// A rule that cannot work.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LabelRuleError {
    #[error("a prefix is 1 to 3 digits, starting with 2 (or 02 for UPC-A labels)")]
    Prefix,
    #[error("the item code and value do not fit in 13 digits (value needs at least 3)")]
    Layout,
    #[error("two rules use the prefix {0}")]
    Duplicate(String),
    #[error(
        "{0:?}: write a layout as prefix, weight or price, item digits, and \"check\" if the scale prints a price check digit (e.g. 25 weight 5)"
    )]
    Syntax(String),
}

impl LabelRule {
    /// Digits left for the value.
    fn value_digits(&self) -> usize {
        13usize
            .saturating_sub(self.prefix.len())
            .saturating_sub(usize::from(self.item_digits))
            .saturating_sub(usize::from(self.value_check_digit))
            .saturating_sub(1)
    }

    pub fn validate(&self) -> Result<(), LabelRuleError> {
        let p = &self.prefix;
        let ok = (1..=3).contains(&p.len())
            && p.chars().all(|c| c.is_ascii_digit())
            && (p.starts_with('2') || p.starts_with("02"));
        if !ok {
            return Err(LabelRuleError::Prefix);
        }
        if self.item_digits < 1 || self.value_digits() < 3 {
            return Err(LabelRuleError::Layout);
        }
        Ok(())
    }
}

impl LabelRule {
    /// Reads one line of the `scanner.label_rules` setting: `prefix kind item-digits [check]`,
    /// e.g. `25 weight 5` or `02 price 5 check`.
    pub fn parse_line(line: &str) -> Result<Self, LabelRuleError> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let (prefix, kind, digits, check) = match words.as_slice() {
            [p, k, d] => (*p, *k, *d, false),
            [p, k, d, c] if c.eq_ignore_ascii_case("check") => (*p, *k, *d, true),
            _ => return Err(LabelRuleError::Syntax(line.trim().to_owned())),
        };
        let kind = match kind.to_ascii_lowercase().as_str() {
            "weight" => LabelKind::Weight,
            "price" => LabelKind::Price,
            _ => return Err(LabelRuleError::Syntax(line.trim().to_owned())),
        };
        let item_digits = digits
            .parse()
            .map_err(|_| LabelRuleError::Syntax(line.trim().to_owned()))?;
        let rule = Self {
            prefix: prefix.to_owned(),
            kind,
            item_digits,
            value_check_digit: check,
        };
        rule.validate()?;
        Ok(rule)
    }
}

/// The rules in the setting's lines (blank lines ignored), checked as a set.
pub fn rules_from_lines<S: AsRef<str>>(lines: &[S]) -> Result<Vec<LabelRule>, LabelRuleError> {
    let rules = lines
        .iter()
        .map(AsRef::as_ref)
        .filter(|l| !l.trim().is_empty())
        .map(LabelRule::parse_line)
        .collect::<Result<Vec<_>, _>>()?;
    validate_rules(&rules)?;
    Ok(rules)
}

/// Checks a whole rule set.
pub fn validate_rules(rules: &[LabelRule]) -> Result<(), LabelRuleError> {
    for (i, r) in rules.iter().enumerate() {
        r.validate()?;
        if rules[..i].iter().any(|o| o.prefix == r.prefix) {
            return Err(LabelRuleError::Duplicate(r.prefix.clone()));
        }
    }
    Ok(())
}

/// What a label says.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LabelValue {
    Weight(Quantity),
    Price(Money),
}

/// A scanned label: the product's barcode to look up, and the value printed on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub base_code: String,
    pub value: LabelValue,
}

/// Reads a normalised 13-digit code as a label, by the first rule whose prefix it starts with
/// (longest prefix first). `None` if no rule matches, or the code is not 13 digits.
pub fn parse_label(code: &str, rules: &[LabelRule]) -> Option<Label> {
    if code.len() != 13 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rule = rules
        .iter()
        .filter(|r| r.validate().is_ok() && code.starts_with(&r.prefix))
        .max_by_key(|r| r.prefix.len())?;
    let d: Vec<u8> = code.bytes().map(|b| b - b'0').collect();
    let value_start = rule.prefix.len() + usize::from(rule.item_digits) + usize::from(rule.value_check_digit);
    let value_digits = &d[value_start..12];
    let n = value_digits.iter().fold(0i64, |acc, x| acc * 10 + i64::from(*x));
    let mut base = d[..12].to_vec();
    for x in &mut base[rule.prefix.len() + usize::from(rule.item_digits)..12] {
        *x = 0;
    }
    base.push(gtin_check_digit(&base));
    let base_code = base.iter().map(|x| char::from(b'0' + x)).collect();
    let value = match rule.kind {
        LabelKind::Weight => LabelValue::Weight(Quantity::from_milli(n)),
        LabelKind::Price => LabelValue::Price(Money::from_minor(n)),
    };
    Some(Label { base_code, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(prefix: &str, kind: LabelKind, item_digits: u8) -> LabelRule {
        LabelRule {
            prefix: prefix.into(),
            kind,
            item_digits,
            value_check_digit: false,
        }
    }

    fn with_check(body12: &str) -> String {
        let d: Vec<u8> = body12.bytes().map(|b| b - b'0').collect();
        format!("{body12}{}", gtin_check_digit(&d))
    }

    #[test]
    fn weight_and_price_labels() {
        let rules = [rule("21", LabelKind::Price, 5), rule("25", LabelKind::Weight, 5)];
        // 25 01234 00734: item 01234, 734 g.
        let l = parse_label(&with_check("250123400734"), &rules).unwrap();
        assert_eq!(l.value, LabelValue::Weight(Quantity::from_milli(734)));
        assert_eq!(l.base_code, with_check("250123400000"));
        // 21 00042 01299: item 42, 12.99.
        let l = parse_label(&with_check("210004201299"), &rules).unwrap();
        assert_eq!(l.value, LabelValue::Price(Money::from_minor(1299)));
        assert_eq!(l.base_code, with_check("210004200000"));
        // Other prefixes, other lengths: not labels.
        assert!(parse_label(&with_check("500015940723"), &rules).is_none());
        assert!(parse_label("25012340073", &rules).is_none());
    }

    #[test]
    fn a_price_check_digit_and_upc_labels() {
        let r = LabelRule {
            prefix: "02".into(),
            kind: LabelKind::Price,
            item_digits: 5,
            value_check_digit: true,
        };
        // UPC-A 2 12345 6 0599 C normalised to 13 digits: 0 2 12345 6 0599 C.
        let l = parse_label(&with_check("021234560599"), &[r]).unwrap();
        assert_eq!(l.value, LabelValue::Price(Money::from_minor(599)));
        assert_eq!(l.base_code, with_check("021234500000"));
    }

    #[test]
    fn rules_from_the_setting_lines() {
        let rules = rules_from_lines(&["21 price 5", "", "02 PRICE 5 check", "25 weight 5"]).unwrap();
        assert_eq!(rules.len(), 3);
        assert!(rules[1].value_check_digit);
        assert!(matches!(
            rules_from_lines(&["21 cost 5"]),
            Err(LabelRuleError::Syntax(_))
        ));
        assert!(matches!(
            rules_from_lines(&["21 price"]),
            Err(LabelRuleError::Syntax(_))
        ));
        assert_eq!(rules_from_lines(&["31 price 5"]), Err(LabelRuleError::Prefix));
        assert!(matches!(
            rules_from_lines(&["21 price 5", "21 weight 4"]),
            Err(LabelRuleError::Duplicate(_))
        ));
    }

    #[test]
    fn the_longest_prefix_wins_and_bad_rules_are_refused() {
        let rules = [rule("2", LabelKind::Price, 6), rule("29", LabelKind::Weight, 4)];
        assert!(matches!(
            parse_label(&with_check("290011012345"), &rules).unwrap().value,
            LabelValue::Weight(_)
        ));
        assert_eq!(
            rule("3", LabelKind::Price, 5).validate(),
            Err(LabelRuleError::Prefix)
        );
        assert_eq!(
            rule("21", LabelKind::Price, 9).validate(),
            Err(LabelRuleError::Layout)
        );
        assert_eq!(
            validate_rules(&[rule("21", LabelKind::Price, 5), rule("21", LabelKind::Weight, 5)]),
            Err(LabelRuleError::Duplicate("21".into()))
        );
    }
}
