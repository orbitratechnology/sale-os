//! Customer displays: the 2×20 VFD pole displays that face the customer, on a serial or
//! USB-serial port, implementing [`CustomerDisplayPort`].
//!
//! Two command sets ([`DisplayProtocol`]):
//!
//! - **Epson ESC/POS** (DM-D110/D210 and compatibles): `ESC @` initialise, `US MD1`
//!   overwrite mode, `US C 0` cursor off, `CLR` clear; each line written at `US $ 1 n`.
//! - **CD5220**, used by many unbranded displays: `ESC @`, `ESC DC1` overwrite mode, `CLR`;
//!   each line written with `ESC Q A` (upper) / `ESC Q B` (lower), ended by `CR`.
//!
//! Every line is written whole, padded to 20 columns, so nothing needs clearing between
//! frames (no flicker). Text is reduced to printable ASCII, which every display's
//! character table has: accents and other scripts are transliterated (`Café` → `Cafe`),
//! and what has no transliteration is dropped. A name too long for its place is cut at
//! the column limit, dropping a word fragment of up to three letters and any trailing
//! separator.
//!
//! Showing never waits: [`HwCustomerDisplay::show`] replaces the pending frame and returns;
//! a task writes the latest one. The port is kept open between frames and reopened after
//! an error; errors are logged and become the display's health, nothing else.

use crate::config::{FieldError, SerialSettings};
use crate::transport::{Link, SerialTransport, Transport, TransportError};
use pos_device_ports::{CustomerDisplayPort, DeviceError, DeviceHealth, DisplayFrame, DisplayLine};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Characters per line of a pole display.
pub const COLUMNS: usize = 20;

/// The display's command set.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum DisplayProtocol {
    /// Epson ESC/POS customer display commands.
    #[default]
    EscPos,
    /// The CD5220 command set.
    Cd5220,
}

impl DisplayProtocol {
    pub fn name(self) -> &'static str {
        match self {
            DisplayProtocol::EscPos => "ESC/POS",
            DisplayProtocol::Cd5220 => "CD5220",
        }
    }
}

/// One customer display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DisplayConfig {
    pub serial: SerialSettings,
    #[serde(default)]
    pub protocol: DisplayProtocol,
}

impl DisplayConfig {
    pub fn new(serial: SerialSettings, protocol: DisplayProtocol) -> Self {
        Self { serial, protocol }
    }

    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errors = Vec::new();
        self.serial.validate_into("serial.", &mut errors);
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}

// ---------------------------------------------------------------------------------------
// Text and layout

/// `s` in printable ASCII: accents and other scripts transliterated, control characters
/// and anything without a transliteration dropped, runs of spaces collapsed.
pub fn display_text(s: &str) -> String {
    let mut raw = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_graphic() {
            raw.push(c);
        } else if c.is_whitespace() || c.is_ascii_control() {
            raw.push(' ');
        } else if is_pictograph(c) {
            // Emoji and symbols would come back as words ("tea"): dropped.
        } else if let Some(t) = deunicode::deunicode_char(c) {
            raw.extend(t.chars().filter(|c| c.is_ascii_graphic() || *c == ' '));
        }
    }
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Emoji, pictographs, dingbats and their variation selectors.
fn is_pictograph(c: char) -> bool {
    matches!(u32::from(c),
        0x2190..=0x21FF | 0x2300..=0x23FF | 0x25A0..=0x27BF | 0x2B00..=0x2BFF | 0xFE00..=0xFE0F
        | 0x1F000..=0x1FAFF | 0xE0000..=0xE007F)
}

/// Cuts `s` (ASCII, from [`display_text`]) to at most `width` columns, cleanly: a word
/// cut after three letters or fewer is dropped, as are trailing spaces and separators.
fn fit(s: &str, width: usize) -> String {
    if !s.is_ascii() {
        return fit(&display_text(s), width);
    }
    if s.len() <= width {
        return s.to_owned();
    }
    let mut cut = &s[..width];
    let mid_word = s.as_bytes()[width] != b' ';
    if mid_word
        && let Some(space) = cut.rfind(' ')
        && width - space - 1 <= 3
    {
        cut = &cut[..space];
    }
    cut.trim_end_matches([' ', '-', ',', '/', ';', ':', '(', '&', '+'])
        .to_owned()
}

/// One line, exactly `width` columns of printable ASCII.
pub fn layout(line: &DisplayLine, width: usize) -> String {
    let text = match line {
        DisplayLine::Text(t) => fit(&display_text(t), width),
        DisplayLine::Centered(t) => {
            let t = fit(&display_text(t), width);
            format!("{}{t}", " ".repeat((width - t.len()) / 2))
        }
        DisplayLine::Pair(label, value) => {
            let value = display_text(value);
            if value.len() >= width {
                // An amount is never cut in the middle: keep its right end (the decimals).
                value[value.len() - width..].to_owned()
            } else {
                let room = width - value.len();
                let label = fit(&display_text(label), room.saturating_sub(1));
                format!("{label}{}{value}", " ".repeat(room - label.len()))
            }
        }
    };
    format!("{text:<width$}")
}

// ---------------------------------------------------------------------------------------
// Commands

const ESC: u8 = 0x1B;
const US: u8 = 0x1F;
const CLR: u8 = 0x0C;
const CR: u8 = 0x0D;

/// Sent once after the port opens: initialise, overwrite mode, cursor off, clear.
pub fn init_bytes(protocol: DisplayProtocol) -> Vec<u8> {
    match protocol {
        // ESC @, US MD1 (overwrite), US C 0 (cursor off), CLR.
        DisplayProtocol::EscPos => vec![ESC, b'@', US, 0x01, US, b'C', 0x00, CLR],
        // ESC @, ESC DC1 (overwrite), CLR.
        DisplayProtocol::Cd5220 => vec![ESC, b'@', ESC, 0x11, CLR],
    }
}

/// Both lines of `frame`, each written whole.
pub fn frame_bytes(protocol: DisplayProtocol, frame: &DisplayFrame) -> Vec<u8> {
    let upper = layout(&frame.upper, COLUMNS);
    let lower = layout(&frame.lower, COLUMNS);
    let mut out = Vec::with_capacity(2 * (COLUMNS + 4));
    match protocol {
        DisplayProtocol::EscPos => {
            // US $ x y: cursor to column x, row y (from 1).
            out.extend_from_slice(&[US, b'$', 1, 1]);
            out.extend_from_slice(upper.as_bytes());
            out.extend_from_slice(&[US, b'$', 1, 2]);
            out.extend_from_slice(lower.as_bytes());
        }
        DisplayProtocol::Cd5220 => {
            out.extend_from_slice(&[ESC, b'Q', b'A']);
            out.extend_from_slice(upper.as_bytes());
            out.push(CR);
            out.extend_from_slice(&[ESC, b'Q', b'B']);
            out.extend_from_slice(lower.as_bytes());
            out.push(CR);
        }
    }
    out
}

/// What "Test display" shows: the name, and a ruler that must fill the line exactly.
pub fn test_frame() -> DisplayFrame {
    DisplayFrame::new(
        DisplayLine::Centered("SALE OS DISPLAY TEST".into()),
        DisplayLine::Text("12345678901234567890".into()),
    )
}

// ---------------------------------------------------------------------------------------
// The device

/// Timeouts for a display: opening the port, and writing one frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DisplayTimeouts {
    pub open: Duration,
    pub write: Duration,
}

impl Default for DisplayTimeouts {
    fn default() -> Self {
        Self {
            open: Duration::from_secs(2),
            write: Duration::from_secs(2),
        }
    }
}

struct Shared {
    transport: Arc<dyn Transport>,
    protocol: DisplayProtocol,
    timeouts: DisplayTimeouts,
    link: tokio::sync::Mutex<Option<Box<dyn Link>>>,
    health: watch::Sender<DeviceHealth>,
}

impl Shared {
    async fn write(&self, frame: &DisplayFrame) -> Result<(), DeviceError> {
        let result = self.write_inner(frame).await;
        let next = match &result {
            Ok(()) => DeviceHealth::Ready,
            Err(e) => DeviceHealth::Offline(e.to_string()),
        };
        self.health.send_if_modified(|h| {
            if *h == next {
                return false;
            }
            match &next {
                DeviceHealth::Offline(why) => {
                    tracing::warn!(target: "pos_device_hw", display = %self.transport.describe(), problem = %why, "customer display not working");
                }
                _ => {
                    tracing::info!(target: "pos_device_hw", display = %self.transport.describe(), "customer display working");
                }
            }
            *h = next;
            true
        });
        result
    }

    /// Opens the port. A port still being closed by the display this one replaced (new
    /// settings) is busy for a moment: tried again briefly.
    async fn open(&self) -> Result<Box<dyn Link>, TransportError> {
        let mut tries = 0;
        loop {
            match self.transport.open(self.timeouts.open).await {
                Err(TransportError::Busy(_)) if tries < 5 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                other => return other,
            }
        }
    }

    async fn write_inner(&self, frame: &DisplayFrame) -> Result<(), DeviceError> {
        let mut slot = self.link.lock().await;
        if slot.is_none() {
            let mut link = self.open().await.map_err(|e| e.to_device_error())?;
            link.write_all(&init_bytes(self.protocol), self.timeouts.write)
                .await
                .map_err(|e| e.to_device_error())?;
            *slot = Some(link);
        }
        let Some(link) = slot.as_mut() else {
            return Err(DeviceError::Disconnected("the display port is not open".into()));
        };
        let bytes = frame_bytes(self.protocol, frame);
        if let Err(e) = link.write_all(&bytes, self.timeouts.write).await {
            // Open again next time (unplugged and plugged back in, say).
            *slot = None;
            return Err(e.to_device_error());
        }
        Ok(())
    }
}

/// A customer display on any [`Transport`] (in practice a serial port).
pub struct HwCustomerDisplay {
    shared: Arc<Shared>,
    pending: watch::Sender<Option<DisplayFrame>>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for HwCustomerDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwCustomerDisplay")
            .field("display", &self.shared.transport.describe())
            .field("protocol", &self.shared.protocol)
            .finish_non_exhaustive()
    }
}

impl HwCustomerDisplay {
    /// Starts the display's writer task. Needs a Tokio runtime.
    pub fn new(transport: Arc<dyn Transport>, protocol: DisplayProtocol, timeouts: DisplayTimeouts) -> Self {
        let shared = Arc::new(Shared {
            transport,
            protocol,
            timeouts,
            link: tokio::sync::Mutex::new(None),
            health: watch::channel(DeviceHealth::Unknown).0,
        });
        let (pending, mut rx) = watch::channel::<Option<DisplayFrame>>(None);
        let writer = Arc::clone(&shared);
        let task = tokio::spawn(async move {
            // `changed` wakes once however many frames were queued meanwhile: only the
            // latest is written.
            while rx.changed().await.is_ok() {
                let frame = rx.borrow_and_update().clone();
                if let Some(frame) = frame {
                    let _ = writer.write(&frame).await;
                }
            }
        });
        Self {
            shared,
            pending,
            task,
        }
    }

    /// The display these settings describe. Validates them; needs a Tokio runtime.
    pub fn from_config(config: &DisplayConfig) -> Result<Self, DeviceError> {
        config.validate().map_err(|errors| {
            DeviceError::Unsupported(format!(
                "invalid display settings: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
        })?;
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(DeviceError::Unsupported("no runtime to drive the display".into()));
        }
        Ok(Self::new(
            Arc::new(SerialTransport::new(config.serial.clone())),
            config.protocol,
            DisplayTimeouts::default(),
        ))
    }

    pub fn protocol(&self) -> DisplayProtocol {
        self.shared.protocol
    }
}

impl Drop for HwCustomerDisplay {
    fn drop(&mut self) {
        // The writer task holds the port; stopping it closes the port.
        self.task.abort();
    }
}

#[async_trait::async_trait]
impl CustomerDisplayPort for HwCustomerDisplay {
    fn describe(&self) -> String {
        format!(
            "{}, {}",
            self.shared.transport.describe(),
            self.shared.protocol.name()
        )
    }

    fn show(&self, frame: DisplayFrame) {
        self.pending.send_replace(Some(frame));
    }

    async fn test(&self, frame: DisplayFrame) -> Result<(), DeviceError> {
        self.shared.write(&frame).await
    }

    fn health(&self) -> watch::Receiver<DeviceHealth> {
        self.shared.health.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(l: &str, r: &str) -> DisplayLine {
        DisplayLine::Pair(l.into(), r.into())
    }

    #[test]
    fn text_is_reduced_to_ascii() {
        assert_eq!(display_text("Café crème"), "Cafe creme");
        assert_eq!(display_text("Größe  Ñandú\tÅ"), "Grosse Nandu A");
        assert_eq!(display_text("Żółć"), "Zolc");
        assert_eq!(display_text("Привет"), "Privet");
        assert_eq!(display_text("a\u{0}b\nc"), "a b c");
        // Other scripts are written by sound (Sinhala and Tamil "milk"; Chinese as pinyin);
        // emoji and symbols are dropped.
        assert_eq!(display_text("කිරි"), "kiri");
        assert_eq!(display_text("பால்"), "paal");
        assert_eq!(display_text("牛奶"), "Niu Nai");
        assert_eq!(display_text("Tea 🍵"), "Tea");
        assert_eq!(display_text("★ Special ☕"), "Special");
    }

    #[test]
    fn long_names_are_cut_cleanly() {
        assert_eq!(fit("Short", 20), "Short");
        assert_eq!(fit("12345678901234567890", 20), "12345678901234567890");
        // A cut inside a long word keeps the letters.
        assert_eq!(fit("Supercalifragilistic expialidocious", 10), "Supercalif");
        // A fragment of three letters or fewer is dropped, and so is a dangling separator.
        assert_eq!(fit("Chocolate Biscuits Family", 11), "Chocolate");
        assert_eq!(fit("Milk - Full Cream 1L", 6), "Milk");
        assert_eq!(fit("Rice, Red Raw 5kg bag", 12), "Rice, Red");
        assert_eq!(fit("Basmati Rice Premium", 13), "Basmati Rice");
    }

    #[test]
    fn every_line_is_exactly_twenty_columns() {
        for line in [
            DisplayLine::Text(String::new()),
            DisplayLine::Text("A very long product name indeed, longer than twenty".into()),
            DisplayLine::Centered("Kamal Stores".into()),
            pair("Chocolate Biscuits Family Pack", "1,250.00"),
            pair("TOTAL", "123,456,789.00"),
            pair("", "99,999,999,999.00"),
            pair("කිරි පැකට්", "250.00"),
        ] {
            let l = layout(&line, COLUMNS);
            assert_eq!(l.len(), COLUMNS, "{l:?}");
            assert!(l.bytes().all(|b| (0x20..0x7F).contains(&b)), "{l:?}");
        }
        assert_eq!(
            layout(&pair("Chocolate Biscuits Family Pack", "1,250.00"), 20),
            "Chocolate   1,250.00"
        );
        assert_eq!(layout(&pair("Milk", "250.00"), 20), "Milk          250.00");
        assert_eq!(layout(&pair("TOTAL", "1,250.00"), 20), "TOTAL       1,250.00");
        assert_eq!(layout(&pair("Café crème", "450.00"), 20), "Cafe creme    450.00");
        assert_eq!(
            layout(&pair("Label", "99,999,999,999.00"), 20),
            "La 99,999,999,999.00"
        );
        // An amount wider than the display keeps its right end.
        assert_eq!(
            layout(&pair("X", "1,000,000,000,000,000.00"), 20),
            "0,000,000,000,000.00"
        );
        assert_eq!(
            layout(&DisplayLine::Centered("Kamal Stores".into()), 20),
            "    Kamal Stores    "
        );
        assert_eq!(
            layout(&DisplayLine::Text("Hi".into()), 20),
            "Hi                  "
        );
    }

    #[test]
    fn esc_pos_bytes() {
        assert_eq!(
            init_bytes(DisplayProtocol::EscPos),
            [0x1B, 0x40, 0x1F, 0x01, 0x1F, 0x43, 0x00, 0x0C]
        );
        let frame = DisplayFrame::new(pair("Milk", "250.00"), pair("TOTAL", "750.00"));
        let mut want = vec![0x1F, 0x24, 1, 1];
        want.extend_from_slice(b"Milk          250.00");
        want.extend_from_slice(&[0x1F, 0x24, 1, 2]);
        want.extend_from_slice(b"TOTAL         750.00");
        assert_eq!(frame_bytes(DisplayProtocol::EscPos, &frame), want);
    }

    #[test]
    fn cd5220_bytes() {
        assert_eq!(
            init_bytes(DisplayProtocol::Cd5220),
            [0x1B, 0x40, 0x1B, 0x11, 0x0C]
        );
        let frame = DisplayFrame::new(pair("Milk", "250.00"), pair("TOTAL", "750.00"));
        let mut want = vec![0x1B, b'Q', b'A'];
        want.extend_from_slice(b"Milk          250.00");
        want.push(0x0D);
        want.extend_from_slice(&[0x1B, b'Q', b'B']);
        want.extend_from_slice(b"TOTAL         750.00");
        want.push(0x0D);
        assert_eq!(frame_bytes(DisplayProtocol::Cd5220, &frame), want);
    }

    #[test]
    fn config_json_and_validation() {
        let c: DisplayConfig =
            serde_json::from_str(r#"{"serial":{"port":"/dev/ttyUSB1","baud":9600},"protocol":"cd5220"}"#)
                .unwrap();
        assert_eq!(c.protocol, DisplayProtocol::Cd5220);
        let c: DisplayConfig = serde_json::from_str(r#"{"serial":{"port":"/dev/ttyUSB1"}}"#).unwrap();
        assert_eq!(c.protocol, DisplayProtocol::EscPos);
        assert_eq!(serde_json::to_value(&c).unwrap()["protocol"], "escPos");
        if cfg!(unix) {
            assert!(c.validate().is_ok());
        }
        let bad = DisplayConfig::new(SerialSettings::new("", 9601), DisplayProtocol::EscPos);
        let fields: Vec<_> = bad.validate().unwrap_err().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, ["serial.port", "serial.baud"]);
    }
}
