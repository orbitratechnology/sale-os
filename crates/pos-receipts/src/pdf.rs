//! PDF receipts (E13-T04, RCP-003, ADR-014): receipt-width (58/80 mm continuous paper) and
//! A4 invoice-style documents, rendered in Rust with fonts embedded in the binary, so what
//! a customer gets never depends on the fonts of the computer that made it.
//!
//! Fonts (SIL Open Font License, `fonts/OFL.txt`): Noto Sans Mono for the receipt-width
//! layout (the same lines as the preview and the printer), Noto Sans for A4, and fallbacks
//! for Arabic, Devanagari, Chinese/Japanese (GB 2312 + JIS X 0208 subset) and Korean
//! (KS X 1001 Hangul subset). Each run of text is shaped with rustybuzz in the font that
//! covers it, so Arabic letters join and Devanagari forms conjuncts; only the glyphs used
//! are embedded (subset) with a ToUnicode map, so the text can be copied and searched.
//!
//! Limits: no bidirectional reordering inside a line (a run of Arabic is shaped and laid
//! out right to left, but runs are placed in logical order), so a line mixing Arabic and
//! Latin words may read out of order (full RTL is a D-02 follow-up). Characters outside
//! the embedded sets are reported by [`missing_glyphs`].

// Page geometry is in points (f32, as PDF and the shaper use). No money passes through it:
// amounts are formatted from `Money` into text before layout.
#![allow(clippy::disallowed_types)]

use crate::document::{CopyKind, DocumentKind, ReceiptDocument, amount};
use crate::text::{self, COLUMNS_58MM, COLUMNS_80MM, Style, char_columns};
use krilla::Document;
use krilla::geom::{PathBuilder, Point, Size, Transform};
use krilla::page::PageSettings;
use krilla::paint::{Fill, Stroke};
use krilla::text::{Font as PdfFont, GlyphId, KrillaGlyph};
use rustybuzz::{Direction, Face, UnicodeBuffer};
use std::sync::LazyLock;

/// Points per millimetre.
pub const MM: f32 = 72.0 / 25.4;
/// A4 in points.
pub const A4: (f32, f32) = (210.0 * MM, 297.0 * MM);
/// The tallest page a PDF reader is guaranteed to open (200 inches); a longer receipt is
/// split across pages.
const MAX_PAGE_HEIGHT: f32 = 14_400.0;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PdfFormat {
    /// 58 mm roll: the 32-column receipt.
    Receipt58,
    /// 80 mm roll: the 48-column receipt.
    Receipt80,
    /// A4 page, invoice style.
    A4,
}

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("the PDF could not be written: {0}")]
    Write(String),
}

/// The embedded fonts, in fallback order after the base font.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum FontId {
    Mono,
    MonoBold,
    Sans,
    SansBold,
    Arabic,
    Devanagari,
    Cjk,
    Hangul,
}

const FALLBACK: [FontId; 6] = [
    FontId::Arabic,
    FontId::Devanagari,
    FontId::Cjk,
    FontId::Hangul,
    FontId::Sans,
    FontId::Mono,
];

impl FontId {
    fn bytes(self) -> &'static [u8] {
        match self {
            FontId::Mono => include_bytes!("../fonts/NotoSansMono-Regular.ttf"),
            FontId::MonoBold => include_bytes!("../fonts/NotoSansMono-Bold.ttf"),
            FontId::Sans => include_bytes!("../fonts/NotoSans-Regular.ttf"),
            FontId::SansBold => include_bytes!("../fonts/NotoSans-Bold.ttf"),
            FontId::Arabic => include_bytes!("../fonts/NotoSansArabic-Regular.ttf"),
            FontId::Devanagari => include_bytes!("../fonts/NotoSansDevanagari-Regular.ttf"),
            FontId::Cjk => include_bytes!("../fonts/NotoSansCJK-Subset.ttf"),
            FontId::Hangul => include_bytes!("../fonts/NotoSansKR-Subset.ttf"),
        }
    }
}

struct Loaded {
    shaper: Face<'static>,
    pdf: PdfFont,
}

static FONTS: LazyLock<Vec<(FontId, Loaded)>> = LazyLock::new(|| {
    [
        FontId::Mono,
        FontId::MonoBold,
        FontId::Sans,
        FontId::SansBold,
        FontId::Arabic,
        FontId::Devanagari,
        FontId::Cjk,
        FontId::Hangul,
    ]
    .into_iter()
    .map(|id| {
        let bytes = id.bytes();
        let shaper = Face::from_slice(bytes, 0).expect("embedded font parses");
        let pdf = PdfFont::new(bytes.into(), 0).expect("embedded font loads");
        (id, Loaded { shaper, pdf })
    })
    .collect()
});

fn font(id: FontId) -> &'static Loaded {
    &FONTS
        .iter()
        .find(|(f, _)| *f == id)
        .expect("every font is loaded")
        .1
}

fn covers(id: FontId, c: char) -> bool {
    font(id).shaper.glyph_index(c).is_some()
}

/// The font that draws `c`: the base font when it can, else the first fallback that can.
fn font_for(base: FontId, c: char) -> Option<FontId> {
    if covers(base, c) {
        return Some(base);
    }
    FALLBACK.into_iter().find(|f| covers(*f, c))
}

/// Characters of `text` that no embedded font can draw (they would print as empty boxes).
/// Controls and whitespace are ignored.
pub fn missing_glyphs(text: &str) -> Vec<char> {
    let mut out: Vec<char> = text
        .chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .filter(|c| font_for(FontId::Sans, *c).is_none())
        .collect();
    out.dedup();
    out
}

/// A run of text in one font, shaped.
struct Run {
    font: FontId,
    text: String,
    glyphs: Vec<KrillaGlyph>,
    /// Advance in ems.
    advance: f32,
    /// Receipt columns the run takes.
    columns: usize,
}

/// Splits `text` into runs by font (a run keeps going while its font covers the next
/// character, so spaces and digits stay inside an Arabic or CJK run) and shapes each.
fn runs(text: &str, base: FontId) -> Vec<Run> {
    let mut parts: Vec<(FontId, String)> = Vec::new();
    for c in text.chars() {
        match parts.last_mut() {
            Some((f, s)) if covers(*f, c) => s.push(c),
            _ => {
                let f = font_for(base, c).unwrap_or(base);
                match parts.last_mut() {
                    Some((last, s)) if *last == f => s.push(c),
                    _ => parts.push((f, c.to_string())),
                }
            }
        }
    }
    parts
        .into_iter()
        .map(|(f, s)| {
            let (glyphs, advance) = shape(f, &s);
            let columns = s.chars().map(char_columns).sum();
            Run {
                font: f,
                text: s,
                glyphs,
                advance,
                columns,
            }
        })
        .collect()
}

/// Shapes `text` in one font (as krilla's own simple shaping does, with the cluster
/// ranges it needs for the ToUnicode map). Returns the glyphs and the advance in ems.
fn shape(id: FontId, text: &str) -> (Vec<KrillaGlyph>, f32) {
    let face = &font(id).shaper;
    let upem = face.units_per_em() as f32;
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let ltr = matches!(
        buffer.direction(),
        Direction::LeftToRight | Direction::TopToBottom
    );
    let output = rustybuzz::shape(face, &[], buffer);
    let positions = output.glyph_positions();
    let infos = output.glyph_infos();
    let mut glyphs = Vec::with_capacity(infos.len());
    let mut advance = 0.0;
    for (i, (info, pos)) in infos.iter().zip(positions).enumerate() {
        let start = info.cluster as usize;
        // The cluster ends where the next cluster (in text order) starts.
        let next = if ltr {
            infos[i + 1..].iter().find(|x| x.cluster != info.cluster)
        } else {
            infos[..i].iter().rev().find(|x| x.cluster != info.cluster)
        };
        let end = next.map_or(text.len(), |x| x.cluster as usize);
        let x_advance = pos.x_advance as f32 / upem;
        advance += x_advance;
        glyphs.push(KrillaGlyph::new(
            GlyphId::new(info.glyph_id),
            x_advance,
            pos.x_offset as f32 / upem,
            pos.y_offset as f32 / upem,
            pos.y_advance as f32 / upem,
            start..end.max(start),
            None,
        ));
    }
    (glyphs, advance)
}

/// Width of `text` in points at `size`.
fn width(text: &str, base: FontId, size: f32) -> f32 {
    runs(text, base).iter().map(|r| r.advance * size).sum()
}

// ---------- layout ----------

#[derive(Copy, Clone, Debug, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

/// One thing to draw. `y` is the baseline, from the top of the page.
#[derive(Clone, Debug)]
enum Op {
    Text {
        x: f32,
        y: f32,
        text: String,
        font: FontId,
        size: f32,
        align: Align,
    },
    /// A monospace line laid out on the receipt's column grid.
    Grid {
        x: f32,
        y: f32,
        text: String,
        bold: bool,
        cell: f32,
        size: f32,
    },
    Rule {
        x1: f32,
        x2: f32,
        y: f32,
    },
}

struct Layout {
    page: (f32, f32),
    pages: Vec<Vec<Op>>,
}

/// The receipt PDF: `format` chooses the paper. The document is the stored one, so the PDF
/// says exactly what the printed receipt said.
pub fn render_pdf(doc: &ReceiptDocument, format: PdfFormat) -> Result<Vec<u8>, PdfError> {
    let layout = match format {
        PdfFormat::Receipt58 => roll(doc, 58.0, COLUMNS_58MM),
        PdfFormat::Receipt80 => roll(doc, 80.0, COLUMNS_80MM),
        PdfFormat::A4 if doc.kind == DocumentKind::CashSession => a4_grid(doc),
        PdfFormat::A4 => a4_invoice(doc),
    };
    draw(&layout, doc)
}

/// Font size at which a Noto Sans Mono character is exactly `cell` points wide.
fn mono_size(cell: f32) -> f32 {
    let face = &font(FontId::Mono).shaper;
    let adv = face
        .glyph_index('0')
        .and_then(|g| face.glyph_hor_advance(g))
        .unwrap_or(600);
    cell * face.units_per_em() as f32 / f32::from(adv)
}

/// Continuous paper: the same lines as the preview and the printer, 1.5 mm per column,
/// one page as long as the receipt.
fn roll(doc: &ReceiptDocument, paper_mm: f32, columns: usize) -> Layout {
    let lines = text::render(doc, columns);
    let cell = 1.5 * MM;
    let size = mono_size(cell);
    let line_h = size * 1.35;
    let margin_x = (paper_mm * MM - columns as f32 * cell) / 2.0;
    let margin_y = 4.0 * MM;
    let per_page = (((MAX_PAGE_HEIGHT - 2.0 * margin_y) / line_h) as usize).max(1);
    let pages: Vec<Vec<Op>> = lines
        .chunks(per_page)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .map(|(i, l)| Op::Grid {
                    x: margin_x,
                    y: margin_y + (i as f32 + 1.0) * line_h - line_h * 0.25,
                    text: l.text.trim_end().to_owned(),
                    bold: l.style == Style::Bold,
                    cell,
                    size,
                })
                .collect()
        })
        .collect();
    let longest = lines.len().min(per_page);
    Layout {
        page: (paper_mm * MM, 2.0 * margin_y + longest.max(1) as f32 * line_h),
        pages,
    }
}

/// A Z summary on A4: the 48-column lines, larger, on as many pages as needed.
fn a4_grid(doc: &ReceiptDocument) -> Layout {
    let lines = text::render(doc, COLUMNS_80MM);
    let cell = 2.6 * MM;
    let size = mono_size(cell);
    let line_h = size * 1.4;
    let margin_x = (A4.0 - COLUMNS_80MM as f32 * cell) / 2.0;
    let top = 20.0 * MM;
    let per_page = (((A4.1 - 2.0 * top) / line_h) as usize).max(1);
    Layout {
        page: A4,
        pages: lines
            .chunks(per_page)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .map(|(i, l)| Op::Grid {
                        x: margin_x,
                        y: top + (i as f32 + 1.0) * line_h,
                        text: l.text.trim_end().to_owned(),
                        bold: l.style == Style::Bold,
                        cell,
                        size,
                    })
                    .collect()
            })
            .collect(),
    }
}

/// Builds A4 pages top to bottom, starting a new page when the next block does not fit.
struct Pager {
    pages: Vec<Vec<Op>>,
    y: f32,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

impl Pager {
    fn ops(&mut self) -> &mut Vec<Op> {
        self.pages.last_mut().expect("there is always a page")
    }
    /// Makes room for `h` points, on a new page if needed. Returns true on a new page.
    fn room(&mut self, h: f32) -> bool {
        if self.y + h <= self.bottom {
            return false;
        }
        self.pages.push(Vec::new());
        self.y = self.top;
        true
    }
    fn text(&mut self, x: f32, text: &str, font: FontId, size: f32, align: Align) {
        let y = self.y;
        self.ops().push(Op::Text {
            x,
            y,
            text: text.to_owned(),
            font,
            size,
            align,
        });
    }
    fn rule(&mut self) {
        let (x1, x2, y) = (self.left, self.right, self.y);
        self.ops().push(Op::Rule { x1, x2, y });
    }
}

/// Word wrap by measured width; a word wider than the line is cut by characters.
fn wrap_width(text: &str, font: FontId, size: f32, max: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let candidate = if cur.is_empty() {
            word.to_owned()
        } else {
            format!("{cur} {word}")
        };
        if width(&candidate, font, size) <= max {
            cur = candidate;
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        for c in word.chars() {
            let mut next = cur.clone();
            next.push(c);
            if width(&next, font, size) > max && !cur.is_empty() {
                lines.push(std::mem::replace(&mut cur, c.to_string()));
            } else {
                cur = next;
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

const BODY: f32 = 9.5;
const LINE: f32 = 13.0;

/// A sale or refund as an A4 invoice: shop and receipt details, a table of items, totals,
/// payments and the footer.
fn a4_invoice(doc: &ReceiptDocument) -> Layout {
    let margin = 18.0 * MM;
    let mut p = Pager {
        pages: vec![Vec::new()],
        y: margin,
        left: margin,
        right: A4.0 - margin,
        top: margin,
        bottom: A4.1 - margin,
    };
    let exp = doc.currency_exponent;
    let m = |v| amount(v, exp);
    let refund = doc.kind == DocumentKind::Return;
    let (left, right) = (p.left, p.right);

    // Header: the shop on the left, the receipt on the right.
    let mut shop: Vec<(String, FontId, f32)> =
        vec![(doc.header.business_name.clone(), FontId::SansBold, 14.0)];
    shop.extend(
        doc.header
            .address_lines
            .iter()
            .map(|l| (l.clone(), FontId::Sans, BODY)),
    );
    if let Some(ph) = &doc.header.phone {
        shop.push((format!("Tel {ph}"), FontId::Sans, BODY));
    }
    if let Some(t) = &doc.header.tax_registration_no {
        shop.push((format!("Tax no. {t}"), FontId::Sans, BODY));
    }
    shop.extend(doc.header.lines.iter().map(|l| (l.clone(), FontId::Sans, BODY)));
    let mut info: Vec<(String, FontId, f32)> = vec![(
        if refund { "Refund receipt" } else { "Receipt" }.to_owned(),
        FontId::SansBold,
        14.0,
    )];
    info.push((format!("No. {}", doc.meta.receipt_number), FontId::Sans, BODY));
    if let Some(o) = &doc.original_receipt {
        info.push((format!("Original receipt {o}"), FontId::Sans, BODY));
    }
    info.push((doc.meta.local_time.clone(), FontId::Sans, BODY));
    info.push((
        format!("{} ({})", doc.meta.register_name, doc.meta.register_code),
        FontId::Sans,
        BODY,
    ));
    info.push((format!("Served by {}", doc.meta.cashier_name), FontId::Sans, BODY));
    if doc.copy == CopyKind::Reprint {
        info.push(("REPRINT".into(), FontId::SansBold, BODY));
    }
    if doc.sandbox {
        info.push(("SANDBOX - PRACTICE SALE".into(), FontId::SansBold, BODY));
    }
    let top = p.y;
    for (i, (t, f, s)) in shop.iter().enumerate() {
        p.y = top + 14.0 + i as f32 * LINE + if i > 0 { 4.0 } else { 0.0 };
        p.text(left, t, *f, *s, Align::Left);
    }
    let shop_end = p.y;
    for (i, (t, f, s)) in info.iter().enumerate() {
        p.y = top + 14.0 + i as f32 * LINE + if i > 0 { 4.0 } else { 0.0 };
        p.text(right, t, *f, *s, Align::Right);
    }
    p.y = p.y.max(shop_end) + 10.0 * MM;

    // Items.
    let col_qty = left + 104.0 * MM;
    let col_unit = left + 128.0 * MM;
    let col_disc = left + 150.0 * MM;
    let item_w = 98.0 * MM;
    let head = |p: &mut Pager| {
        p.text(left, "Item", FontId::SansBold, BODY, Align::Left);
        p.text(
            col_qty,
            if refund { "Qty returned" } else { "Qty" },
            FontId::SansBold,
            BODY,
            Align::Right,
        );
        if !refund {
            p.text(col_unit, "Unit price", FontId::SansBold, BODY, Align::Right);
            p.text(col_disc, "Discount", FontId::SansBold, BODY, Align::Right);
        }
        p.text(
            right,
            if refund { "Refund" } else { "Amount" },
            FontId::SansBold,
            BODY,
            Align::Right,
        );
        p.y += 4.0;
        p.rule();
        p.y += LINE;
    };
    head(&mut p);
    for l in &doc.lines {
        let name = if l.variant_label.is_empty() {
            l.name.clone()
        } else {
            format!("{} {}", l.name, l.variant_label)
        };
        let mut rows = wrap_width(&name, FontId::Sans, BODY, item_w);
        if let Some(label) = &l.discount_label {
            rows.push(if label.is_empty() {
                "Discount".into()
            } else {
                format!("Discount {label}")
            });
        }
        if p.room(rows.len() as f32 * LINE) {
            p.y += LINE;
            head(&mut p);
        }
        p.text(left, &rows[0], FontId::Sans, BODY, Align::Left);
        p.text(col_qty, &l.quantity, FontId::Sans, BODY, Align::Right);
        if refund {
            p.text(
                right,
                &format!("-{}", m(l.gross)),
                FontId::Sans,
                BODY,
                Align::Right,
            );
        } else {
            let unit = format!(
                "{}{}",
                m(l.unit_price),
                if l.price_overridden { " *" } else { "" }
            );
            p.text(col_unit, &unit, FontId::Sans, BODY, Align::Right);
            if l.discount_label.is_some() {
                p.text(
                    col_disc,
                    &format!("-{}", m(l.discount)),
                    FontId::Sans,
                    BODY,
                    Align::Right,
                );
            }
            let net = pos_domain::Money::from_minor(l.gross.minor() - l.discount.minor());
            p.text(right, &m(net), FontId::Sans, BODY, Align::Right);
        }
        for r in &rows[1..] {
            p.y += LINE;
            p.text(left + 3.0 * MM, r, FontId::Sans, BODY - 1.0, Align::Left);
        }
        p.y += LINE;
    }
    p.y -= LINE - 4.0;
    p.rule();
    p.y += LINE + 2.0;

    // Totals, then payments: label at the left of the block, amount at the right margin.
    let label_x = left + 100.0 * MM;
    let mut rows: Vec<(String, String, bool)> = Vec::new();
    rows.push(("Subtotal".into(), m(doc.totals.subtotal), false));
    if doc.totals.line_discounts.minor() != 0 {
        rows.push((
            "Item discounts".into(),
            format!("-{}", m(doc.totals.line_discounts)),
            false,
        ));
    }
    if doc.totals.order_discount.minor() != 0 {
        let label = match doc.totals.order_discount_label.as_deref() {
            Some(l) if !l.is_empty() => format!("Order discount {l}"),
            _ => "Order discount".into(),
        };
        rows.push((label, format!("-{}", m(doc.totals.order_discount)), false));
    }
    if !doc.totals.prices_include_tax {
        rows.extend(
            doc.totals
                .tax
                .iter()
                .map(|t| (t.name.clone(), m(t.amount), false)),
        );
    }
    if doc.totals.rounding.minor() != 0 {
        rows.push(("Rounding".into(), m(doc.totals.rounding), false));
    }
    rows.push((
        if refund { "Refund total" } else { "Total" }.into(),
        m(doc.totals.total),
        true,
    ));
    if doc.totals.prices_include_tax {
        rows.extend(
            doc.totals
                .tax
                .iter()
                .map(|t| (format!("incl. {}", t.name), m(t.amount), false)),
        );
    }
    rows.push((String::new(), String::new(), false));
    for t in &doc.tenders {
        let label = if refund {
            format!("Refunded: {}", t.label)
        } else {
            t.label.clone()
        };
        rows.push((label, m(t.received.unwrap_or(t.amount)), false));
        if t.external {
            rows.push(("Recorded by operator".into(), String::new(), false));
        }
        if let Some(r) = &t.reference {
            rows.push((format!("Ref {r}"), String::new(), false));
        }
    }
    if doc.change.minor() != 0 {
        rows.push(("Change".into(), m(doc.change), true));
    }
    p.room(rows.len() as f32 * LINE);
    for (label, value, bold) in &rows {
        let (f, s) = if *bold {
            (FontId::SansBold, 11.0)
        } else {
            (FontId::Sans, BODY)
        };
        if !label.is_empty() {
            p.text(label_x, label, f, s, Align::Left);
        }
        if !value.is_empty() {
            p.text(right, value, f, s, Align::Right);
        }
        p.y += LINE + if *bold { 2.0 } else { 0.0 };
    }

    // Footer.
    let mut tail: Vec<String> = doc.footer.clone();
    tail.push(doc.barcode.clone());
    p.y += LINE;
    p.room(tail.len() as f32 * LINE);
    let mid = (left + right) / 2.0;
    for l in &tail {
        p.text(mid, l, FontId::Sans, BODY, Align::Center);
        p.y += LINE;
    }
    Layout {
        page: A4,
        pages: p.pages,
    }
}

// ---------- drawing ----------

fn draw(layout: &Layout, doc: &ReceiptDocument) -> Result<Vec<u8>, PdfError> {
    let mut document = Document::new();
    let title = match doc.kind {
        DocumentKind::Sale => format!("Receipt {}", doc.meta.receipt_number),
        DocumentKind::Return => format!("Refund receipt {}", doc.meta.receipt_number),
        DocumentKind::CashSession => format!("Z summary {}", doc.meta.receipt_number),
    };
    document.set_metadata(
        krilla::metadata::Metadata::new()
            .title(title)
            .creator("Universal Retail POS".into()),
    );
    let size =
        Size::from_wh(layout.page.0, layout.page.1).ok_or_else(|| PdfError::Write("empty page".into()))?;
    for ops in &layout.pages {
        let mut page = document.start_page_with(PageSettings::new(size));
        let mut surface = page.surface();
        for op in ops {
            match op {
                Op::Text {
                    x,
                    y,
                    text,
                    font,
                    size,
                    align,
                } => {
                    let runs = runs(text, *font);
                    let w: f32 = runs.iter().map(|r| r.advance * size).sum();
                    let mut cursor = match align {
                        Align::Left => *x,
                        Align::Right => x - w,
                        Align::Center => x - w / 2.0,
                    };
                    for r in &runs {
                        put(&mut surface, r, cursor, *y, *size, None);
                        cursor += r.advance * size;
                    }
                }
                Op::Grid {
                    x,
                    y,
                    text,
                    bold,
                    cell,
                    size,
                } => {
                    let base = if *bold { FontId::MonoBold } else { FontId::Mono };
                    let mut col = 0usize;
                    for r in runs(text, base) {
                        let room = r.columns as f32 * cell;
                        put(&mut surface, &r, x + col as f32 * cell, *y, *size, Some(room));
                        col += r.columns;
                    }
                }
                Op::Rule { x1, x2, y } => {
                    let mut b = PathBuilder::new();
                    b.move_to(*x1, *y);
                    b.line_to(*x2, *y);
                    if let Some(path) = b.finish() {
                        surface.set_fill(None);
                        surface.set_stroke(Some(Stroke {
                            width: 0.6,
                            ..Stroke::default()
                        }));
                        surface.draw_path(&path);
                        surface.set_stroke(None);
                    }
                }
            }
        }
        surface.finish();
        page.finish();
    }
    document.finish().map_err(|e| PdfError::Write(format!("{e:?}")))
}

/// Draws one run with its baseline at `y`. With `room`, a run wider than its columns (a
/// fallback font on the monospace grid) is narrowed to fit, so the amounts stay aligned.
fn put(surface: &mut krilla::surface::Surface<'_>, r: &Run, x: f32, y: f32, size: f32, room: Option<f32>) {
    if r.glyphs.is_empty() || r.text.trim().is_empty() {
        return;
    }
    surface.set_fill(Some(Fill::default()));
    let w = r.advance * size;
    let squeeze = room.filter(|room| w > *room && *room > 0.0).map(|room| room / w);
    if let Some(s) = squeeze {
        surface.push_transform(&Transform::from_row(s, 0.0, 0.0, 1.0, x * (1.0 - s), 0.0));
    }
    surface.draw_glyphs(
        Point::from_xy(x, y),
        &r.glyphs,
        font(r.font).pdf.clone(),
        &r.text,
        size,
        false,
    );
    if squeeze.is_some() {
        surface.pop();
    }
}
