//! Serial scales (E20-T06): RS-232 or USB-serial counter scales, implementing [`ScalePort`].
//!
//! Two protocols, chosen per scale:
//!
//! - **Toledo 8217** ("8217 mode", also offered by many CAS, Dibal and scanner-scale
//!   combos): the till sends `W`; the scale answers `STX` weight `CR` when the weight is
//!   stable, or `STX ? s CR` with a status byte `s` (bit 0 in motion, bit 1 at zero, bits 2
//!   and 3 RAM/EEPROM error, bit 4 under capacity, bit 5 over capacity; bit 6 always set so
//!   the byte is printable), optionally followed by a second status byte whose low bits are
//!   further faults. The frame carries no unit: the scale's unit (kg or g) is a setting.
//! - **Continuous ASCII** (many indicator heads, e.g. A&D and unbranded ones): the scale
//!   sends a line several times a second, `ST,GS,+  1.234kg`: `ST` stable, `US` unstable,
//!   `OL` over (or under, with a minus sign) range; `GS` gross or `NT` net; the signed weight
//!   with its unit. The till takes the first whole line after it asks.
//!
//! Parsing is pure ([`parse_toledo`], [`parse_continuous`]) and exact: weights are decimal
//! text read into milligrams ([`Weight::parse`]), never floats. Pounds and ounces are refused
//! with a message saying to set the scale to kilograms.
//!
//! [`HwScale`] keeps the port open between readings, reopens it after an error, and makes
//! each reading bounded in time; its health says why it is not working.

use crate::config::{FieldError, SerialSettings};
use crate::transport::{Link, SerialTransport, Transport, TransportError};
use pos_device_ports::{DeviceError, DeviceHealth, ScalePort, ScaleReading, Weight, WeightUnit};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const STX: u8 = 0x02;
const CR: u8 = 0x0D;
const LF: u8 = 0x0A;

/// The scale's protocol.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum ScaleProtocol {
    /// Request `W`, answer `STX weight CR` or `STX ? status CR`.
    #[default]
    Toledo8217,
    /// The scale sends `ST,GS,+  1.234kg` lines by itself.
    ContinuousAscii,
}

impl ScaleProtocol {
    pub fn name(self) -> &'static str {
        match self {
            ScaleProtocol::Toledo8217 => "Toledo 8217",
            ScaleProtocol::ContinuousAscii => "continuous ASCII",
        }
    }
}

/// The unit a Toledo-8217 scale weighs in (its frames do not say).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum ScaleUnit {
    #[default]
    Kg,
    G,
}

impl From<ScaleUnit> for WeightUnit {
    fn from(u: ScaleUnit) -> Self {
        match u {
            ScaleUnit::Kg => WeightUnit::Kg,
            ScaleUnit::G => WeightUnit::G,
        }
    }
}

/// One scale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ScaleConfig {
    pub serial: SerialSettings,
    #[serde(default)]
    pub protocol: ScaleProtocol,
    /// Toledo 8217 only: what the weight in its frames is in.
    #[serde(default)]
    pub unit: ScaleUnit,
}

impl ScaleConfig {
    pub fn new(serial: SerialSettings, protocol: ScaleProtocol) -> Self {
        Self {
            serial,
            protocol,
            unit: ScaleUnit::Kg,
        }
    }

    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errors = Vec::new();
        self.serial.validate_into("serial.", &mut errors);
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}

/// Why a scale's answer could not be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("unreadable answer {0:?} (check the speed, data bits and protocol)")]
    Bad(String),
    #[error("the scale weighs in {0}; set it to kilograms")]
    Unit(String),
    #[error("the scale reports a fault: {0}")]
    Fault(String),
}

fn shown(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|&b| std::ascii::escape_default(b))
        .map(char::from)
        .collect()
}

/// Reads one Toledo-8217 answer: the bytes between `STX` and `CR`.
pub fn parse_toledo(frame: &[u8], unit: WeightUnit) -> Result<ScaleReading, FrameError> {
    let bad = || FrameError::Bad(shown(frame));
    if let Some(status) = frame.strip_prefix(b"?") {
        let s1 = *status.first().ok_or_else(bad)?;
        // Bit 6 is always set: anything else is not a status byte (wrong speed or parity).
        if s1 & 0x40 == 0 {
            return Err(bad());
        }
        let mut faults = Vec::new();
        if s1 & 0x04 != 0 {
            faults.push("RAM error");
        }
        if s1 & 0x08 != 0 {
            faults.push("EEPROM error");
        }
        if let Some(&s2) = status.get(1) {
            if s2 & 0x40 == 0 {
                return Err(bad());
            }
            if s2 & 0x01 != 0 {
                faults.push("ROM error");
            }
            if s2 & 0x02 != 0 {
                faults.push("calibration error");
            }
            if s2 & 0x04 != 0 {
                faults.push("initial zero out of range");
            }
        }
        if !faults.is_empty() {
            return Err(FrameError::Fault(faults.join(", ")));
        }
        return Ok(if s1 & 0x20 != 0 {
            ScaleReading::Overload
        } else if s1 & 0x10 != 0 {
            ScaleReading::Underload
        } else if s1 & 0x01 != 0 {
            ScaleReading::Moving(None)
        } else if s1 & 0x02 != 0 {
            ScaleReading::Stable(Weight::ZERO)
        } else {
            // A status with nothing set gives no weight: ask again.
            ScaleReading::Moving(None)
        });
    }
    let text = std::str::from_utf8(frame).map_err(|_| bad())?;
    Weight::parse(text, unit)
        .map(ScaleReading::Stable)
        .ok_or_else(bad)
}

/// Reads one continuous-output line (without its CR/LF), e.g. `ST,GS,+  1.234kg`.
pub fn parse_continuous(line: &[u8]) -> Result<ScaleReading, FrameError> {
    let bad = || FrameError::Bad(shown(line));
    let text = std::str::from_utf8(line).map_err(|_| bad())?;
    let text = text.trim_matches(|c: char| c == '\u{2}' || c.is_whitespace());
    let header = text.get(..2).ok_or_else(bad)?.to_ascii_uppercase();
    let mut rest = text[2..].trim_start_matches([',', ' ']);
    // The second header: gross, net, or tare (a tare value is not a weight to sell).
    if let Some(h2) = rest.get(..2).map(str::to_ascii_uppercase) {
        match h2.as_str() {
            "GS" | "NT" => rest = rest[2..].trim_start_matches([',', ' ']),
            "TR" => return Err(FrameError::Bad(format!("{} (a tare value)", shown(line)))),
            _ => {}
        }
    }
    let number_end = rest.find(|c: char| c.is_ascii_alphabetic()).ok_or_else(bad)?;
    let (number, unit) = rest.split_at(number_end);
    let unit_text = unit.trim();
    let unit = WeightUnit::from_symbol(unit_text).ok_or_else(|| {
        if ["lb", "oz", "t", "ct", "dwt", "ozt"].contains(&unit_text.to_ascii_lowercase().as_str()) {
            FrameError::Unit(unit_text.to_owned())
        } else {
            bad()
        }
    })?;
    let negative = number.trim_start().starts_with('-');
    match header.as_str() {
        "OL" => {
            return Ok(if negative {
                ScaleReading::Underload
            } else {
                ScaleReading::Overload
            });
        }
        "UL" => return Ok(ScaleReading::Underload),
        _ => {}
    }
    let weight = Weight::parse(number, unit).ok_or_else(bad)?;
    match header.as_str() {
        "ST" => Ok(ScaleReading::Stable(weight)),
        "US" => Ok(ScaleReading::Moving(Some(weight))),
        _ => Err(bad()),
    }
}

/// How long a reading may take.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ScaleTimeouts {
    pub open: Duration,
    pub write: Duration,
    /// For the answer (Toledo) or for a whole line (continuous).
    pub reply: Duration,
}

impl Default for ScaleTimeouts {
    fn default() -> Self {
        Self {
            open: Duration::from_secs(2),
            write: Duration::from_millis(500),
            reply: Duration::from_millis(1500),
        }
    }
}

/// A scale on any [`Transport`] (in practice a serial port).
pub struct HwScale {
    transport: Arc<dyn Transport>,
    protocol: ScaleProtocol,
    unit: ScaleUnit,
    timeouts: ScaleTimeouts,
    link: tokio::sync::Mutex<Option<Box<dyn Link>>>,
    health: watch::Sender<DeviceHealth>,
}

impl std::fmt::Debug for HwScale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwScale")
            .field("scale", &self.transport.describe())
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

enum ReadError {
    Link(TransportError),
    NoAnswer,
    Frame(FrameError),
}

impl HwScale {
    pub fn new(
        transport: Arc<dyn Transport>,
        protocol: ScaleProtocol,
        unit: ScaleUnit,
        timeouts: ScaleTimeouts,
    ) -> Self {
        Self {
            transport,
            protocol,
            unit,
            timeouts,
            link: tokio::sync::Mutex::new(None),
            health: watch::channel(DeviceHealth::Unknown).0,
        }
    }

    /// The scale these settings describe (validated). Nothing is opened until a reading.
    pub fn from_config(config: &ScaleConfig) -> Result<Self, DeviceError> {
        config.validate().map_err(|errors| {
            DeviceError::Unsupported(format!(
                "invalid scale settings: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
        })?;
        Ok(Self::new(
            Arc::new(SerialTransport::new(config.serial.clone())),
            config.protocol,
            config.unit,
            ScaleTimeouts::default(),
        ))
    }

    pub fn with_timeouts(mut self, timeouts: ScaleTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    fn set_health(&self, next: DeviceHealth) {
        self.health.send_if_modified(|h| {
            if *h == next {
                return false;
            }
            match &next {
                DeviceHealth::Offline(why) => {
                    tracing::warn!(target: "pos_device_hw", scale = %self.transport.describe(), problem = %why, "scale not working");
                }
                _ => tracing::info!(target: "pos_device_hw", scale = %self.transport.describe(), "scale working"),
            }
            *h = next;
            true
        });
    }

    async fn open(&self) -> Result<Box<dyn Link>, TransportError> {
        // A port still being closed by the scale this one replaced is busy for a moment.
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

    /// Throws away whatever arrived before this reading.
    async fn drain(link: &mut Box<dyn Link>) -> Result<(), TransportError> {
        for _ in 0..64 {
            if link.read(4096, Duration::from_millis(1)).await?.is_empty() {
                break;
            }
        }
        Ok(())
    }

    async fn exchange(&self, link: &mut Box<dyn Link>) -> Result<ScaleReading, ReadError> {
        Self::drain(link).await.map_err(ReadError::Link)?;
        let deadline = Instant::now() + self.timeouts.reply;
        let mut buf: Vec<u8> = Vec::new();
        match self.protocol {
            ScaleProtocol::Toledo8217 => {
                link.write_all(b"W", self.timeouts.write)
                    .await
                    .map_err(ReadError::Link)?;
                loop {
                    if let Some(start) = buf.iter().position(|&b| b == STX)
                        && let Some(len) = buf[start + 1..].iter().position(|&b| b == CR)
                    {
                        return parse_toledo(&buf[start + 1..start + 1 + len], self.unit.into())
                            .map_err(ReadError::Frame);
                    }
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(if buf.is_empty() {
                            ReadError::NoAnswer
                        } else {
                            ReadError::Frame(FrameError::Bad(shown(&buf)))
                        });
                    }
                    buf.extend(link.read(256, left).await.map_err(ReadError::Link)?);
                }
            }
            ScaleProtocol::ContinuousAscii => {
                // The first bytes may be the middle of a line: skip to the first line end,
                // then take the next whole line.
                let mut synced = false;
                loop {
                    while let Some(end) = buf.iter().position(|&b| b == CR || b == LF) {
                        let line: Vec<u8> = buf.drain(..=end).collect();
                        let line = &line[..line.len() - 1];
                        if !synced {
                            synced = true;
                            continue;
                        }
                        if line.iter().all(|b| b.is_ascii_whitespace() || *b == STX) {
                            continue;
                        }
                        return parse_continuous(line).map_err(ReadError::Frame);
                    }
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(if buf.is_empty() && !synced {
                            ReadError::NoAnswer
                        } else {
                            ReadError::Frame(FrameError::Bad(shown(&buf)))
                        });
                    }
                    buf.extend(link.read(256, left).await.map_err(ReadError::Link)?);
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl ScalePort for HwScale {
    fn describe(&self) -> String {
        match self.protocol {
            ScaleProtocol::Toledo8217 => format!(
                "{}, {} ({})",
                self.transport.describe(),
                self.protocol.name(),
                match self.unit {
                    ScaleUnit::Kg => "kg",
                    ScaleUnit::G => "g",
                }
            ),
            ScaleProtocol::ContinuousAscii => {
                format!("{}, {}", self.transport.describe(), self.protocol.name())
            }
        }
    }

    async fn read(&self) -> Result<ScaleReading, DeviceError> {
        let mut slot = self.link.lock().await;
        if slot.is_none() {
            match self.open().await {
                Ok(link) => *slot = Some(link),
                Err(e) => {
                    let e = e.to_device_error();
                    self.set_health(DeviceHealth::Offline(e.to_string()));
                    return Err(e);
                }
            }
        }
        let Some(link) = slot.as_mut() else {
            return Err(DeviceError::Disconnected("the scale port is not open".into()));
        };
        match self.exchange(link).await {
            Ok(reading) => {
                self.set_health(DeviceHealth::Ready);
                Ok(reading)
            }
            Err(e) => {
                // Open afresh next time (unplugged and plugged back in, say).
                *slot = None;
                let (health, error) = match e {
                    ReadError::Link(e) => {
                        let e = e.to_device_error();
                        (e.to_string(), e)
                    }
                    ReadError::NoAnswer => (
                        "the scale did not answer (check the cable, speed and protocol)".to_owned(),
                        DeviceError::Timeout,
                    ),
                    ReadError::Frame(f) => (f.to_string(), DeviceError::Io(f.to_string())),
                };
                self.set_health(DeviceHealth::Offline(health));
                Err(error)
            }
        }
    }

    fn health(&self) -> watch::Receiver<DeviceHealth> {
        self.health.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kg(mg: i64) -> Weight {
        Weight::from_milligrams(mg)
    }

    #[test]
    fn toledo_frames() {
        let p = |f: &[u8]| parse_toledo(f, WeightUnit::Kg);
        // Stable weights.
        assert_eq!(p(b"01.234"), Ok(ScaleReading::Stable(kg(1_234_000))));
        assert_eq!(p(b" 0.455"), Ok(ScaleReading::Stable(kg(455_000))));
        assert_eq!(
            parse_toledo(b"0250", WeightUnit::G),
            Ok(ScaleReading::Stable(kg(250_000)))
        );
        // Status: in motion, at zero, under and over capacity.
        assert_eq!(p(b"?A"), Ok(ScaleReading::Moving(None)));
        assert_eq!(p(b"?B"), Ok(ScaleReading::Stable(Weight::ZERO)));
        assert_eq!(p(b"?P"), Ok(ScaleReading::Underload));
        assert_eq!(p(b"?`"), Ok(ScaleReading::Overload));
        assert_eq!(
            p(b"?a"),
            Ok(ScaleReading::Overload),
            "over capacity wins over motion"
        );
        assert_eq!(p(b"?@@"), Ok(ScaleReading::Moving(None)));
        // Faults.
        assert_eq!(p(b"?D"), Err(FrameError::Fault("RAM error".into())));
        assert_eq!(p(b"?@B"), Err(FrameError::Fault("calibration error".into())));
        // Bad frames: garbage (wrong speed), a status byte without bit 6, two points.
        for bad in [&b"\xff\x81"[..], b"?\x01", b"1.2.3", b"", b"?", b"12kg"] {
            assert!(matches!(p(bad), Err(FrameError::Bad(_))), "{bad:?}");
        }
    }

    #[test]
    fn continuous_frames() {
        let p = |f: &[u8]| parse_continuous(f);
        assert_eq!(p(b"ST,GS,+  1.234kg"), Ok(ScaleReading::Stable(kg(1_234_000))));
        assert_eq!(p(b"ST,NT,+0001.230kg"), Ok(ScaleReading::Stable(kg(1_230_000))));
        assert_eq!(p(b"ST,GS,+ 250.5 g"), Ok(ScaleReading::Stable(kg(250_500))));
        assert_eq!(p(b"\x02ST,GS,-  0.012kg"), Ok(ScaleReading::Stable(kg(-12_000))));
        assert_eq!(
            p(b"US,GS,+  1.230kg"),
            Ok(ScaleReading::Moving(Some(kg(1_230_000))))
        );
        assert_eq!(p(b"ST GS +  0.000kg"), Ok(ScaleReading::Stable(Weight::ZERO)));
        assert_eq!(p(b"OL,GS,+99999.9kg"), Ok(ScaleReading::Overload));
        assert_eq!(p(b"OL,GS,-    .   kg"), Ok(ScaleReading::Underload));
        assert_eq!(p(b"UL,GS,-  9.999kg"), Ok(ScaleReading::Underload));
        assert_eq!(p(b"ST,GS,+  2.500lb"), Err(FrameError::Unit("lb".into())));
        for bad in [
            &b"XX,GS,+  1.234kg"[..],
            b"ST,GS,+  1.2.3kg",
            b"ST,GS,+  1.234",
            b"ST,GS,+  1.234kq",
            b"ST,TR,+  0.100kg",
            b"S",
            b"\xff\xfe",
        ] {
            assert!(matches!(p(bad), Err(FrameError::Bad(_))), "{bad:?}");
        }
    }

    #[test]
    fn settings_validate_by_field() {
        let mut c = ScaleConfig::new(SerialSettings::new("", 9600), ScaleProtocol::Toledo8217);
        assert_eq!(c.validate().unwrap_err()[0].field, "serial.port");
        let port = if cfg!(unix) { "/dev/ttyUSB0" } else { "COM3" };
        c.serial = SerialSettings::new(port, 1234);
        assert_eq!(c.validate().unwrap_err()[0].field, "serial.baud");
        c.serial.baud = 9600;
        assert!(c.validate().is_ok());
        let json: ScaleConfig = serde_json::from_value(
            serde_json::json!({"serial": {"port": "COM3"}, "protocol": "continuousAscii"}),
        )
        .unwrap();
        assert_eq!(json.protocol, ScaleProtocol::ContinuousAscii);
        assert_eq!(json.unit, ScaleUnit::Kg);
        assert_eq!(json.serial.baud, 9600);
    }
}
