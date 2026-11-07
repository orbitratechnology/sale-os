//! The receipt as lines of monospace text for a paper width (E13-T03/T05): 32 columns for
//! 58 mm paper, 48 for 80 mm. The preview shows exactly these lines, and the ESC/POS
//! encoder prints them, so what the cashier sees is what prints.

use crate::document::{CopyKind, ReceiptDocument, amount};

pub const COLUMNS_58MM: usize = 32;
pub const COLUMNS_80MM: usize = 48;

/// One rendered line and how to print it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLine {
    pub text: String,
    pub style: Style,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Style {
    Normal,
    Centered,
    Bold,
}

pub fn render(doc: &ReceiptDocument, width: usize) -> Vec<TextLine> {
    let width = width.max(24);
    let mut out = Vec::new();
    let exp = doc.currency_exponent;
    let m = |v| amount(v, exp);
    let centered = |out: &mut Vec<TextLine>, s: &str| {
        for l in wrap(s, width) {
            out.push(TextLine {
                text: l,
                style: Style::Centered,
            });
        }
    };
    let plain = |out: &mut Vec<TextLine>, s: &str| {
        for l in wrap(s, width) {
            out.push(TextLine {
                text: l,
                style: Style::Normal,
            });
        }
    };
    let pair = |out: &mut Vec<TextLine>, left: &str, right: &str, style: Style| {
        out.push(TextLine {
            text: columns(left, right, width),
            style,
        });
    };
    let rule = |out: &mut Vec<TextLine>| {
        out.push(TextLine {
            text: "-".repeat(width),
            style: Style::Normal,
        })
    };

    centered(&mut out, &doc.header.business_name);
    for l in &doc.header.address_lines {
        centered(&mut out, l);
    }
    if let Some(p) = &doc.header.phone {
        centered(&mut out, &format!("Tel {p}"));
    }
    if let Some(t) = &doc.header.tax_registration_no {
        centered(&mut out, &format!("Tax no. {t}"));
    }
    for l in &doc.header.lines {
        centered(&mut out, l);
    }
    if doc.copy == CopyKind::Reprint {
        centered(&mut out, "*** REPRINT ***");
    }
    if doc.sandbox {
        centered(&mut out, "*** SANDBOX - PRACTICE SALE ***");
    }
    rule(&mut out);
    pair(
        &mut out,
        &format!("Receipt {}", doc.meta.receipt_number),
        "",
        Style::Normal,
    );
    pair(
        &mut out,
        &doc.meta.local_time,
        &doc.meta.register_code,
        Style::Normal,
    );
    plain(&mut out, &format!("Served by {}", doc.meta.cashier_name));
    rule(&mut out);

    for l in &doc.lines {
        let name = if l.variant_label.is_empty() {
            l.name.clone()
        } else {
            format!("{} {}", l.name, l.variant_label)
        };
        plain(&mut out, &name);
        let each = format!(
            "  {} x {}{}",
            l.quantity,
            m(l.unit_price),
            if l.price_overridden { " *" } else { "" }
        );
        pair(&mut out, &each, &m(l.gross), Style::Normal);
        if let Some(label) = &l.discount_label {
            let what = if label.is_empty() {
                "  Discount".to_owned()
            } else {
                format!("  Discount {label}")
            };
            pair(&mut out, &what, &format!("-{}", m(l.discount)), Style::Normal);
        }
    }
    rule(&mut out);
    pair(&mut out, "Subtotal", &m(doc.totals.subtotal), Style::Normal);
    if doc.totals.line_discounts.minor() != 0 {
        pair(
            &mut out,
            "Item discounts",
            &format!("-{}", m(doc.totals.line_discounts)),
            Style::Normal,
        );
    }
    if doc.totals.order_discount.minor() != 0 {
        let label = match doc.totals.order_discount_label.as_deref() {
            Some(l) if !l.is_empty() => format!("Order discount {l}"),
            _ => "Order discount".into(),
        };
        pair(
            &mut out,
            &label,
            &format!("-{}", m(doc.totals.order_discount)),
            Style::Normal,
        );
    }
    if !doc.totals.prices_include_tax {
        for t in &doc.totals.tax {
            pair(&mut out, &t.name, &m(t.amount), Style::Normal);
        }
    }
    if doc.totals.rounding.minor() != 0 {
        pair(&mut out, "Rounding", &m(doc.totals.rounding), Style::Normal);
    }
    pair(&mut out, "TOTAL", &m(doc.totals.total), Style::Bold);
    if doc.totals.prices_include_tax {
        for t in &doc.totals.tax {
            pair(
                &mut out,
                &format!("  incl. {}", t.name),
                &m(t.amount),
                Style::Normal,
            );
        }
    }
    rule(&mut out);
    for t in &doc.tenders {
        pair(
            &mut out,
            &t.label,
            &m(t.received.unwrap_or(t.amount)),
            Style::Normal,
        );
        if t.external {
            plain(&mut out, "  Recorded by operator");
        }
        if let Some(r) = &t.reference {
            plain(&mut out, &format!("  Ref {r}"));
        }
    }
    if doc.change.minor() != 0 {
        pair(&mut out, "Change", &m(doc.change), Style::Bold);
    }
    if !doc.footer.is_empty() {
        rule(&mut out);
        for l in &doc.footer {
            centered(&mut out, l);
        }
    }
    centered(&mut out, &doc.barcode);
    for l in &mut out {
        if l.style == Style::Centered {
            l.text = center(&l.text, width);
        }
    }
    out
}

/// Left text and right-aligned amount on one line; the left side is cut to fit.
fn columns(left: &str, right: &str, width: usize) -> String {
    let rlen = right.chars().count();
    let room = width.saturating_sub(rlen + if right.is_empty() { 0 } else { 1 });
    let left: String = left.chars().take(room).collect();
    let pad = width - left.chars().count() - rlen;
    format!("{left}{}{right}", " ".repeat(pad))
}

fn center(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.to_owned();
    }
    format!("{}{s}", " ".repeat((width - len) / 2))
}

/// Word wrap keeping the leading indent on every line; words longer than the width are split.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let indent: String = s.chars().take_while(|c| *c == ' ').collect();
    let body = &s[indent.len()..];
    let room = width.saturating_sub(indent.len()).max(1);
    wrap_words(body, room)
        .into_iter()
        .map(|l| format!("{indent}{l}"))
        .collect()
}

fn wrap_words(s: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while word.len() > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            lines.push(word.drain(..width).collect());
        }
        let word: String = word.into_iter().collect();
        let need = cur.chars().count() + usize::from(!cur.is_empty()) + word.chars().count();
        if need > width && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// The rendered lines as plain text, e.g. for a `.txt` capture or a golden file.
pub fn to_plain_text(lines: &[TextLine]) -> String {
    let mut s = String::new();
    for l in lines {
        s.push_str(l.text.trim_end());
        s.push('\n');
    }
    s
}

/// ESC/POS bytes for the rendered lines (alignment and bold from the style).
pub fn to_escpos(lines: &[TextLine]) -> Vec<u8> {
    use crate::escpos::{Cut, EscPos};
    let mut p = EscPos::new();
    for l in lines {
        p.bold(l.style == Style::Bold);
        p.line(&l.text);
    }
    p.bold(false).feed(3).cut(Cut::Partial);
    p.into_bytes()
}
