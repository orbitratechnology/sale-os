//! Device-file printing (into a temp file), the spooler without a real spooler, and
//! network discovery against the in-test emulator.

mod support;

use pos_device_hw::config::{DeviceFileSettings, PrinterConnection};
use pos_device_hw::discovery::{self, DiscoveryError};
use pos_device_hw::transport::DeviceFileTransport;
use pos_device_hw::{HwPrinter, PrinterConfig, build_printer};
use pos_device_ports::{
    CashDrawerPort, DrawerPin, PrintFailure, PrintResult, PrinterCapabilities, ReceiptOutputPort,
};
use std::sync::Arc;
use std::time::Duration;
use support::{Script, TcpEmulator};

#[tokio::test]
async fn device_file_receives_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lp0");
    std::fs::write(&path, b"").unwrap();
    let p = HwPrinter::new(
        Arc::new(DeviceFileTransport::new(&path, false)),
        PrinterCapabilities::default(),
    );
    assert_eq!(p.adapter(), "escpos_file");
    assert_eq!(p.target(), path.display().to_string());
    assert_eq!(p.submit("R1", b"FILE RECEIPT\n").await, PrintResult::Printed);
    assert_eq!(std::fs::read(&path).unwrap(), b"FILE RECEIPT\n");
}

#[tokio::test]
async fn device_file_probe_finds_a_one_way_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lp0");
    std::fs::write(&path, b"").unwrap();
    let config = PrinterConfig::new(
        PrinterConnection::DeviceFile(DeviceFileSettings {
            path: path.to_str().unwrap().into(),
        }),
        "generic-escpos-80mm-48",
    );
    let p = build_printer(&config).unwrap();
    assert!(!p.transport().bidirectional(), "unknown until probed");
    assert_eq!(p.submit("R1", b"JOB").await, PrintResult::Printed);
    // The probe asked DLE EOT 1, got end-of-file, and settled on one-way.
    assert!(!p.transport().bidirectional());
    assert_eq!(std::fs::read(&path).unwrap(), b"\x10\x04\x01JOB");
    p.open(DrawerPin::Pin2).await.unwrap();
    assert!(std::fs::read(&path).unwrap().ends_with(&[0x1B, 0x70, 0, 25, 250]));
    assert_eq!(p.is_open().await.unwrap(), None);
}

#[tokio::test]
async fn missing_device_file_is_disconnected() {
    let p = HwPrinter::new(
        Arc::new(DeviceFileTransport::new("/nonexistent/pos-device-hw/lp9", true)),
        PrinterCapabilities::default(),
    );
    assert_eq!(
        p.submit("R1", b"X").await,
        PrintResult::Failed(PrintFailure::Disconnected)
    );
}

#[tokio::test]
async fn probe_and_scan_find_the_emulated_printer() {
    let emu = TcpEmulator::start(Script {
        maker: Some("EPSON".into()),
        model: Some("TM-T88V".into()),
        ..Script::default()
    })
    .await;
    assert!(
        discovery::probe("127.0.0.1", emu.port(), Duration::from_secs(1))
            .await
            .is_ok()
    );

    // Bound but not listening: the port stays reserved for this test (a released port can
    // be handed to a listener in another test meanwhile), and connecting is refused.
    let reserved = tokio::net::TcpSocket::new_v4().unwrap();
    reserved.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let closed = reserved.local_addr().unwrap().port();
    assert!(
        discovery::probe("127.0.0.1", closed, Duration::from_millis(500))
            .await
            .is_err()
    );

    let found = discovery::scan_subnet("127.0.0.1/32", emu.port(), 8, Duration::from_millis(500), true)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].host, "127.0.0.1");
    assert_eq!(found[0].profile_id.as_deref(), Some("TM-T88V"));
    assert_eq!(
        found[0].identified.as_ref().unwrap().maker.as_deref(),
        Some("EPSON")
    );

    let quiet = discovery::scan_subnet("127.0.0.0/30", emu.port(), 2, Duration::from_millis(300), false)
        .await
        .unwrap();
    assert_eq!(quiet.len(), 1);
    assert_eq!(quiet[0].identified, None);

    assert_eq!(
        discovery::scan_subnet("10.0.0.0/16", 9100, 8, Duration::from_millis(10), false).await,
        Err(DiscoveryError::TooLarge)
    );
    assert_eq!(
        discovery::scan_subnet("not a net", 9100, 8, Duration::from_millis(10), false).await,
        Err(DiscoveryError::BadSubnet)
    );
}

#[tokio::test]
async fn local_discovery_is_bounded() {
    // These read the real machine: they must return (possibly empty) within their timeout.
    let t = Duration::from_secs(3);
    let _ = tokio::time::timeout(Duration::from_secs(5), discovery::serial_ports(t))
        .await
        .unwrap();
    let files = tokio::time::timeout(Duration::from_secs(5), discovery::device_files(t))
        .await
        .unwrap();
    assert!(files.iter().all(|f| f.starts_with("/dev/")));
    let _ = tokio::time::timeout(Duration::from_secs(5), discovery::spooler_queues(t))
        .await
        .unwrap();
}
