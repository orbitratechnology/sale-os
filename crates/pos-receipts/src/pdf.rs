//! PDF receipts (E13-T04, RCP-003, ADR-014): receipt-width (58/80 mm continuous paper) and
//! A4 invoice-style documents, rendered in Rust with fonts embedded in the binary, so what
//! a customer gets never depends on the fonts of the computer that made it.
//!
//! Fonts (SIL Open Font License, `fonts/OFL.txt`): Noto Sans Mono for the receipt-width
//! layout (the same lines as the preview and the printer), Noto Sans for A4, and fallbacks
//! for Sinhala, Tamil, Arabic, Devanagari, Chinese/Japanese (GB 2312 + JIS X 0208 subset)
//! and Korean (KS X 1001 Hangul subset). Each run of text is shaped with rustybuzz in the font that
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

use crate::document::{CopyKind, DocumentKind, ReceiptDocument, ZSection, amount, tender_label};
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
pub(crate) enum FontId {
    Mono,
    MonoBold,
    Sans,
    SansBold,
    Sinhala,
    Tamil,
    Arabic,
    Devanagari,
    Cjk,
    Hangul,
}

const FALLBACK: [FontId; 8] = [
    FontId::Sinhala,
    FontId::Tamil,
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
            FontId::Sinhala => include_bytes!("../fonts/NotoSansSinhala-Regular.ttf"),
            FontId::Tamil => include_bytes!("../fonts/NotoSansTamil-Regular.ttf"),
            FontId::Arabic => include_bytes!("../fonts/NotoSansArabic-Regular.ttf"),
            FontId::Devanagari => include_bytes!("../fonts/NotoSansDevanagari-Regular.ttf"),
            FontId::Cjk => include_bytes!("../fonts/NotoSansCJK-Subset.ttf"),
            FontId::Hangul => include_bytes!("../fonts/NotoSansKR-Subset.ttf"),
        }
    }
}

pub(crate) struct Loaded {
    pub(crate) shaper: Face<'static>,
    pdf: PdfFont,
}

static FONTS: LazyLock<Vec<(FontId, Loaded)>> = LazyLock::new(|| {
    [
        FontId::Mono,
        FontId::MonoBold,
        FontId::Sans,
        FontId::SansBold,
        FontId::Sinhala,
        FontId::Tamil,
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

pub(crate) fn font(id: FontId) -> &'static Loaded {
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
pub(crate) struct Run {
    pub(crate) font: FontId,
    pub(crate) text: String,
    glyphs: Vec<KrillaGlyph>,
    /// The same glyphs, for drawing outlines (the raster renderer).
    pub(crate) placed: Vec<Placed>,
    /// Advance in ems.
    pub(crate) advance: f32,
    /// Receipt columns the run takes.
    pub(crate) columns: usize,
}

/// A shaped glyph: its id and its advance and offsets in ems.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Placed {
    pub(crate) id: u16,
    pub(crate) x_advance: f32,
    pub(crate) x_offset: f32,
    pub(crate) y_offset: f32,
}

/// Whether `c` is drawn by a fallback script font (Sinhala, Tamil, Arabic, Devanagari,
/// CJK, Hangul) that neither base font covers: a receipt printer's code pages have none of
/// these, and transliteration would lose them, so such a line prints as an image.
pub(crate) fn needs_script_font(c: char) -> bool {
    !c.is_control()
        && !c.is_whitespace()
        && !covers(FontId::Mono, c)
        && !covers(FontId::Sans, c)
        && FALLBACK
            .into_iter()
            .any(|f| !matches!(f, FontId::Mono | FontId::Sans) && covers(f, c))
}

/// Splits `text` into runs by font (a run keeps going while its font covers the next
/// character, so spaces and digits stay inside an Arabic or CJK run) and shapes each.
pub(crate) fn runs(text: &str, base: FontId) -> Vec<Run> {
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
            let (glyphs, placed, advance) = shape(f, &s);
            let columns = s.chars().map(char_columns).sum();
            Run {
                font: f,
                text: s,
                glyphs,
                placed,
                advance,
                columns,
            }
        })
        .collect()
}

/// Shapes `text` in one font (as krilla's own simple shaping does, with the cluster
/// ranges it needs for the ToUnicode map). Returns the glyphs, the same glyphs as plain
/// positions, and the advance in ems.
fn shape(id: FontId, text: &str) -> (Vec<KrillaGlyph>, Vec<Placed>, f32) {
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
    let mut placed = Vec::with_capacity(infos.len());
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
        placed.push(Placed {
            id: u16::try_from(info.glyph_id).unwrap_or(0),
            x_advance,
            x_offset: pos.x_offset as f32 / upem,
            y_offset: pos.y_offset as f32 / upem,
        });
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
    (glyphs, placed, advance)
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
    /// The business logo: top-left corner at (`x`, `y`), `w` × `h` points.
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
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
    render_pdf_with_logo(doc, format, None)
}

/// As [`render_pdf`], with the business logo at the top of sale and refund receipts
/// (SET-001). A Z summary has none.
pub fn render_pdf_with_logo(
    doc: &ReceiptDocument,
    format: PdfFormat,
    logo: Option<&crate::logo::Logo>,
) -> Result<Vec<u8>, PdfError> {
    let logo = logo.filter(|l| l.width > 0 && l.height > 0 && doc.kind != DocumentKind::CashSession);
    let layout = match format {
        PdfFormat::Receipt58 => roll(doc, 58.0, COLUMNS_58MM, logo),
        PdfFormat::Receipt80 => roll(doc, 80.0, COLUMNS_80MM, logo),
        // A Z summary is laid out from its own section: session, takings and the count.
        PdfFormat::A4 if doc.kind == DocumentKind::CashSession => match &doc.z {
            Some(z) => a4_z(doc, z),
            None => a4_grid(doc),
        },
        // The tax invoice is laid out as text on A4 (its particulars in the order the text
        // renderer gives them, which follows the gazette).
        PdfFormat::A4 if doc.kind == DocumentKind::TaxInvoice => a4_grid(doc),
        PdfFormat::A4 => a4_invoice(doc, logo),
    };
    draw(&layout, doc, logo)
}

/// The logo's size in points inside a `max_w` × `max_h` box, keeping its shape.
fn fit(logo: &crate::logo::Logo, max_w: f32, max_h: f32) -> (f32, f32) {
    let ratio = logo.height as f32 / logo.width as f32;
    let w = max_w.min(max_h / ratio);
    (w, w * ratio)
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
fn roll(doc: &ReceiptDocument, paper_mm: f32, columns: usize, logo: Option<&crate::logo::Logo>) -> Layout {
    let lines = text::render(doc, columns);
    let cell = 1.5 * MM;
    let size = mono_size(cell);
    let line_h = size * 1.35;
    let margin_x = (paper_mm * MM - columns as f32 * cell) / 2.0;
    let margin_y = 4.0 * MM;
    // The logo, centred above the first line, at most 40 × 25 mm.
    let image = logo.map(|l| {
        let (w, h) = fit(l, (columns as f32 * cell).min(40.0 * MM), 25.0 * MM);
        (
            Op::Image {
                x: (paper_mm * MM - w) / 2.0,
                y: margin_y,
                w,
                h,
            },
            h + 2.0 * MM,
        )
    });
    let offset = image.as_ref().map_or(0.0, |(_, dy)| *dy);
    let per_page = (((MAX_PAGE_HEIGHT - 2.0 * margin_y - offset) / line_h) as usize).max(1);
    let mut pages: Vec<Vec<Op>> = lines
        .chunks(per_page)
        .enumerate()
        .map(|(page, chunk)| {
            let top = margin_y + if page == 0 { offset } else { 0.0 };
            chunk
                .iter()
                .enumerate()
                .map(|(i, l)| Op::Grid {
                    x: margin_x,
                    y: top + (i as f32 + 1.0) * line_h - line_h * 0.25,
                    text: l.text.trim_end().to_owned(),
                    bold: l.style == Style::Bold,
                    cell,
                    size,
                })
                .collect()
        })
        .collect();
    if let (Some((op, _)), Some(first)) = (image, pages.first_mut()) {
        first.insert(0, op);
    }
    let longest = lines.len().min(per_page);
    Layout {
        page: (
            paper_mm * MM,
            2.0 * margin_y + offset + longest.max(1) as f32 * line_h,
        ),
        pages,
    }
}

/// A document on A4 as its 48-column lines, larger, on as many pages as needed (the tax
/// invoice, and a Z summary stored without its section).
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
fn a4_invoice(doc: &ReceiptDocument, logo: Option<&crate::logo::Logo>) -> Layout {
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
    // The logo above the shop's name, at most 50 × 20 mm.
    if let Some(l) = logo {
        let (w, h) = fit(l, 50.0 * MM, 20.0 * MM);
        if let Some(page) = p.pages.last_mut() {
            page.push(Op::Image {
                x: left,
                y: p.y,
                w,
                h,
            });
        }
        p.y += h + 4.0 * MM;
    }
    a4_header(&mut p, doc, &info);

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

/// The A4 header: the shop on the left, `info` (the document's title and details) on the
/// right; leaves `p.y` below both.
fn a4_header(p: &mut Pager, doc: &ReceiptDocument, info: &[(String, FontId, f32)]) {
    let (left, right) = (p.left, p.right);
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
}

/// One row of a Z summary table on A4.
struct ZRow {
    label: String,
    count: Option<u32>,
    value: String,
    /// Indented under the row above (a part of it, such as the tax in the sales).
    sub: bool,
    bold: bool,
}

impl ZRow {
    fn new(label: &str, count: Option<u32>, value: String) -> Self {
        Self {
            label: label.to_owned(),
            count,
            value,
            sub: false,
            bold: false,
        }
    }
    fn sub(mut self) -> Self {
        self.sub = true;
        self
    }
    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
}

/// A Z summary on A4, laid out from its [`ZSection`]: the shop and the session on top, then
/// tables for the takings, the payments taken, the refunds paid and the cash count, each
/// amount right-aligned in one column and each count in another. It states the same figures
/// as the printed summary.
fn a4_z(doc: &ReceiptDocument, z: &ZSection) -> Layout {
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
    let m = |v: pos_domain::Money| amount(v, exp);
    let neg = |v: pos_domain::Money| {
        if v.minor() == 0 {
            m(v)
        } else {
            format!("-{}", m(v))
        }
    };
    let (left, right) = (p.left, p.right);

    let mut info: Vec<(String, FontId, f32)> = vec![("Z summary".into(), FontId::SansBold, 14.0)];
    info.push((format!("No. {}", doc.meta.receipt_number), FontId::Sans, BODY));
    info.push((
        format!("{} ({})", doc.meta.register_name, doc.meta.register_code),
        FontId::Sans,
        BODY,
    ));
    if doc.copy == CopyKind::Reprint {
        info.push(("REPRINT".into(), FontId::SansBold, BODY));
    }
    if doc.sandbox {
        info.push(("SANDBOX - PRACTICE".into(), FontId::SansBold, BODY));
    }
    a4_header(&mut p, doc, &info);

    // The session: when it opened and closed, and who by.
    let when_x = left + 30.0 * MM;
    p.text(left, "Session", FontId::SansBold, 11.0, Align::Left);
    p.y += 4.0;
    p.rule();
    p.y += LINE;
    for (what, time, by) in [
        ("Opened", &z.opened_local, &z.opened_by),
        ("Closed", &doc.meta.local_time, &doc.meta.cashier_name),
    ] {
        p.text(left, what, FontId::Sans, BODY, Align::Left);
        p.text(when_x, &format!("{time} by {by}"), FontId::Sans, BODY, Align::Left);
        p.y += LINE;
    }
    p.y += LINE;

    // Tables: label, count, amount.
    let count_x = left + 130.0 * MM;
    let table = |p: &mut Pager, title: &str, counts: bool, rows: &[ZRow]| {
        // A table is kept on one page (it moves to the next whole).
        p.room(LINE + 4.0 + rows.len() as f32 * (LINE + 1.0));
        p.text(left, title, FontId::SansBold, 11.0, Align::Left);
        if counts {
            p.text(count_x, "Count", FontId::SansBold, BODY, Align::Right);
        }
        p.text(right, "Amount", FontId::SansBold, BODY, Align::Right);
        p.y += 4.0;
        p.rule();
        p.y += LINE;
        for r in rows {
            let (f, size) = if r.bold {
                (FontId::SansBold, 11.0)
            } else {
                (FontId::Sans, BODY)
            };
            let x = if r.sub { left + 5.0 * MM } else { left };
            p.text(x, &r.label, f, size, Align::Left);
            if let Some(c) = r.count {
                p.text(count_x, &c.to_string(), f, size, Align::Right);
            }
            if !r.value.is_empty() {
                p.text(right, &r.value, f, size, Align::Right);
            }
            p.y += LINE + if r.bold { 1.0 } else { 0.0 };
        }
        p.y += LINE;
    };

    let t = &z.totals;
    let mut takings = vec![
        ZRow::new("Sales", Some(t.sales_count), m(t.sales_total)),
        ZRow::new("incl. tax", None, m(t.tax_total)).sub(),
    ];
    if t.discount_total.minor() != 0 {
        takings.push(ZRow::new("Discounts given", None, neg(t.discount_total)).sub());
    }
    if t.rounding_total.minor() != 0 {
        takings.push(ZRow::new("Cash rounding", None, m(t.rounding_total)).sub());
    }
    takings.push(ZRow::new("Returns", Some(t.returns_count), neg(t.refunds_total)));
    table(&mut p, "Takings", true, &takings);

    let mut taken: Vec<ZRow> = t
        .tenders
        .iter()
        .map(|k| ZRow::new(tender_label(k.kind), Some(k.count), m(k.amount)))
        .collect();
    if taken.is_empty() {
        taken.push(ZRow::new("None", None, String::new()));
    }
    table(&mut p, "Payments taken", true, &taken);
    if !t.refunds.is_empty() {
        let paid: Vec<ZRow> = t
            .refunds
            .iter()
            .map(|k| ZRow::new(tender_label(k.kind), Some(k.count), neg(k.amount)))
            .collect();
        table(&mut p, "Refunds paid", true, &paid);
    }

    let cash_in = t.cash_in().unwrap_or_default();
    let cash_out = t.cash_out().unwrap_or_default();
    let (variance_label, variance) = match z.variance.minor().signum() {
        0 => ("Variance", m(z.variance)),
        1 => ("Variance (over)", format!("+{}", m(z.variance))),
        _ => ("Variance (short)", m(z.variance)),
    };
    table(
        &mut p,
        "Cash in the drawer",
        false,
        &[
            ZRow::new("Opening float", None, m(z.opening_float)),
            ZRow::new("+ Cash from sales", None, m(cash_in)),
            ZRow::new("- Cash refunds", None, neg(cash_out)),
            ZRow::new("Expected cash", None, m(z.expected)).bold(),
            ZRow::new("Counted cash", None, m(z.counted)).bold(),
            ZRow::new(variance_label, None, variance).bold(),
        ],
    );

    if let Some(note) = &z.note {
        let rows = wrap_width(note, FontId::Sans, BODY, right - left);
        p.room(LINE * (rows.len() as f32 + 1.0));
        p.text(left, "Note", FontId::SansBold, BODY, Align::Left);
        p.y += LINE;
        for r in &rows {
            p.text(left, r, FontId::Sans, BODY, Align::Left);
            p.y += LINE;
        }
        p.y += LINE;
    }

    p.room(LINE);
    p.text((left + right) / 2.0, &doc.barcode, FontId::Sans, BODY, Align::Center);
    Layout {
        page: A4,
        pages: p.pages,
    }
}

// ---------- drawing ----------

fn draw(
    layout: &Layout,
    doc: &ReceiptDocument,
    logo: Option<&crate::logo::Logo>,
) -> Result<Vec<u8>, PdfError> {
    let image = logo
        .map(|l| {
            krilla::image::Image::from_png(krilla::Data::from(l.png.clone()), true)
                .map_err(|e| PdfError::Write(format!("logo: {e}")))
        })
        .transpose()?;
    let mut document = Document::new();
    let title = match doc.kind {
        DocumentKind::Sale => format!("Receipt {}", doc.meta.receipt_number),
        DocumentKind::Return => format!("Refund receipt {}", doc.meta.receipt_number),
        DocumentKind::CashSession => format!("Z summary {}", doc.meta.receipt_number),
        DocumentKind::TaxInvoice => format!("Tax invoice {}", doc.meta.receipt_number),
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
                Op::Image { x, y, w, h } => {
                    if let (Some(img), Some(size)) = (&image, Size::from_wh(*w, *h)) {
                        surface.push_transform(&Transform::from_translate(*x, *y));
                        surface.draw_image(img.clone(), size);
                        surface.pop();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// On the receipt grid a Sinhala or Tamil name fits the columns the layout gives it
    /// (`char_columns`), so it is drawn at full size, not squeezed.
    #[test]
    fn sinhala_and_tamil_fit_their_columns_on_the_grid() {
        // The grid's cell is the monospace advance (0.6 em) at the grid's font size.
        let mono = &font(FontId::Mono).shaper;
        let zero = mono.glyph_index('0').expect("zero");
        let cell = f32::from(mono.glyph_hor_advance(zero).expect("advance")) / mono.units_per_em() as f32;
        for word in [
            "සීනි",
            "පාන්",
            "කිරි",
            "බිත්තර",
            "තේ කොළ",
            "පොල් තෙල්",
            "සබන්",
            "சர்க்கரை",
            "பால்",
            "முட்டை",
            "தேநீர்",
            "சோப்பு",
            "தேங்காய் எண்ணெய்",
            "ரொட்டி",
        ] {
            for r in runs(word, FontId::Mono) {
                let room = r.columns as f32 * cell;
                assert!(
                    r.advance <= room + 1e-3,
                    "{word}: run {:?} is {:.2} em for {} columns ({room:.2} em)",
                    r.text,
                    r.advance,
                    r.columns
                );
            }
        }
    }
}
