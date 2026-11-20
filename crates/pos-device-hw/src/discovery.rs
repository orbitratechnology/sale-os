//! Finding devices: serial ports (with USB vendor hints), device files, spooler queues and
//! raw-TCP printers on the local network. Everything is bounded by a timeout, and network
//! scans are limited to a /24 or smaller with bounded concurrency.

use crate::printer::{IdentifiedPrinter, identify_on};
use crate::profiles;
use crate::transport::{TcpTransport, Transport, TransportError, spooler};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

/// What a device on a port probably is, from its USB vendor.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum LikelyDevice {
    Printer,
    Scanner,
    /// The vendor makes both, or the id belongs to a chip used in both.
    Either,
    /// A generic USB-serial adapter: anything could be behind it.
    Unknown,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KnownVendor {
    pub vid: u16,
    pub name: &'static str,
    pub likely: LikelyDevice,
    /// `false`: the id is shared or our attribution is a best guess.
    pub sure: bool,
}

const fn v(vid: u16, name: &'static str, likely: LikelyDevice, sure: bool) -> KnownVendor {
    KnownVendor {
        vid,
        name,
        likely,
        sure,
    }
}

/// USB vendor ids of point-of-sale makers and of USB-serial chips common in POS devices.
/// Sunmi devices enumerate with their chip makers' ids, so they have no entry of their own.
pub const KNOWN_VENDORS: &[KnownVendor] = &[
    v(0x04B8, "Epson", LikelyDevice::Printer, true),
    v(0x0519, "Star Micronics", LikelyDevice::Printer, true),
    v(0x1504, "Bixolon", LikelyDevice::Printer, true),
    v(0x1D90, "Citizen", LikelyDevice::Printer, true),
    v(0x2730, "Citizen", LikelyDevice::Printer, false),
    v(0x0A5F, "Zebra (printers)", LikelyDevice::Printer, true),
    v(0x0DD4, "Custom", LikelyDevice::Printer, false),
    v(0x154F, "SNBC", LikelyDevice::Printer, false),
    v(0x20D1, "HPRT", LikelyDevice::Printer, false),
    v(
        0x0416,
        "Winbond (Xprinter, Zjiang and other clones)",
        LikelyDevice::Printer,
        false,
    ),
    v(
        0x0483,
        "STMicroelectronics (Xprinter, Rongta and other clones)",
        LikelyDevice::Either,
        false,
    ),
    v(
        0x0FE6,
        "ICS Advent (generic Chinese POS printers)",
        LikelyDevice::Printer,
        false,
    ),
    v(0x0C2E, "Honeywell (Metrologic)", LikelyDevice::Scanner, true),
    v(
        0x0536,
        "Honeywell (Hand Held Products)",
        LikelyDevice::Scanner,
        true,
    ),
    v(0x05F9, "Datalogic", LikelyDevice::Scanner, true),
    v(0x1EAB, "Newland", LikelyDevice::Scanner, true),
    v(0x05E0, "Zebra (Symbol scanners)", LikelyDevice::Scanner, true),
    v(0x065A, "Opticon", LikelyDevice::Scanner, false),
    v(0x0403, "FTDI USB-serial adapter", LikelyDevice::Unknown, true),
    v(0x067B, "Prolific USB-serial adapter", LikelyDevice::Unknown, true),
    v(
        0x10C4,
        "Silicon Labs USB-serial adapter",
        LikelyDevice::Unknown,
        true,
    ),
    v(
        0x1A86,
        "QinHeng CH340 USB-serial adapter",
        LikelyDevice::Unknown,
        true,
    ),
];

pub fn known_vendor(vid: u16) -> Option<&'static KnownVendor> {
    KNOWN_VENDORS.iter().find(|k| k.vid == vid)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SerialPortKind {
    Usb {
        vid: u16,
        pid: u16,
        manufacturer: Option<String>,
        product: Option<String>,
        serial_number: Option<String>,
    },
    Bluetooth,
    Pci,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct SerialCandidate {
    pub port: String,
    pub kind: SerialPortKind,
    pub vendor: Option<String>,
    pub likely: LikelyDevice,
    /// Whether `likely` comes from a vendor id we are sure about.
    pub likely_sure: bool,
    /// For the settings screen, e.g. "COM5: Epson TM-T20III (likely printer)".
    pub label: String,
}

/// Describes one port from `serialport`'s enumeration.
pub fn classify(port: &str, port_type: &serialport::SerialPortType) -> SerialCandidate {
    let (kind, vendor, likely, sure) = match port_type {
        serialport::SerialPortType::UsbPort(u) => {
            let known = known_vendor(u.vid);
            (
                SerialPortKind::Usb {
                    vid: u.vid,
                    pid: u.pid,
                    manufacturer: u.manufacturer.clone(),
                    product: u.product.clone(),
                    serial_number: u.serial_number.clone(),
                },
                known
                    .map(|k| k.name.to_owned())
                    .or_else(|| u.manufacturer.clone()),
                known.map_or(LikelyDevice::Unknown, |k| k.likely),
                known.is_some_and(|k| k.sure),
            )
        }
        serialport::SerialPortType::BluetoothPort => {
            (SerialPortKind::Bluetooth, None, LikelyDevice::Either, false)
        }
        serialport::SerialPortType::PciPort => (SerialPortKind::Pci, None, LikelyDevice::Unknown, false),
        serialport::SerialPortType::Unknown if port.contains("rfcomm") => {
            (SerialPortKind::Bluetooth, None, LikelyDevice::Either, false)
        }
        serialport::SerialPortType::Unknown => (SerialPortKind::Other, None, LikelyDevice::Unknown, false),
    };
    let product = match &kind {
        SerialPortKind::Usb { product, .. } => product.clone(),
        SerialPortKind::Bluetooth => Some("Bluetooth serial".into()),
        _ => None,
    };
    let what = [vendor.clone(), product]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let hint = match (likely, sure) {
        (LikelyDevice::Printer, true) => " (likely printer)",
        (LikelyDevice::Printer, false) => " (possibly a printer)",
        (LikelyDevice::Scanner, true) => " (likely scanner)",
        (LikelyDevice::Scanner, false) => " (possibly a scanner)",
        _ => "",
    };
    let label = if what.is_empty() {
        format!("{port}{hint}")
    } else {
        format!("{port}: {what}{hint}")
    };
    SerialCandidate {
        port: port.to_owned(),
        kind,
        vendor,
        likely,
        likely_sure: sure,
        label,
    }
}

/// Serial ports (RS-232, USB COM, Bluetooth SPP), within `timeout`.
pub async fn serial_ports(timeout: Duration) -> Vec<SerialCandidate> {
    let task = tokio::task::spawn_blocking(|| serialport::available_ports().unwrap_or_default());
    match tokio::time::timeout(timeout, task).await {
        Ok(Ok(ports)) => ports
            .iter()
            .map(|p| classify(&p.port_name, &p.port_type))
            .collect(),
        _ => Vec::new(),
    }
}

/// Printer device files: Linux `/dev/usb/lp*`, `/dev/lp*`, `/dev/rfcomm*`. Empty elsewhere.
pub async fn device_files(timeout: Duration) -> Vec<String> {
    let task = tokio::task::spawn_blocking(|| {
        let mut out = Vec::new();
        if cfg!(target_os = "linux") {
            for (dir, prefix) in [("/dev/usb", "lp"), ("/dev", "lp"), ("/dev", "rfcomm")] {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let digits = name.strip_prefix(prefix);
                    if digits.is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())) {
                        out.push(format!("{dir}/{name}"));
                    }
                }
            }
        }
        out.sort();
        out
    });
    tokio::time::timeout(timeout, task)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

/// Spooler queues (Windows printers, CUPS queues), within `timeout`.
pub async fn spooler_queues(timeout: Duration) -> Vec<String> {
    spooler::list_queues(timeout).await
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    #[error("not a subnet: use a form like 192.168.1.0/24")]
    BadSubnet,
    #[error("only /24 or smaller networks can be scanned")]
    TooLarge,
    #[error("port must be between 1 and 65535")]
    BadPort,
}

/// An IPv4 network of at most 256 addresses.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Subnet {
    pub network: Ipv4Addr,
    pub prefix: u8,
}

impl Subnet {
    /// Parses `a.b.c.d/nn` (nn from 24 to 32). Host bits in the address are ignored.
    pub fn parse(s: &str) -> Result<Self, DiscoveryError> {
        let (addr, prefix) = s.trim().split_once('/').ok_or(DiscoveryError::BadSubnet)?;
        let addr: Ipv4Addr = addr.parse().map_err(|_| DiscoveryError::BadSubnet)?;
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_digit()) {
            return Err(DiscoveryError::BadSubnet);
        }
        let prefix: u8 = prefix.parse().map_err(|_| DiscoveryError::BadSubnet)?;
        if prefix > 32 {
            return Err(DiscoveryError::BadSubnet);
        }
        if prefix < 24 {
            return Err(DiscoveryError::TooLarge);
        }
        let mask = u32::MAX << (32 - u32::from(prefix));
        Ok(Self {
            network: Ipv4Addr::from(u32::from(addr) & mask),
            prefix,
        })
    }

    /// Host addresses: without the network and broadcast addresses, except in /31 and /32.
    pub fn hosts(&self) -> Vec<Ipv4Addr> {
        let base = u32::from(self.network);
        let size = 1u32 << (32 - u32::from(self.prefix));
        let range = if size <= 2 { 0..size } else { 1..size - 1 };
        range.map(|i| Ipv4Addr::from(base + i)).collect()
    }
}

/// Whether something accepts TCP connections at `host:port`; the connect time if so.
pub async fn probe(host: &str, port: u16, timeout: Duration) -> Result<Duration, TransportError> {
    let started = Instant::now();
    let link = TcpTransport::new(host, port).open(timeout).await?;
    let elapsed = started.elapsed();
    drop(link);
    Ok(elapsed)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct NetworkResponder {
    pub host: String,
    pub port: u16,
    pub identified: Option<IdentifiedPrinter>,
    /// The matching profile, when the printer identified itself.
    pub profile_id: Option<String>,
}

/// Connects to every host of `subnet` on `port` (at most `concurrency` at a time, each
/// within `per_host_timeout`) and lists those that answer, sorted by address.
///
/// With `identify`, each responder is asked `GS I` for its maker and model. Port 9100 is
/// also the raw port of office laser printers, which may print a few stray characters on a
/// page when asked; let the operator choose.
pub async fn scan_subnet(
    subnet: &str,
    port: u16,
    concurrency: usize,
    per_host_timeout: Duration,
    identify: bool,
) -> Result<Vec<NetworkResponder>, DiscoveryError> {
    let subnet = Subnet::parse(subnet)?;
    if port == 0 {
        return Err(DiscoveryError::BadPort);
    }
    let limit = Arc::new(tokio::sync::Semaphore::new(concurrency.clamp(1, 128)));
    let mut tasks = tokio::task::JoinSet::new();
    for ip in subnet.hosts() {
        let limit = Arc::clone(&limit);
        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await.ok()?;
            let host = ip.to_string();
            let transport = TcpTransport::new(host.clone(), port);
            let mut link = transport.open(per_host_timeout).await.ok()?;
            let identified = if identify {
                identify_on(link.as_mut(), per_host_timeout).await
            } else {
                None
            };
            let _ = link.finish(Duration::from_millis(200)).await;
            let profile_id = identified
                .as_ref()
                .and_then(profiles::match_identified)
                .map(|p| p.id.clone());
            Some((
                ip,
                NetworkResponder {
                    host,
                    port,
                    identified,
                    profile_id,
                },
            ))
        });
    }
    let mut found = Vec::new();
    while let Some(r) = tasks.join_next().await {
        if let Ok(Some(hit)) = r {
            found.push(hit);
        }
    }
    found.sort_by_key(|(ip, _)| IpAddr::V4(*ip));
    Ok(found.into_iter().map(|(_, r)| r).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnets() {
        let s = Subnet::parse("192.168.1.77/24").unwrap();
        assert_eq!(s.network, Ipv4Addr::new(192, 168, 1, 0));
        let hosts = s.hosts();
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(hosts[253], Ipv4Addr::new(192, 168, 1, 254));
        assert_eq!(Subnet::parse("10.0.0.8/30").unwrap().hosts().len(), 2);
        assert_eq!(
            Subnet::parse("10.0.0.8/32").unwrap().hosts(),
            [Ipv4Addr::new(10, 0, 0, 8)]
        );
        assert_eq!(Subnet::parse("10.0.0.0/16"), Err(DiscoveryError::TooLarge));
        assert_eq!(Subnet::parse("10.0.0.0/23"), Err(DiscoveryError::TooLarge));
        for bad in [
            "",
            "10.0.0.0",
            "10.0.0/24",
            "10.0.0.0/33",
            "10.0.0.0/+24",
            "10.0.0.0/",
            "fe80::/120",
            "x/24",
        ] {
            assert_eq!(Subnet::parse(bad), Err(DiscoveryError::BadSubnet), "{bad}");
        }
    }

    #[test]
    fn port_labels() {
        use serialport::{SerialPortType, UsbPortInfo};
        let usb = |vid| {
            SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid: 0x0E28,
                serial_number: None,
                manufacturer: Some("EPSON".into()),
                product: Some("TM-T20III".into()),
            })
        };
        let c = classify("COM5", &usb(0x04B8));
        assert_eq!(c.likely, LikelyDevice::Printer);
        assert!(c.likely_sure);
        assert_eq!(c.label, "COM5: Epson TM-T20III (likely printer)");
        let c = classify("/dev/ttyACM0", &usb(0x05F9));
        assert_eq!(c.likely, LikelyDevice::Scanner);
        let c = classify("/dev/ttyUSB0", &usb(0x0416));
        assert_eq!((c.likely, c.likely_sure), (LikelyDevice::Printer, false));
        assert!(c.label.ends_with("(possibly a printer)"));
        let c = classify("/dev/ttyUSB1", &usb(0x1234));
        assert_eq!(c.likely, LikelyDevice::Unknown);
        assert_eq!(c.vendor.as_deref(), Some("EPSON"));
        let c = classify("/dev/rfcomm0", &SerialPortType::Unknown);
        assert_eq!(c.kind, SerialPortKind::Bluetooth);
        let vids: std::collections::HashSet<_> = KNOWN_VENDORS.iter().map(|k| k.vid).collect();
        assert_eq!(vids.len(), KNOWN_VENDORS.len());
    }
}
