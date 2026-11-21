//! Device settings the app stores and passes in (camelCase JSON, exposed over IPC), their
//! validation with per-field errors for the settings screen, and [`build_printer`].

use crate::printer::{DrawerOpenLevel, HwPrinter, PrinterTimeouts};
use crate::profiles::{self, FontChoice};
use crate::transport::{
    DeviceFileTransport, SerialTransport, SpoolerTransport, TcpTransport, Transport, spooler,
};
use pos_device_ports::{CommandDialect, CutSupport, DeviceError, DrawerPin};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

/// Baud rates offered and accepted.
pub const STANDARD_BAUD_RATES: &[u32] = &[
    1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200, 230400, 460800, 921600,
];

// ---------------------------------------------------------------------------------------
// Serial settings (shared by printers and scanners)

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum DataBits {
    Five,
    Six,
    Seven,
    #[default]
    Eight,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum StopBits {
    #[default]
    One,
    Two,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum FlowControl {
    #[default]
    None,
    /// Hardware handshake (RTS/CTS): most serial receipt printers.
    RtsCts,
    /// Software handshake (XON/XOFF).
    XonXoff,
}

/// A serial port: RS-232, USB virtual COM, or Bluetooth SPP (`COM5`, `/dev/rfcomm0`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct SerialSettings {
    /// `COM3`, `/dev/ttyUSB0`, `/dev/rfcomm0`…
    pub port: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    #[serde(default)]
    pub data_bits: DataBits,
    #[serde(default)]
    pub parity: Parity,
    #[serde(default)]
    pub stop_bits: StopBits,
    #[serde(default)]
    pub flow: FlowControl,
    /// Drive DTR to this level after opening. `None`: leave the driver default.
    #[serde(default)]
    pub dtr: Option<bool>,
    /// Drive RTS to this level after opening (ignored with RTS/CTS flow control).
    #[serde(default)]
    pub rts: Option<bool>,
}

fn default_baud() -> u32 {
    9600
}

impl SerialSettings {
    pub fn new(port: impl Into<String>, baud: u32) -> Self {
        Self {
            port: port.into(),
            baud,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow: FlowControl::None,
            dtr: None,
            rts: None,
        }
    }

    pub(crate) fn validate_into(&self, prefix: &str, errors: &mut Vec<FieldError>) {
        if let Err(m) = check_port_name(&self.port) {
            errors.push(FieldError::new(format!("{prefix}port"), m));
        }
        if !STANDARD_BAUD_RATES.contains(&self.baud) {
            errors.push(FieldError::new(
                format!("{prefix}baud"),
                format!("{} is not a standard baud rate", self.baud),
            ));
        }
    }
}

// ---------------------------------------------------------------------------------------
// Printer settings

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct NetworkSettings {
    /// IP address or host name.
    pub host: String,
    #[serde(default = "default_tcp_port")]
    pub port: u16,
}

fn default_tcp_port() -> u16 {
    crate::transport::tcp::DEFAULT_PORT
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DeviceFileSettings {
    /// `/dev/usb/lp0`, `/dev/lp0`, `/dev/rfcomm0`, `LPT1`…
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct SpoolerSettings {
    /// The printer name in Windows, or the CUPS queue name.
    pub queue: String,
}

/// How the till reaches the printer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PrinterConnection {
    Network(NetworkSettings),
    Serial(SerialSettings),
    DeviceFile(DeviceFileSettings),
    Spooler(SpoolerSettings),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum CutMode {
    None,
    Partial,
    Full,
}

impl From<CutMode> for CutSupport {
    fn from(c: CutMode) -> Self {
        match c {
            CutMode::None => CutSupport::None,
            CutMode::Partial => CutSupport::Partial,
            CutMode::Full => CutSupport::Full,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum DrawerPinChoice {
    #[default]
    Pin2,
    Pin5,
}

impl From<DrawerPinChoice> for DrawerPin {
    fn from(p: DrawerPinChoice) -> Self {
        match p {
            DrawerPinChoice::Pin2 => DrawerPin::Pin2,
            DrawerPinChoice::Pin5 => DrawerPin::Pin5,
        }
    }
}

/// Timeouts in milliseconds. Defaults keep a whole job under the print queue's 10 s.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase", default)]
pub struct TimeoutSettings {
    pub connect_ms: u32,
    pub write_ms: u32,
    pub status_ms: u32,
    pub post_status_ms: u32,
    pub drain_ms: u32,
    pub spool_confirm_ms: u32,
    pub job_ms: u32,
}

impl Default for TimeoutSettings {
    fn default() -> Self {
        let t = PrinterTimeouts::default();
        let ms = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
        Self {
            connect_ms: ms(t.connect),
            write_ms: ms(t.write),
            status_ms: ms(t.status),
            post_status_ms: ms(t.post_status),
            drain_ms: ms(t.drain),
            spool_confirm_ms: ms(t.spool_confirm),
            job_ms: ms(t.job),
        }
    }
}

impl TimeoutSettings {
    pub fn to_timeouts(self) -> PrinterTimeouts {
        let d = |ms: u32| Duration::from_millis(u64::from(ms));
        PrinterTimeouts {
            connect: d(self.connect_ms),
            write: d(self.write_ms),
            status: d(self.status_ms),
            post_status: d(self.post_status_ms),
            drain: d(self.drain_ms),
            spool_confirm: d(self.spool_confirm_ms),
            job: d(self.job_ms),
        }
    }

    fn validate_into(&self, errors: &mut Vec<FieldError>) {
        for (field, v, min, max) in [
            ("timeouts.connectMs", self.connect_ms, 100, 60_000),
            ("timeouts.writeMs", self.write_ms, 100, 120_000),
            ("timeouts.statusMs", self.status_ms, 50, 10_000),
            ("timeouts.postStatusMs", self.post_status_ms, 50, 60_000),
            ("timeouts.drainMs", self.drain_ms, 0, 30_000),
            ("timeouts.spoolConfirmMs", self.spool_confirm_ms, 0, 120_000),
            ("timeouts.jobMs", self.job_ms, 500, 300_000),
        ] {
            if !(min..=max).contains(&v) {
                errors.push(FieldError::new(
                    field,
                    format!("must be between {min} and {max} ms"),
                ));
            }
        }
    }
}

/// One receipt printer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct PrinterConfig {
    pub connection: PrinterConnection,
    /// A profile id from [`profiles::profiles`], e.g. `TM-T88V` or `generic-escpos-80mm-48`.
    pub profile_id: String,
    /// Characters per line, overriding the profile.
    #[serde(default)]
    pub columns_override: Option<u16>,
    #[serde(default)]
    pub font: FontChoice,
    /// Cut mode, overriding the profile.
    #[serde(default)]
    pub cut: Option<CutMode>,
    #[serde(default)]
    pub drawer_pin: DrawerPinChoice,
    /// Which level of the drawer sensor (kick connector pin 3) means "open".
    #[serde(default)]
    pub drawer_open_level: DrawerOpenLevel,
    /// Native barcodes, overriding the profile.
    #[serde(default)]
    pub barcode: Option<bool>,
    /// Native QR codes, overriding the profile.
    #[serde(default)]
    pub qr: Option<bool>,
    /// Real-time status queries, overriding the profile (only used on two-way links).
    #[serde(default)]
    pub status_query: Option<bool>,
    #[serde(default)]
    pub timeouts: TimeoutSettings,
}

/// One serial barcode scanner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ScannerConfig {
    pub serial: SerialSettings,
    #[serde(default)]
    pub terminator: Terminator,
    /// Removed from the start of each scan when present (e.g. a configured scanner prefix).
    #[serde(default)]
    pub prefix_strip: Option<String>,
    /// A scan without a terminator ends after this much silence.
    #[serde(default = "default_inter_byte_ms")]
    pub inter_byte_timeout_ms: u32,
}

fn default_inter_byte_ms() -> u32 {
    100
}

impl ScannerConfig {
    pub fn new(serial: SerialSettings) -> Self {
        Self {
            serial,
            terminator: Terminator::Auto,
            prefix_strip: None,
            inter_byte_timeout_ms: default_inter_byte_ms(),
        }
    }

    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errors = Vec::new();
        self.serial.validate_into("serial.", &mut errors);
        if !(10..=5_000).contains(&self.inter_byte_timeout_ms) {
            errors.push(FieldError::new(
                "interByteTimeoutMs",
                "must be between 10 and 5000 ms",
            ));
        }
        if let Some(p) = &self.prefix_strip
            && (p.is_empty() || p.len() > 32)
        {
            errors.push(FieldError::new("prefixStrip", "must be 1 to 32 characters"));
        }
        if let Terminator::Byte { value } = self.terminator
            && value == 0x1D
        {
            errors.push(FieldError::new(
                "terminator",
                "GS (0x1D) is FNC1 data, not a terminator",
            ));
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}

/// How the scanner ends each scan.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Terminator {
    /// CR, LF, CR LF or ETX (with an optional leading STX).
    #[default]
    Auto,
    /// One specific byte.
    Byte { value: u8 },
    /// No terminator: a scan ends after the inter-byte timeout.
    TimeoutOnly,
}

/// A settings problem tied to one field (camelCase path such as `connection.host`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl FieldError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl PrinterConfig {
    /// A printer on `connection` with a profile and everything else from the profile.
    pub fn new(connection: PrinterConnection, profile_id: impl Into<String>) -> Self {
        Self {
            connection,
            profile_id: profile_id.into(),
            columns_override: None,
            font: FontChoice::A,
            cut: None,
            drawer_pin: DrawerPinChoice::Pin2,
            drawer_open_level: DrawerOpenLevel::default(),
            barcode: None,
            qr: None,
            status_query: None,
            timeouts: TimeoutSettings::default(),
        }
    }

    /// Every problem, by field, so the settings screen can mark them all at once.
    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errors = Vec::new();
        match &self.connection {
            PrinterConnection::Network(n) => {
                if let Err(m) = check_host(&n.host) {
                    errors.push(FieldError::new("connection.host", m));
                }
                if n.port == 0 {
                    errors.push(FieldError::new("connection.port", "must be between 1 and 65535"));
                }
            }
            PrinterConnection::Serial(s) => s.validate_into("connection.", &mut errors),
            PrinterConnection::DeviceFile(d) => {
                if let Err(m) = check_device_path(&d.path) {
                    errors.push(FieldError::new("connection.path", m));
                }
            }
            PrinterConnection::Spooler(s) => {
                if let Err(m) = spooler::check_queue_name(&s.queue) {
                    errors.push(FieldError::new("connection.queue", m));
                }
            }
        }
        match profiles::profile(&self.profile_id) {
            None => errors.push(FieldError::new("profileId", "unknown printer model")),
            Some(p) if !p.text_supported => errors.push(FieldError::new(
                "profileId",
                format!(
                    "{} {} prints raster images only and cannot print text receipts directly",
                    p.vendor, p.name
                ),
            )),
            Some(_) => {}
        }
        if let Some(c) = self.columns_override
            && !(16..=255).contains(&c)
        {
            errors.push(FieldError::new("columnsOverride", "must be between 16 and 255"));
        }
        self.timeouts.validate_into(&mut errors);
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    /// The transport these settings describe (not opened).
    pub fn transport(&self) -> Arc<dyn Transport> {
        let probe = self
            .capabilities()
            .is_some_and(|c| c.status_query && c.dialect == CommandDialect::EscPos);
        match &self.connection {
            PrinterConnection::Network(n) => Arc::new(TcpTransport::new(n.host.clone(), n.port)),
            PrinterConnection::Serial(s) => Arc::new(SerialTransport::new(s.clone())),
            PrinterConnection::DeviceFile(d) => Arc::new(DeviceFileTransport::new(&d.path, probe)),
            PrinterConnection::Spooler(s) => Arc::new(
                SpoolerTransport::new(s.queue.clone())
                    .with_confirm_timeout(Duration::from_millis(u64::from(self.timeouts.spool_confirm_ms))),
            ),
        }
    }

    /// The profile's capabilities with this printer's overrides applied.
    pub fn capabilities(&self) -> Option<pos_device_ports::PrinterCapabilities> {
        let mut caps = profiles::profile(&self.profile_id)?.capabilities(self.font);
        if let Some(c) = self.columns_override {
            caps.columns = Some(c);
        }
        if let Some(c) = self.cut {
            caps.cut = c.into();
        }
        if let Some(b) = self.barcode {
            caps.barcode = b;
        }
        if let Some(q) = self.qr {
            caps.qr = q;
        }
        if let Some(s) = self.status_query {
            caps.status_query = s;
        }
        // The spooler is one-way whatever the model can do.
        if matches!(self.connection, PrinterConnection::Spooler(_)) {
            caps.status_query = false;
        }
        Some(caps)
    }
}

/// Validates `config` and builds the printer. Invalid settings give `Unsupported` with the
/// field errors joined; use [`PrinterConfig::validate`] to show them per field.
pub fn build_printer(config: &PrinterConfig) -> Result<Arc<HwPrinter>, DeviceError> {
    config.validate().map_err(|errors| {
        DeviceError::Unsupported(format!(
            "invalid printer settings: {}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        ))
    })?;
    let caps = config
        .capabilities()
        .ok_or_else(|| DeviceError::Unsupported(format!("unknown printer model {}", config.profile_id)))?;
    let printer = HwPrinter::new(config.transport(), caps)
        .with_drawer_pin(config.drawer_pin.into())
        .with_drawer_open_level(config.drawer_open_level)
        .with_timeouts(config.timeouts.to_timeouts())
        .with_profile_id(config.profile_id.clone());
    Ok(Arc::new(printer))
}

// ---------------------------------------------------------------------------------------
// Field checks

/// An IP address or an RFC 1123 host name.
pub fn check_host(host: &str) -> Result<(), String> {
    let h = host.trim();
    if h.is_empty() {
        return Err("enter an IP address or host name".into());
    }
    if h != host {
        return Err("remove the spaces".into());
    }
    let literal = h.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(h);
    if literal.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    if h.len() > 253 {
        return Err("host name is too long".into());
    }
    let labels: Vec<&str> = h.strip_suffix('.').unwrap_or(h).split('.').collect();
    // Looks like an IPv4 address but did not parse as one: say so rather than "bad name".
    if labels
        .iter()
        .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("not a valid IP address".into());
    }
    for label in labels {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !ok {
            return Err("not a valid IP address or host name".into());
        }
    }
    Ok(())
}

/// A serial port name: `COM3`, `\\.\COM10`, `/dev/ttyUSB0`, `/dev/rfcomm0`…
pub fn check_port_name(port: &str) -> Result<(), String> {
    if port.trim().is_empty() {
        return Err("choose a port".into());
    }
    if port.len() > 256 || port.chars().any(char::is_control) {
        return Err("not a valid port name".into());
    }
    if cfg!(unix) && !port.starts_with('/') {
        return Err("use the full device path, e.g. /dev/ttyUSB0".into());
    }
    Ok(())
}

/// A device file path: absolute, or a DOS device name (`LPT1`, `COM3`, `PRN`) on Windows.
pub fn check_device_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("enter the device path".into());
    }
    if path.len() > 4096 || path.chars().any(char::is_control) {
        return Err("not a valid path".into());
    }
    if std::path::Path::new(path)
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err("must not contain ..".into());
    }
    if cfg!(windows) && is_dos_device(path) {
        return Ok(());
    }
    if !std::path::Path::new(path).is_absolute() {
        return Err(if cfg!(windows) {
            "use LPT1, COM3 or a full path".into()
        } else {
            "use the full device path, e.g. /dev/usb/lp0".into()
        });
    }
    Ok(())
}

fn is_dos_device(path: &str) -> bool {
    let p = path.trim_start_matches(r"\\.\").to_ascii_uppercase();
    p == "PRN"
        || ["LPT", "COM"].iter().any(|pre| {
            p.strip_prefix(pre)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(host: &str, port: u16) -> PrinterConfig {
        PrinterConfig::new(
            PrinterConnection::Network(NetworkSettings {
                host: host.into(),
                port,
            }),
            "generic-escpos-80mm-48",
        )
    }

    #[test]
    fn hosts() {
        for ok in [
            "192.168.1.50",
            "printer",
            "tm-t88.shop.local",
            "::1",
            "[fe80::1]",
            "a.b.c.",
        ] {
            assert!(check_host(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            " 10.0.0.1",
            "300.1.1.1",
            "1.2.3",
            "-bad.local",
            "bad_name",
            "a..b",
            "tm t88",
        ] {
            assert!(check_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn printer_config_field_errors() {
        assert!(net("10.0.0.9", 9100).validate().is_ok());
        let errs = net("nope nope", 0).validate().unwrap_err();
        let fields: Vec<_> = errs.iter().map(|e| e.field.as_str()).collect();
        assert_eq!(fields, ["connection.host", "connection.port"]);

        let mut c = net("10.0.0.9", 9100);
        c.profile_id = "no-such".into();
        c.columns_override = Some(3);
        let fields: Vec<_> = c.validate().unwrap_err().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, ["profileId", "columnsOverride"]);

        c.profile_id = "star-tsp100".into();
        c.columns_override = None;
        assert_eq!(c.validate().unwrap_err()[0].field, "profileId");
        assert!(build_printer(&c).is_err());
    }

    #[test]
    fn serial_and_paths() {
        let mut s = SerialSettings::new(if cfg!(unix) { "/dev/ttyUSB0" } else { "COM3" }, 9600);
        let c = PrinterConfig::new(PrinterConnection::Serial(s.clone()), "generic-escpos-80mm-48");
        assert!(c.validate().is_ok());
        s.baud = 9601;
        let c = PrinterConfig::new(PrinterConnection::Serial(s), "generic-escpos-80mm-48");
        assert_eq!(c.validate().unwrap_err()[0].field, "connection.baud");

        assert!(check_device_path("").is_err());
        assert!(check_device_path("/dev/usb/../sda").is_err());
        assert!(check_device_path("/dev/\u{7}").is_err());
        if cfg!(unix) {
            assert!(check_device_path("/dev/usb/lp0").is_ok());
            assert!(check_device_path("dev/usb/lp0").is_err());
        }
        assert!(is_dos_device("LPT1") && is_dos_device(r"\\.\COM12") && !is_dos_device("LPTX"));
    }

    #[test]
    fn json_round_trip_is_camel_case() {
        let mut c = PrinterConfig::new(
            PrinterConnection::Serial(SerialSettings::new("/dev/ttyS0", 19200)),
            "TM-T88V",
        );
        c.cut = Some(CutMode::Full);
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["connection"]["kind"], "serial");
        assert_eq!(json["connection"]["dataBits"], "eight");
        assert_eq!(json["connection"]["stopBits"], "one");
        assert_eq!(json["profileId"], "TM-T88V");
        assert_eq!(json["timeouts"]["connectMs"], 3000);
        let back: PrinterConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, c);

        // Minimal JSON from the UI: defaults fill the rest.
        let c: PrinterConfig = serde_json::from_str(
            r#"{"connection":{"kind":"network","host":"10.0.0.2"},"profileId":"generic-escpos-80mm-42"}"#,
        )
        .unwrap();
        assert_eq!(
            c.connection,
            PrinterConnection::Network(NetworkSettings {
                host: "10.0.0.2".into(),
                port: 9100
            })
        );
        assert_eq!(c.timeouts, TimeoutSettings::default());
        assert!(c.validate().is_ok());

        let s: ScannerConfig = serde_json::from_str(
            r#"{"serial":{"port":"/dev/ttyACM0"},"terminator":{"kind":"byte","value":9},"prefixStrip":"]C1"}"#,
        )
        .unwrap();
        assert_eq!(s.serial.baud, 9600);
        assert_eq!(s.terminator, Terminator::Byte { value: 9 });
        assert_eq!(s.inter_byte_timeout_ms, 100);
    }

    #[test]
    fn overrides_apply_to_capabilities() {
        let mut c = net("10.0.0.9", 9100);
        c.columns_override = Some(44);
        c.cut = Some(CutMode::None);
        c.qr = Some(false);
        let caps = c.capabilities().unwrap();
        assert_eq!(caps.columns, Some(44));
        assert_eq!(caps.cut, CutSupport::None);
        assert!(!caps.qr);
        assert!(caps.status_query);

        let s = PrinterConfig::new(
            PrinterConnection::Spooler(SpoolerSettings {
                queue: "EPSON_TM".into(),
            }),
            "TM-T88V",
        );
        assert!(!s.capabilities().unwrap().status_query);
        let p = build_printer(&s).unwrap();
        use pos_device_ports::ReceiptOutputPort;
        assert_eq!(p.adapter(), "os_spooler");
    }
}
