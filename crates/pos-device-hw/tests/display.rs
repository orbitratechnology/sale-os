//! Customer displays over a pseudo-terminal pair (a virtual null-modem cable): the bytes a
//! 2×20 pole display receives, per protocol; only the latest of many queued frames is
//! written; a missing port becomes the display's health and never blocks `show`.
#![cfg(unix)]

use pos_device_hw::config::SerialSettings;
use pos_device_hw::display::{frame_bytes, init_bytes, test_frame};
use pos_device_hw::{DisplayConfig, DisplayProtocol, HwCustomerDisplay};
use pos_device_ports::{CustomerDisplayPort, DeviceHealth, DisplayFrame, DisplayLine};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The display's side of the cable: records every byte it receives.
struct PtyDisplay {
    path: String,
    received: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    _slave: serialport::TTYPort,
}

impl PtyDisplay {
    fn start() -> Self {
        let (mut master, slave) = serialport::TTYPort::pair().expect("pty pair");
        let path = serialport::SerialPort::name(&slave).expect("slave name");
        serialport::SerialPort::set_timeout(&mut master, Duration::from_millis(20)).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (sink, st) = (Arc::clone(&received), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            while !st.load(Ordering::SeqCst) {
                if let Ok(n) = master.read(&mut buf)
                    && n > 0
                {
                    sink.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }
        });
        Self {
            path,
            received,
            stop,
            _slave: slave,
        }
    }

    fn received(&self) -> Vec<u8> {
        self.received.lock().unwrap().clone()
    }

    async fn wait_for(&self, len: usize) -> Vec<u8> {
        let start = Instant::now();
        while self.received().len() < len && start.elapsed() < Duration::from_secs(3) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.received()
    }
}

impl Drop for PtyDisplay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn display(path: &str, protocol: DisplayProtocol) -> HwCustomerDisplay {
    HwCustomerDisplay::from_config(&DisplayConfig::new(SerialSettings::new(path, 9600), protocol)).unwrap()
}

fn item(name: &str, price: &str, total: &str) -> DisplayFrame {
    DisplayFrame::new(
        DisplayLine::Pair(name.into(), price.into()),
        DisplayLine::Pair("TOTAL".into(), total.into()),
    )
}

#[tokio::test]
async fn esc_pos_display_receives_init_then_whole_lines() {
    let pty = PtyDisplay::start();
    let d = display(&pty.path, DisplayProtocol::EscPos);
    assert_eq!(*d.health().borrow(), DeviceHealth::Unknown);
    d.show(item("Café crème", "450.00", "450.00"));
    let mut want = init_bytes(DisplayProtocol::EscPos);
    want.extend_from_slice(&[0x1F, 0x24, 1, 1]);
    want.extend_from_slice(b"Cafe creme    450.00");
    want.extend_from_slice(&[0x1F, 0x24, 1, 2]);
    want.extend_from_slice(b"TOTAL         450.00");
    assert_eq!(pty.wait_for(want.len()).await, want);
    assert_eq!(*d.health().borrow(), DeviceHealth::Ready);
    assert!(d.describe().ends_with(", ESC/POS"), "{}", d.describe());

    // The port stays open: the next frame comes without another init.
    let next = item("Milk", "250.00", "700.00");
    d.show(next.clone());
    want.extend(frame_bytes(DisplayProtocol::EscPos, &next));
    assert_eq!(pty.wait_for(want.len()).await, want);
}

#[tokio::test]
async fn cd5220_display_receives_its_own_commands() {
    let pty = PtyDisplay::start();
    let d = display(&pty.path, DisplayProtocol::Cd5220);
    d.test(test_frame()).await.unwrap();
    let mut want = vec![0x1B, b'@', 0x1B, 0x11, 0x0C];
    want.extend_from_slice(b"\x1bQA");
    want.extend_from_slice(b"SALE OS DISPLAY TEST\r");
    want.extend_from_slice(b"\x1bQB");
    want.extend_from_slice(b"12345678901234567890\r");
    assert_eq!(pty.wait_for(want.len()).await, want);
}

#[tokio::test]
async fn only_the_latest_of_many_queued_frames_is_written() {
    let pty = PtyDisplay::start();
    let d = display(&pty.path, DisplayProtocol::EscPos);
    // Fifty scans in a burst: `show` never waits, and the display catches up with the last.
    let started = Instant::now();
    for n in 1..=50 {
        d.show(item(&format!("Item {n}"), "1.00", &format!("{n}.00")));
    }
    assert!(started.elapsed() < Duration::from_millis(50));
    let mut want = init_bytes(DisplayProtocol::EscPos);
    want.extend(frame_bytes(
        DisplayProtocol::EscPos,
        &item("Item 50", "1.00", "50.00"),
    ));
    assert_eq!(pty.wait_for(want.len()).await, want);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(pty.received(), want, "nothing older written after it");
}

#[tokio::test]
async fn a_missing_port_is_the_displays_status_never_a_wait() {
    let d = display("/dev/sale-os-no-such-display", DisplayProtocol::EscPos);
    let mut health = d.health();
    let started = Instant::now();
    d.show(item("Milk", "250.00", "250.00"));
    assert!(started.elapsed() < Duration::from_millis(20));
    tokio::time::timeout(
        Duration::from_secs(3),
        health.wait_for(|h| matches!(h, DeviceHealth::Offline(_))),
    )
    .await
    .unwrap()
    .unwrap();
    let err = d.test(test_frame()).await.unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}
