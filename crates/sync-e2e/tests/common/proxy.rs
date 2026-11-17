//! A fault-injecting HTTP/1.1 proxy between a register and the cloud (E17-T10). It works one
//! request at a time (it reads the request and the server's whole response by
//! Content-Length), so a fault hits exactly one request: refuse it, lose the response after
//! the server applied it, reset the connection halfway through the response body, answer 503
//! itself, delay it, or send it to the server twice.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What happens to one request.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Pass,
    /// The connection closes before the request reaches the server.
    Refuse,
    /// The server gets the request and answers; the answer never reaches the register.
    DropResponse,
    /// Half of the server's answer arrives, then the connection is reset.
    ResetMidBody,
    /// The proxy answers 503 itself.
    Unavailable,
    /// The proxy answers 500 itself.
    ServerError,
    /// Passed on after a pause.
    Delay(u64),
    /// Sent to the server twice; the register gets the second answer.
    Duplicate,
}

/// Which requests get which action: `every`-th request (1-based) gets `fault`; while
/// `offline` is set, every request is refused (flapping).
pub struct Schedule {
    pub fault: Action,
    pub every: usize,
    pub offline: AtomicBool,
    pub seen: AtomicUsize,
    pub faulted: AtomicUsize,
    /// Only requests whose path starts with this are faulted (`""`: all).
    pub only: Mutex<String>,
    /// How many more times the fault may fire (then the network recovers).
    pub budget: AtomicUsize,
}

impl Schedule {
    pub fn new(fault: Action, every: usize) -> Arc<Self> {
        Arc::new(Self {
            fault,
            every: every.max(1),
            offline: AtomicBool::new(false),
            seen: AtomicUsize::new(0),
            faulted: AtomicUsize::new(0),
            only: Mutex::new(String::new()),
            budget: AtomicUsize::new(usize::MAX),
        })
    }

    /// As [`new`](Self::new), but the fault fires at most `times` times.
    pub fn limited(fault: Action, every: usize, times: usize) -> Arc<Self> {
        let s = Self::new(fault, every);
        s.budget.store(times, Ordering::SeqCst);
        s
    }

    fn decide(&self, path: &str) -> Action {
        if self.offline.load(Ordering::SeqCst) {
            self.faulted.fetch_add(1, Ordering::SeqCst);
            return Action::Refuse;
        }
        if !path.starts_with(self.only.lock().unwrap().as_str()) {
            return Action::Pass;
        }
        let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        let due = self.fault != Action::Pass && n.is_multiple_of(self.every);
        // Spend the budget only on requests the fault would hit.
        if due
            && self
                .budget
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |b| b.checked_sub(1))
                .is_ok()
        {
            self.faulted.fetch_add(1, Ordering::SeqCst);
            self.fault
        } else {
            Action::Pass
        }
    }
}

/// Starts the proxy in front of `upstream` (`host:port`); returns its URL.
pub async fn start(upstream: String, schedule: Arc<Schedule>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let (upstream, schedule) = (upstream.clone(), schedule.clone());
            tokio::spawn(async move {
                let _ = serve(client, &upstream, &schedule).await;
            });
        }
    });
    url
}

/// One HTTP message: its bytes, and the request path (requests only).
async fn read_message(s: &mut TcpStream) -> std::io::Result<Option<(Vec<u8>, String)>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let length: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
    Ok(Some((buf, path)))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

async fn upstream_round_trip(upstream: &str, request: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut server = TcpStream::connect(upstream).await?;
    server.write_all(request).await?;
    Ok(read_message(&mut server)
        .await?
        .map(|(b, _)| b)
        .unwrap_or_default())
}

fn canned(status: u16, reason: &str) -> Vec<u8> {
    let body = format!(r#"{{"type":"about:blank","title":"{reason}","status":{status},"code":"INTERNAL"}}"#);
    format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/problem+json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

async fn serve(mut client: TcpStream, upstream: &str, schedule: &Schedule) -> std::io::Result<()> {
    while let Some((request, path)) = read_message(&mut client).await? {
        match schedule.decide(&path) {
            Action::Pass => {
                let response = upstream_round_trip(upstream, &request).await?;
                client.write_all(&response).await?;
            }
            Action::Refuse => return Ok(()),
            Action::DropResponse => {
                let _ = upstream_round_trip(upstream, &request).await?;
                return Ok(());
            }
            Action::ResetMidBody => {
                let response = upstream_round_trip(upstream, &request).await?;
                client.write_all(&response[..response.len() / 2]).await?;
                client.set_zero_linger()?;
                return Ok(());
            }
            Action::Unavailable => client.write_all(&canned(503, "Service Unavailable")).await?,
            Action::ServerError => client.write_all(&canned(500, "Internal Server Error")).await?,
            Action::Delay(ms) => {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let response = upstream_round_trip(upstream, &request).await?;
                client.write_all(&response).await?;
            }
            Action::Duplicate => {
                let _ = upstream_round_trip(upstream, &request).await?;
                let response = upstream_round_trip(upstream, &request).await?;
                client.write_all(&response).await?;
            }
        }
    }
    Ok(())
}
