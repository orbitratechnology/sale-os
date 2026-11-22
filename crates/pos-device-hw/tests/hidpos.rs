//! USB HID-POS scanners against an emulated device that emits report bytes: single and
//! multi-report scans, unknown report IDs, a disconnect mid-scan and a reconnect under a
//! new path.

use pos_device_hw::hidpos::{EmulatedHid, default_reports, example_descriptor};
use pos_device_hw::scanner::Backoff;
use pos_device_hw::{HidPosConfig, HidPosDevice, HidPosScanner};
use pos_device_ports::{BarcodeInputPort, DeviceHealth, ScanEvent, ScanSource};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};

fn honeywell(path: &str) -> HidPosDevice {
    HidPosDevice {
        path: path.into(),
        vendor_id: 0x0c2e,
        product_id: 0x0b61,
        serial_number: Some("19200B1234".into()),
        manufacturer: Some("Honeywell".into()),
        product: Some("Xenon 1900".into()),
    }
}

fn scanner(emu: &EmulatedHid, device: &HidPosDevice) -> HidPosScanner {
    HidPosScanner::new(HidPosConfig::for_device(device), Arc::new(emu.clone())).with_backoff(Backoff {
        initial: Duration::from_millis(20),
        max: Duration::from_millis(80),
    })
}

async fn next_scan(rx: &mut broadcast::Receiver<ScanEvent>) -> ScanEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a scan within 5 s")
        .expect("scan channel open")
}

async fn wait_health(
    rx: &mut watch::Receiver<DeviceHealth>,
    want: impl Fn(&DeviceHealth) -> bool,
) -> DeviceHealth {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let h = rx.borrow_and_update().clone();
            if want(&h) {
                return h;
            }
            rx.changed().await.expect("health channel open");
        }
    })
    .await
    .expect("health within 5 s")
}

#[tokio::test]
async fn single_and_multi_report_scans_reach_the_pipeline_with_fresh_ids() {
    let device = honeywell("/dev/hidraw3");
    let emu = EmulatedHid::new(device.clone());
    let s = scanner(&emu, &device);
    let mut scans = s.subscribe();
    let mut health = s.health();
    s.start().await.unwrap();
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;

    // One EAN-13 in one report.
    emu.send(default_reports(b"]E0", b"4006381333931"));
    let first = next_scan(&mut scans).await;
    assert_eq!(first.raw, "]E04006381333931");
    assert_eq!(
        first.source,
        ScanSource::HidPos("Honeywell Xenon 1900 (0c2e:0b61)".into())
    );

    // A 200-byte GS1 DataMatrix in four reports, with a status report (ID 4) in between.
    let mut long = b"0109501234567896\x1d10LOT42\x1d".to_vec();
    long.extend(std::iter::repeat_n(b'Z', 200 - long.len()));
    let mut reports = default_reports(b"]d2", &long);
    assert_eq!(reports.len(), 4);
    reports.insert(2, vec![0x04, 0x01, 0x00, 0x00]);
    emu.send(reports);
    let second = next_scan(&mut scans).await;
    assert_eq!(second.raw.as_bytes()[..3], *b"]d2");
    assert_eq!(second.raw.as_bytes()[3..], long[..]);

    // The same code twice is two scans with two ids (BAR-007: never de-duplicated by value).
    emu.send(default_reports(b"]E0", b"4006381333931"));
    emu.send(default_reports(b"]E0", b"4006381333931"));
    let a = next_scan(&mut scans).await;
    let b = next_scan(&mut scans).await;
    assert_eq!(
        (a.raw.as_str(), b.raw.as_str()),
        ("]E04006381333931", "]E04006381333931")
    );
    let ids = [first.id, second.id, a.id, b.id];
    for (i, x) in ids.iter().enumerate() {
        assert!(!ids[i + 1..].contains(x), "scan ids are unique");
    }
    s.stop().await.unwrap();
}

#[tokio::test]
async fn unknown_reports_are_ignored() {
    let device = honeywell("/dev/hidraw3");
    let emu = EmulatedHid::new(device.clone());
    let s = scanner(&emu, &device);
    let mut scans = s.subscribe();
    s.start().await.unwrap();
    // Vendor, trigger and malformed reports; then a real scan.
    emu.send([
        vec![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        vec![0xFD; 64],
        vec![0x02, 0x05],
        vec![],
    ]);
    emu.send(default_reports(b"]A0", b"CODE39-OK"));
    let scan = next_scan(&mut scans).await;
    assert_eq!(scan.raw, "]A0CODE39-OK");
    assert!(emu.drained());
    assert!(scans.try_recv().is_err(), "nothing else was emitted");
    s.stop().await.unwrap();
}

#[tokio::test]
async fn a_disconnect_drops_the_half_scan_and_the_scanner_comes_back_under_a_new_path() {
    let device = honeywell("/dev/hidraw3");
    let emu = EmulatedHid::new(device.clone());
    let s = scanner(&emu, &device);
    let mut scans = s.subscribe();
    let mut health = s.health();
    s.start().await.unwrap();
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;

    // The first of two reports, then the cable is pulled.
    let long = default_reports(b"]Q1", &[b'Q'; 100]);
    emu.send([long[0].clone()]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(emu.drained());
    emu.unplug();
    let h = wait_health(&mut health, |h| matches!(h, DeviceHealth::Offline(_))).await;
    let DeviceHealth::Offline(why) = h else {
        unreachable!()
    };
    assert!(why.contains("Honeywell Xenon 1900"), "{why}");

    // Plugged into another port: a new hidraw node, found again by its serial number.
    emu.plug(Some("/dev/hidraw7"));
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;
    assert_eq!(emu.opens(), 2);
    // The half scan was dropped, never sent.
    assert!(scans.try_recv().is_err(), "no scan from half a code");
    emu.send(default_reports(b"]E0", b"9780201379624"));
    let scan = next_scan(&mut scans).await;
    assert_eq!(scan.raw, "]E09780201379624");
    s.stop().await.unwrap();
    assert_eq!(*s.health().borrow(), DeviceHealth::Unknown);
}

#[tokio::test]
async fn a_scanner_that_is_not_plugged_in_is_offline_until_it_is() {
    let device = honeywell("/dev/hidraw3");
    let emu = EmulatedHid::new(device.clone());
    emu.unplug();
    let s = scanner(&emu, &device);
    let mut scans = s.subscribe();
    let mut health = s.health();
    s.start().await.unwrap();
    let h = wait_health(&mut health, |h| matches!(h, DeviceHealth::Offline(_))).await;
    assert_eq!(
        h,
        DeviceHealth::Offline("Honeywell Xenon 1900 (0c2e:0b61) is not connected".into())
    );
    emu.plug(None);
    wait_health(&mut health, |h| *h == DeviceHealth::Ready).await;
    emu.send(default_reports(b"]E0", b"12345670"));
    assert_eq!(next_scan(&mut scans).await.raw, "]E012345670");
    s.stop().await.unwrap();
}

#[tokio::test]
async fn a_scanner_with_its_own_report_descriptor_is_read_by_it() {
    // The HID-POS spec's example layout: no length byte, data at byte 4, continued bit in
    // byte 60. Read with the default layout this would lose the first data byte.
    let device = honeywell("/dev/hidraw3");
    let emu = EmulatedHid::new(device.clone()).with_descriptor(example_descriptor());
    let s = scanner(&emu, &device);
    let mut scans = s.subscribe();
    s.start().await.unwrap();
    let mut report = vec![0u8; 61];
    report[0] = 2;
    report[1..4].copy_from_slice(b"]E0");
    report[4..17].copy_from_slice(b"4006381333931");
    emu.send([report]);
    assert_eq!(next_scan(&mut scans).await.raw, "]E04006381333931");
    s.stop().await.unwrap();
}
