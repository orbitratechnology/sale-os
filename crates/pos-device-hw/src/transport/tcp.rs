//! Raw TCP ("port 9100", "RAW", "AppSocket"): the Ethernet and Wi-Fi interfaces of Epson,
//! Star, Bixolon, Citizen, Xprinter and most other receipt printers.
//!
//! One connection per exchange, never kept: many interfaces serve one client at a time and
//! drop idle sockets silently. After a job the write side is half-closed and the socket is
//! drained until the printer closes its side, so a printer that resets the connection
//! with job bytes still unread is noticed.

use super::{Link, SendOutcome, Transport, TransportError, remaining};
use std::io::ErrorKind;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;

pub const DEFAULT_PORT: u16 = 9100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcpTransport {
    host: String,
    port: u16,
}

impl TcpTransport {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    fn addr(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port) // IPv6 literal
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[async_trait::async_trait]
impl Transport for TcpTransport {
    fn adapter(&self) -> &'static str {
        "escpos_net"
    }

    fn describe(&self) -> String {
        self.addr()
    }

    fn bidirectional(&self) -> bool {
        true
    }

    async fn open(&self, timeout: Duration) -> Result<Box<dyn Link>, TransportError> {
        let addr = self.addr();
        let stream = match tokio::time::timeout(timeout, connect_bounded(self.host.as_str(), self.port)).await
        {
            Err(_) => {
                return Err(TransportError::Unreachable(format!(
                    "{addr} did not answer within {} ms",
                    timeout.as_millis()
                )));
            }
            Ok(Err(e)) => return Err(TransportError::Unreachable(format!("{addr}: {e}"))),
            Ok(Ok(s)) => s,
        };
        let _ = stream.set_nodelay(true);
        tracing::debug!(target: "pos_device_hw", %addr, "connected");
        Ok(Box::new(TcpLink { stream, addr }))
    }
}

/// The OS send buffer for a printer link. A fixed size matters on Windows: without one,
/// dynamic send buffering lets the OS accept megabytes for a printer that reads nothing,
/// and the job would look sent. Bounded, a finished write means the job is at most this far
/// from the printer, and one that is never read times out (uncertain) on every platform.
/// Receipts are a few kilobytes; even a raster job streams fine through 64 KiB.
const SEND_BUFFER: u32 = 64 * 1024;

async fn connect_bounded(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut last = None;
    for addr in tokio::net::lookup_host((host, port)).await? {
        let socket = if addr.is_ipv4() {
            tokio::net::TcpSocket::new_v4()?
        } else {
            tokio::net::TcpSocket::new_v6()?
        };
        socket.set_send_buffer_size(SEND_BUFFER)?;
        match socket.connect(addr).await {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "the host name has no address")))
}

struct TcpLink {
    stream: TcpStream,
    addr: String,
}

#[async_trait::async_trait]
impl Link for TcpLink {
    async fn write_all(&mut self, bytes: &[u8], timeout: Duration) -> Result<(), TransportError> {
        let deadline = Instant::now() + timeout;
        let mut sent = 0;
        while sent < bytes.len() {
            match tokio::time::timeout_at(deadline, self.stream.write(&bytes[sent..])).await {
                Err(_) => return Err(TransportError::Timeout { sent }),
                Ok(Ok(0)) => {
                    return Err(TransportError::Io {
                        sent,
                        message: format!("{} closed the connection", self.addr),
                    });
                }
                Ok(Ok(n)) => sent += n,
                Ok(Err(e)) => {
                    return Err(TransportError::Io {
                        sent,
                        message: format!("{}: {e}", self.addr),
                    });
                }
            }
        }
        Ok(())
    }

    async fn read(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let mut buf = vec![0; max.max(1)];
        match tokio::time::timeout(timeout, self.stream.read(&mut buf)).await {
            Err(_) => Ok(Vec::new()),
            Ok(Ok(0)) => Err(TransportError::Io {
                sent: 0,
                message: format!("{} closed the connection", self.addr),
            }),
            Ok(Ok(n)) => {
                buf.truncate(n);
                Ok(buf)
            }
            Ok(Err(e)) => Err(TransportError::Io {
                sent: 0,
                message: format!("{}: {e}", self.addr),
            }),
        }
    }

    async fn finish(mut self: Box<Self>, timeout: Duration) -> Result<SendOutcome, TransportError> {
        let deadline = Instant::now() + timeout;
        // Half-close: the printer sees end-of-data after the last job byte.
        match tokio::time::timeout_at(deadline, self.stream.shutdown()).await {
            Ok(Err(e)) if is_reset(&e) => {
                return Ok(SendOutcome::Unconfirmed(format!(
                    "{} reset the connection at the end of the job: part of it may be lost",
                    self.addr
                )));
            }
            _ => {}
        }
        // Drain until the printer closes its side. A printer that keeps the socket open is
        // fine (every byte was accepted); a reset means it dropped data it had not read.
        let mut sink = [0u8; 256];
        loop {
            match tokio::time::timeout(remaining(deadline), self.stream.read(&mut sink)).await {
                Err(_) | Ok(Ok(0)) => return Ok(SendOutcome::Delivered),
                Ok(Ok(_)) => {}
                Ok(Err(e)) if is_reset(&e) => {
                    return Ok(SendOutcome::Unconfirmed(format!(
                        "{} reset the connection before reading the whole job",
                        self.addr
                    )));
                }
                Ok(Err(e)) => {
                    tracing::debug!(target: "pos_device_hw", addr = %self.addr, error = %e, "drain");
                    return Ok(SendOutcome::Delivered);
                }
            }
        }
    }
}

fn is_reset(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe
    )
}
