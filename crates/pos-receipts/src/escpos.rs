//! ESC/POS command encoding (E13-T01, E13-T05).
//!
//! Only standard Epson commands are emitted, the ones common to Epson-compatible thermal
//! printers (Bixolon, Citizen, Xprinter, Rongta, Star in ESC/POS emulation, …). **Control
//! bytes in text are never passed through**: a product name containing `ESC p` must not be
//! able to open the cash drawer or change printer state.
//!
//! [`EscPos`] is the low-level builder. Receipts are encoded by [`crate::printer::encode`],
//! which adds code pages, transliteration, barcodes and the other dialects.

use crate::printer::DrawerPin;

pub(crate) const ESC: u8 = 0x1B;
pub(crate) const GS: u8 = 0x1D;
pub(crate) const LF: u8 = 0x0A;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Align {
    Left = 0,
    Center = 1,
    Right = 2,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Cut {
    /// GS V 66 0: feed to the cutter, then partial cut (leaves a hinge).
    Partial,
    /// GS V 65 0: feed to the cutter, then full cut.
    Full,
}

/// Builder for an ESC/POS byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscPos {
    buf: Vec<u8>,
}

impl Default for EscPos {
    fn default() -> Self {
        Self::new()
    }
}

impl EscPos {
    /// Starts with `ESC @` (initialise printer: clears styles left by a previous job and
    /// selects the printer's default code page).
    pub fn new() -> Self {
        Self { buf: vec![ESC, b'@'] }
    }

    /// `ESC a n`
    pub fn align(&mut self, a: Align) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'a', a as u8]);
        self
    }

    /// `ESC E n`
    pub fn bold(&mut self, on: bool) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'E', u8::from(on)]);
        self
    }

    /// ASCII text: printable ASCII passes through, any other character becomes `?` and
    /// control characters are dropped. For other scripts use [`crate::printer::encode`],
    /// which selects code pages and transliterates.
    pub fn text(&mut self, s: &str) -> &mut Self {
        for ch in s.chars() {
            match ch {
                ' '..='~' => self.buf.push(ch as u8),
                c if c.is_control() => {}
                _ => self.buf.push(b'?'),
            }
        }
        self
    }

    /// Bytes already encoded in the selected code page. Control bytes (0x00–0x1F, 0x7F)
    /// are dropped, so these can never form a command.
    pub fn encoded(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf
            .extend(bytes.iter().copied().filter(|b| *b >= 0x20 && *b != 0x7F));
        self
    }

    /// Text followed by a line feed.
    pub fn line(&mut self, s: &str) -> &mut Self {
        self.text(s);
        self.lf()
    }

    /// `LF`: print the line and feed one line.
    pub fn lf(&mut self) -> &mut Self {
        self.buf.push(LF);
        self
    }

    /// `ESC d n`: print and feed `n` lines.
    pub fn feed(&mut self, lines: u8) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'd', lines]);
        self
    }

    /// `GS V m 0`: feed to the cut position and cut.
    pub fn cut(&mut self, cut: Cut) -> &mut Self {
        self.cut_after(cut, 0)
    }

    /// `GS V 65 n` / `GS V 66 n`: feed to the cut position plus `n` motion units, then cut.
    pub fn cut_after(&mut self, cut: Cut, n: u8) -> &mut Self {
        let m = match cut {
            Cut::Full => 65,
            Cut::Partial => 66,
        };
        self.buf.extend_from_slice(&[GS, b'V', m, n]);
        self
    }

    /// `GS V 0` / `GS V 1`: cut where the paper is now, without feeding.
    pub fn cut_now(&mut self, cut: Cut) -> &mut Self {
        let m = match cut {
            Cut::Full => 0,
            Cut::Partial => 1,
        };
        self.buf.extend_from_slice(&[GS, b'V', m]);
        self
    }

    /// `ESC t n`: select the character code table (code page) `n`.
    pub fn code_page(&mut self, n: u8) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b't', n]);
        self
    }

    /// `ESC p m 25 250`: pulse the drawer kick connector pin (m = 0 for pin 2, 1 for pin 5)
    /// on for 50 ms and off for 500 ms (units of 2 ms).
    pub fn drawer(&mut self, pin: DrawerPin) -> &mut Self {
        let m = match pin {
            DrawerPin::Pin2 => 0,
            DrawerPin::Pin5 => 1,
        };
        self.buf.extend_from_slice(&[ESC, b'p', m, 25, 250]);
        self
    }

    /// A CODE128 barcode in code set B with the human-readable text below it:
    /// `GS h height`, `GS w module`, `GS H 2`, `GS k 73 n {B data`. `{` in the data is
    /// escaped as `{{`. Returns `false` (and emits nothing) if the data is not printable
    /// ASCII or too long for one command.
    pub fn code128(&mut self, data: &str, module: u8, height: u8) -> bool {
        if data.is_empty() || !data.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
            return false;
        }
        let mut payload = b"{B".to_vec();
        for b in data.bytes() {
            if b == b'{' {
                payload.push(b'{');
            }
            payload.push(b);
        }
        let Ok(n) = u8::try_from(payload.len()) else {
            return false;
        };
        self.buf
            .extend_from_slice(&[GS, b'h', height, GS, b'w', module, GS, b'H', 2]);
        self.buf.extend_from_slice(&[GS, b'k', 73, n]);
        self.buf.extend_from_slice(&payload);
        true
    }

    /// A QR code, model 2, error correction M, cell size `size` dots (`GS ( k`: fn 65 model,
    /// fn 67 size, fn 69 level, fn 80 store, fn 81 print). Returns `false` (and emits
    /// nothing) if the data does not fit one store command.
    pub fn qr(&mut self, data: &[u8], size: u8) -> bool {
        let Ok(len) = u16::try_from(data.len() + 3) else {
            return false;
        };
        if data.is_empty() {
            return false;
        }
        let [pl, ph] = len.to_le_bytes();
        let k = |buf: &mut Vec<u8>, body: &[u8]| {
            let [l, h] = u16::try_from(body.len()).unwrap_or(0).to_le_bytes();
            buf.extend_from_slice(&[GS, b'(', b'k', l, h]);
            buf.extend_from_slice(body);
        };
        k(&mut self.buf, &[49, 65, 50, 0]);
        k(&mut self.buf, &[49, 67, size]);
        k(&mut self.buf, &[49, 69, 49]);
        self.buf.extend_from_slice(&[GS, b'(', b'k', pl, ph, 49, 80, 48]);
        self.buf.extend_from_slice(data);
        k(&mut self.buf, &[49, 81, 48]);
        true
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// Human-readable decode of the ESC/POS commands this encoder emits, for reviewing golden
/// files and virtual-printer captures. Unknown bytes are shown as `[0xNN]`; text bytes
/// above 0x7F as `\xNN`, since the code page numbers are printer-specific (use
/// [`crate::printer::decode`] with the printer's code pages to see the characters).
pub fn decode(bytes: &[u8]) -> String {
    crate::printer::decode(bytes, crate::printer::Dialect::EscPos, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_in_text_cannot_inject_commands() {
        // ESC p 0 25 250 would pulse the cash drawer.
        let mut p = EscPos::new();
        p.line("Cola\u{1b}p\u{0}\u{19}\u{fa}");
        assert_eq!(p.bytes(), b"\x1b@Colap?\n");
        assert!(!p.bytes()[2..].contains(&ESC));
        let mut p = EscPos::new();
        p.encoded(b"Cola\x1bp\x00\x19\xfa");
        assert_eq!(p.bytes(), b"\x1b@Colap\xfa");
    }

    #[test]
    fn builder_text_is_ascii_only() {
        let mut p = EscPos::new();
        p.text("Café £2");
        assert_eq!(&p.bytes()[2..], b"Caf? ?2");
    }

    #[test]
    fn decode_round_trips_readably() {
        let mut p = EscPos::new();
        p.align(Align::Center)
            .bold(true)
            .line("SHOP")
            .bold(false)
            .feed(2)
            .cut(Cut::Full);
        assert_eq!(
            decode(p.bytes()),
            "[INIT]\n[ALIGN center][BOLD on]\"SHOP\"[LF]\n[BOLD off][FEED 2]\n[CUT full]\n"
        );
        assert_eq!(decode(&[0x10, 0x04, 0x01]), "[0x10][0x04][0x01]");
    }

    #[test]
    fn decodes_every_command_the_builder_emits() {
        let mut p = EscPos::new();
        p.drawer(DrawerPin::Pin2).drawer(DrawerPin::Pin5).code_page(19);
        p.encoded(b"Caf\x82").lf();
        assert!(p.code128("R01-{1}", 2, 80));
        assert!(p.qr(b"R01-000001", 6));
        p.cut_after(Cut::Partial, 3)
            .cut_now(Cut::Full)
            .cut_now(Cut::Partial);
        assert_eq!(
            decode(p.bytes()),
            "[INIT]\n[DRAWER pin2]\n[DRAWER pin5]\n[CODEPAGE 19]\"Caf\\x82\"[LF]\n\
             [BARCODE height 80][BARCODE width 2][BARCODE hri below][BARCODE CODE128 \"R01-{1}\"]\n\
             [QR model 2][QR size 6][QR ecc M][QR store \"R01-000001\"][QR print]\n\
             [CUT partial feed 3]\n[CUT full no-feed]\n[CUT partial no-feed]\n"
        );
    }

    #[test]
    fn barcode_escapes_braces_and_refuses_unprintable_data() {
        let mut p = EscPos::new();
        assert!(p.code128("{x", 2, 80));
        assert!(
            p.bytes()
                .ends_with(&[GS, b'k', 73, 5, b'{', b'B', b'{', b'{', b'x'])
        );
        let before = p.bytes().len();
        assert!(!p.code128("a\u{1b}b", 2, 80));
        assert!(!p.code128("Café", 2, 80));
        assert!(!p.code128("", 2, 80));
        assert!(!p.qr(b"", 6));
        assert_eq!(p.bytes().len(), before);
    }
}
