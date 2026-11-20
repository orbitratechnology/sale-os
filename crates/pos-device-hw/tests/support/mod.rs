//! An ESC/POS printer emulator for tests: answers `DLE EOT n` from a scripted state and
//! `GS I n` with a scripted identity, and records every other byte as job data. Served over
//! TCP (tokio listener) or, on unix, over a pseudo-terminal pair (virtual null-modem).

#![allow(dead_code)]

use std::net::SocketAddr;
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const IDLE: u8 = 0x12;
pub const OFFLINE_BIT: u8 = 0x08;

/// Replies to `DLE EOT 1, 2, 3, 4`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub printer: u8,
    pub offline: u8,
    pub error: u8,
    pub paper: u8,
}

impl Status {
    pub const READY: Status = Status {
        printer: IDLE,
        offline: IDLE,
        error: IDLE,
        paper: IDLE,
    };
    pub const PAPER_OUT: Status = Status {
        printer: IDLE | OFFLINE_BIT,
        offline: IDLE | 0x20,
        error: IDLE,
        paper: IDLE | 0x60,
    };
    pub const COVER_OPEN: Status = Status {
        printer: IDLE | OFFLINE_BIT,
        offline: IDLE | 0x04,
        error: IDLE,
        paper: IDLE,
    };
    pub const CUTTER_ERROR: Status = Status {
        printer: IDLE | OFFLINE_BIT,
        offline: IDLE | 0x40,
        error: IDLE | 0x08,
        paper: IDLE,
    };
    pub const NEAR_END_DRAWER_HIGH: Status = Status {
        printer: IDLE | 0x04,
        offline: IDLE,
        error: IDLE,
        paper: IDLE | 0x0C,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Accepts the connection and never reads from it.
    NeverRead,
    /// Drops the connection once this many job bytes arrived.
    DropAfter(usize),
}

#[derive(Clone, Debug)]
pub struct Script {
    pub status: Status,
    /// The state reported once job bytes have arrived (paper ran out while printing).
    pub after_job: Option<Status>,
    /// Whether `DLE EOT` gets an answer at all.
    pub answers_status: bool,
    pub maker: Option<String>,
    pub model: Option<String>,
    pub model_id: Option<u8>,
    pub mode: Mode,
    /// Time spent "printing" each chunk, to widen race windows.
    pub chunk_delay: Duration,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            status: Status::READY,
            after_job: None,
            answers_status: true,
            maker: None,
            model: None,
            model_id: None,
            mode: Mode::Normal,
            chunk_delay: Duration::ZERO,
        }
    }
}

/// The byte-level protocol, shared by the TCP and serial emulators.
#[derive(Debug, Default)]
pub struct Proto {
    pending: Vec<u8>,
    pub job: Vec<u8>,
    pub queries: Vec<u8>,
}

impl Proto {
    /// Feeds received bytes; returns the reply to send.
    pub fn feed(&mut self, script: &Script, bytes: &[u8]) -> Vec<u8> {
        let mut reply = Vec::new();
        for &b in bytes {
            self.byte(script, b, &mut reply);
        }
        reply
    }

    fn byte(&mut self, script: &Script, b: u8, reply: &mut Vec<u8>) {
        match self.pending.as_slice() {
            [] if b == 0x10 || b == 0x1D => self.pending.push(b),
            [] => self.job.push(b),
            [0x10] if b == 0x04 => self.pending.push(b),
            [0x1D] if b == 0x49 => self.pending.push(b),
            [0x10, 0x04] => {
                self.pending.clear();
                self.queries.push(b);
                if script.answers_status {
                    let st = match script.after_job {
                        Some(after) if !self.job.is_empty() => after,
                        _ => script.status,
                    };
                    reply.push(match b {
                        1 => st.printer,
                        2 => st.offline,
                        3 => st.error,
                        _ => st.paper,
                    });
                }
            }
            [0x1D, 0x49] => {
                self.pending.clear();
                let text = |t: &Option<String>, reply: &mut Vec<u8>| {
                    if let Some(t) = t {
                        reply.push(0x5F);
                        reply.extend_from_slice(t.as_bytes());
                        reply.push(0);
                    }
                };
                match b {
                    1 => reply.extend(script.model_id),
                    0x42 => text(&script.maker, reply),
                    0x43 => text(&script.model, reply),
                    _ => {}
                }
            }
            _ => {
                // Not a query after all: what was held back is job data.
                let held = std::mem::take(&mut self.pending);
                self.job.extend_from_slice(&held);
                self.byte(script, b, reply);
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Connection {
    pub job: Vec<u8>,
    pub queries: Vec<u8>,
}

pub struct TcpEmulator {
    pub addr: SocketAddr,
    pub script: Arc<Mutex<Script>>,
    pub connections: Arc<Mutex<Vec<Connection>>>,
    active: Arc<AtomicUsize>,
    pub max_active: Arc<AtomicUsize>,
}

impl TcpEmulator {
    pub async fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let emu = Self {
            addr,
            script: Arc::new(Mutex::new(script)),
            connections: Arc::default(),
            active: Arc::default(),
            max_active: Arc::default(),
        };
        let (script, conns, active, max) = (
            Arc::clone(&emu.script),
            Arc::clone(&emu.connections),
            Arc::clone(&emu.active),
            Arc::clone(&emu.max_active),
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (script, conns, active, max) = (
                    Arc::clone(&script),
                    Arc::clone(&conns),
                    Arc::clone(&active),
                    Arc::clone(&max),
                );
                tokio::spawn(async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max.fetch_max(now, Ordering::SeqCst);
                    let mut proto = Proto::default();
                    let s = script.lock().unwrap().clone();
                    if s.mode == Mode::NeverRead {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    } else {
                        let mut buf = [0u8; 4096];
                        loop {
                            let n = match stream.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n,
                            };
                            let s = script.lock().unwrap().clone();
                            let reply = proto.feed(&s, &buf[..n]);
                            if !s.chunk_delay.is_zero() {
                                tokio::time::sleep(s.chunk_delay).await;
                            }
                            if !reply.is_empty() && stream.write_all(&reply).await.is_err() {
                                break;
                            }
                            if let Mode::DropAfter(limit) = s.mode
                                && proto.job.len() >= limit
                            {
                                break;
                            }
                        }
                    }
                    conns.lock().unwrap().push(Connection {
                        job: proto.job,
                        queries: proto.queries,
                    });
                    // Count down before closing, so a client that saw our close never
                    // overlaps with this connection.
                    active.fetch_sub(1, Ordering::SeqCst);
                    drop(stream);
                });
            }
        });
        emu
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Job bytes of every finished connection, in order of completion.
    pub fn jobs(&self) -> Vec<Vec<u8>> {
        self.connections
            .lock()
            .unwrap()
            .iter()
            .filter(|c| !c.job.is_empty())
            .map(|c| c.job.clone())
            .collect()
    }

    pub fn all_job_bytes(&self) -> Vec<u8> {
        self.jobs().concat()
    }

    pub fn set(&self, f: impl FnOnce(&mut Script)) {
        f(&mut self.script.lock().unwrap());
    }

    /// Waits until `n` connections have finished (the emulator side closes after the client).
    pub async fn wait_connections(&self, n: usize) {
        for _ in 0..200 {
            if self.connections.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// The serial emulator: answers on the master side of a pty pair from a thread.
#[cfg(unix)]
pub struct SerialEmulator {
    pub slave_path: String,
    pub proto: Arc<Mutex<Proto>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    _slave: serialport::TTYPort,
}

#[cfg(unix)]
impl SerialEmulator {
    pub fn start(script: Script) -> Self {
        use std::io::{Read, Write};
        let (mut master, slave) = serialport::TTYPort::pair().expect("pty pair");
        let slave_path = serialport::SerialPort::name(&slave).expect("slave name");
        // The code under test opens the slave path itself. Our own slave handle stays open
        // so the master never sees a hang-up (which would hide unread bytes) between opens.
        serialport::SerialPort::set_timeout(&mut master, Duration::from_millis(20)).unwrap();
        let proto = Arc::new(Mutex::new(Proto::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (p, st) = (Arc::clone(&proto), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while !st.load(Ordering::SeqCst) {
                match master.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let reply = p.lock().unwrap().feed(&script, &buf[..n]);
                        if !reply.is_empty() {
                            let _ = master.write_all(&reply);
                        }
                    }
                    _ => {}
                }
            }
        });
        Self {
            slave_path,
            proto,
            stop,
            thread: Some(thread),
            _slave: slave,
        }
    }

    pub fn job(&self) -> Vec<u8> {
        self.proto.lock().unwrap().job.clone()
    }

    /// Waits until at least `n` job bytes arrived.
    pub fn wait_job(&self, n: usize) -> Vec<u8> {
        for _ in 0..200 {
            let job = self.job();
            if job.len() >= n {
                return job;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.job()
    }
}

#[cfg(unix)]
impl Drop for SerialEmulator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
