//! Right-to-left text on receipts (D-02): the Unicode Bidirectional Algorithm (UAX #9, via
//! `unicode-bidi`) orders the runs of a line that mixes Arabic or Hebrew with Latin words
//! and digits before they are shaped and placed, in the PDF ([`crate::pdf`]) and on
//! printed images ([`crate::raster`]).
//!
//! Choices:
//!
//! - **The receipt grid stays left to right.** A receipt line is laid out in columns by
//!   [`crate::text::render`] (name or label on the left, amount on the right). Reordering a
//!   whole line would move the amount to the left, out of its column, so a line is split
//!   into **fields** ([`fields`]: text separated by two or more spaces, as the layout pads
//!   between a label and its amount) and each field is reordered within its own columns.
//! - **Each field takes its direction from its first strong letter** (UAX #9 rules P2 and
//!   P3): an Arabic product name reads right to left, with a Latin brand and numbers inside
//!   it left to right in their place; an English label ("Discount", "Served by") reads left
//!   to right with an Arabic word inside it placed after it. A field with no letters (an
//!   amount) is left to right. A receipt-wide right-to-left direction was rejected: the
//!   receipt's own labels are English, and under a right-to-left base their trailing
//!   punctuation would jump to the front ("!Thank you").
//! - **Explicit direction controls are ignored** (LRE, RLE, LRO, RLO, PDF and the isolates):
//!   a product name cannot reverse the order of the text around it.
//! - **Plain text is not reordered.** The preview and the text sent to a printer as
//!   characters stay in logical order: the WebView applies the algorithm itself, and a
//!   printer that transliterates prints Latin, which reads left to right.

use unicode_bidi::{BidiClass, ParagraphBidiInfo, bidi_class};

/// Whether `text` has a right-to-left letter (Arabic, Hebrew, Syriac, …).
pub fn has_rtl(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL))
}

fn is_control(c: char) -> bool {
    matches!(
        bidi_class(c),
        BidiClass::LRE
            | BidiClass::RLE
            | BidiClass::LRO
            | BidiClass::RLO
            | BidiClass::PDF
            | BidiClass::LRI
            | BidiClass::RLI
            | BidiClass::FSI
            | BidiClass::PDI
    )
}

fn is_strong(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::L | BidiClass::R | BidiClass::AL)
}

/// The runs of `text` in display order, left to right: each run's characters in logical
/// order and whether the run reads right to left (an odd embedding level). The direction
/// comes from the first strong letter (left to right when there is none). Direction
/// controls are dropped.
pub fn visual_runs(text: &str) -> Vec<(String, bool)> {
    let text: String = text.chars().filter(|c| !is_control(*c)).collect();
    if text.is_empty() {
        return Vec::new();
    }
    if !has_rtl(&text) {
        return vec![(text, false)];
    }
    let info = ParagraphBidiInfo::new(&text, None);
    let (levels, runs) = info.visual_runs(0..text.len());
    runs.into_iter()
        .filter(|r| !r.is_empty())
        .map(|r| (text[r.clone()].to_owned(), levels[r.start].is_rtl()))
        .collect()
}

/// `text` as it reads on paper, left to right: the runs of [`visual_runs`] with each
/// right-to-left run's characters reversed (before shaping joins them). For checks and
/// diagnostics.
pub fn visual(text: &str) -> String {
    visual_runs(text)
        .into_iter()
        .map(|(s, rtl)| if rtl { s.chars().rev().collect() } else { s })
        .collect()
}

/// A receipt line split into fields, each with the column it starts in: text separated by
/// two or more spaces (leading and separating spaces belong to no field). When a line full
/// to its last column ends in an amount pushed against a right-to-left label by a single
/// space (the label was cut to fit), the amount is a field of its own, so it stays at the
/// right edge.
pub fn fields(line: &str, columns: usize) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut col = 0;
    let mut spaces = 0;
    let mut open = false;
    for c in line.chars() {
        if c == ' ' {
            spaces += 1;
        } else {
            if open && spaces == 1 {
                if let Some((_, f)) = out.last_mut() {
                    f.push(' ');
                }
            } else if !open || spaces >= 2 {
                out.push((col, String::new()));
            }
            open = true;
            spaces = 0;
            if let Some((_, f)) = out.last_mut() {
                f.push(c);
            }
        }
        col += crate::text::char_columns(c);
    }
    let end = crate::text::text_columns(line.trim_end());
    if end >= columns
        && let Some((start, last)) = out.last()
        && let Some((head, tail)) = last.rsplit_once(' ')
        && !tail.chars().any(is_strong)
        && has_rtl(head)
    {
        let (start, head, tail) = (*start, head.to_owned(), tail.to_owned());
        out.pop();
        let tail_col = start + crate::text::text_columns(&head) + 1;
        out.push((start, head));
        out.push((tail_col, tail));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_text_is_one_left_to_right_run() {
        assert_eq!(
            visual_runs("Tea 2 x 2.50"),
            vec![("Tea 2 x 2.50".to_owned(), false)]
        );
        assert!(!has_rtl("Café 250 g"));
        assert!(has_rtl("كولا"));
        assert!(has_rtl("שלום"));
    }

    /// An Arabic product name with a Latin brand and a size: the name reads right to left,
    /// so its first word is at the right; the brand and the number keep their own order.
    #[test]
    fn an_arabic_name_with_a_latin_brand_and_digits_reads_right_to_left() {
        // Logical: juice, "Rani", 250, ml. Visual, left to right: ml, then the brand and
        // the number as one left-to-right run ("Rani 250"), then juice at the right.
        let runs = visual_runs("عصير Rani 250 مل");
        let order: Vec<(&str, bool)> = runs.iter().map(|(s, r)| (s.trim(), *r)).collect();
        assert_eq!(
            order,
            [("مل", true), ("Rani 250", false), ("عصير", true)],
            "{runs:?}"
        );
        assert_eq!(visual("عصير Rani 250 مل"), "لم Rani 250 ريصع");
    }

    /// A Latin first word makes the field left to right: the Arabic words follow it, and
    /// read right to left among themselves.
    #[test]
    fn a_latin_first_word_keeps_the_field_left_to_right() {
        let runs = visual_runs("Pepsi بيبسي كولا 330");
        assert_eq!(runs[0], ("Pepsi ".to_owned(), false));
        // "بيبسي كولا" right to left: كولا is drawn first (leftmost).
        assert_eq!(visual("Pepsi بيبسي كولا"), "Pepsi الوك يسبيب");
        // Arabic letters followed by European digits: the digits stay in order.
        assert_eq!(visual("كولا 330"), "330 الوك");
        // Hebrew too.
        assert_eq!(visual("חלב 3%"), "3% בלח");
    }

    #[test]
    fn direction_controls_cannot_reverse_a_line() {
        assert_eq!(visual("\u{202E}Tea\u{202C} 2.50"), "Tea 2.50");
        assert_eq!(visual("\u{2067}Tea\u{2069}"), "Tea");
    }

    #[test]
    fn a_line_splits_into_fields_at_wide_gaps() {
        let line = format!("{:<26}{:>6}", "  2 x 2.50", "5.00");
        assert_eq!(
            fields(&line, 32),
            vec![(2, "2 x 2.50".to_owned()), (28, "5.00".to_owned())]
        );
        assert_eq!(
            fields("كولا غازية       250.00", 23),
            vec![(0, "كولا غازية".to_owned()), (17, "250.00".to_owned())]
        );
        // A right-to-left label cut to fit, one space before its amount: the amount is
        // still its own field at the right edge.
        assert_eq!(
            fields("خصم الصيف 250.00", 16),
            vec![(0, "خصم الصيف".to_owned()), (10, "250.00".to_owned())]
        );
        // Not at the edge (a name on its own line): the number belongs to the name.
        assert_eq!(fields("كولا 330", 32), vec![(0, "كولا 330".to_owned())]);
        // Wide characters count two columns.
        assert_eq!(
            fields("可乐  9.99", 10),
            vec![(0, "可乐".to_owned()), (6, "9.99".to_owned())]
        );
    }
}
