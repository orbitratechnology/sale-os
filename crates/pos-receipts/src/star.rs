//! Star Line Mode command encoding (E13-T05): Star TSP100/TSP650/TSP700/SP700/mC-Print
//! printers in their native emulation.
//!
//! Star Line Mode shares `ESC @` and `ESC E` with ESC/POS but differs in most other commands,
//! and some byte sequences mean different things: `ESC d n` **cuts** (in ESC/POS it feeds),
//! `ESC a n` **feeds** (in ESC/POS it aligns). Sending ESC/POS to a printer in Star mode
//! therefore mangles the receipt.
//!
//! The commands are implemented from the Star Line Mode command specification.
//! **Verification status:** none of these has been checked on Star hardware yet; they are
//! checked against the specification and the decoder only. The ones most worth checking
//! first are the CODE128 parameters (`ESC b`: HRI mode, module width, and whether Star's
//! CODE128 needs a code set prefix), the QR sequence (`ESC GS y …`), and the code page
//! numbers each model uses with `ESC GS t n`. Text, emphasis, alignment, feed, cut and the
//! `BEL` / `SUB` drawer kicks are the long-standing basic commands.
//!
//! Like [`crate::escpos::EscPos`], control bytes in text are never passed through.

use crate::escpos::{Align, ESC, GS, LF};
use crate::printer::DrawerPin;

/// `BEL`: open drawer 1 (kick connector pin 2).
pub(crate) const BEL: u8 = 0x07;
/// `SUB`: open drawer 2 (kick connector pin 5).
pub(crate) const SUB: u8 = 0x1A;
/// `RS`: ends barcode data.
pub(crate) const RS: u8 = 0x1E;

/// Builder for a Star Line Mode byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarLine {
    buf: Vec<u8>,
}

impl Default for StarLine {
    fn default() -> Self {
        Self::new()
    }
}

impl StarLine {
    /// Starts with `ESC @` (initialise: clears styles and selects the default code page).
    pub fn new() -> Self {
        Self { buf: vec![ESC, b'@'] }
    }

    /// `ESC GS a n`
    pub fn align(&mut self, a: Align) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, GS, b'a', a as u8]);
        self
    }

    /// `ESC E` (emphasis on) / `ESC F` (emphasis off).
    pub fn bold(&mut self, on: bool) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, if on { b'E' } else { b'F' }]);
        self
    }

    /// Bytes already encoded in the selected code page; control bytes are dropped.
    pub fn encoded(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf
            .extend(bytes.iter().copied().filter(|b| *b >= 0x20 && *b != 0x7F));
        self
    }

    /// `LF`
    pub fn lf(&mut self) -> &mut Self {
        self.buf.push(LF);
        self
    }

    /// `ESC a n`: feed `n` lines.
    pub fn feed(&mut self, lines: u8) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, b'a', lines]);
        self
    }

    /// `ESC d n`: cut. n = 0 full and 1 partial where the paper is now; 2 full and 3 partial
    /// after feeding to the cutter.
    pub fn cut(&mut self, full: bool, feed_to_cutter: bool) -> &mut Self {
        let n = match (full, feed_to_cutter) {
            (true, false) => 0,
            (false, false) => 1,
            (true, true) => 2,
            (false, true) => 3,
        };
        self.buf.extend_from_slice(&[ESC, b'd', n]);
        self
    }

    /// `ESC GS t n`: select code page `n`.
    pub fn code_page(&mut self, n: u8) -> &mut Self {
        self.buf.extend_from_slice(&[ESC, GS, b't', n]);
        self
    }

    /// `BEL` (drawer 1, pin 2) or `SUB` (drawer 2, pin 5), with the printer's configured
    /// pulse width.
    pub fn drawer(&mut self, pin: DrawerPin) -> &mut Self {
        self.buf.push(match pin {
            DrawerPin::Pin2 => BEL,
            DrawerPin::Pin5 => SUB,
        });
        self
    }

    /// `ESC b n1 n2 n3 n4 d… RS`: CODE128 (n1 = '6') with the human-readable text below and
    /// a line feed after (n2 = '2'), minimum module `module` dots (n3: '1' = 2 dots,
    /// '2' = 3 dots, '3' = 4 dots), `height` dots high. Returns `false` (and emits nothing)
    /// unless the data is printable ASCII of at most 255 bytes.
    pub fn code128(&mut self, data: &str, module: u8, height: u8) -> bool {
        if data.is_empty() || data.len() > 255 || !data.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
            return false;
        }
        let n3 = b'0' + module.clamp(2, 4) - 1;
        self.buf
            .extend_from_slice(&[ESC, b'b', b'6', b'2', n3, height.max(1)]);
        self.buf.extend_from_slice(data.as_bytes());
        self.buf.push(RS);
        true
    }

    /// A QR code: `ESC GS y S 0 2` (model 2), `ESC GS y S 1 1` (error correction M),
    /// `ESC GS y S 2 n` (cell size), `ESC GS y D 1 0 nL nH d…` (store, automatic mode),
    /// `ESC GS y P` (print). Returns `false` (and emits nothing) if the data is empty or
    /// too long.
    pub fn qr(&mut self, data: &[u8], size: u8) -> bool {
        let Ok(len) = u16::try_from(data.len()) else {
            return false;
        };
        if data.is_empty() {
            return false;
        }
        let [nl, nh] = len.to_le_bytes();
        let y = [ESC, GS, b'y'];
        for (k, v) in [(b'0', 2), (b'1', 1), (b'2', size.clamp(1, 8))] {
            self.buf.extend_from_slice(&y);
            self.buf.extend_from_slice(&[b'S', k, v]);
        }
        self.buf.extend_from_slice(&y);
        self.buf.extend_from_slice(&[b'D', b'1', 0, nl, nh]);
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(&y);
        self.buf.push(b'P');
        true
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// Human-readable decode of a Star Line Mode stream (see [`crate::escpos::decode`]).
pub fn decode(bytes: &[u8]) -> String {
    crate::printer::decode(bytes, crate::printer::Dialect::StarLine, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_every_command_the_builder_emits() {
        let mut p = StarLine::new();
        p.drawer(DrawerPin::Pin2).drawer(DrawerPin::Pin5);
        p.align(Align::Center).bold(true).code_page(4);
        p.encoded(b"Caf\x82\x07\x1b").lf().bold(false).align(Align::Left);
        assert!(p.code128("R01-000001", 2, 80));
        assert!(p.qr(b"R01-000001", 6));
        p.feed(3)
            .cut(false, true)
            .cut(true, true)
            .cut(true, false)
            .cut(false, false);
        assert_eq!(
            decode(p.bytes()),
            "[INIT]\n[DRAWER pin2]\n[DRAWER pin5]\n[ALIGN center][BOLD on][CODEPAGE 4]\"Caf\\x82\"[LF]\n\
             [BOLD off][ALIGN left][BARCODE CODE128 \"R01-000001\" hri module 2 height 80]\n\
             [QR model 2][QR ecc M][QR size 6][QR store \"R01-000001\"][QR print]\n\
             [FEED 3]\n[CUT partial]\n[CUT full]\n[CUT full no-feed]\n[CUT partial no-feed]\n"
        );
    }

    #[test]
    fn barcode_data_cannot_end_early_or_carry_commands() {
        let mut p = StarLine::new();
        assert!(!p.code128("R01\u{1e}\u{7}", 2, 80));
        assert!(!p.code128("Żubr", 2, 80));
        assert_eq!(p.bytes(), [ESC, b'@']);
    }
}
