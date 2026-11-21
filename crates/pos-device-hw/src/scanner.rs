//! `SerialScanner`: a barcode scanner on a serial port (RS-232, USB virtual COM, Bluetooth
//! SPP), implementing [`BarcodeInputPort`].
//!
//! A reader thread owns the port. It splits the byte stream into scans on CR, LF, CR LF or
//! ETX (or one configured byte), falling back to an inter-byte silence for scanners that
//! send no terminator. GS (0x1D, FNC1 in GS1 codes) is kept. Each scan becomes a
//! [`ScanEvent`] with a fresh id and is broadcast. When the port fails or disappears
//! (unplugged), the thread reopens it with capped exponential backoff, and every change is
//! published as [`DeviceHealth`].
//!
//! Known gap: HID-POS scanners (USB HID usage page 0x8C) need raw HID access (`hidraw`
//! ioctls on Linux), which is not done here. Keyboard-wedge scanners are captured by the UI.

use crate::config::{ScannerConfig, Terminator};
use crate::transport::serial::{is_vanished, open_port};
use pos_device_ports::{BarcodeInputPort, DeviceError, DeviceHealth, ScanEvent, ScanSource};
use pos_domain::{Clock, IdGenerator, ScanEventId, SystemClock, UuidV7Generator};
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};

const CR: u8 = 0x0D;
const LF: u8 = 0x0A;
const STX: u8 = 0x02;
const ETX: u8 = 0x03;

/// Splits a scanner's byte stream into scans. Pure, so it is tested without a port.
#[derive(Debug)]
pub struct ScanSplitter {
    terminator: Terminator,
    prefix: Option<Vec<u8>>,
    inter_byte: Duration,
    buf: Vec<u8>,
    last_byte: Option<Instant>,
    /// A CR ended the last scan: an LF right after it belongs to the same terminator.
    after_cr: bool,
}

impl ScanSplitter {
    pub fn new(terminator: Terminator, prefix: Option<&str>, inter_byte: Duration) -> Self {
        Self {
            terminator,
            prefix: prefix.filter(|p| !p.is_empty()).map(|p| p.as_bytes().to_vec()),
            inter_byte,
            buf: Vec::new(),
            last_byte: None,
            after_cr: false,
        }
    }

    fn is_terminator(&self, b: u8) -> bool {
        match self.terminator {
            Terminator::Auto => matches!(b, CR | LF | ETX),
            Terminator::Byte { value } => b == value,
            Terminator::TimeoutOnly => false,
        }
    }

    /// Feeds bytes received at `now`; returns the scans they completed.
    pub fn push(&mut self, bytes: &[u8], now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        // A partial scan left from an earlier burst ends at the silence before this one.
        out.extend(self.poll(now));
        for &b in bytes {
            if self.after_cr {
                self.after_cr = false;
                if b == LF {
                    continue;
                }
            }
            if self.is_terminator(b) {
                out.extend(self.take());
                self.after_cr = b == CR && self.terminator == Terminator::Auto;
            } else {
                self.buf.push(b);
            }
        }
        self.last_byte = Some(now);
        out
    }

    /// Ends a pending scan after the inter-byte silence.
    pub fn poll(&mut self, now: Instant) -> Option<String> {
        let last = self.last_byte?;
        if self.buf.is_empty() || now.saturating_duration_since(last) < self.inter_byte {
            return None;
        }
        self.take()
    }

    /// Drops a partial scan (the port went away mid-scan).
    pub fn reset(&mut self) {
        self.buf.clear();
        self.last_byte = None;
        self.after_cr = false;
    }

    fn take(&mut self) -> Option<String> {
        let mut bytes = std::mem::take(&mut self.buf);
        if self.terminator == Terminator::Auto && bytes.first() == Some(&STX) {
            bytes.remove(0);
        }
        if let Some(p) = &self.prefix
            && bytes.starts_with(p)
        {
            bytes.drain(..p.len());
        }
        if bytes.is_empty() {
            return None;
        }
        Some(match String::from_utf8(bytes) {
            Ok(s) => s,
            // Not UTF-8: read it as Latin-1 so no byte is lost.
            Err(e) => e.into_bytes().into_iter().map(char::from).collect(),
        })
    }
}

/// Reconnect delays: doubled after each failed open, up to `max`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(5),
        }
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct SerialScanner {
    config: ScannerConfig,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    backoff: Backoff,
    scans: broadcast::Sender<ScanEvent>,
    health: Arc<watch::Sender<DeviceHealth>>,
    worker: Mutex<Option<Worker>>,
}

impl std::fmt::Debug for SerialScanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialScanner")
            .field("port", &self.config.serial.port)
            .finish_non_exhaustive()
    }
}

impl SerialScanner {
    pub fn new(config: ScannerConfig) -> Self {
        let (scans, _) = broadcast::channel(64);
        let (health, _) = watch::channel(DeviceHealth::Unknown);
        Self {
            config,
            clock: Arc::new(SystemClock),
            ids: Arc::new(UuidV7Generator),
            backoff: Backoff::default(),
            scans,
            health: Arc::new(health),
            worker: Mutex::new(None),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_ids(mut self, ids: Arc<dyn IdGenerator>) -> Self {
        self.ids = ids;
        self
    }

    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn config(&self) -> &ScannerConfig {
        &self.config
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Worker>> {
        self.worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for SerialScanner {
    fn drop(&mut self) {
        if let Some(w) = self.lock().take() {
            w.stop.store(true, Ordering::SeqCst);
        }
    }
}

#[async_trait::async_trait]
impl BarcodeInputPort for SerialScanner {
    async fn start(&self) -> Result<(), DeviceError> {
        self.config.validate().map_err(|errors| {
            DeviceError::Unsupported(format!(
                "invalid scanner settings: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
        })?;
        let mut worker = self.lock();
        if worker.as_ref().is_some_and(|w| !w.thread.is_finished()) {
            return Ok(());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let reader = Reader {
            config: self.config.clone(),
            clock: Arc::clone(&self.clock),
            ids: Arc::clone(&self.ids),
            backoff: self.backoff,
            scans: self.scans.clone(),
            health: Arc::clone(&self.health),
            stop: Arc::clone(&stop),
        };
        let thread = std::thread::Builder::new()
            .name(format!("scanner {}", self.config.serial.port))
            .spawn(move || reader.run())
            .map_err(|e| DeviceError::Io(e.to_string()))?;
        *worker = Some(Worker { stop, thread });
        Ok(())
    }

    async fn stop(&self) -> Result<(), DeviceError> {
        let Some(w) = self.lock().take() else {
            return Ok(());
        };
        w.stop.store(true, Ordering::SeqCst);
        // The reader checks the flag at least every 50 ms.
        let _ = tokio::task::spawn_blocking(move || w.thread.join()).await;
        self.health.send_replace(DeviceHealth::Unknown);
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<ScanEvent> {
        self.scans.subscribe()
    }

    fn health(&self) -> watch::Receiver<DeviceHealth> {
        self.health.subscribe()
    }
}

struct Reader {
    config: ScannerConfig,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    backoff: Backoff,
    scans: broadcast::Sender<ScanEvent>,
    health: Arc<watch::Sender<DeviceHealth>>,
    stop: Arc<AtomicBool>,
}

impl Reader {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn set_health(&self, h: DeviceHealth) {
        if *self.health.borrow() != h {
            tracing::info!(target: "pos_device_hw", port = %self.config.serial.port, health = ?h, "scanner");
            self.health.send_replace(h);
        }
    }

    /// Sleeps `d` in short slices so stop() is prompt.
    fn pause(&self, d: Duration) {
        let until = Instant::now() + d;
        while !self.stopped() && Instant::now() < until {
            std::thread::sleep(
                Duration::from_millis(20).min(until.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn run(self) {
        let inter_byte = Duration::from_millis(u64::from(self.config.inter_byte_timeout_ms));
        let slice = (inter_byte / 2).clamp(Duration::from_millis(5), Duration::from_millis(50));
        let mut splitter = ScanSplitter::new(
            self.config.terminator,
            self.config.prefix_strip.as_deref(),
            inter_byte,
        );
        let mut delay = self.backoff.initial;
        while !self.stopped() {
            let mut port = match open_port(&self.config.serial, slice) {
                Ok(p) => p,
                Err(e) => {
                    self.set_health(DeviceHealth::Offline(e.to_string()));
                    self.pause(delay);
                    delay = (delay * 2).min(self.backoff.max);
                    continue;
                }
            };
            // Ready only once a read has worked: a port that was unplugged can still open
            // (a hung-up tty) and then fail at once.
            let mut working = false;
            let mut chunk = [0u8; 256];
            while !self.stopped() {
                let read = port.read(&mut chunk);
                let ok = matches!(read, Ok(n) if n > 0)
                    || matches!(&read, Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted));
                if ok && !working {
                    working = true;
                    delay = self.backoff.initial;
                    self.set_health(DeviceHealth::Ready);
                }
                match read {
                    Ok(0) => {
                        self.set_health(DeviceHealth::Offline("the scanner disconnected".into()));
                        break;
                    }
                    Ok(n) => {
                        for raw in splitter.push(&chunk[..n], Instant::now()) {
                            self.emit(raw);
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        if let Some(raw) = splitter.poll(Instant::now()) {
                            self.emit(raw);
                        }
                    }
                    Err(e) => {
                        let why = if is_vanished(&e) {
                            format!("{} disappeared (unplugged?)", self.config.serial.port)
                        } else {
                            format!("{}: {e}", self.config.serial.port)
                        };
                        self.set_health(DeviceHealth::Offline(why));
                        break;
                    }
                }
            }
            // A scan cut off by the disconnect is incomplete: drop it rather than guess.
            splitter.reset();
            drop(port);
            self.pause(delay);
            if !working {
                delay = (delay * 2).min(self.backoff.max);
            }
        }
    }

    fn emit(&self, raw: String) {
        let event = ScanEvent {
            id: ScanEventId::generate(self.ids.as_ref()),
            source: ScanSource::Serial(self.config.serial.port.clone()),
            raw,
            symbology_hint: None,
            received_at: self.clock.now(),
        };
        tracing::debug!(target: "pos_device_hw", port = %self.config.serial.port, id = %event.id, "scan");
        // No subscriber: nobody is selling right now; the scan is dropped like a keypress.
        let _ = self.scans.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(t: Terminator, prefix: Option<&str>, chunks: &[&[u8]]) -> Vec<String> {
        let mut s = ScanSplitter::new(t, prefix, Duration::from_millis(100));
        let now = Instant::now();
        chunks.iter().flat_map(|c| s.push(c, now)).collect()
    }

    #[test]
    fn splits_on_cr_lf_crlf_etx() {
        assert_eq!(
            split(Terminator::Auto, None, &[b"4006381333931\r5901234123457\n"]),
            ["4006381333931", "5901234123457"]
        );
        assert_eq!(
            split(Terminator::Auto, None, &[b"123\r\n456\r\n"]),
            ["123", "456"]
        );
        // CR LF split across reads.
        assert_eq!(
            split(Terminator::Auto, None, &[b"123\r", b"\n456\r"]),
            ["123", "456"]
        );
        assert_eq!(
            split(Terminator::Auto, None, &[b"\x02ABC\x03\x02DEF\x03"]),
            ["ABC", "DEF"]
        );
        // Empty scans (double terminators) are ignored.
        assert_eq!(split(Terminator::Auto, None, &[b"\n\n1\r\r"]), ["1"]);
        assert_eq!(
            split(Terminator::Byte { value: b'\t' }, None, &[b"12\r3\t45\t"]),
            ["12\r3", "45"]
        );
    }

    #[test]
    fn keeps_gs_and_strips_prefix() {
        let gs1 = b"]C1010950123456789\x1d10ABC123\r";
        assert_eq!(
            split(Terminator::Auto, Some("]C1"), &[gs1]),
            ["010950123456789\u{1d}10ABC123"]
        );
        // A scan without the prefix is kept whole.
        assert_eq!(split(Terminator::Auto, Some("]C1"), &[b"123\r"]), ["123"]);
        // Latin-1 bytes survive.
        assert_eq!(split(Terminator::Auto, None, &[b"caf\xe9\r"]), ["caf\u{e9}"]);
    }

    #[test]
    fn no_terminator_ends_on_silence() {
        let mut s = ScanSplitter::new(Terminator::TimeoutOnly, None, Duration::from_millis(100));
        let t0 = Instant::now();
        assert!(s.push(b"1234", t0).is_empty());
        assert!(s.push(b"5678", t0 + Duration::from_millis(30)).is_empty());
        assert_eq!(s.poll(t0 + Duration::from_millis(80)), None);
        assert_eq!(
            s.poll(t0 + Duration::from_millis(140)).as_deref(),
            Some("12345678")
        );
        assert_eq!(s.poll(t0 + Duration::from_millis(500)), None);
        // A new burst after silence starts a new scan even before poll() ran.
        assert!(s.push(b"AAA", t0 + Duration::from_secs(1)).is_empty());
        assert_eq!(s.push(b"BBB", t0 + Duration::from_secs(2)), ["AAA"]);

        // With a terminator configured, silence still ends a scan that lost its terminator.
        let mut s = ScanSplitter::new(Terminator::Auto, None, Duration::from_millis(100));
        assert!(s.push(b"999", t0).is_empty());
        assert_eq!(s.poll(t0 + Duration::from_millis(150)).as_deref(), Some("999"));
    }
}
