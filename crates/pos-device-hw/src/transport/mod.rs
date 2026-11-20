//! Byte transports to a device: TCP raw (port 9100), serial (RS-232, USB virtual COM,
//! Bluetooth SPP), device files (`/dev/usb/lp0`, `/dev/lp0`, `LPT1`) and the OS spooler.
//!
//! A transport opens a [`Link`] for one exchange (one print job, one status query, one
//! drawer kick) and closes it afterwards: printers often accept one client at a time, and a
//! long-lived socket goes stale silently. Every operation takes a timeout, and every error
//! says how many job bytes had already been handed over ([`TransportError::sent`]), because
//! that decides between "failed, safe to retry" and "uncertain, may be on paper".

pub mod device_file;
pub mod serial;
pub mod spooler;
pub mod tcp;

pub use device_file::DeviceFileTransport;
pub use serial::SerialTransport;
pub use spooler::SpoolerTransport;
pub use tcp::TcpTransport;

use pos_device_ports::DeviceError;
use std::fmt;
use std::time::Duration;
use tokio::time::Instant;

/// How a completed send ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// Every byte was handed to the device (or the spooler saw the job leave its queue).
    Delivered,
    /// The bytes were handed over but the end cannot be observed or looked wrong: the job
    /// may or may not be on paper.
    Unconfirmed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// Could not connect or open: refused, unreachable, no such port, queue or file. Nothing
    /// was sent.
    #[error("not reachable: {0}")]
    Unreachable(String),
    /// The port or device is held by another program. Nothing was sent.
    #[error("in use by another program: {0}")]
    Busy(String),
    /// The operation did not finish in time; `sent` job bytes had been handed over.
    #[error("timed out after {sent} byte(s) were sent")]
    Timeout { sent: usize },
    /// The link failed (reset, unplugged, I/O error) after `sent` bytes.
    #[error("{message} (after {sent} byte(s) were sent)")]
    Io { sent: usize, message: String },
    /// The receiving side refused the job (e.g. the spooler rejected it). Nothing printed.
    #[error("refused: {0}")]
    Rejected(String),
    /// Not possible on this link (e.g. reading from a one-way spooler).
    #[error("not supported: {0}")]
    Unsupported(String),
    /// The settings cannot work (bad baud rate, bad queue name…).
    #[error("invalid settings: {0}")]
    Invalid(String),
}

impl TransportError {
    /// Job bytes handed over before the error. Zero means nothing can have printed.
    pub fn sent(&self) -> usize {
        match self {
            TransportError::Timeout { sent } | TransportError::Io { sent, .. } => *sent,
            _ => 0,
        }
    }

    pub fn to_device_error(&self) -> DeviceError {
        match self {
            TransportError::Unreachable(m) | TransportError::Busy(m) => DeviceError::Disconnected(m.clone()),
            TransportError::Timeout { .. } => DeviceError::Timeout,
            TransportError::Io { message, .. } => DeviceError::Disconnected(message.clone()),
            TransportError::Rejected(m) => DeviceError::Io(m.clone()),
            TransportError::Unsupported(m) | TransportError::Invalid(m) => {
                DeviceError::Unsupported(m.clone())
            }
        }
    }
}

/// One open connection to a device.
///
/// Contract for [`Link::finish`]: an error with `sent() == 0` means nothing was handed over
/// (the spooler refused the job); any other trouble after a complete write is reported as
/// [`SendOutcome::Unconfirmed`] or an error with `sent() > 0`.
#[async_trait::async_trait]
pub trait Link: Send {
    /// A name for the job where the link shows one (the spooler queue).
    fn set_title(&mut self, _title: &str) {}

    /// Writes every byte or fails, within `timeout` for the whole write. Errors carry how
    /// many of these bytes were accepted.
    async fn write_all(&mut self, bytes: &[u8], timeout: Duration) -> Result<(), TransportError>;

    /// Reads at most `max` bytes, waiting up to `timeout` for the first. Empty: nothing came.
    async fn read(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>, TransportError>;

    /// Ends the exchange (half-close and drain, submit to the spooler…) and closes the link.
    async fn finish(self: Box<Self>, timeout: Duration) -> Result<SendOutcome, TransportError>;
}

/// A way to reach a device.
#[async_trait::async_trait]
pub trait Transport: Send + Sync + fmt::Debug {
    /// The `print_jobs.adapter` value for printers on this transport: `escpos_net`,
    /// `escpos_serial`, `escpos_file` or `os_spooler`.
    fn adapter(&self) -> &'static str;

    /// A short description for people and `print_jobs.target`, e.g. `192.168.1.50:9100`.
    fn describe(&self) -> String;

    /// Whether the device can answer over this link. For device files this is known only
    /// after the first open has probed it.
    fn bidirectional(&self) -> bool;

    /// Opens a link, within `timeout`.
    async fn open(&self, timeout: Duration) -> Result<Box<dyn Link>, TransportError>;

    /// Opens, writes `bytes`, and finishes, all within `timeout`.
    async fn send(&self, bytes: &[u8], timeout: Duration) -> Result<SendOutcome, TransportError> {
        let deadline = Instant::now() + timeout;
        let mut link = self.open(timeout).await?;
        link.write_all(bytes, remaining(deadline)).await?;
        link.finish(remaining(deadline)).await
    }

    /// Opens, writes `bytes` and reads up to `reply_len` bytes until `timeout`. Returns what
    /// arrived, which may be shorter than `reply_len`; `Timeout` if nothing came at all.
    async fn request(
        &self,
        bytes: &[u8],
        reply_len: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        if !self.bidirectional() {
            return Err(TransportError::Unsupported(format!(
                "{} cannot send replies",
                self.describe()
            )));
        }
        let deadline = Instant::now() + timeout;
        let mut link = self.open(timeout).await?;
        link.write_all(bytes, remaining(deadline)).await?;
        let reply = read_up_to(link.as_mut(), reply_len, deadline).await?;
        // The exchange is over: closing politely must not hold the caller.
        let _ = link.finish(Duration::from_millis(200)).await;
        if reply.is_empty() {
            return Err(TransportError::Timeout { sent: 0 });
        }
        Ok(reply)
    }
}

/// Time left until `deadline` (zero once passed).
pub(crate) fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Reads until `len` bytes arrived or `deadline` passed; returns what arrived.
pub async fn read_up_to(
    link: &mut dyn Link,
    len: usize,
    deadline: Instant,
) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let left = remaining(deadline);
        if left.is_zero() {
            break;
        }
        let chunk = link.read(len - out.len(), left).await?;
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Reads until `stop` arrives (included) or `deadline` passed; returns what arrived.
pub async fn read_until(
    link: &mut dyn Link,
    stop: u8,
    max: usize,
    deadline: Instant,
) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::new();
    while out.len() < max && !out.contains(&stop) {
        let left = remaining(deadline);
        if left.is_zero() {
            break;
        }
        let chunk = link.read(max - out.len(), left).await?;
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
