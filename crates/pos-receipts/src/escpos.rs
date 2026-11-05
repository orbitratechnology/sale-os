//! ESC/POS command encoding (E13-T01 spike; E13-T05 completes it).
//!
//! Only commands common to Epson-compatible thermal printers are emitted. Text is encoded
//! to the printer's code page, and **control bytes in text are never passed through**:
//! a product name containing `ESC p` must not be able to open the cash drawer or change
//! printer state.

const ESC: u8 = 0x1B;
const GS: u8 = 0x1D;
const LF: u8 = 0x0A;

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
    /// Starts with `ESC @` (initialise printer: clears styles left by a previous job).
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

    /// Text in the printer code page. Spike: printable ASCII passes through; any other
    /// character becomes `?` and control characters are dropped. Code-page tables and
    /// transliteration arrive with E13-T05.
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

    /// Text followed by a line feed.
    pub fn line(&mut self, s: &str) -> &mut Self {
        self.text(s);
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
        let m = match cut {
            Cut::Full => 65,
            Cut::Partial => 66,
        };
        self.buf.extend_from_slice(&[GS, b'V', m, 0]);
        self
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// Human-readable decode of the commands this encoder emits, for reviewing golden files
/// and virtual-printer captures. Unknown bytes are shown as `[0xNN]`.
pub fn decode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut String| {
        if !text.is_empty() {
            out.push_str(&format!("\"{text}\""));
            text.clear();
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let (cmd, len) = match rest {
            [ESC, b'@', ..] => ("[INIT]".to_owned(), 2),
            [ESC, b'a', n, ..] => (
                format!(
                    "[ALIGN {}]",
                    ["left", "center", "right"].get(usize::from(*n)).unwrap_or(&"?")
                ),
                3,
            ),
            [ESC, b'E', n, ..] => (format!("[BOLD {}]", if *n == 0 { "off" } else { "on" }), 3),
            [ESC, b'd', n, ..] => (format!("[FEED {n}]"), 3),
            [GS, b'V', 65, 0, ..] => ("[CUT full]".to_owned(), 4),
            [GS, b'V', 66, 0, ..] => ("[CUT partial]".to_owned(), 4),
            [LF, ..] => {
                flush(&mut text, &mut out);
                out.push_str("[LF]\n");
                i += 1;
                continue;
            }
            [b, ..] if (0x20..0x7F).contains(b) => {
                text.push(char::from(*b));
                i += 1;
                continue;
            }
            [b, ..] => (format!("[0x{b:02X}]"), 1),
            [] => break,
        };
        flush(&mut text, &mut out);
        out.push_str(&cmd);
        if matches!(cmd.as_str(), "[INIT]" | "[CUT full]" | "[CUT partial]") || cmd.starts_with("[FEED") {
            out.push('\n');
        }
        i += len;
    }
    flush(&mut text, &mut out);
    out
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
    }

    #[test]
    fn non_ascii_becomes_question_mark_until_code_pages_land() {
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
}
