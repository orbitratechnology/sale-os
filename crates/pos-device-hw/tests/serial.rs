//! Serial printer and scanner over a pseudo-terminal pair (a virtual null-modem cable).
#![cfg(unix)]

mod support;

use pos_device_hw::config::SerialSettings;
use pos_device_hw::scanner::Backoff;
use pos_device_hw::transport::{SerialTransport, Transport, TransportError};
use pos_device_hw::{HwPrinter, PrinterTimeouts, ScannerConfig, SerialScanner, Terminator};
use pos_device_ports::{
    BarcodeInputPort, CashDrawerPort, DeviceHealth, DrawerPin, PrintFailure, PrintResult,
    PrinterCapabilities, ReceiptOutputPort, ScanEvent, ScanSource,
};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use support::{Script, SerialEmulator, Status};

fn timeouts() -> PrinterTimeouts {
    PrinterTimeouts {
        connect: Duration::from_secs(1),
        write: Duration::from_secs(2),
        status: Duration::from_millis(300),
        post_status: Duration::from_millis(800),
        drain: Duration::from_millis(100),
        spool_confirm: Duration::from_millis(100),
        job: Duration::from_secs(5),
    }
}

fn serial_printer(path: &str) -> HwPrinter {
    let caps = PrinterCapabilities {
        status_query: true,
        drawer_kick: true,
        ..PrinterCapabilities::default()
    };
    HwPrinter::new(
        Arc::new(SerialTransport::new(SerialSettings::new(path, 19200))),
        caps,
    )
    .with_timeouts(timeouts())
}

#[tokio::test]
async fn serial_printer_prints_with_status() {
    let emu = SerialEmulator::start(Script::default());
    let p = serial_printer(&emu.slave_path);
    assert_eq!(p.adapter(), "escpos_serial");
    assert_eq!(p.target(), format!("{} @ 19200", emu.slave_path));
    assert_eq!(p.submit("R1", b"SERIAL RECEIPT\n").await, PrintResult::Printed);
    assert_eq!(emu.wait_job(15), b"SERIAL RECEIPT\n");
    assert_eq!(emu.proto.lock().unwrap().queries, [1, 2, 4, 1, 2, 4]);
}

#[tokio::test]
async fn serial_printer_paper_out_sends_nothing() {
    let emu = SerialEmulator::start(Script {
        status: Status::PAPER_OUT,
        ..Script::default()
    });
    let p = serial_printer(&emu.slave_path);
    assert_eq!(
        p.submit("R1", b"X\n").await,
        PrintResult::Failed(PrintFailure::PaperOut)
    );
    assert!(emu.job().is_empty());
}

#[tokio::test]
async fn serial_drawer_kick() {
    let emu = SerialEmulator::start(Script::default());
    serial_printer(&emu.slave_path)
        .open(DrawerPin::Pin5)
        .await
        .unwrap();
    assert_eq!(emu.wait_job(5), [0x1B, 0x70, 1, 25, 250]);
}

#[tokio::test]
async fn missing_and_busy_ports_are_told_apart() {
    let missing = SerialTransport::new(SerialSettings::new("/dev/pos-device-hw-no-such-tty", 9600));
    assert!(matches!(
        missing.open(Duration::from_secs(1)).await.err(),
        Some(TransportError::Unreachable(_))
    ));
    let p = serial_printer("/dev/pos-device-hw-no-such-tty");
    assert_eq!(
        p.submit("R1", b"X").await,
        PrintResult::Failed(PrintFailure::Disconnected)
    );

    let emu = SerialEmulator::start(Script::default());
    let t = SerialTransport::new(SerialSettings::new(&emu.slave_path, 9600));
    let held = t.open(Duration::from_secs(1)).await.unwrap();
    assert!(matches!(
        t.open(Duration::from_secs(1)).await.err(),
        Some(TransportError::Busy(_))
    ));
    assert_eq!(
        serial_printer(&emu.slave_path).submit("R1", b"X").await,
        PrintResult::Failed(PrintFailure::Disconnected)
    );
    drop(held);
}

// ---------------------------------------------------------------------------------------
// Scanner

/// A scanner "cable": a pty pair whose slave is reached through a symlink we control, so
/// the test can unplug it (close the master) and plug a new one in at the same path.
struct Cable {
    master: serialport::TTYPort,
}

fn plug(link: &std::path::Path) -> Cable {
    let (master, slave) = serialport::TTYPort::pair().unwrap();
    let path = serialport::SerialPort::name(&slave).unwrap();
    drop(slave);
    let _ = std::fs::remove_file(link);
    std::os::unix::fs::symlink(path, link).unwrap();
    Cable { master }
}

impl Cable {
    fn scan(&mut self, bytes: &[u8]) {
        // The master reports a hang-up until the reader has the slave open.
        for _ in 0..100 {
            if self.master.write_all(bytes).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the scanner never opened the port");
    }
}

async fn wait_health(rx: &mut tokio::sync::watch::Receiver<DeviceHealth>, want: fn(&DeviceHealth) -> bool) {
    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(want))
        .await
        .expect("health change in time")
        .expect("scanner alive");
}

async fn next(rx: &mut tokio::sync::broadcast::Receiver<ScanEvent>) -> ScanEvent {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("scan in time")
        .expect("channel open")
}

fn scanner(link: &std::path::Path, f: impl FnOnce(&mut ScannerConfig)) -> SerialScanner {
    let mut cfg = ScannerConfig::new(SerialSettings::new(link.to_str().unwrap(), 9600));
    f(&mut cfg);
    SerialScanner::new(cfg).with_backoff(Backoff {
        initial: Duration::from_millis(20),
        max: Duration::from_millis(200),
    })
}

#[tokio::test]
async fn scanner_splits_keeps_gs_and_strips_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("scanner");
    let mut cable = plug(&link);
    let s = scanner(&link, |c| c.prefix_strip = Some("]C1".into()));
    let mut scans = s.subscribe();
    let mut health = s.health();
    s.start().await.unwrap();
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;

    cable.scan(b"4006381333931\r5901234123457\n96385074\r\n]C1010950123456789\x1d10LOT7\x03");
    let got: Vec<ScanEvent> = vec![
        next(&mut scans).await,
        next(&mut scans).await,
        next(&mut scans).await,
        next(&mut scans).await,
    ];
    let raws: Vec<&str> = got.iter().map(|e| e.raw.as_str()).collect();
    assert_eq!(
        raws,
        [
            "4006381333931",
            "5901234123457",
            "96385074",
            "010950123456789\u{1d}10LOT7"
        ]
    );
    assert!(
        got.iter()
            .all(|e| e.source == ScanSource::Serial(link.to_str().unwrap().into()))
    );
    let ids: std::collections::HashSet<_> = got.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 4, "every scan gets a fresh id");
    s.stop().await.unwrap();
}

#[tokio::test]
async fn scanner_without_terminator_uses_silence() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("scanner");
    let mut cable = plug(&link);
    let s = scanner(&link, |c| {
        c.terminator = Terminator::TimeoutOnly;
        c.inter_byte_timeout_ms = 60;
    });
    let mut scans = s.subscribe();
    let mut health = s.health();
    s.start().await.unwrap();
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;
    cable.scan(b"123456");
    assert_eq!(next(&mut scans).await.raw, "123456");
    tokio::time::sleep(Duration::from_millis(150)).await;
    cable.scan(b"ABC");
    assert_eq!(next(&mut scans).await.raw, "ABC");
    s.stop().await.unwrap();
}

#[tokio::test]
async fn scanner_reconnects_after_unplug() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("scanner");
    let mut cable = plug(&link);
    let s = scanner(&link, |_| {});
    let mut scans = s.subscribe();
    let mut health = s.health();
    s.start().await.unwrap();
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;
    cable.scan(b"111\r");
    assert_eq!(next(&mut scans).await.raw, "111");

    // Unplug: the device node disappears with the port. (Removing the link first also keeps
    // the retrying scanner off a pty number that a parallel test may reuse.)
    std::fs::remove_file(&link).unwrap();
    drop(cable);
    wait_health(&mut health, |h| matches!(h, DeviceHealth::Offline(_))).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Plug back in at the same path.
    let mut cable = plug(&link);
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;
    cable.scan(b"222\r");
    assert_eq!(next(&mut scans).await.raw, "222");

    s.stop().await.unwrap();
    assert_eq!(*s.health().borrow(), DeviceHealth::Unknown);
}

#[tokio::test]
async fn scanner_starts_offline_when_the_port_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("not-yet");
    let s = scanner(&link, |_| {});
    let mut health = s.health();
    s.start().await.unwrap();
    wait_health(
        &mut health,
        |h| matches!(h, DeviceHealth::Offline(m) if m.contains("not found")),
    )
    .await;
    // Plugged in later: it connects by itself.
    let mut cable = plug(&link);
    let mut scans = s.subscribe();
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;
    cable.scan(b"333\n");
    assert_eq!(next(&mut scans).await.raw, "333");
    s.stop().await.unwrap();

    let bad = SerialScanner::new(ScannerConfig::new(SerialSettings::new("/dev/ttyS0", 1234)));
    assert!(bad.start().await.is_err());
}
