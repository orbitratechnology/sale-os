//! ESC/POS and Star Line Mode control sequences used by the adapter (status, identify,
//! drawer kick) and the parsing of the printer's answers. Receipt content is encoded by
//! `pos-receipts`, not here.

use crate::printer::DrawerOpenLevel;
use pos_device_ports::{CommandDialect, DrawerPin, PaperLevel, PrinterStatus};

/// `DLE EOT 1`: printer status (online/offline, drawer sensor).
pub const DLE_EOT_PRINTER: [u8; 3] = [0x10, 0x04, 0x01];
/// `DLE EOT 2`: offline cause (cover open, paper end, error).
pub const DLE_EOT_OFFLINE: [u8; 3] = [0x10, 0x04, 0x02];
/// `DLE EOT 3`: error cause (cutter, unrecoverable, auto-recoverable).
pub const DLE_EOT_ERROR: [u8; 3] = [0x10, 0x04, 0x03];
/// `DLE EOT 4`: roll paper sensor (near end, end).
pub const DLE_EOT_PAPER: [u8; 3] = [0x10, 0x04, 0x04];

/// `GS I 1`: printer model id (one byte).
pub const GS_I_MODEL_ID: [u8; 3] = [0x1D, 0x49, 0x01];
/// `GS I 66`: maker name (`_` + ASCII + NUL).
pub const GS_I_MAKER: [u8; 3] = [0x1D, 0x49, 0x42];
/// `GS I 67`: model name (`_` + ASCII + NUL).
pub const GS_I_MODEL_NAME: [u8; 3] = [0x1D, 0x49, 0x43];

/// Drawer pulse on time / off time in 2 ms units: 50 ms on, 500 ms off.
pub const KICK_ON: u8 = 25;
pub const KICK_OFF: u8 = 250;

/// Every `DLE EOT` reply has bits 0 and 7 clear and bits 1 and 4 set (`0xx1xx10`).
pub fn is_status_byte(b: u8) -> bool {
    b & 0b1001_0011 == 0b0001_0010
}

/// The drawer-kick sequence for a dialect, or `None` where it has none (plain text).
pub fn drawer_kick(dialect: CommandDialect, pin: DrawerPin) -> Option<Vec<u8>> {
    match dialect {
        // ESC p m t1 t2: m = 0 drives pin 2, m = 1 drives pin 5.
        CommandDialect::EscPos => Some(vec![
            0x1B,
            0x70,
            match pin {
                DrawerPin::Pin2 => 0,
                DrawerPin::Pin5 => 1,
            },
            KICK_ON,
            KICK_OFF,
        ]),
        // BEL opens drawer 1 (pin 2), SUB opens drawer 2 (pin 5).
        CommandDialect::StarLine => Some(vec![match pin {
            DrawerPin::Pin2 => 0x07,
            DrawerPin::Pin5 => 0x1A,
        }]),
        CommandDialect::PlainText => None,
    }
}

/// The drawer sensor from a `DLE EOT 1` reply (bit 2 is the level of kick connector pin 3).
pub fn drawer_open(printer: u8, level: DrawerOpenLevel) -> bool {
    let high = printer & 0x04 != 0;
    match level {
        DrawerOpenLevel::High => high,
        DrawerOpenLevel::Low => !high,
    }
}

/// The raw replies to `DLE EOT 1/2/3/4` (each `None` when not asked or not answered).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusBytes {
    pub printer: Option<u8>,
    pub offline: Option<u8>,
    pub error: Option<u8>,
    pub paper: Option<u8>,
}

impl StatusBytes {
    /// Whether the offline-cause reply says an error occurred (worth asking `DLE EOT 3`).
    pub fn has_error(&self) -> bool {
        self.offline.is_some_and(|b| b & 0x40 != 0)
    }

    pub fn to_status(&self, level: DrawerOpenLevel) -> PrinterStatus {
        let mut st = PrinterStatus::unknown();
        if let Some(p) = self.printer {
            st.online = p & 0x08 == 0;
            st.drawer_open = Some(drawer_open(p, level));
        }
        let mut paper_end_stop = false;
        if let Some(o) = self.offline {
            st.cover_open = o & 0x04 != 0;
            paper_end_stop = o & 0x20 != 0;
            if o & 0x40 != 0 {
                st.error = Some(describe_error(self.error));
            }
        }
        st.paper = match self.paper {
            Some(r) if r & 0x60 != 0 => PaperLevel::Out,
            _ if paper_end_stop => PaperLevel::Out,
            Some(r) if r & 0x0C != 0 => PaperLevel::NearEnd,
            Some(_) => PaperLevel::Ok,
            None => PaperLevel::Unknown,
        };
        st
    }
}

fn describe_error(error: Option<u8>) -> String {
    let Some(e) = error else {
        return "the printer reports an error".into();
    };
    let mut parts = Vec::new();
    if e & 0x08 != 0 {
        parts.push("auto-cutter error (paper jam?)");
    }
    if e & 0x20 != 0 {
        parts.push("unrecoverable error: switch the printer off and on");
    }
    if e & 0x40 != 0 {
        parts.push("recoverable error (e.g. print head too hot)");
    }
    if e & 0x04 != 0 {
        parts.push("mechanical error");
    }
    if parts.is_empty() {
        "the printer reports an error".into()
    } else {
        parts.join(", ")
    }
}

/// The text in a `GS I 66/67` reply: header `0x5F`, ASCII, `NUL`. Bytes before the header
/// (a late answer to an earlier query) are skipped.
pub fn parse_gs_i_text(reply: &[u8]) -> Option<String> {
    let start = reply.iter().position(|b| *b == 0x5F)? + 1;
    let body = &reply[start..];
    let end = body.iter().position(|b| *b == 0).unwrap_or(body.len());
    let text: String = body[..end]
        .iter()
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|b| char::from(*b))
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_device_ports::PrintFailure;

    const IDLE: u8 = 0x12; // 0001_0010

    #[test]
    fn status_byte_shape() {
        assert!(is_status_byte(IDLE));
        assert!(is_status_byte(0x16));
        assert!(is_status_byte(0x72));
        assert!(!is_status_byte(0x00));
        assert!(!is_status_byte(0x5F));
        assert!(!is_status_byte(0x92));
    }

    #[test]
    fn ready_printer() {
        let b = StatusBytes {
            printer: Some(IDLE),
            offline: Some(IDLE),
            error: None,
            paper: Some(IDLE),
        };
        let st = b.to_status(DrawerOpenLevel::High);
        assert!(st.online && !st.cover_open && st.error.is_none());
        assert_eq!(st.paper, PaperLevel::Ok);
        assert_eq!(st.drawer_open, Some(false));
        assert_eq!(st.blocking_failure(), None);
    }

    #[test]
    fn failures_from_bits() {
        // Paper end: offline bit, paper-end stop, sensor end bits.
        let st = StatusBytes {
            printer: Some(IDLE | 0x08),
            offline: Some(IDLE | 0x20),
            error: None,
            paper: Some(IDLE | 0x60),
        }
        .to_status(DrawerOpenLevel::High);
        assert_eq!(st.paper, PaperLevel::Out);
        assert_eq!(st.blocking_failure(), Some(PrintFailure::PaperOut));

        let st = StatusBytes {
            printer: Some(IDLE | 0x08),
            offline: Some(IDLE | 0x04),
            error: None,
            paper: Some(IDLE),
        }
        .to_status(DrawerOpenLevel::High);
        assert!(st.cover_open);
        assert_eq!(st.blocking_failure(), Some(PrintFailure::CoverOpen));

        let st = StatusBytes {
            printer: Some(IDLE | 0x08),
            offline: Some(IDLE | 0x40),
            error: Some(IDLE | 0x08),
            paper: Some(IDLE | 0x0C),
        }
        .to_status(DrawerOpenLevel::High);
        assert_eq!(st.paper, PaperLevel::NearEnd);
        assert!(st.error.as_deref().unwrap().contains("cutter"));
        assert_eq!(st.blocking_failure(), Some(PrintFailure::Offline));

        let st = StatusBytes {
            printer: Some(IDLE | 0x04),
            ..Default::default()
        };
        assert_eq!(st.to_status(DrawerOpenLevel::High).drawer_open, Some(true));
        assert_eq!(st.to_status(DrawerOpenLevel::Low).drawer_open, Some(false));
        assert_eq!(st.to_status(DrawerOpenLevel::Low).paper, PaperLevel::Unknown);
    }

    #[test]
    fn kicks() {
        assert_eq!(
            drawer_kick(CommandDialect::EscPos, DrawerPin::Pin2).unwrap(),
            [0x1B, 0x70, 0, 25, 250]
        );
        assert_eq!(
            drawer_kick(CommandDialect::EscPos, DrawerPin::Pin5).unwrap(),
            [0x1B, 0x70, 1, 25, 250]
        );
        assert_eq!(
            drawer_kick(CommandDialect::StarLine, DrawerPin::Pin2).unwrap(),
            [0x07]
        );
        assert_eq!(
            drawer_kick(CommandDialect::StarLine, DrawerPin::Pin5).unwrap(),
            [0x1A]
        );
        assert_eq!(drawer_kick(CommandDialect::PlainText, DrawerPin::Pin2), None);
    }

    #[test]
    fn gs_i_text() {
        assert_eq!(parse_gs_i_text(b"_EPSON\0").as_deref(), Some("EPSON"));
        assert_eq!(parse_gs_i_text(b"\x20_TM-T88V\0").as_deref(), Some("TM-T88V"));
        assert_eq!(parse_gs_i_text(b"_\0"), None);
        assert_eq!(parse_gs_i_text(b"EPSON"), None);
    }
}
