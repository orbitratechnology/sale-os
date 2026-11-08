//! E13-T04 PDF goldens: each golden receipt rendered as a PDF, its text read back out of
//! the PDF (pdf-extract parses it; characters are grouped into lines by their position),
//! compared with the expected lines, and the page size and embedded fonts checked.
//! Regenerate the A4 goldens with `UPDATE_GOLDEN=1 cargo test -p pos-receipts --test pdf`.

// Page coordinates in points, as the PDF reader reports them; no money is involved.
#![allow(clippy::disallowed_types)]

use pos_receipts::ReceiptDocument;
use pos_receipts::pdf::{A4, MM, PdfFormat, missing_glyphs, render_pdf};
use pos_receipts::text::{COLUMNS_58MM, COLUMNS_80MM, render, text_columns};
use std::collections::BTreeSet;
use std::path::PathBuf;

const GOLDEN: [&str; 7] = [
    "sale",
    "discounts",
    "multi_rate",
    "split",
    "sandbox_reprint",
    "return",
    "z_summary",
];

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn doc(name: &str) -> ReceiptDocument {
    serde_json::from_str(&std::fs::read_to_string(golden_dir().join(format!("{name}.json"))).unwrap())
        .unwrap()
}

/// A page's size, and its characters as (baseline y, x, text) in drawing order.
type Page = ((f64, f64), Vec<(f64, f64, String)>);

/// Characters with their position, per page.
#[derive(Default)]
struct Lines {
    pages: Vec<Page>,
}

impl pdf_extract::OutputDev for Lines {
    fn begin_page(
        &mut self,
        _: u32,
        mb: &pdf_extract::MediaBox,
        _: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), pdf_extract::OutputError> {
        self.pages.push(((mb.urx - mb.llx, mb.ury - mb.lly), Vec::new()));
        Ok(())
    }
    fn end_page(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
    fn output_character(
        &mut self,
        trm: &pdf_extract::Transform,
        _: f64,
        _: f64,
        _: f64,
        c: &str,
    ) -> Result<(), pdf_extract::OutputError> {
        if let Some(p) = self.pages.last_mut() {
            p.1.push((trm.m32, trm.m31, c.to_owned()));
        }
        Ok(())
    }
    fn begin_word(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
    fn end_word(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
    fn end_line(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
}

/// The PDF's pages: size in points and the text lines, top to bottom, with each line's
/// characters left to right and gaps wider than a space turned into one space.
fn read(pdf: &[u8]) -> Vec<((f64, f64), Vec<String>)> {
    let doc = lopdf::Document::load_mem(pdf).expect("a valid PDF");
    let mut out = Lines::default();
    pdf_extract::output_doc(&doc, &mut out).expect("text extracts");
    out.pages
        .into_iter()
        .map(|(size, mut chars)| {
            // Top to bottom by baseline; within a line, the order it was drawn in (the
            // layout draws left to right). pdf-extract's x is unreliable after kerning
            // adjustments, so it is used only to see gaps between separately drawn texts.
            chars.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut lines: Vec<(f64, Vec<(f64, String)>)> = Vec::new();
            for (y, x, c) in chars {
                match lines.last_mut() {
                    Some((ly, l)) if (*ly - y).abs() < 1.0 => l.push((x, c)),
                    _ => lines.push((y, vec![(x, c)])),
                }
            }
            let text = lines
                .into_iter()
                .map(|(_, l)| {
                    let mut s = String::new();
                    let mut last_x: Option<f64> = None;
                    for (x, c) in l {
                        if last_x.is_some_and(|lx| x - lx > 9.0) && !s.ends_with(' ') {
                            s.push(' ');
                        }
                        s.push_str(&c);
                        last_x = Some(x);
                    }
                    norm(&s)
                })
                .filter(|l| !l.is_empty())
                .collect();
            (size, text)
        })
        .collect()
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn expected_roll(doc: &ReceiptDocument, columns: usize) -> Vec<String> {
    render(doc, columns)
        .iter()
        .map(|l| norm(&l.text))
        .filter(|l| !l.is_empty())
        .collect()
}

fn close(a: f64, b: f32) -> bool {
    (a - f64::from(b)).abs() < 0.05
}

#[test]
fn receipt_width_pdfs_say_what_the_printed_receipt_says() {
    for name in GOLDEN {
        let d = doc(name);
        for (format, columns, width_mm) in [
            (PdfFormat::Receipt58, COLUMNS_58MM, 58.0),
            (PdfFormat::Receipt80, COLUMNS_80MM, 80.0),
        ] {
            let pdf = render_pdf(&d, format).unwrap();
            let pages = read(&pdf);
            assert_eq!(pages.len(), 1, "{name} {format:?}: one continuous page");
            let ((w, h), lines) = &pages[0];
            assert!(close(*w, width_mm * MM), "{name} {format:?}: page width {w}");
            assert!(*h > 50.0 && *h < 14_400.0, "{name} {format:?}: page height {h}");
            assert_eq!(lines, &expected_roll(&d, columns), "{name} {format:?}");
        }
    }
}

#[test]
fn a4_pdfs_match_their_goldens() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for name in GOLDEN {
        let pdf = render_pdf(&doc(name), PdfFormat::A4).unwrap();
        let pages = read(&pdf);
        for ((w, h), _) in &pages {
            assert!(
                close(*w, A4.0) && close(*h, A4.1),
                "{name}: A4 page, got {w} x {h}"
            );
        }
        let text: String = pages
            .iter()
            .enumerate()
            .flat_map(|(i, (_, lines))| {
                std::iter::once(format!("--- page {}", i + 1)).chain(lines.iter().cloned())
            })
            .map(|l| l + "\n")
            .collect();
        let path = golden_dir().join(format!("{name}.a4.txt"));
        if update {
            std::fs::write(&path, &text).unwrap();
        }
        assert_eq!(
            text,
            std::fs::read_to_string(&path).unwrap_or_default(),
            "{name}.a4.txt (UPDATE_GOLDEN=1 to regenerate, then review)"
        );
    }
}

/// The fonts in the PDF: every one embedded (a font file in its descriptor) and a Noto font.
fn fonts(pdf: &[u8]) -> BTreeSet<String> {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let mut names = BTreeSet::new();
    for obj in doc.objects.values() {
        let Ok(dict) = obj.as_dict() else { continue };
        if dict.get(b"Type").and_then(|t| t.as_name()).ok() != Some(b"FontDescriptor".as_slice()) {
            continue;
        }
        let name = String::from_utf8_lossy(dict.get(b"FontName").unwrap().as_name().unwrap()).into_owned();
        assert!(
            dict.has(b"FontFile2") || dict.has(b"FontFile3"),
            "{name} is not embedded"
        );
        // Subsets are named "ABCDEF+Font".
        names.insert(name.split_once('+').map_or(name.clone(), |(_, n)| n.to_owned()));
    }
    names
}

#[test]
fn fonts_are_embedded_noto_subsets() {
    let pdf = render_pdf(&doc("sale"), PdfFormat::Receipt80).unwrap();
    let f = fonts(&pdf);
    assert!(f.iter().all(|n| n.starts_with("NotoSans")), "{f:?}");
    assert!(
        f.contains("NotoSansMono-Regular") && f.contains("NotoSansMono-Bold"),
        "{f:?}"
    );
    // Subset: far smaller than the 389 KB font file.
    assert!(pdf.len() < 60_000, "{} bytes", pdf.len());
    let pdf = render_pdf(&doc("sale"), PdfFormat::A4).unwrap();
    let f = fonts(&pdf);
    assert!(
        f.contains("NotoSans-Regular") && f.contains("NotoSans-Bold"),
        "{f:?}"
    );
}

const NAMES: [&str; 6] = [
    "كولا غازية",
    "मसाला चाय",
    "可口可乐 330ml",
    "緑茶 コーラ",
    "콜라 제로",
    "Café Crème",
];

fn world_doc() -> ReceiptDocument {
    let mut d = doc("sale");
    let template = d.lines[0].clone();
    d.lines = NAMES
        .iter()
        .map(|n| pos_receipts::document::Line {
            name: (*n).into(),
            ..template.clone()
        })
        .collect();
    d
}

#[test]
fn non_latin_names_have_glyphs_and_come_back_out() {
    for n in NAMES {
        assert_eq!(missing_glyphs(n), Vec::<char>::new(), "{n}");
    }
    assert_eq!(missing_glyphs("Cola 😀 ok 𓀀"), vec!['😀', '𓀀']);
    let d = world_doc();
    for format in [PdfFormat::Receipt58, PdfFormat::Receipt80, PdfFormat::A4] {
        let pdf = render_pdf(&d, format).unwrap();
        let f = fonts(&pdf);
        for want in ["NotoSansArabic", "NotoSansDevanagari", "NotoSansSC", "NotoSansKR"] {
            assert!(
                f.iter().any(|n| n.starts_with(want)),
                "{format:?}: {want} in {f:?}"
            );
        }
        let text = read(&pdf)
            .into_iter()
            .flat_map(|(_, l)| l)
            .collect::<Vec<_>>()
            .join("\n");
        // Left-to-right scripts read back as typed (the ToUnicode map is complete).
        for n in ["可口可乐 330ml", "緑茶 コーラ", "콜라 제로", "Café Crème"] {
            assert!(text.contains(n), "{format:?}: {n} not in\n{text}");
        }
        // Devanagari is shaped (conjuncts, reordered vowel signs), so glyphs and characters
        // differ; every letter is still mapped back. Arabic is shaped and drawn right to
        // left; pdf-extract does not reassemble right-to-left runs (MuPDF reads
        // "كولا غازية" back in order), so here it is checked by its font and coverage above.
        for c in "मसाला चाय".chars().filter(|c| !c.is_whitespace()) {
            assert!(text.contains(c), "{format:?}: {c} not in\n{text}");
        }
    }
}

#[test]
fn wide_characters_take_two_columns_so_amounts_stay_aligned() {
    let d = world_doc();
    for columns in [COLUMNS_58MM, COLUMNS_80MM] {
        for l in render(&d, columns) {
            assert!(
                text_columns(&l.text) <= columns,
                "{:?} is wider than {columns}",
                l.text
            );
        }
        let amounts: Vec<String> = render(&d, columns)
            .into_iter()
            .filter(|l| l.text.contains(" x "))
            .map(|l| l.text)
            .collect();
        assert_eq!(amounts.len(), NAMES.len());
        for a in amounts {
            assert_eq!(text_columns(&a), columns, "{a:?}");
        }
    }
}

#[test]
fn long_receipts_page_on_a4_and_stay_one_roll() {
    let mut d = doc("sale");
    let line = d.lines[0].clone();
    d.lines = (0..120)
        .map(|i| pos_receipts::document::Line {
            name: format!("Item {i:03}"),
            ..line.clone()
        })
        .collect();
    let a4 = read(&render_pdf(&d, PdfFormat::A4).unwrap());
    assert!(a4.len() >= 2, "{} pages", a4.len());
    // The table header repeats on every page, and every item is there once.
    for (_, lines) in &a4 {
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Item Qty Unit price Discount Amount")),
            "{lines:?}"
        );
    }
    let items: Vec<&String> = a4
        .iter()
        .flat_map(|(_, l)| l)
        .filter(|l| l.starts_with("Item 0") || l.starts_with("Item 1"))
        .collect();
    assert_eq!(items.len(), 120);
    let roll = read(&render_pdf(&d, PdfFormat::Receipt80).unwrap());
    assert_eq!(roll.len(), 1);
    assert_eq!(roll[0].1, expected_roll(&d, COLUMNS_80MM));
}
