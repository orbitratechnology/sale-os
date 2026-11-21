//! Single-byte printer code pages (E13-T05).
//!
//! Receipt printers print one byte per character. Bytes 0x00–0x7F are ASCII on every page
//! here; 0x80–0xFF depend on the page the printer has selected (`ESC t n` in ESC/POS,
//! `ESC GS t n` in Star Line Mode). The tables for 0x80–0xFF are generated from Python's
//! `codecs` module by `tools/gen_codepages.py` into `codepage_tables.rs`; they are never
//! edited by hand.
//!
//! The number that selects a page differs between printer models, so it is not part of
//! [`CodePage`]: the printer profile pairs each page with its number
//! ([`crate::printer::EncodeOptions::code_pages`]).

use crate::codepage_tables as t;

/// A single-byte code page the encoder can print in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CodePage {
    /// IBM PC, US: the usual power-on default.
    Cp437,
    /// Greek.
    Cp737,
    /// Baltic.
    Cp775,
    /// Western European (DOS).
    Cp850,
    /// Central European (DOS).
    Cp852,
    /// Cyrillic (DOS, Serbian/Macedonian/Bulgarian order).
    Cp855,
    /// Turkish (DOS).
    Cp857,
    /// CP850 with `€` at 0xD5.
    Cp858,
    /// Portuguese (DOS).
    Cp860,
    /// Icelandic (DOS).
    Cp861,
    /// Hebrew (DOS).
    Cp862,
    /// Canadian French (DOS).
    Cp863,
    /// Arabic (DOS). Its ARABIC PERCENT SIGN at 0x25 is ignored: 0x25 is always `%`.
    Cp864,
    /// Nordic (DOS).
    Cp865,
    /// Cyrillic (DOS, Russian).
    Cp866,
    /// Greek (DOS, modern).
    Cp869,
    /// Windows Central European.
    Cp1250,
    /// Windows Cyrillic.
    Cp1251,
    /// Windows Western European (Epson's "WPC1252").
    Cp1252,
    /// Windows Greek.
    Cp1253,
    /// Windows Turkish.
    Cp1254,
    /// Windows Hebrew.
    Cp1255,
    /// Windows Arabic.
    Cp1256,
    /// Windows Baltic.
    Cp1257,
    /// Windows Vietnamese. Its combining tone marks are never used: receipt text is
    /// precomposed, and a printer would print a mark in a cell of its own.
    Cp1258,
    /// Latin-1.
    Iso8859_1,
    /// Latin-2.
    Iso8859_2,
    /// Greek.
    Iso8859_7,
    /// Latin-9 (Latin-1 with `€`).
    Iso8859_15,
    /// JIS X0201 half-width katakana at 0xA1–0xDF (Epson's "Katakana" page).
    JisX0201,
}

impl CodePage {
    /// Every supported page.
    pub const ALL: [CodePage; 30] = [
        CodePage::Cp437,
        CodePage::Cp737,
        CodePage::Cp775,
        CodePage::Cp850,
        CodePage::Cp852,
        CodePage::Cp855,
        CodePage::Cp857,
        CodePage::Cp858,
        CodePage::Cp860,
        CodePage::Cp861,
        CodePage::Cp862,
        CodePage::Cp863,
        CodePage::Cp864,
        CodePage::Cp865,
        CodePage::Cp866,
        CodePage::Cp869,
        CodePage::Cp1250,
        CodePage::Cp1251,
        CodePage::Cp1252,
        CodePage::Cp1253,
        CodePage::Cp1254,
        CodePage::Cp1255,
        CodePage::Cp1256,
        CodePage::Cp1257,
        CodePage::Cp1258,
        CodePage::Iso8859_1,
        CodePage::Iso8859_2,
        CodePage::Iso8859_7,
        CodePage::Iso8859_15,
        CodePage::JisX0201,
    ];

    fn table(self) -> &'static [char; 128] {
        match self {
            CodePage::Cp437 => &t::CP437,
            CodePage::Cp737 => &t::CP737,
            CodePage::Cp775 => &t::CP775,
            CodePage::Cp850 => &t::CP850,
            CodePage::Cp852 => &t::CP852,
            CodePage::Cp855 => &t::CP855,
            CodePage::Cp857 => &t::CP857,
            CodePage::Cp858 => &t::CP858,
            CodePage::Cp860 => &t::CP860,
            CodePage::Cp861 => &t::CP861,
            CodePage::Cp862 => &t::CP862,
            CodePage::Cp863 => &t::CP863,
            CodePage::Cp864 => &t::CP864,
            CodePage::Cp865 => &t::CP865,
            CodePage::Cp866 => &t::CP866,
            CodePage::Cp869 => &t::CP869,
            CodePage::Cp1250 => &t::CP1250,
            CodePage::Cp1251 => &t::CP1251,
            CodePage::Cp1252 => &t::CP1252,
            CodePage::Cp1253 => &t::CP1253,
            CodePage::Cp1254 => &t::CP1254,
            CodePage::Cp1255 => &t::CP1255,
            CodePage::Cp1256 => &t::CP1256,
            CodePage::Cp1257 => &t::CP1257,
            CodePage::Cp1258 => &t::CP1258,
            CodePage::Iso8859_1 => &t::ISO_8859_1,
            CodePage::Iso8859_2 => &t::ISO_8859_2,
            CodePage::Iso8859_7 => &t::ISO_8859_7,
            CodePage::Iso8859_15 => &t::ISO_8859_15,
            CodePage::JisX0201 => &t::JIS_X0201,
        }
    }

    /// The name escpos-printer-db profiles use (`"CP858"`, `"ISO_8859-15"`, …).
    pub fn name(self) -> &'static str {
        match self {
            CodePage::Cp437 => "CP437",
            CodePage::Cp737 => "CP737",
            CodePage::Cp775 => "CP775",
            CodePage::Cp850 => "CP850",
            CodePage::Cp852 => "CP852",
            CodePage::Cp855 => "CP855",
            CodePage::Cp857 => "CP857",
            CodePage::Cp858 => "CP858",
            CodePage::Cp860 => "CP860",
            CodePage::Cp861 => "CP861",
            CodePage::Cp862 => "CP862",
            CodePage::Cp863 => "CP863",
            CodePage::Cp864 => "CP864",
            CodePage::Cp865 => "CP865",
            CodePage::Cp866 => "CP866",
            CodePage::Cp869 => "CP869",
            CodePage::Cp1250 => "CP1250",
            CodePage::Cp1251 => "CP1251",
            CodePage::Cp1252 => "CP1252",
            CodePage::Cp1253 => "CP1253",
            CodePage::Cp1254 => "CP1254",
            CodePage::Cp1255 => "CP1255",
            CodePage::Cp1256 => "CP1256",
            CodePage::Cp1257 => "CP1257",
            CodePage::Cp1258 => "CP1258",
            CodePage::Iso8859_1 => "ISO_8859-1",
            CodePage::Iso8859_2 => "ISO_8859-2",
            CodePage::Iso8859_7 => "ISO_8859-7",
            CodePage::Iso8859_15 => "ISO_8859-15",
            CodePage::JisX0201 => "JIS_X0201",
        }
    }

    /// Reads a code page name as printer profiles and drivers write it, ignoring case and
    /// punctuation: `CP858`, `PC858`, `IBM858`, `858`, `WPC1252`, `windows-1252`,
    /// `ISO_8859-15`, `iso-8859-15`, `Latin-9`, `Katakana`, `JIS X0201`. A trailing
    /// description is allowed (`PC850_MULTILINGUAL`). Pages this encoder cannot print,
    /// such as the double-byte `CP932`, give `None`.
    pub fn from_name(name: &str) -> Option<Self> {
        let n: String = name
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_uppercase())
            .collect();
        match n.as_str() {
            "KATAKANA" | "JISX0201" | "JIS0201" | "X0201" => return Some(CodePage::JisX0201),
            "LATIN1" => return Some(CodePage::Iso8859_1),
            "LATIN2" => return Some(CodePage::Iso8859_2),
            "LATIN9" => return Some(CodePage::Iso8859_15),
            _ => {}
        }
        if let Some(part) = n.strip_prefix("ISO8859") {
            return match part {
                "1" => Some(CodePage::Iso8859_1),
                "2" => Some(CodePage::Iso8859_2),
                "7" => Some(CodePage::Iso8859_7),
                "15" => Some(CodePage::Iso8859_15),
                _ => None,
            };
        }
        let rest = ["WINDOWS", "WPC", "WIN", "IBM", "PC", "CP"]
            .iter()
            .find_map(|p| n.strip_prefix(p))
            .unwrap_or(&n);
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !rest[digits.len()..].chars().all(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        Some(match digits.as_str() {
            "437" => CodePage::Cp437,
            "737" => CodePage::Cp737,
            "775" => CodePage::Cp775,
            "850" => CodePage::Cp850,
            "852" => CodePage::Cp852,
            "855" => CodePage::Cp855,
            "857" => CodePage::Cp857,
            "858" => CodePage::Cp858,
            "860" => CodePage::Cp860,
            "861" => CodePage::Cp861,
            "862" => CodePage::Cp862,
            "863" => CodePage::Cp863,
            "864" => CodePage::Cp864,
            "865" => CodePage::Cp865,
            "866" => CodePage::Cp866,
            "869" => CodePage::Cp869,
            "1250" => CodePage::Cp1250,
            "1251" => CodePage::Cp1251,
            "1252" => CodePage::Cp1252,
            "1253" => CodePage::Cp1253,
            "1254" => CodePage::Cp1254,
            "1255" => CodePage::Cp1255,
            "1256" => CodePage::Cp1256,
            "1257" => CodePage::Cp1257,
            "1258" => CodePage::Cp1258,
            _ => return None,
        })
    }

    /// The byte that prints `c` on this page. `None` for characters the page lacks and for
    /// every control character, so encoded text can never carry a command.
    pub fn encode(self, c: char) -> Option<u8> {
        if (' '..='~').contains(&c) {
            return u8::try_from(c).ok();
        }
        if c.is_control() || c.is_ascii() {
            return None;
        }
        let i = self.table().iter().position(|&x| x == c)?;
        u8::try_from(0x80 + i).ok()
    }

    /// The character byte `b` prints on this page, if it is a printable one.
    pub fn decode(self, b: u8) -> Option<char> {
        match b {
            0x20..=0x7E => Some(char::from(b)),
            0x80..=0xFF => Some(self.table()[usize::from(b - 0x80)]).filter(|&c| c != '\0'),
            _ => None,
        }
    }
}

impl std::fmt::Display for CodePage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for p in CodePage::ALL {
            assert_eq!(CodePage::from_name(p.name()), Some(p), "{p}");
        }
    }

    #[test]
    fn reads_profile_and_driver_spellings() {
        let cases = [
            ("CP437", Some(CodePage::Cp437)),
            ("cp858", Some(CodePage::Cp858)),
            ("PC858", Some(CodePage::Cp858)),
            ("PC850_MULTILINGUAL", Some(CodePage::Cp850)),
            ("IBM866", Some(CodePage::Cp866)),
            ("858", Some(CodePage::Cp858)),
            ("CP1252", Some(CodePage::Cp1252)),
            ("WPC1252", Some(CodePage::Cp1252)),
            ("windows-1252", Some(CodePage::Cp1252)),
            ("Windows-1250", Some(CodePage::Cp1250)),
            ("ISO_8859-15", Some(CodePage::Iso8859_15)),
            ("ISO-8859-15", Some(CodePage::Iso8859_15)),
            ("iso8859_2", Some(CodePage::Iso8859_2)),
            ("ISO_8859-7", Some(CodePage::Iso8859_7)),
            ("Latin-9", Some(CodePage::Iso8859_15)),
            ("Katakana", Some(CodePage::JisX0201)),
            ("JIS X0201", Some(CodePage::JisX0201)),
            ("CP932", None),
            ("CP851", None),
            ("ISO_8859-5", None),
            ("Unknown", None),
            ("", None),
            ("CP437x9", None),
        ];
        for (name, want) in cases {
            assert_eq!(CodePage::from_name(name), want, "{name}");
        }
    }

    #[test]
    fn known_bytes_from_the_generated_tables() {
        assert_eq!(CodePage::Cp858.encode('€'), Some(0xD5));
        assert_eq!(CodePage::Cp850.encode('€'), None);
        assert_eq!(CodePage::Cp437.encode('é'), Some(0x82));
        assert_eq!(CodePage::Cp1252.encode('€'), Some(0x80));
        assert_eq!(CodePage::Cp1252.encode('—'), Some(0x97));
        assert_eq!(CodePage::Cp852.encode('Ż'), Some(0xBD));
        assert_eq!(CodePage::Cp866.encode('Ж'), Some(0x86));
        assert_eq!(CodePage::Cp737.encode('Ω'), Some(0x97));
        assert_eq!(CodePage::Iso8859_15.encode('€'), Some(0xA4));
        assert_eq!(CodePage::Iso8859_1.encode('£'), Some(0xA3));
        assert_eq!(CodePage::JisX0201.encode('ｱ'), Some(0xB1));
        assert_eq!(CodePage::Cp437.decode(0x9C), Some('£'));
        assert_eq!(CodePage::Cp1252.decode(0x81), None);
        assert_eq!(
            CodePage::Iso8859_1.decode(0x85),
            None,
            "C1 controls are unassigned"
        );
    }

    #[test]
    fn control_characters_never_encode() {
        for p in CodePage::ALL {
            for c in ('\0'..='\u{1F}').chain('\u{7F}'..='\u{9F}') {
                assert_eq!(p.encode(c), None, "{p} {c:?}");
            }
            assert_eq!(p.encode('%'), Some(b'%'));
            // Every assigned byte decodes and encodes back to itself.
            for b in 0x80..=0xFF_u8 {
                if let Some(c) = p.decode(b) {
                    assert!(!c.is_control(), "{p} {b:#X}");
                    let back = p.encode(c).unwrap();
                    assert_eq!(p.decode(back), Some(c), "{p} {b:#X}");
                }
            }
        }
    }
}
