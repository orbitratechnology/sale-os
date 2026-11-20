//! Encoding rendered receipt lines for a receipt printer (E13-T05).
//!
//! [`encode`] turns the lines of [`crate::text::render`] into one print job for a printer
//! [`Dialect`]: Epson ESC/POS, Star Line Mode or plain text. It:
//!
//! - prints every character it can in one of the printer's code pages, selecting pages
//!   only when the text needs a different one ([`EncodeOptions::code_pages`]);
//! - transliterates what no listed page has (`Ω` → `O`, `中` → `Zh`) and never lets a line
//!   print wider than it was laid out (see [`fit_line`]);
//! - never passes a control character from text through, so a product name cannot carry a
//!   command (drawer kick, cut, code page change);
//! - prints the receipt number line ([`Style::Barcode`]) as a CODE128 barcode or a QR code
//!   when the printer can;
//! - optionally kicks the cash drawer at the start of the job and cuts at the end.
//!
//! [`decode`] turns any job back into readable text for golden files and virtual-printer
//! captures.

use crate::codepage::CodePage;
use crate::escpos::{Align, Cut, ESC, EscPos, GS, LF};
use crate::star::{BEL, RS, SUB, StarLine};
use crate::text::{Style, TextLine, char_columns};

/// The command language of the printer.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// Epson ESC/POS and compatibles.
    #[default]
    EscPos,
    /// Star Line Mode (Star printers in their native emulation). See [`crate::star`] for
    /// which commands are not yet verified on hardware.
    StarLine,
    /// Text and line feeds only, and a form feed (0x0C) at the end if a cut is requested.
    /// The first listed code page is assumed to be the printer's (it cannot be selected).
    PlainText,
}

/// What happens at the end of the job.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum CutMode {
    /// Feed only.
    None,
    /// Cut leaving a hinge.
    #[default]
    Partial,
    Full,
}

/// The drawer kick connector pin a cash drawer is wired to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DrawerPin {
    /// Pin 2: the first drawer (Star: `BEL`, drawer 1).
    Pin2,
    /// Pin 5: the second drawer (Star: `SUB`, drawer 2).
    Pin5,
}

/// How to encode a job for a particular printer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeOptions {
    pub dialect: Dialect,
    /// The printer's code pages, in order of preference, each with the number its select
    /// command uses (`ESC t n` / `ESC GS t n`; these numbers differ between models). Empty:
    /// ASCII only, everything else is transliterated.
    pub code_pages: Vec<(CodePage, u8)>,
    pub cut: CutMode,
    /// Lines to feed before cutting (or at the end of the job, without a cut).
    pub feed_before_cut: u8,
    /// Print the receipt number line ([`Style::Barcode`]) as a CODE128 barcode with the
    /// number below it. Takes precedence over `qr`; when the number cannot be a CODE128
    /// barcode (too long for the paper, not ASCII) `qr` is tried next.
    pub barcode: bool,
    /// Print the receipt number line as a QR code (with the number as text below it) when
    /// it is not printed as a barcode.
    pub qr: bool,
    /// Pulse this drawer pin at the start of the job.
    pub drawer_kick: Option<DrawerPin>,
    /// Characters per line. `None`: the widest line's width, which for lines from
    /// [`crate::text::render`] is the width they were rendered for (every receipt has a
    /// full-width rule). Transliteration never makes a line wider than this.
    pub columns: Option<usize>,
}

impl Default for EncodeOptions {
    /// What the encoder has always produced: ESC/POS, ASCII only, partial cut after feeding
    /// three lines, the receipt number as text, no drawer kick. ASCII receipts encode
    /// byte-for-byte as before; other characters are now transliterated instead of `?`.
    fn default() -> Self {
        Self {
            dialect: Dialect::EscPos,
            code_pages: Vec::new(),
            cut: CutMode::Partial,
            feed_before_cut: 3,
            barcode: false,
            qr: false,
            drawer_kick: None,
            columns: None,
        }
    }
}

/// Bar height of a CODE128 barcode: 80 dots, 10 mm at 203 dpi.
const BARCODE_HEIGHT: u8 = 80;

/// The print job for `lines`.
pub fn encode(lines: &[TextLine], opts: &EncodeOptions) -> Vec<u8> {
    let columns = opts.columns.unwrap_or_else(|| {
        lines
            .iter()
            .map(|l| l.text.chars().map(char_columns).sum::<usize>())
            .max()
            .unwrap_or(0)
    });
    let pages: &[(CodePage, u8)] = match opts.dialect {
        Dialect::PlainText => &opts.code_pages[..opts.code_pages.len().min(1)],
        _ => &opts.code_pages,
    };
    let page_set: Vec<CodePage> = pages.iter().map(|p| p.0).collect();
    let mut w = Writer::new(opts.dialect);
    // After `ESC @` the printer is on its configured default page, which we do not know.
    let mut current = (opts.dialect == Dialect::PlainText && !pages.is_empty()).then_some(0);
    if let Some(pin) = opts.drawer_kick {
        w.drawer(pin);
    }
    let mut bold = false;
    for l in lines {
        let want = l.style == Style::Bold;
        if want != bold {
            w.bold(want);
            bold = want;
        }
        if l.style == Style::Barcode && w.symbol(l.text.trim(), opts, columns) == Symbol::Barcode {
            // The barcode prints the number below itself.
            continue;
        }
        w.text(&fit(&l.text, columns, &page_set), pages, &mut current);
        w.lf();
    }
    if bold {
        w.bold(false);
    }
    w.finish(opts.cut, opts.feed_before_cut);
    w.into_bytes()
}

/// A job that only opens the cash drawer (empty for [`Dialect::PlainText`], which has no
/// drawer command).
pub fn drawer_kick(dialect: Dialect, pin: DrawerPin) -> Vec<u8> {
    match dialect {
        Dialect::EscPos => {
            let mut p = EscPos::new();
            p.drawer(pin);
            p.into_bytes()[2..].to_vec()
        }
        Dialect::StarLine => {
            let mut p = StarLine::new();
            p.drawer(pin);
            p.into_bytes()[2..].to_vec()
        }
        Dialect::PlainText => Vec::new(),
    }
}

/// `line` as it will print with `code_pages`: every character is printable ASCII or in one
/// of the pages, control characters are gone, and it is no wider than `columns` or the
/// line itself, whichever is wider.
///
/// Characters no page has are transliterated (`deunicode`). A transliteration can be longer
/// than the character (`€` → `EUR`, `ß` → `ss`, `中` → `Zhong`), which would push the rest
/// of the line out of its column, so each character gets two forms:
///
/// - a **short** form exactly as wide as the character was laid out: the first letters of
///   the transliteration (`ß` → `s`, `中` → `Zh`), or a hand-picked single character for
///   common symbols (`€` → `E`, `£` → `L`, `—` → `-`, `…` → `.`), or `?` as a last resort;
/// - the **long** transliteration, used only when the line has room for it: spaces taken
///   from the gap to the right of the character (between a name and its amount, keeping
///   at least one space, so amounts stay right-aligned) or from the free space at the end
///   of the line. Earlier characters get the room first.
///
/// A wide character whose transliteration is narrower (`あ` → `a`) gives its spare column
/// to that gap rather than printing a space inside the word, and syllables of CJK words
/// stay separated (`抹茶` → `Mo Cha`) when there is room.
///
/// So `Café crème 250 g — Größe L` prints as `Cafe creme 250 g - Grosse L` on a line with
/// room, and as `Cafe creme 250 g - Grose L` on a full one; with CP858 listed it prints
/// natively except for `—`.
pub fn fit_line(line: &str, columns: usize, code_pages: &[CodePage]) -> String {
    fit(line, columns, code_pages).into_iter().collect()
}

struct Piece {
    /// What prints now: one native character, or ASCII exactly as wide as the source.
    short: String,
    /// Another ASCII form, longer or shorter, to use if the line allows.
    long: Option<String>,
}

impl Piece {
    fn is_space(&self) -> bool {
        self.short == " " && self.long.is_none()
    }

    fn space() -> Self {
        Piece {
            short: " ".into(),
            long: None,
        }
    }
}

fn fit(line: &str, columns: usize, pages: &[CodePage]) -> Vec<char> {
    let chars: Vec<char> = line.chars().filter(|c| !c.is_control()).collect();
    let mut pieces: Vec<Piece> = Vec::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        let native =
            (' '..='~').contains(&c) || (char_columns(c) == 1 && pages.iter().any(|p| p.encode(c).is_some()));
        if native {
            pieces.push(Piece {
                short: c.to_string(),
                long: None,
            });
            continue;
        }
        let (short, mut long, separate) = transliterate(c);
        // deunicode ends a syllable of a CJK word with a space: keep one before the next
        // letter, so `抹茶` reads `Mo Cha`, not `MoCha`.
        if separate && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric()) {
            long.push(' ');
        }
        let long = (long != short).then_some(long);
        pieces.push(Piece { short, long });
    }
    let laid_out: usize = pieces.iter().map(|p| p.short.chars().count()).sum();
    // First the forms narrower than their layout (`あ` is 2 columns, `a` one): the spare
    // columns go into the gap to the right, so whatever follows stays where it was; with no
    // gap they are dropped, unless the line is full (it may end in a right-aligned amount).
    let mut i = 0;
    while i < pieces.len() {
        let short_len = pieces[i].short.len();
        if let Some(long) = pieces[i].long.take_if(|l| l.len() < short_len) {
            let spare = short_len - long.len();
            if let Some(at) = gap_after(&pieces, i + 1, 0) {
                pieces.splice(at..at, std::iter::repeat_with(Piece::space).take(spare));
                pieces[i].short = long;
            } else if laid_out < columns {
                pieces[i].short = long;
            }
        }
        i += 1;
    }
    // Then the wider forms, while there is room.
    let width: usize = pieces.iter().map(|p| p.short.chars().count()).sum();
    let mut room = columns.saturating_sub(width);
    let mut i = 0;
    while i < pieces.len() {
        if let Some(long) = pieces[i].long.take() {
            let extra = long.len() - pieces[i].short.len();
            if let Some(at) = gap_after(&pieces, i + 1, extra) {
                pieces.drain(at..at + extra);
                pieces[i].short = long;
            } else if room >= extra {
                room -= extra;
                pieces[i].short = long;
            }
        }
        i += 1;
    }
    pieces
        .into_iter()
        .flat_map(|p| p.short.chars().collect::<Vec<_>>())
        .collect()
}

/// The start of the first run of two or more spaces at or after `from`, if `extra` spaces
/// can be taken from it leaving at least one.
fn gap_after(pieces: &[Piece], from: usize, extra: usize) -> Option<usize> {
    let mut i = from;
    while i < pieces.len() {
        if pieces[i].is_space() {
            let run = pieces[i..].iter().take_while(|p| p.is_space()).count();
            if run >= 2 {
                return (run > extra).then_some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// The ASCII forms of a character no listed code page has: (short, long, whether a word
/// break follows). The short form is exactly as wide as the character was laid out.
fn transliterate(c: char) -> (String, String, bool) {
    let width = char_columns(c);
    let (long, short, separate) = match symbol_fallback(c) {
        Some((long, short)) => (long.to_owned(), Some(short), false),
        None => {
            let t: String = deunicode::deunicode_char(c)
                .unwrap_or("")
                .chars()
                .filter(|c| (' '..='~').contains(c))
                .collect();
            let separate = t.ends_with(' ');
            let t = t.trim();
            let t = if t.is_empty() && width > 0 { "?" } else { t };
            (t.to_owned(), None, separate && !t.is_empty())
        }
    };
    let short: String = short
        .unwrap_or(&long)
        .chars()
        .chain(std::iter::repeat(' '))
        .take(width)
        .collect();
    (short, long, separate)
}

/// Hand-picked (long, short) forms where the first letters of the transliteration would
/// mislead: currency symbols (deunicode gives `PS` for `£`), dashes and quotes (one
/// character each), and fractions (`1` for `½` would change a quantity, so `?`).
fn symbol_fallback(c: char) -> Option<(&'static str, &'static str)> {
    Some(match c {
        '€' => ("EUR", "E"),
        '£' => ("GBP", "L"),
        '¥' => ("Y", "Y"),
        '¢' => ("c", "c"),
        '₹' => ("Rs", "R"),
        '₽' => ("RUB", "R"),
        '₺' => ("TL", "L"),
        '₴' => ("UAH", "G"),
        '₩' => ("W", "W"),
        '₱' => ("PHP", "P"),
        '₦' => ("NGN", "N"),
        '₪' => ("ILS", "S"),
        '₫' => ("d", "d"),
        '–' | '—' | '―' | '−' | '‐' | '‑' => ("-", "-"),
        '‘' | '’' | '‚' | '′' => ("'", "'"),
        '“' | '”' | '„' | '″' | '«' | '»' => ("\"", "\""),
        '…' => ("...", "."),
        '•' | '∙' => ("*", "*"),
        '·' => (".", "."),
        '×' => ("x", "x"),
        '÷' => ("/", "/"),
        '°' => ("deg", "o"),
        '½' => ("1/2", "?"),
        '¼' => ("1/4", "?"),
        '¾' => ("3/4", "?"),
        '№' => ("No", "N"),
        '\u{A0}' => (" ", " "),
        _ => return None,
    })
}

/// The printable width of the paper in dots at 203 dpi, from the line width. 58 mm paper
/// is 384 dots; on 80 mm paper printable widths vary (512 or 576), so the narrower is used.
fn paper_dots(columns: usize) -> usize {
    if columns <= 35 { 384 } else { 512 }
}

/// The module width (2 or 3 dots) at which `data` fits the paper as CODE128 code set B
/// with 10-module quiet zones, if it can be a CODE128 barcode at all. One dot is too thin
/// for most scanners.
fn code128_module(data: &str, dots: usize) -> Option<u8> {
    if data.is_empty() || !data.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return None;
    }
    // Start (11) + 11 per character + check (11) + stop (13) + quiet zones (20).
    let modules = 11 * data.len() + 55;
    let module = (dots / modules).min(3);
    u8::try_from(module).ok().filter(|m| *m >= 2)
}

/// Byte-mode capacity of QR versions 1–40 at error correction level M.
const QR_CAPACITY_M: [usize; 40] = [
    14, 26, 42, 62, 84, 106, 122, 152, 180, 213, 251, 287, 331, 362, 412, 450, 504, 560, 624, 666, 711, 779,
    857, 911, 997, 1059, 1125, 1190, 1264, 1370, 1452, 1538, 1628, 1722, 1809, 1911, 1989, 2099, 2213, 2331,
];

/// The QR cell size (2–6 dots) at which `len` bytes fit the paper with a 4-cell quiet zone.
fn qr_size(len: usize, dots: usize) -> Option<u8> {
    let version = QR_CAPACITY_M.iter().position(|&cap| cap >= len)? + 1;
    let modules = 17 + 4 * version + 8;
    let size = (dots / modules).min(6);
    u8::try_from(size).ok().filter(|s| *s >= 2)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Symbol {
    None,
    Barcode,
    Qr,
}

enum Writer {
    EscPos(EscPos),
    Star(StarLine),
    Plain(Vec<u8>),
}

impl Writer {
    fn new(dialect: Dialect) -> Self {
        match dialect {
            Dialect::EscPos => Writer::EscPos(EscPos::new()),
            Dialect::StarLine => Writer::Star(StarLine::new()),
            Dialect::PlainText => Writer::Plain(Vec::new()),
        }
    }

    fn bold(&mut self, on: bool) {
        match self {
            Writer::EscPos(p) => {
                p.bold(on);
            }
            Writer::Star(p) => {
                p.bold(on);
            }
            Writer::Plain(_) => {}
        }
    }

    fn align(&mut self, a: Align) {
        match self {
            Writer::EscPos(p) => {
                p.align(a);
            }
            Writer::Star(p) => {
                p.align(a);
            }
            Writer::Plain(_) => {}
        }
    }

    fn code_page(&mut self, n: u8) {
        match self {
            Writer::EscPos(p) => {
                p.code_page(n);
            }
            Writer::Star(p) => {
                p.code_page(n);
            }
            Writer::Plain(_) => {}
        }
    }

    fn encoded(&mut self, bytes: &[u8]) {
        match self {
            Writer::EscPos(p) => {
                p.encoded(bytes);
            }
            Writer::Star(p) => {
                p.encoded(bytes);
            }
            Writer::Plain(buf) => buf.extend(bytes.iter().copied().filter(|b| *b >= 0x20 && *b != 0x7F)),
        }
    }

    fn lf(&mut self) {
        match self {
            Writer::EscPos(p) => {
                p.lf();
            }
            Writer::Star(p) => {
                p.lf();
            }
            Writer::Plain(buf) => buf.push(LF),
        }
    }

    fn drawer(&mut self, pin: DrawerPin) {
        match self {
            Writer::EscPos(p) => {
                p.drawer(pin);
            }
            Writer::Star(p) => {
                p.drawer(pin);
            }
            Writer::Plain(_) => {}
        }
    }

    /// Prints `data` as a centred barcode or QR code if the options and the printer allow.
    fn symbol(&mut self, data: &str, opts: &EncodeOptions, columns: usize) -> Symbol {
        if matches!(self, Writer::Plain(_)) || data.is_empty() {
            return Symbol::None;
        }
        let dots = paper_dots(columns);
        if opts.barcode
            && let Some(module) = code128_module(data, dots)
        {
            self.align(Align::Center);
            let ok = match self {
                Writer::EscPos(p) => p.code128(data, module, BARCODE_HEIGHT),
                Writer::Star(p) => p.code128(data, module, BARCODE_HEIGHT),
                Writer::Plain(_) => false,
            };
            self.align(Align::Left);
            if ok {
                return Symbol::Barcode;
            }
        }
        if opts.qr
            && let Some(size) = qr_size(data.len(), dots)
        {
            self.align(Align::Center);
            let ok = match self {
                Writer::EscPos(p) => p.qr(data.as_bytes(), size),
                Writer::Star(p) => p.qr(data.as_bytes(), size),
                Writer::Plain(_) => false,
            };
            self.align(Align::Left);
            if ok {
                return Symbol::Qr;
            }
        }
        Symbol::None
    }

    /// Prints fitted text, selecting code pages as it goes. At the first non-ASCII
    /// character, and at any character the current page lacks, it picks the page that
    /// prints furthest into the rest of the line ([`choose_page`]): the current page if it
    /// ties (no switch), otherwise the first listed. So a line the current page can print
    /// entirely never switches, a line one listed page can print entirely switches once
    /// before it, and a mixed-script line switches as few times as possible.
    fn text(&mut self, chars: &[char], pages: &[(CodePage, u8)], current: &mut Option<usize>) {
        let mut bytes = Vec::with_capacity(chars.len());
        let mut chosen = false;
        for (i, &c) in chars.iter().enumerate() {
            if c.is_ascii() {
                bytes.push(u8::try_from(c).unwrap_or(b'?'));
                continue;
            }
            let covered = current.is_some_and(|p| pages[p].0.encode(c).is_some());
            if !covered || !chosen {
                let best = choose_page(&chars[i..], pages, *current);
                if let Some(p) = best
                    && best != *current
                {
                    // The first switch goes before the line: only ASCII precedes it.
                    if chosen {
                        self.encoded(&std::mem::take(&mut bytes));
                    }
                    self.code_page(pages[p].1);
                    *current = best;
                }
                chosen = true;
            }
            // `fit` only leaves characters some listed page has, so `?` is unreachable.
            bytes.push(current.and_then(|p| pages[p].0.encode(c)).unwrap_or(b'?'));
        }
        self.encoded(&bytes);
    }

    fn finish(&mut self, cut: CutMode, feed: u8) {
        match self {
            Writer::EscPos(p) => {
                if feed > 0 {
                    p.feed(feed);
                }
                match cut {
                    CutMode::None => {}
                    CutMode::Partial => {
                        p.cut(Cut::Partial);
                    }
                    CutMode::Full => {
                        p.cut(Cut::Full);
                    }
                }
            }
            Writer::Star(p) => {
                if feed > 0 {
                    p.feed(feed);
                }
                match cut {
                    CutMode::None => {}
                    CutMode::Partial => {
                        p.cut(false, true);
                    }
                    CutMode::Full => {
                        p.cut(true, true);
                    }
                }
            }
            Writer::Plain(buf) => {
                buf.extend(std::iter::repeat_n(LF, usize::from(feed)));
                if cut != CutMode::None {
                    buf.push(FF);
                }
            }
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        match self {
            Writer::EscPos(p) => p.into_bytes(),
            Writer::Star(p) => p.into_bytes(),
            Writer::Plain(buf) => buf,
        }
    }
}

/// Of the pages that have `rest[0]`, the one that prints furthest into `rest` before
/// another switch is needed (non-ASCII characters counted; ASCII prints on every page);
/// `current` wins a tie, then the first listed. Taking the furthest reach each time gives
/// the fewest switches for the line.
fn choose_page(rest: &[char], pages: &[(CodePage, u8)], current: Option<usize>) -> Option<usize> {
    let reach = |p: CodePage| {
        rest.iter()
            .filter(|c| !c.is_ascii())
            .take_while(|c| p.encode(**c).is_some())
            .count()
    };
    let mut best: Option<(usize, usize)> = current.map(|k| (k, reach(pages[k].0))).filter(|b| b.1 > 0);
    for (k, (page, _)) in pages.iter().enumerate() {
        let n = reach(*page);
        if n > 0 && best.is_none_or(|(_, b)| n > b) {
            best = Some((k, n));
        }
    }
    best.map(|(k, _)| k)
}

const FF: u8 = 0x0C;

/// One element of a decoded job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Printable bytes (0x20–0x7E, 0x80–0xFF) in the selected code page.
    Text(Vec<u8>),
    Lf,
    /// Select code page `n`.
    CodePage(u8),
    /// Any other known command, as its label: `INIT`, `CUT partial`, `DRAWER pin2`,
    /// `BARCODE CODE128 "R01-000001"`, `QR print`, …
    Command(String),
    /// A byte that is not text and not part of a known command.
    Unknown(u8),
}

/// The commands and text in a job, for checks and [`decode`].
pub fn parse(bytes: &[u8], dialect: Dialect) -> Vec<Op> {
    let mut ops: Vec<Op> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let (op, len) = match dialect {
            Dialect::EscPos => parse_escpos(rest),
            Dialect::StarLine => parse_star(rest),
            Dialect::PlainText => None,
        }
        .unwrap_or_else(|| match rest[0] {
            LF => (Op::Lf, 1),
            FF if dialect == Dialect::PlainText => (Op::Command("FORM FEED".into()), 1),
            b if b >= 0x80 || (0x20..0x7F).contains(&b) => (Op::Text(vec![b]), 1),
            b => (Op::Unknown(b), 1),
        });
        match (ops.last_mut(), op) {
            (Some(Op::Text(t)), Op::Text(more)) => t.extend(more),
            (_, op) => ops.push(op),
        }
        i += len;
    }
    ops
}

fn cmd(s: impl Into<String>) -> Op {
    Op::Command(s.into())
}

fn align_name(n: u8) -> &'static str {
    match n {
        0 | 48 => "left",
        1 | 49 => "center",
        2 | 50 => "right",
        _ => "?",
    }
}

fn pin_name(m: u8) -> &'static str {
    match m {
        0 | 48 => "pin2",
        1 | 49 => "pin5",
        _ => "pin?",
    }
}

const ECC: [&str; 4] = ["L", "M", "Q", "H"];

fn ecc_name(n: u8) -> &'static str {
    ECC.get(usize::from(n)).unwrap_or(&"?")
}

/// Data shown in quotes: printable text as is, anything else as `\xNN`.
fn quoted(data: &[u8]) -> String {
    let s = match std::str::from_utf8(data) {
        Ok(s) if !s.chars().any(char::is_control) => s.to_owned(),
        _ => data
            .iter()
            .map(|&b| {
                if (0x20..0x7F).contains(&b) {
                    char::from(b).to_string()
                } else {
                    format!("\\x{b:02X}")
                }
            })
            .collect(),
    };
    format!("\"{s}\"")
}

fn parse_escpos(rest: &[u8]) -> Option<(Op, usize)> {
    let r = match rest {
        [ESC, b'@', ..] => (cmd("INIT"), 2),
        [ESC, b'a', n, ..] => (cmd(format!("ALIGN {}", align_name(*n))), 3),
        [ESC, b'E', n, ..] => (cmd(format!("BOLD {}", if *n & 1 == 0 { "off" } else { "on" })), 3),
        [ESC, b'd', n, ..] => (cmd(format!("FEED {n}")), 3),
        [ESC, b't', n, ..] => (Op::CodePage(*n), 3),
        [ESC, b'p', m, t1, t2, ..] => {
            let timing = if (*t1, *t2) == (25, 250) {
                String::new()
            } else {
                format!(" on {}ms off {}ms", u32::from(*t1) * 2, u32::from(*t2) * 2)
            };
            (cmd(format!("DRAWER {}{timing}", pin_name(*m))), 5)
        }
        [GS, b'V', m @ (65 | 66), n, ..] => {
            let kind = if *m == 65 { "full" } else { "partial" };
            let label = if *n == 0 {
                format!("CUT {kind}")
            } else {
                format!("CUT {kind} feed {n}")
            };
            (cmd(label), 4)
        }
        [GS, b'V', m @ (0 | 1 | 48 | 49), ..] => {
            let kind = if *m & 1 == 0 { "full" } else { "partial" };
            (cmd(format!("CUT {kind} no-feed")), 3)
        }
        [GS, b'h', n, ..] => (cmd(format!("BARCODE height {n}")), 3),
        [GS, b'w', n, ..] => (cmd(format!("BARCODE width {n}")), 3),
        [GS, b'H', n, ..] => {
            let pos = ["none", "above", "below", "both"]
                .get(usize::from(*n & 3))
                .unwrap_or(&"?");
            (cmd(format!("BARCODE hri {pos}")), 3)
        }
        [GS, b'k', 73, n, data @ ..] if data.len() >= usize::from(*n) => {
            let data = &data[..usize::from(*n)];
            let label = match data.strip_prefix(b"{B") {
                Some(d) => {
                    let mut out = Vec::with_capacity(d.len());
                    let mut j = 0;
                    while j < d.len() {
                        out.push(d[j]);
                        j += if d[j] == b'{' && d.get(j + 1) == Some(&b'{') {
                            2
                        } else {
                            1
                        };
                    }
                    format!("BARCODE CODE128 {}", quoted(&out))
                }
                None => format!("BARCODE CODE128 raw {}", quoted(data)),
            };
            (cmd(label), 4 + usize::from(*n))
        }
        [GS, b'(', b'k', pl, ph, body @ ..] => {
            let len = usize::from(u16::from_le_bytes([*pl, *ph]));
            if body.len() < len || len < 2 {
                return None;
            }
            let body = &body[..len];
            let arg = body.get(2).copied().unwrap_or(0);
            let label = match (body[0], body[1]) {
                (49, 65) => format!("QR model {}", arg.wrapping_sub(48)),
                (49, 67) => format!("QR size {arg}"),
                (49, 69) => format!("QR ecc {}", ecc_name(arg.wrapping_sub(48))),
                (49, 80) => format!("QR store {}", quoted(body.get(3..).unwrap_or(&[]))),
                (49, 81) => "QR print".to_owned(),
                (cn, f) => format!("GS ( k {cn} {f}"),
            };
            (cmd(label), 5 + len)
        }
        _ => return None,
    };
    Some(r)
}

fn parse_star(rest: &[u8]) -> Option<(Op, usize)> {
    let digit = |b: u8| if b >= b'0' { b - b'0' } else { b };
    let r = match rest {
        [ESC, b'@', ..] => (cmd("INIT"), 2),
        [ESC, GS, b'a', n, ..] => (cmd(format!("ALIGN {}", align_name(*n))), 4),
        [ESC, GS, b't', n, ..] => (Op::CodePage(*n), 4),
        [ESC, GS, b'y', b'S', k, n, ..] => {
            let label = match k {
                b'0' => format!("QR model {n}"),
                b'1' => format!("QR ecc {}", ecc_name(*n)),
                b'2' => format!("QR size {n}"),
                _ => format!("QR setting {k} {n}"),
            };
            (cmd(label), 6)
        }
        [ESC, GS, b'y', b'D', b'1', _m, nl, nh, data @ ..] => {
            let len = usize::from(u16::from_le_bytes([*nl, *nh]));
            if data.len() < len {
                return None;
            }
            (cmd(format!("QR store {}", quoted(&data[..len]))), 8 + len)
        }
        [ESC, GS, b'y', b'P', ..] => (cmd("QR print"), 4),
        [ESC, b'E', ..] => (cmd("BOLD on"), 2),
        [ESC, b'F', ..] => (cmd("BOLD off"), 2),
        [ESC, b'a', n, ..] => (cmd(format!("FEED {n}")), 3),
        [ESC, b'd', n, ..] => {
            let label = match n {
                0 | 48 => "CUT full no-feed",
                1 | 49 => "CUT partial no-feed",
                2 | 50 => "CUT full",
                3 | 51 => "CUT partial",
                _ => "CUT ?",
            };
            (cmd(label), 3)
        }
        [ESC, b'b', n1, n2, n3, n4, data @ ..] => {
            let end = data.iter().position(|b| *b == RS)?;
            let kind = match digit(*n1) {
                6 => "CODE128".to_owned(),
                t => format!("type {t}"),
            };
            let hri = if matches!(digit(*n2), 2 | 4) { " hri" } else { "" };
            let module = digit(*n3) + 1;
            let label = format!(
                "BARCODE {kind} {}{hri} module {module} height {n4}",
                quoted(&data[..end])
            );
            (cmd(label), 6 + end + 1)
        }
        [BEL, ..] => (cmd("DRAWER pin2"), 1),
        [SUB, ..] => (cmd("DRAWER pin5"), 1),
        _ => return None,
    };
    Some(r)
}

/// Human-readable decode of a job in `dialect`. `code_pages` maps the job's code page
/// numbers back to pages so text above 0x7F shows as characters; bytes on an unknown page
/// show as `\xNN`. Unknown bytes outside text show as `[0xNN]`.
pub fn decode(bytes: &[u8], dialect: Dialect, code_pages: &[(CodePage, u8)]) -> String {
    let mut out = String::new();
    let mut page = match dialect {
        Dialect::PlainText => code_pages.first().map(|p| p.0),
        _ => None,
    };
    for op in parse(bytes, dialect) {
        match op {
            Op::Text(t) => {
                out.push('"');
                for b in t {
                    match page.and_then(|p| p.decode(b)) {
                        Some(c) => out.push(c),
                        None if (0x20..0x7F).contains(&b) => out.push(char::from(b)),
                        None => out.push_str(&format!("\\x{b:02X}")),
                    }
                }
                out.push('"');
            }
            Op::Lf => out.push_str("[LF]\n"),
            Op::CodePage(n) => {
                page = code_pages.iter().find(|p| p.1 == n).map(|p| p.0);
                match page {
                    Some(p) => out.push_str(&format!("[CODEPAGE {n} {p}]")),
                    None => out.push_str(&format!("[CODEPAGE {n}]")),
                }
            }
            Op::Command(c) => {
                out.push_str(&format!("[{c}]"));
                let ends_line = [
                    "INIT",
                    "CUT",
                    "FEED",
                    "DRAWER",
                    "FORM FEED",
                    "BARCODE CODE128",
                    "BARCODE type",
                ]
                .iter()
                .any(|p| c.starts_with(p))
                    || c == "QR print";
                if ends_line {
                    out.push('\n');
                }
            }
            Op::Unknown(b) => out.push_str(&format!("[0x{b:02X}]")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, style: Style) -> TextLine {
        TextLine {
            text: text.into(),
            style,
        }
    }

    fn opts(dialect: Dialect, pages: &[(CodePage, u8)]) -> EncodeOptions {
        EncodeOptions {
            dialect,
            code_pages: pages.to_vec(),
            ..EncodeOptions::default()
        }
    }

    /// The printed text of each line (code page bytes, one per column).
    fn printed(bytes: &[u8], dialect: Dialect) -> Vec<Vec<u8>> {
        let mut lines = vec![Vec::new()];
        for op in parse(bytes, dialect) {
            match op {
                Op::Text(t) => lines.last_mut().unwrap().extend(t),
                Op::Lf => lines.push(Vec::new()),
                _ => {}
            }
        }
        lines
    }

    #[test]
    fn default_options_match_the_ascii_spike() {
        let lines = [
            line("  SHOP", Style::Centered),
            line("Tea      2.50", Style::Normal),
            line("TOTAL    2.50", Style::Bold),
            line("   R01-000001", Style::Barcode),
        ];
        let mut p = EscPos::new();
        p.line("  SHOP")
            .line("Tea      2.50")
            .bold(true)
            .line("TOTAL    2.50");
        p.bold(false).line("   R01-000001").feed(3).cut(Cut::Partial);
        assert_eq!(encode(&lines, &EncodeOptions::default()), p.into_bytes());
    }

    #[test]
    fn control_characters_cannot_inject_commands_in_any_dialect() {
        let evil = [line(
            "Cola\u{1b}p\u{0}\u{19}\u{fa}\u{7}\u{1a}\u{1d}V\u{1}\u{c}\u{1e}",
            Style::Normal,
        )];
        let clean = [line("Colap\u{fa}V", Style::Normal)];
        for dialect in [Dialect::EscPos, Dialect::StarLine, Dialect::PlainText] {
            for pages in [&[][..], &[(CodePage::Cp437, 0)][..]] {
                let o = EncodeOptions {
                    cut: CutMode::None,
                    feed_before_cut: 0,
                    ..opts(dialect, pages)
                };
                let got = encode(&evil, &o);
                assert_eq!(got, encode(&clean, &o), "{dialect:?}");
                let text: Vec<u8> = printed(&got, dialect).concat();
                assert!(text.iter().all(|b| *b >= 0x20 && *b != 0x7F), "{dialect:?}");
                let commands = parse(&got, dialect)
                    .into_iter()
                    .filter(|op| !matches!(op, Op::Text(_) | Op::Lf))
                    .collect::<Vec<_>>();
                let expected: &[Op] = match dialect {
                    Dialect::PlainText => &[],
                    _ if pages.is_empty() => &[Op::Command("INIT".into())],
                    _ => &[Op::Command("INIT".into()), Op::CodePage(0)],
                };
                assert_eq!(commands, expected, "{dialect:?}");
            }
        }
    }

    #[test]
    fn selects_a_code_page_only_when_the_page_changes() {
        let pages = [
            (CodePage::Cp858, 19),
            (CodePage::Cp852, 18),
            (CodePage::Cp737, 14),
        ];
        let lines = [
            line("Café", Style::Normal),
            line("Crème brûlée €", Style::Normal),
            line("plain", Style::Normal),
            line("Żubrówka", Style::Normal),
            line("Ωμέγα", Style::Normal),
        ];
        let got = encode(&lines, &opts(Dialect::EscPos, &pages));
        assert_eq!(
            decode(&got, Dialect::EscPos, &pages),
            "[INIT]\n[CODEPAGE 19 CP858]\"Café\"[LF]\n\"Crème brûlée €\"[LF]\n\"plain\"[LF]\n\
             [CODEPAGE 18 CP852]\"Żubrówka\"[LF]\n[CODEPAGE 14 CP737]\"Ωμέγα\"[LF]\n\
             [FEED 3]\n[CUT partial]\n"
        );
    }

    #[test]
    fn prefers_the_page_that_covers_the_whole_line() {
        // CP858 has é and ö but not —; CP1252 has all three.
        let pages = [(CodePage::Cp858, 19), (CodePage::Cp1252, 16)];
        let got = encode(
            &[line("Café — Größe", Style::Normal)],
            &opts(Dialect::EscPos, &pages),
        );
        assert_eq!(
            decode(&got, Dialect::EscPos, &pages),
            "[INIT]\n[CODEPAGE 16 CP1252]\"Café — Größe\"[LF]\n[FEED 3]\n[CUT partial]\n"
        );
    }

    #[test]
    fn switches_mid_line_for_mixed_scripts() {
        let pages = [(CodePage::Cp852, 18), (CodePage::Cp866, 17)];
        let got = encode(
            &[line("Łódź Квас ż", Style::Normal)],
            &opts(Dialect::StarLine, &pages),
        );
        assert_eq!(
            decode(&got, Dialect::StarLine, &pages),
            "[INIT]\n[CODEPAGE 18 CP852]\"Łódź \"[CODEPAGE 17 CP866]\"Квас \"\
             [CODEPAGE 18 CP852]\"ż\"[LF]\n[FEED 3]\n[CUT partial]\n"
        );
    }

    #[test]
    fn mixed_lines_switch_as_few_times_as_possible() {
        // CP737 has the middle dot and Greek, CP866 the dot and Cyrillic: the dot before
        // `Спасибо` goes with the Cyrillic, not with the Greek further on.
        let pages = [
            (CodePage::Cp852, 18),
            (CodePage::Cp737, 14),
            (CodePage::Cp866, 17),
        ];
        let got = encode(
            &[line("Dziękujemy · Спасибо · Ευχαριστώ", Style::Normal)],
            &opts(Dialect::EscPos, &pages),
        );
        assert_eq!(
            decode(&got, Dialect::EscPos, &pages),
            "[INIT]\n[CODEPAGE 18 CP852]\"Dziękujemy \"[CODEPAGE 17 CP866]\"· Спасибо · \"\
             [CODEPAGE 14 CP737]\"Ευχαριστώ\"[LF]\n[FEED 3]\n[CUT partial]\n"
        );
    }

    #[test]
    fn transliterates_what_no_page_has_within_the_column_budget() {
        let s = "Café crème 250 g — Größe L";
        assert_eq!(fit_line(s, 32, &[]), "Cafe creme 250 g - Grosse L");
        // A full line: every character keeps its own column.
        assert_eq!(fit_line(s, 26, &[]), "Cafe creme 250 g - Grose L");
        // An amount stays right-aligned: the long forms take spaces from the gap.
        assert_eq!(fit_line("Größe €    4.50", 15, &[]), "Grosse EUR 4.50");
        assert_eq!(fit_line("Größe €  4.50", 13, &[]), "Grosse E 4.50");
        assert_eq!(fit_line("£ € ¥ ½ Ż ñ", 11, &[]), "L E Y ? Z n");
        assert_eq!(fit_line("£ € ¥ ½ Ż ñ", 40, &[]), "GBP EUR Y 1/2 Z n");
        assert_eq!(fit_line("£ € ¥ ½ Ż ñ", 11, &[CodePage::Cp437]), "£ E ¥ ½ Z ñ");
        // Wide characters keep both columns.
        assert_eq!(fit_line("中文  9.99", 10, &[]), "ZhWen 9.99");
        assert_eq!(fit_line("中文 9.99", 9, &[]), "ZhWe 9.99");
        // CJK syllables stay apart; a narrower form gives its spare column to the gap.
        assert_eq!(fit_line("抹茶ラテ", 20, &[]), "Mo Cha rate");
        assert_eq!(fit_line("ありがとう 谢谢", 20, &[]), "arigatou Xie Xie");
        assert_eq!(fit_line("あ  1.00", 8, &[]), "a   1.00");
        assert_eq!(fit_line("あ 1.00", 7, &[]), "a  1.00");
        assert_eq!(fit_line("e\u{301}x", 2, &[]), "ex");
        assert_eq!(fit_line("\u{E000}", 1, &[]), "?");
    }

    #[test]
    fn plain_text_has_no_control_codes_but_lf_and_form_feed() {
        let pages = [(CodePage::Cp1252, 0), (CodePage::Cp866, 17)];
        let lines = [
            line("Café", Style::Bold),
            line("Квас", Style::Normal),
            line("R01-000001", Style::Barcode),
        ];
        let o = EncodeOptions {
            barcode: true,
            qr: true,
            drawer_kick: Some(DrawerPin::Pin2),
            ..opts(Dialect::PlainText, &pages)
        };
        let got = encode(&lines, &o);
        assert_eq!(got, b"Caf\xe9\nKvas\nR01-000001\n\n\n\n\x0c");
        assert!(drawer_kick(Dialect::PlainText, DrawerPin::Pin2).is_empty());
        let o = EncodeOptions {
            cut: CutMode::None,
            feed_before_cut: 0,
            ..o
        };
        assert_eq!(encode(&lines[..1], &o), b"Caf\xe9\n");
    }

    #[test]
    fn star_line_mode_cuts_and_kicks_with_its_own_commands() {
        let o = EncodeOptions {
            cut: CutMode::Full,
            feed_before_cut: 2,
            drawer_kick: Some(DrawerPin::Pin5),
            ..opts(Dialect::StarLine, &[])
        };
        let got = encode(&[line("TOTAL", Style::Bold)], &o);
        assert_eq!(got, b"\x1b@\x1a\x1bETOTAL\n\x1bF\x1ba\x02\x1bd\x02");
        assert_eq!(drawer_kick(Dialect::StarLine, DrawerPin::Pin2), [BEL]);
        assert_eq!(
            drawer_kick(Dialect::EscPos, DrawerPin::Pin5),
            [ESC, b'p', 1, 25, 250]
        );
        let o = EncodeOptions {
            cut: CutMode::None,
            ..o
        };
        assert_eq!(
            decode(&encode(&[], &o), Dialect::StarLine, &[]),
            "[INIT]\n[DRAWER pin5]\n[FEED 2]\n"
        );
    }

    #[test]
    fn the_receipt_number_prints_as_a_barcode_or_qr_when_allowed() {
        let lines = [line("           R01-000001", Style::Barcode)];
        let with = |barcode, qr, columns| EncodeOptions {
            barcode,
            qr,
            columns: Some(columns),
            cut: CutMode::None,
            feed_before_cut: 0,
            ..EncodeOptions::default()
        };
        let d = |o: &EncodeOptions| decode(&encode(&lines, o), Dialect::EscPos, &[]);
        assert_eq!(
            d(&with(true, true, 48)),
            "[INIT]\n[ALIGN center][BARCODE height 80][BARCODE width 3][BARCODE hri below]\
             [BARCODE CODE128 \"R01-000001\"]\n[ALIGN left]"
        );
        assert_eq!(
            d(&with(false, true, 32)),
            "[INIT]\n[ALIGN center][QR model 2][QR size 6][QR ecc M][QR store \"R01-000001\"]\
             [QR print]\n[ALIGN left]\"           R01-000001\"[LF]\n"
        );
        assert_eq!(
            d(&with(false, false, 32)),
            "[INIT]\n\"           R01-000001\"[LF]\n"
        );
        // Too long for a CODE128 on 58 mm paper: the QR code takes over.
        let long = [line("R01-000001-ABCDEFGHIJKLMNOP", Style::Barcode)];
        let got = decode(&encode(&long, &with(true, true, 32)), Dialect::EscPos, &[]);
        assert!(
            got.contains("[QR store \"R01-000001-ABCDEFGHIJKLMNOP\"]"),
            "{got}"
        );
        let got = decode(&encode(&long, &with(true, false, 32)), Dialect::EscPos, &[]);
        assert!(!got.contains("BARCODE"), "{got}");
        // Star Line Mode.
        let o = EncodeOptions {
            dialect: Dialect::StarLine,
            ..with(true, false, 32)
        };
        assert_eq!(
            decode(&encode(&lines, &o), Dialect::StarLine, &[]),
            "[INIT]\n[ALIGN center][BARCODE CODE128 \"R01-000001\" hri module 2 height 80]\n[ALIGN left]"
        );
    }

    #[test]
    fn barcode_and_qr_fit_the_paper() {
        assert_eq!(code128_module("R01-000001", 384), Some(2));
        assert_eq!(code128_module("R01-000001", 512), Some(3));
        assert_eq!(code128_module("R01-R-000001", 384), Some(2));
        assert_eq!(code128_module(&"9".repeat(20), 384), None);
        assert_eq!(code128_module("Ż", 512), None);
        assert_eq!(qr_size(10, 384), Some(6));
        assert_eq!(qr_size(2331, 384), Some(2));
        assert_eq!(qr_size(2332, 576), None);
    }

    #[test]
    fn decode_shows_unknown_bytes_and_unknown_pages() {
        assert_eq!(
            decode(&[ESC, b't', 7, 0xE9, 0x10], Dialect::EscPos, &[]),
            "[CODEPAGE 7]\"\\xE9\"[0x10]"
        );
        assert_eq!(
            decode(b"a\x0cb", Dialect::PlainText, &[]),
            "\"a\"[FORM FEED]\n\"b\""
        );
        assert_eq!(decode(&[ESC, b'b', b'6'], Dialect::StarLine, &[]), "[0x1B]\"b6\"");
    }
}
