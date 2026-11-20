//! Device files: Linux `usblp` (`/dev/usb/lp0`), parallel ports (`/dev/lp0`), Bluetooth
//! RFCOMM (`/dev/rfcomm0`), and on Windows `LPT1` / `COM3` opened as files.
//!
//! On unix the file is opened non-blocking, so a printer that stops reading (switched off,
//! out of paper with a full buffer) can never wedge a thread: writes and reads poll until
//! their deadline. `usblp` allows one opener at a time, which makes that matter.
//!
//! Whether the device can answer is **probed, not assumed**: when asked to, the first open
//! sends `DLE EOT 1` and waits briefly for a valid status byte. A regular file, a write-only
//! parallel port or a printer without a back channel gives no reply and stays one-way.

use super::{Link, SendOutcome, Transport, TransportError};
use crate::escpos;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const GRACE: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(5);
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

const UNKNOWN: u8 = 0;
const YES: u8 = 1;
const NO: u8 = 2;

#[derive(Debug)]
pub struct DeviceFileTransport {
    path: PathBuf,
    /// 0 not probed yet, 1 answers, 2 one-way.
    bidirectional: Arc<AtomicU8>,
}

impl DeviceFileTransport {
    /// `probe`: find out on first open whether the device answers ESC/POS status. Without
    /// it the link is treated as one-way.
    pub fn new(path: impl AsRef<Path>, probe: bool) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            bidirectional: Arc::new(AtomicU8::new(if probe { UNKNOWN } else { NO })),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn open_file(path: &Path) -> io::Result<(File, bool)> {
    let mut rw = OpenOptions::new();
    rw.read(true).write(true);
    let mut wo = OpenOptions::new();
    wo.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Appending is a no-op for character devices and keeps successive jobs when the
        // "device" is a plain file (debugging, tests).
        rw.append(true);
        wo.append(true);
        rw.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
        wo.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    match rw.open(path) {
        Ok(f) => Ok((f, true)),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => wo.open(path).map(|f| (f, false)),
        Err(e) => Err(e),
    }
}

fn map_open_error(path: &Path, e: &io::Error) -> TransportError {
    let p = path.display();
    match e.kind() {
        io::ErrorKind::NotFound => {
            TransportError::Unreachable(format!("{p} not found: printer unplugged or off?"))
        }
        io::ErrorKind::ResourceBusy => TransportError::Busy(format!("{p} is open in another program")),
        io::ErrorKind::PermissionDenied => TransportError::Unreachable(format!(
            "no permission to open {p} (on Linux, add the user to the lp group)"
        )),
        _ if e.raw_os_error() == Some(16) && cfg!(unix) => {
            TransportError::Busy(format!("{p} is open in another program"))
        }
        _ => TransportError::Unreachable(format!("{p}: {e}")),
    }
}

fn would_block(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted)
}

fn write_blocking(
    file: &mut File,
    data: &[u8],
    deadline: Instant,
    progress: &AtomicUsize,
) -> Result<(), TransportError> {
    let mut sent = 0;
    while sent < data.len() {
        match file.write(&data[sent..]) {
            Ok(0) => {}
            Ok(n) => {
                sent += n;
                progress.store(sent, Ordering::SeqCst);
                continue;
            }
            Err(e) if would_block(&e) => {}
            Err(e) => {
                return Err(TransportError::Io {
                    sent,
                    message: e.to_string(),
                });
            }
        }
        if Instant::now() >= deadline {
            return Err(TransportError::Timeout { sent });
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

/// Reads what is available within `timeout`. End of file (a regular file, or a device with
/// no back channel) is "no reply", not an error.
fn read_blocking(file: &mut File, max: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
    let deadline = Instant::now() + timeout;
    let mut buf = vec![0; max.max(1)];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return Ok(Vec::new()),
            Ok(n) => {
                buf.truncate(n);
                return Ok(buf);
            }
            Err(e) if would_block(&e) => {}
            Err(e) => {
                return Err(TransportError::Io {
                    sent: 0,
                    message: e.to_string(),
                });
            }
        }
        if Instant::now() >= deadline {
            return Ok(Vec::new());
        }
        std::thread::sleep(POLL);
    }
}

fn probe(file: &mut File) -> bool {
    let progress = AtomicUsize::new(0);
    let deadline = Instant::now() + PROBE_TIMEOUT;
    if write_blocking(file, &escpos::DLE_EOT_PRINTER, deadline, &progress).is_err() {
        return false;
    }
    matches!(read_blocking(file, 1, PROBE_TIMEOUT), Ok(r) if r.first().is_some_and(|b| escpos::is_status_byte(*b)))
}

#[async_trait::async_trait]
impl Transport for DeviceFileTransport {
    fn adapter(&self) -> &'static str {
        "escpos_file"
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }

    fn bidirectional(&self) -> bool {
        self.bidirectional.load(Ordering::SeqCst) == YES
    }

    async fn open(&self, timeout: Duration) -> Result<Box<dyn Link>, TransportError> {
        let path = self.path.clone();
        let state = Arc::clone(&self.bidirectional);
        let task = tokio::task::spawn_blocking(move || {
            let (mut file, readable) = open_file(&path).map_err(|e| map_open_error(&path, &e))?;
            if state.load(Ordering::SeqCst) == UNKNOWN {
                let answers = readable && probe(&mut file);
                tracing::info!(target: "pos_device_hw", path = %path.display(), answers, "device file probed");
                state.store(if answers { YES } else { NO }, Ordering::SeqCst);
            }
            Ok::<_, TransportError>((file, readable))
        });
        let (file, readable) = match tokio::time::timeout(timeout + GRACE, task).await {
            Err(_) => {
                return Err(TransportError::Unreachable(format!(
                    "{} did not open in time",
                    self.path.display()
                )));
            }
            Ok(Err(e)) => {
                return Err(TransportError::Io {
                    sent: 0,
                    message: e.to_string(),
                });
            }
            Ok(Ok(r)) => r?,
        };
        Ok(Box::new(FileLink {
            file: Some(file),
            readable,
            name: self.describe(),
        }))
    }
}

struct FileLink {
    file: Option<File>,
    readable: bool,
    name: String,
}

impl FileLink {
    fn take(&mut self) -> Result<File, TransportError> {
        self.file.take().ok_or_else(|| TransportError::Io {
            sent: 0,
            message: format!("{}: the link was lost after a timeout", self.name),
        })
    }
}

impl Drop for FileLink {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            super::serial::close_off_thread(f);
        }
    }
}

#[async_trait::async_trait]
impl Link for FileLink {
    async fn write_all(&mut self, bytes: &[u8], timeout: Duration) -> Result<(), TransportError> {
        let mut file = self.take()?;
        let data = bytes.to_vec();
        let sent = Arc::new(AtomicUsize::new(0));
        let progress = Arc::clone(&sent);
        let task = tokio::task::spawn_blocking(move || {
            let result = write_blocking(&mut file, &data, Instant::now() + timeout, &progress);
            (file, result)
        });
        match tokio::time::timeout(timeout + GRACE, task).await {
            // Only reachable where writes block (Windows): that thread is lost until the OS
            // gives up on the write.
            Err(_) => Err(TransportError::Timeout {
                sent: sent.load(Ordering::SeqCst),
            }),
            Ok(Err(e)) => Err(TransportError::Io {
                sent: sent.load(Ordering::SeqCst),
                message: e.to_string(),
            }),
            Ok(Ok((file, result))) => {
                self.file = Some(file);
                result
            }
        }
    }

    async fn read(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        if !self.readable {
            return Ok(Vec::new());
        }
        let mut file = self.take()?;
        let task = tokio::task::spawn_blocking(move || {
            let result = read_blocking(&mut file, max, timeout);
            (file, result)
        });
        match tokio::time::timeout(timeout + GRACE, task).await {
            Err(_) => Ok(Vec::new()),
            Ok(Err(e)) => Err(TransportError::Io {
                sent: 0,
                message: e.to_string(),
            }),
            Ok(Ok((file, result))) => {
                self.file = Some(file);
                result
            }
        }
    }

    async fn finish(mut self: Box<Self>, _timeout: Duration) -> Result<SendOutcome, TransportError> {
        if let Some(f) = self.file.take() {
            super::serial::close_off_thread(f);
        }
        Ok(SendOutcome::Delivered)
    }
}
