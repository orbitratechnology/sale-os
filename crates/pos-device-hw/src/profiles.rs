//! Printer model profiles (`profiles/profiles.json`, embedded): columns per font, paper
//! width, code pages, cutter, drawer port, QR/barcode support and native dialect.
//!
//! Sources, recorded per profile in `source`:
//! - `escpos-printer-db`: derived from <https://github.com/receipt-print-hq/escpos-printer-db>
//!   (CC BY 4.0; attribution and licence in `profiles/LICENSE-escpos-printer-db`);
//! - `generic`: this project's catch-all profiles (`generic-escpos-80mm-48`, …);
//! - `manual`: widespread models missing from the database, from vendor data sheets and
//!   not yet checked on hardware (`"verified": false`).

use crate::printer::IdentifiedPrinter;
use pos_device_ports::{CommandDialect, CutSupport, PrinterCapabilities};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const PROFILES_JSON: &str = include_str!("../profiles/profiles.json");

/// Font A (the normal, larger font) or font B (smaller, more columns).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum FontChoice {
    #[default]
    A,
    B,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum Dialect {
    EscPos,
    StarLine,
    PlainText,
}

impl From<Dialect> for CommandDialect {
    fn from(d: Dialect) -> Self {
        match d {
            Dialect::EscPos => CommandDialect::EscPos,
            Dialect::StarLine => CommandDialect::StarLine,
            Dialect::PlainText => CommandDialect::PlainText,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct PrinterProfile {
    pub id: String,
    pub vendor: String,
    pub name: String,
    /// `escpos-printer-db`, `generic` or `manual`.
    pub source: String,
    /// `Some(false)` for hand-added entries not yet checked on real hardware.
    #[serde(default)]
    pub verified: Option<bool>,
    pub dialect: Dialect,
    pub font_a_columns: u16,
    #[serde(default)]
    pub font_b_columns: Option<u16>,
    /// Nominal paper width (58, 76, 80…).
    #[serde(default)]
    pub paper_width_mm: Option<u16>,
    /// (number that selects it, name), in the vendor's numbering for the dialect.
    #[serde(default)]
    pub code_pages: Vec<(u8, String)>,
    pub full_cut: bool,
    pub partial_cut: bool,
    pub drawer: bool,
    pub qr: bool,
    pub barcode: bool,
    /// Answers `DLE EOT` real-time status (on a two-way link).
    #[serde(default)]
    pub status_query: bool,
    /// `false` for raster-only printers that cannot print text receipts directly.
    #[serde(default = "yes")]
    pub text_supported: bool,
    #[serde(default = "three")]
    pub feed_before_cut: u8,
    /// Other names the printer reports (`GS I 67`) for this profile.
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

fn yes() -> bool {
    true
}

fn three() -> u8 {
    3
}

#[derive(Deserialize)]
struct ProfilesFile {
    profiles: Vec<PrinterProfile>,
}

impl PrinterProfile {
    /// What the encoder may use with this model and font.
    pub fn capabilities(&self, font: FontChoice) -> PrinterCapabilities {
        let columns = match font {
            FontChoice::A => self.font_a_columns,
            FontChoice::B => self.font_b_columns.unwrap_or(self.font_a_columns),
        };
        let text = self.dialect != Dialect::PlainText;
        PrinterCapabilities {
            dialect: self.dialect.into(),
            columns: Some(columns),
            code_pages: self
                .code_pages
                .iter()
                .map(|(n, name)| (name.clone(), *n))
                .collect(),
            // Receipts are normally partially cut (the roll stays attached at one point).
            cut: if !text {
                CutSupport::None
            } else if self.partial_cut {
                CutSupport::Partial
            } else if self.full_cut {
                CutSupport::Full
            } else {
                CutSupport::None
            },
            feed_before_cut: self.feed_before_cut,
            drawer_kick: self.drawer && text,
            barcode: self.barcode && text,
            qr: self.qr && text,
            status_query: self.status_query && self.dialect == Dialect::EscPos,
        }
    }

    /// Hand-added and not yet checked on real hardware.
    pub fn unverified(&self) -> bool {
        self.verified == Some(false)
    }
}

/// Every profile: generic ones first, then the database, then hand-added models.
pub fn profiles() -> &'static [PrinterProfile] {
    static PROFILES: OnceLock<Vec<PrinterProfile>> = OnceLock::new();
    PROFILES.get_or_init(|| {
        // The embedded file is checked by this crate's tests; a broken build fails there.
        serde_json::from_str::<ProfilesFile>(PROFILES_JSON)
            .map(|f| f.profiles)
            .unwrap_or_else(|e| {
                tracing::error!(target: "pos_device_hw", error = %e, "embedded printer profiles do not parse");
                Vec::new()
            })
    })
}

pub fn profile(id: &str) -> Option<&'static PrinterProfile> {
    profiles().iter().find(|p| p.id == id)
}

/// Upper-case letters and digits only: `TM-T88V` and `tm t88v` compare equal.
fn norm(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn vendor_matches(profile_vendor: &str, maker: &str) -> bool {
    let (v, m) = (norm(profile_vendor), norm(maker));
    !v.is_empty() && !m.is_empty() && (v.starts_with(&m) || m.starts_with(&v))
}

/// The profile for what the printer reported, by exact model name (or alias), ignoring case
/// and punctuation. Never a "close" match: `TM-T88V` must not be chosen for a `TM-T88VI`.
pub fn match_identified(found: &IdentifiedPrinter) -> Option<&'static PrinterProfile> {
    let model = norm(found.model.as_deref()?);
    if model.is_empty() {
        return None;
    }
    let candidates = profiles().iter().filter(|p| p.source != "generic").filter(|p| {
        norm(&p.name) == model || norm(&p.id) == model || p.aliases.iter().any(|a| norm(a) == model)
    });
    let mut best = None;
    for p in candidates {
        let vendor_ok = found
            .maker
            .as_deref()
            .is_none_or(|m| vendor_matches(&p.vendor, m));
        if !vendor_ok {
            continue;
        }
        // Prefer the database over a hand-added duplicate.
        if best.is_none() || p.source == "escpos-printer-db" {
            best = Some(p);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn json_parses_and_ids_are_unique() {
        let file: ProfilesFile = serde_json::from_str(PROFILES_JSON).expect("profiles.json");
        assert!(file.profiles.len() > 60);
        assert_eq!(profiles().len(), file.profiles.len());
        let ids: HashSet<_> = profiles().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.len(), profiles().len());
    }

    #[test]
    fn every_profile_gives_coherent_capabilities() {
        for p in profiles() {
            for font in [FontChoice::A, FontChoice::B] {
                let caps = p.capabilities(font);
                assert!(caps.columns.is_some_and(|c| c > 0), "{}", p.id);
                let numbers: HashSet<_> = caps.code_pages.iter().map(|(_, n)| *n).collect();
                assert_eq!(numbers.len(), caps.code_pages.len(), "{}", p.id);
                assert!(
                    caps.code_pages.iter().all(|(name, _)| name != "Unknown"),
                    "{}",
                    p.id
                );
                if p.dialect == Dialect::PlainText {
                    assert!(
                        !caps.drawer_kick && !caps.qr && caps.cut == CutSupport::None,
                        "{}",
                        p.id
                    );
                }
                if caps.status_query {
                    assert_eq!(caps.dialect, CommandDialect::EscPos, "{}", p.id);
                }
            }
            assert!(
                ["escpos-printer-db", "generic", "manual"].contains(&p.source.as_str()),
                "{}",
                p.id
            );
            if p.source == "manual" {
                assert!(p.unverified(), "{}", p.id);
            }
        }
    }

    #[test]
    fn known_models() {
        let t88v = profile("TM-T88V").unwrap();
        assert_eq!(t88v.capabilities(FontChoice::A).columns, Some(42));
        assert_eq!(t88v.capabilities(FontChoice::B).columns, Some(56));
        assert_eq!(t88v.paper_width_mm, Some(80));
        assert!(
            t88v.capabilities(FontChoice::A)
                .code_pages
                .contains(&("CP858".into(), 19))
        );

        for (id, cols) in [
            ("generic-escpos-80mm-48", 48),
            ("generic-escpos-80mm-42", 42),
            ("generic-escpos-58mm-32", 32),
            ("generic-escpos-76mm-impact-40", 40),
            ("star-line-80mm-48", 48),
            ("star-line-58mm-32", 32),
            ("plain-text-80", 80),
            ("plain-text-40", 40),
            ("TM-T88VI", 42),
            ("TM-T88VII", 42),
            ("TM-T82III", 48),
            ("TM-m30II", 48),
            ("TM-T20", 48),
        ] {
            assert_eq!(
                profile(id).unwrap().capabilities(FontChoice::A).columns,
                Some(cols),
                "{id}"
            );
        }
        assert_eq!(
            profile("star-line-80mm-48")
                .unwrap()
                .capabilities(FontChoice::A)
                .dialect,
            CommandDialect::StarLine
        );
        assert_eq!(profile("star-tsp650ii").unwrap().dialect, Dialect::StarLine);
        let tsp100 = profile("star-tsp100").unwrap();
        assert!(!tsp100.text_supported && tsp100.unverified());
        // Font B falls back to font A where a profile has none.
        assert_eq!(
            profile("plain-text-80")
                .unwrap()
                .capabilities(FontChoice::B)
                .columns,
            Some(80)
        );
    }

    fn id(maker: Option<&str>, model: &str) -> IdentifiedPrinter {
        IdentifiedPrinter {
            maker: maker.map(str::to_owned),
            model: Some(model.into()),
            model_id: None,
        }
    }

    #[test]
    fn matching_identified_printers() {
        let m = |maker, model| match_identified(&id(maker, model)).map(|p| p.id.as_str());
        assert_eq!(m(Some("EPSON"), "TM-T88V"), Some("TM-T88V"));
        assert_eq!(m(Some("EPSON"), "TM-T88VI"), Some("TM-T88VI"));
        assert_eq!(m(None, "tm-t20ii"), Some("TM-T20II"));
        assert_eq!(m(Some("EPSON"), "TM-m30II-H"), Some("TM-m30II"));
        assert_eq!(m(Some("BIXOLON"), "SRP-350III"), Some("bixolon-srp-350iii"));
        // Wrong maker, unknown model, nothing reported.
        assert_eq!(m(Some("STAR"), "TM-T88V"), None);
        assert_eq!(m(Some("EPSON"), "TM-X999"), None);
        assert_eq!(match_identified(&IdentifiedPrinter::default()), None);
    }
}
