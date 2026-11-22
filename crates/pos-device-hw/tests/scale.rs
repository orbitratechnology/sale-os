//! Serial scales over a pseudo-terminal pair: an emulated scale on the other end answers
//! `W` the Toledo-8217 way, or streams continuous `ST,GS,…` lines.
#![cfg(unix)]

use pos_device_hw::config::SerialSettings;
use pos_device_hw::scale::ScaleTimeouts;
use pos_device_hw::transport::SerialTransport;
use pos_device_hw::{HwScale, ScaleProtocol, ScaleUnit};
use pos_device_ports::{DeviceError, DeviceHealth, ScalePort, ScaleReading, Weight};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the emulated scale does.
#[derive(Clone)]
enum Behaviour {
    /// Answers each `W` with these bytes (nothing: a silent scale).
    Toledo(Vec<u8>),
    /// Sends this line (CR LF added) every 20 ms, in two pieces so lines arrive split.
    Continuous(Vec<u8>),
}

struct PtyScale {
    path: String,
    behaviour: Arc<Mutex<Behaviour>>,
    requests: Arc<Mutex<Vec<u8>>>,
    _slave: serialport::TTYPort,
}

impl PtyScale {
    fn start(behaviour: Behaviour) -> Self {
        let (mut master, slave) = serialport::TTYPort::pair().expect("pty pair");
        let path = serialport::SerialPort::name(&slave).expect("slave name");
        serialport::SerialPort::set_timeout(&mut master, Duration::from_millis(10)).unwrap();
        let behaviour = Arc::new(Mutex::new(behaviour));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (b, r) = (behaviour.clone(), requests.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let mut last_line = std::time::Instant::now();
            loop {
                match master.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        r.lock().unwrap().extend_from_slice(&buf[..n]);
                        if let Behaviour::Toledo(reply) = &*b.lock().unwrap() {
                            for _ in buf[..n].iter().filter(|&&c| c == b'W') {
                                if master.write_all(reply).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => return,
                }
                if let Behaviour::Continuous(line) = &*b.lock().unwrap()
                    && last_line.elapsed() >= Duration::from_millis(20)
                {
                    last_line = std::time::Instant::now();
                    let mut bytes = line.clone();
                    bytes.extend_from_slice(b"\r\n");
                    let (a, z) = bytes.split_at(bytes.len() / 2);
                    if master.write_all(a).is_err() || master.write_all(z).is_err() {
                        return;
                    }
                }
            }
        });
        Self {
            path,
            behaviour,
            requests,
            _slave: slave,
        }
    }

    fn set(&self, b: Behaviour) {
        *self.behaviour.lock().unwrap() = b;
    }
}

fn scale(path: &str, protocol: ScaleProtocol) -> HwScale {
    HwScale::new(
        Arc::new(SerialTransport::new(SerialSettings::new(path, 9600))),
        protocol,
        ScaleUnit::Kg,
        ScaleTimeouts {
            open: Duration::from_secs(1),
            write: Duration::from_millis(300),
            reply: Duration::from_millis(400),
        },
    )
}

fn kg(mg: i64) -> Weight {
    Weight::from_milligrams(mg)
}

#[tokio::test]
async fn a_toledo_scale_answers_w_with_its_weight_or_its_status() {
    let pty = PtyScale::start(Behaviour::Toledo(b"\x0201.234\r".to_vec()));
    let s = scale(&pty.path, ScaleProtocol::Toledo8217);
    assert_eq!(s.describe(), format!("{} @ 9600, Toledo 8217 (kg)", pty.path));
    assert_eq!(*s.health().borrow(), DeviceHealth::Unknown);
    assert_eq!(s.read().await, Ok(ScaleReading::Stable(kg(1_234_000))));
    assert_eq!(*s.health().borrow(), DeviceHealth::Ready);
    assert_eq!(pty.requests.lock().unwrap().as_slice(), b"W");

    // In motion, over and under capacity, at zero: status frames.
    for (reply, want) in [
        (&b"\x02?A\r"[..], ScaleReading::Moving(None)),
        (b"\x02?`\r", ScaleReading::Overload),
        (b"\x02?P\r", ScaleReading::Underload),
        (b"\x02?B\r", ScaleReading::Stable(Weight::ZERO)),
        (b"\x02 0.455\r", ScaleReading::Stable(kg(455_000))),
    ] {
        pty.set(Behaviour::Toledo(reply.to_vec()));
        assert_eq!(s.read().await, Ok(want));
    }
    // A late answer to an earlier request is not taken for the next one.
    pty.set(Behaviour::Toledo(b"\x0202.000\r\x0202.000\r".to_vec()));
    assert_eq!(s.read().await, Ok(ScaleReading::Stable(kg(2_000_000))));
    pty.set(Behaviour::Toledo(b"\x0203.500\r".to_vec()));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(s.read().await, Ok(ScaleReading::Stable(kg(3_500_000))));
}

#[tokio::test]
async fn a_silent_or_garbled_scale_is_an_error_and_its_health_says_why() {
    let pty = PtyScale::start(Behaviour::Toledo(Vec::new()));
    let s = scale(&pty.path, ScaleProtocol::Toledo8217);
    let started = std::time::Instant::now();
    assert_eq!(s.read().await, Err(DeviceError::Timeout));
    assert!(started.elapsed() < Duration::from_secs(2), "bounded");
    assert!(
        matches!(&*s.health().borrow(), DeviceHealth::Offline(m) if m.contains("did not answer")),
        "{:?}",
        s.health().borrow()
    );
    // Wrong speed: bytes that are not a frame.
    pty.set(Behaviour::Toledo(b"\x02\xff\xfe\x81\r".to_vec()));
    assert!(matches!(s.read().await, Err(DeviceError::Io(m)) if m.contains("unreadable")));
    // Fixed: it works again (the port is reopened).
    pty.set(Behaviour::Toledo(b"\x0200.250\r".to_vec()));
    assert_eq!(s.read().await, Ok(ScaleReading::Stable(kg(250_000))));
    assert_eq!(*s.health().borrow(), DeviceHealth::Ready);
}

#[tokio::test]
async fn a_continuous_scale_gives_its_latest_whole_line() {
    let pty = PtyScale::start(Behaviour::Continuous(b"US,GS,+  1.230kg".to_vec()));
    let s = scale(&pty.path, ScaleProtocol::ContinuousAscii);
    assert_eq!(s.describe(), format!("{} @ 9600, continuous ASCII", pty.path));
    assert_eq!(s.read().await, Ok(ScaleReading::Moving(Some(kg(1_230_000)))));
    pty.set(Behaviour::Continuous(b"ST,GS,+  1.234kg".to_vec()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(s.read().await, Ok(ScaleReading::Stable(kg(1_234_000))));
    pty.set(Behaviour::Continuous(b"ST,NT,-  0.015kg".to_vec()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(s.read().await, Ok(ScaleReading::Stable(kg(-15_000))));
    pty.set(Behaviour::Continuous(b"OL,GS,+99999.9kg".to_vec()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(s.read().await, Ok(ScaleReading::Overload));
    assert!(
        pty.requests.lock().unwrap().is_empty(),
        "a continuous scale is never asked"
    );

    // Pounds: refused, and the health says what to change.
    pty.set(Behaviour::Continuous(b"ST,GS,+  2.500lb".to_vec()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(s.read().await.is_err());
    assert!(
        matches!(&*s.health().borrow(), DeviceHealth::Offline(m) if m.contains("set it to kilograms")),
        "{:?}",
        s.health().borrow()
    );
}

#[tokio::test]
async fn a_missing_port_is_an_answer_not_a_wait() {
    let s = scale("/dev/does-not-exist-scale", ScaleProtocol::Toledo8217);
    assert!(matches!(s.read().await, Err(DeviceError::Disconnected(_))));
    assert!(matches!(&*s.health().borrow(), DeviceHealth::Offline(_)));
}
