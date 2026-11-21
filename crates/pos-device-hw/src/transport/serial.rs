//! Serial ports: RS-232, USB virtual COM (CDC-ACM, FTDI, CH340, Prolific…), and Bluetooth
//! SPP, which the OS shows as a COM port or `/dev/rfcomm*`.
//!
//! The `serialport` crate is blocking, so every operation runs on the blocking pool with
//! the port's own timeout set and an outer async timeout as a backstop. The port is opened
//! per exchange and closed afterwards (closing happens off the async threads: a tty with
//! unsent output can take a while to close).

use super::{Link, SendOutcome, Transport, TransportError};
use crate::config::{DataBits, FlowControl, Parity, SerialSettings, StopBits};
use serialport::{ClearBuffer, ErrorKind, SerialPort};
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Extra time the async side waits for a blocking call that has its own timeout.
const GRACE: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialTransport {
    settings: SerialSettings,
}

impl SerialTransport {
    pub fn new(settings: SerialSettings) -> Self {
        Self { settings }
    }

    pub fn settings(&self) -> &SerialSettings {
        &self.settings
    }
}

/// Opens a port with these settings (blocking). Shared with the scanner.
pub(crate) fn open_port(
    s: &SerialSettings,
    timeout: Duration,
) -> Result<Box<dyn SerialPort>, TransportError> {
    let builder = serialport::new(&s.port, s.baud)
        .data_bits(match s.data_bits {
            DataBits::Five => serialport::DataBits::Five,
            DataBits::Six => serialport::DataBits::Six,
            DataBits::Seven => serialport::DataBits::Seven,
            DataBits::Eight => serialport::DataBits::Eight,
        })
        .parity(match s.parity {
            Parity::None => serialport::Parity::None,
            Parity::Odd => serialport::Parity::Odd,
            Parity::Even => serialport::Parity::Even,
        })
        .stop_bits(match s.stop_bits {
            StopBits::One => serialport::StopBits::One,
            StopBits::Two => serialport::StopBits::Two,
        })
        .flow_control(match s.flow {
            FlowControl::None => serialport::FlowControl::None,
            FlowControl::RtsCts => serialport::FlowControl::Hardware,
            FlowControl::XonXoff => serialport::FlowControl::Software,
        })
        .timeout(timeout);
    let mut port = builder.open().map_err(|e| map_open_error(&s.port, &e))?;
    // Line levels are best effort: virtual ports (and some adapters) do not support them.
    if let Some(dtr) = s.dtr
        && let Err(e) = port.write_data_terminal_ready(dtr)
    {
        tracing::debug!(target: "pos_device_hw", port = %s.port, error = %e, "DTR not set");
    }
    if let Some(rts) = s.rts
        && s.flow != FlowControl::RtsCts
        && let Err(e) = port.write_request_to_send(rts)
    {
        tracing::debug!(target: "pos_device_hw", port = %s.port, error = %e, "RTS not set");
    }
    let _ = port.clear(ClearBuffer::Input);
    Ok(port)
}

/// "Port busy" and "port missing" are different problems with different fixes.
pub(crate) fn map_open_error(name: &str, e: &serialport::Error) -> TransportError {
    match e.kind() {
        ErrorKind::NoDevice => TransportError::Busy(format!(
            "{name} is in use by another program or not ready ({})",
            e.description
        )),
        ErrorKind::Io(io::ErrorKind::NotFound) => {
            TransportError::Unreachable(format!("{name} not found: unplugged or wrong port"))
        }
        ErrorKind::Io(io::ErrorKind::PermissionDenied) => TransportError::Unreachable(format!(
            "no permission to open {name} (on Linux, add the user to the dialout group)"
        )),
        ErrorKind::InvalidInput => TransportError::Invalid(format!("{name}: {}", e.description)),
        _ => TransportError::Unreachable(format!("{name}: {}", e.description)),
    }
}

/// Did the port disappear (unplugged), as opposed to a transient error?
pub(crate) fn is_vanished(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::NotFound
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::PermissionDenied
    ) || e.raw_os_error().is_some_and(|code| {
        // EIO / ENXIO / ENODEV on unix; ERROR_BAD_COMMAND / ERROR_ACCESS_DENIED /
        // ERROR_DEVICE_REMOVED style codes on Windows.
        if cfg!(unix) {
            matches!(code, 5 | 6 | 19)
        } else {
            matches!(code, 5 | 22 | 1167)
        }
    })
}

#[async_trait::async_trait]
impl Transport for SerialTransport {
    fn adapter(&self) -> &'static str {
        "escpos_serial"
    }

    fn describe(&self) -> String {
        format!("{} @ {}", self.settings.port, self.settings.baud)
    }

    fn bidirectional(&self) -> bool {
        true
    }

    async fn open(&self, timeout: Duration) -> Result<Box<dyn Link>, TransportError> {
        let settings = self.settings.clone();
        let name = settings.port.clone();
        let task = tokio::task::spawn_blocking(move || open_port(&settings, Duration::from_millis(50)));
        let port = match tokio::time::timeout(timeout + GRACE, task).await {
            Err(_) => {
                return Err(TransportError::Unreachable(format!(
                    "{name} did not open in time"
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
        Ok(Box::new(SerialLink {
            port: Some(port),
            name,
        }))
    }
}

struct SerialLink {
    port: Option<Box<dyn SerialPort>>,
    name: String,
}

impl SerialLink {
    fn take(&mut self) -> Result<Box<dyn SerialPort>, TransportError> {
        self.port.take().ok_or_else(|| TransportError::Io {
            sent: 0,
            message: format!("{}: the link was lost after a timeout", self.name),
        })
    }
}

impl Drop for SerialLink {
    fn drop(&mut self) {
        if let Some(port) = self.port.take() {
            close_off_thread(port);
        }
    }
}

/// Closes on the blocking pool when there is one: a tty close can wait for unsent output.
pub(crate) fn close_off_thread<T: Send + 'static>(resource: T) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || drop(resource));
        }
        Err(_) => drop(resource),
    }
}

#[async_trait::async_trait]
impl Link for SerialLink {
    async fn write_all(&mut self, bytes: &[u8], timeout: Duration) -> Result<(), TransportError> {
        let mut port = self.take()?;
        let data = bytes.to_vec();
        let sent = Arc::new(AtomicUsize::new(0));
        let progress = Arc::clone(&sent);
        let name = self.name.clone();
        let task = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + timeout;
            let result = write_blocking(port.as_mut(), &data, deadline, &progress, &name);
            (port, result)
        });
        match tokio::time::timeout(timeout + GRACE, task).await {
            Err(_) => Err(TransportError::Timeout {
                sent: sent.load(Ordering::SeqCst),
            }),
            Ok(Err(e)) => Err(TransportError::Io {
                sent: sent.load(Ordering::SeqCst),
                message: e.to_string(),
            }),
            Ok(Ok((port, result))) => {
                self.port = Some(port);
                result
            }
        }
    }

    async fn read(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let mut port = self.take()?;
        let name = self.name.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut buf = vec![0; max.max(1)];
            let result = port
                .set_timeout(timeout.max(Duration::from_millis(1)))
                .map_err(|e| io::Error::other(e.description))
                .and_then(|()| port.read(&mut buf));
            let result = match result {
                Ok(n) => {
                    buf.truncate(n);
                    Ok(buf)
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {
                    Ok(Vec::new())
                }
                Err(e) => Err(TransportError::Io {
                    sent: 0,
                    message: describe_io(&name, &e),
                }),
            };
            (port, result)
        });
        match tokio::time::timeout(timeout + GRACE, task).await {
            Err(_) => Ok(Vec::new()),
            Ok(Err(e)) => Err(TransportError::Io {
                sent: 0,
                message: e.to_string(),
            }),
            Ok(Ok((port, result))) => {
                self.port = Some(port);
                result
            }
        }
    }

    async fn finish(mut self: Box<Self>, timeout: Duration) -> Result<SendOutcome, TransportError> {
        // Wait (bounded) until the driver has sent its buffer. Bytes held back by flow
        // control (printer busy, offline or out of paper) may still print later.
        let Some(mut port) = self.port.take() else {
            return Ok(SendOutcome::Delivered);
        };
        let task = tokio::task::spawn_blocking(move || {
            let flushed = port.flush();
            drop(port);
            flushed
        });
        match tokio::time::timeout(timeout.max(Duration::from_millis(100)), task).await {
            Ok(Ok(Ok(()))) => Ok(SendOutcome::Delivered),
            Ok(Ok(Err(e))) if is_vanished(&e) => Ok(SendOutcome::Unconfirmed(format!(
                "{} disappeared before its buffer was sent",
                self.name
            ))),
            // Ports that cannot report draining (some virtual ones): the write succeeded.
            Ok(Ok(Err(_))) | Ok(Err(_)) => Ok(SendOutcome::Delivered),
            Err(_) => Ok(SendOutcome::Unconfirmed(format!(
                "{}: data is still waiting in the serial buffer (printer busy or offline?); it may print later",
                self.name
            ))),
        }
    }
}

fn describe_io(name: &str, e: &io::Error) -> String {
    if is_vanished(e) {
        format!("{name} disappeared (unplugged?): {e}")
    } else {
        format!("{name}: {e}")
    }
}

fn write_blocking(
    port: &mut dyn SerialPort,
    data: &[u8],
    deadline: Instant,
    progress: &AtomicUsize,
    name: &str,
) -> Result<(), TransportError> {
    let mut sent = 0;
    while sent < data.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(TransportError::Timeout { sent });
        }
        if let Err(e) = port.set_timeout(left) {
            return Err(TransportError::Io {
                sent,
                message: format!("{name}: {}", e.description),
            });
        }
        let end = data.len().min(sent + 1024);
        match port.write(&data[sent..end]) {
            Ok(0) => return Err(TransportError::Timeout { sent }),
            Ok(n) => {
                sent += n;
                progress.store(sent, Ordering::SeqCst);
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {
                // Flow control is holding us (printer busy or offline): retry until the deadline.
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                return Err(TransportError::Io {
                    sent,
                    message: describe_io(name, &e),
                });
            }
        }
    }
    Ok(())
}
