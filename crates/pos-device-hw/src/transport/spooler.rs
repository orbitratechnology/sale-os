//! The OS print spooler, RAW: any printer with a driver (USB printers on Windows, Bluetooth
//! paired printers, parallel, "Generic / Text Only", CUPS raw queues).
//!
//! - **Windows**: winspool through the `printers` crate, datatype `RAW`.
//! - **Unix**: `lp -d <queue> -o raw`, and `lpstat` to follow the job and list queues.
//!
//! The spooler is one-way: no status queries. Once it has accepted a job, the job is
//! followed until it leaves the queue (then `Delivered`), for at most the confirm bound;
//! a job still queued at the bound, or whose fate cannot be observed, is `Unconfirmed`
//! (it may print later, e.g. when the printer comes back online).
//!
//! Known limits: CUPS lists cancelled and aborted jobs the same way as finished ones once
//! they leave the queue, and on Windows the `printers` crate cannot tell "the job left the
//! queue" from "the queue could not be read", so in both cases a job that disappeared is
//! taken as printed.

use super::{Link, SendOutcome, Transport, TransportError};
use std::time::Duration;

/// How long `lp` itself may take to accept the job.
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpoolerTransport {
    queue: String,
    confirm: Duration,
}

impl SpoolerTransport {
    pub fn new(queue: impl Into<String>) -> Self {
        Self {
            queue: queue.into(),
            confirm: Duration::from_secs(6),
        }
    }

    /// How long to follow an accepted job before calling it unconfirmed.
    pub fn with_confirm_timeout(mut self, confirm: Duration) -> Self {
        self.confirm = confirm;
        self
    }

    pub fn queue(&self) -> &str {
        &self.queue
    }
}

/// Spooler queue names: CUPS names have no spaces, `/` or `#`; Windows names may contain
/// spaces and `\\server\printer`. None may start with `-` (it would read as an option).
pub fn check_queue_name(queue: &str) -> Result<(), String> {
    if queue.trim().is_empty() {
        return Err("choose a printer".into());
    }
    if queue.len() > 220 || queue.chars().any(char::is_control) {
        return Err("not a valid printer name".into());
    }
    if queue.starts_with('-') {
        return Err("a printer name cannot start with -".into());
    }
    if cfg!(unix) && queue.chars().any(|c| c.is_whitespace() || c == '/' || c == '#') {
        return Err("a CUPS queue name cannot contain spaces, / or #".into());
    }
    Ok(())
}

/// A job title safe to pass as an argument: printable, at most 64 characters.
pub fn job_title(name: &str) -> String {
    let t: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    if t.trim().is_empty() { "receipt".into() } else { t }
}

/// The `lp` invocation for a raw job (program and arguments; no shell is involved). With no
/// file argument `lp` reads the job from standard input.
pub fn lp_command(queue: &str, title: &str) -> (&'static str, Vec<String>) {
    (
        "lp",
        vec![
            "-d".into(),
            queue.into(),
            "-o".into(),
            "raw".into(),
            "-t".into(),
            job_title(title),
        ],
    )
}

/// The job id in `lp`'s answer: `request id is EPSON-42 (1 file(s))` → `EPSON-42`.
pub fn parse_lp_job_id(stdout: &str) -> Option<String> {
    let rest = stdout.split("request id is ").nth(1)?;
    let id = rest.split_whitespace().next()?;
    (!id.is_empty()).then(|| id.to_owned())
}

/// Whether `lpstat -o <queue>` output still lists `job_id`.
pub fn lpstat_lists_job(lpstat_out: &str, job_id: &str) -> bool {
    lpstat_out
        .lines()
        .any(|l| l.split_whitespace().next() == Some(job_id))
}

/// Queue names from `lpstat -e` (one per line).
pub fn parse_lpstat_e(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && check_queue_name(l).is_ok())
        .map(str::to_owned)
        .collect()
}

/// Lists spooler queues, within `timeout`. Empty when there is no spooler.
pub async fn list_queues(timeout: Duration) -> Vec<String> {
    let task = tokio::task::spawn_blocking(move || platform::list_queues(timeout));
    match tokio::time::timeout(timeout + Duration::from_millis(500), task).await {
        Ok(Ok(q)) => q,
        _ => Vec::new(),
    }
}

#[async_trait::async_trait]
impl Transport for SpoolerTransport {
    fn adapter(&self) -> &'static str {
        "os_spooler"
    }

    fn describe(&self) -> String {
        self.queue.clone()
    }

    fn bidirectional(&self) -> bool {
        false
    }

    async fn open(&self, _timeout: Duration) -> Result<Box<dyn Link>, TransportError> {
        check_queue_name(&self.queue).map_err(TransportError::Invalid)?;
        Ok(Box::new(SpoolLink {
            queue: self.queue.clone(),
            confirm: self.confirm,
            title: "receipt".into(),
            data: Vec::new(),
        }))
    }
}

struct SpoolLink {
    queue: String,
    confirm: Duration,
    title: String,
    data: Vec<u8>,
}

#[async_trait::async_trait]
impl Link for SpoolLink {
    fn set_title(&mut self, title: &str) {
        self.title = job_title(title);
    }

    async fn write_all(&mut self, bytes: &[u8], _timeout: Duration) -> Result<(), TransportError> {
        // Nothing leaves the till until finish() hands the whole job to the spooler.
        self.data.extend_from_slice(bytes);
        Ok(())
    }

    async fn read(&mut self, _max: usize, _timeout: Duration) -> Result<Vec<u8>, TransportError> {
        Err(TransportError::Unsupported("the spooler is one-way".into()))
    }

    async fn finish(self: Box<Self>, _timeout: Duration) -> Result<SendOutcome, TransportError> {
        if self.data.is_empty() {
            return Ok(SendOutcome::Delivered);
        }
        let len = self.data.len();
        let bound = SUBMIT_TIMEOUT + self.confirm + Duration::from_secs(1);
        let SpoolLink {
            queue,
            confirm,
            title,
            data,
        } = *self;
        let task = tokio::task::spawn_blocking(move || platform::submit(&queue, &title, &data, confirm));
        match tokio::time::timeout(bound, task).await {
            Err(_) => Err(TransportError::Timeout { sent: len }),
            Ok(Err(e)) => Err(TransportError::Io {
                sent: len,
                message: e.to_string(),
            }),
            Ok(Ok(r)) => r,
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::Instant;

    pub(super) enum RunError {
        NotInstalled,
        Timeout,
        Io(String),
    }

    pub(super) struct Output {
        pub success: bool,
        pub stdout: String,
        pub stderr: String,
    }

    /// Runs a program with a hard timeout (killed when it passes).
    pub(super) fn run(
        program: &str,
        args: &[String],
        input: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<Output, RunError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    RunError::NotInstalled
                } else {
                    RunError::Io(e.to_string())
                }
            })?;
        let writer = match (input, child.stdin.take()) {
            (Some(data), Some(mut stdin)) => Some(std::thread::spawn(move || {
                let _ = stdin.write_all(&data);
            })),
            _ => None,
        };
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::Timeout);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(RunError::Io(e.to_string())),
            }
        };
        if let Some(w) = writer {
            let _ = w.join();
        }
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut o) = child.stdout.take() {
            let _ = o.read_to_string(&mut stdout);
        }
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        Ok(Output {
            success: status.success(),
            stdout,
            stderr,
        })
    }

    pub(super) fn submit(
        queue: &str,
        title: &str,
        data: &[u8],
        confirm: Duration,
    ) -> Result<SendOutcome, TransportError> {
        let (program, args) = lp_command(queue, title);
        let out = match run(program, &args, Some(data.to_vec()), SUBMIT_TIMEOUT) {
            Err(RunError::NotInstalled) => {
                return Err(TransportError::Unreachable(
                    "the lp command is not installed (CUPS client)".into(),
                ));
            }
            // lp may have queued the job before it was stopped.
            Err(RunError::Timeout) => return Err(TransportError::Timeout { sent: data.len() }),
            Err(RunError::Io(m)) => {
                return Err(TransportError::Unreachable(format!("could not run lp: {m}")));
            }
            Ok(o) => o,
        };
        if !out.success {
            let msg = out.stderr.trim().to_owned();
            return Err(
                if msg.contains("not exist") || msg.contains("Unknown destination") {
                    TransportError::Unreachable(format!("no print queue named {queue}: {msg}"))
                } else {
                    TransportError::Rejected(format!("lp refused the job: {msg}"))
                },
            );
        }
        let Some(job) = parse_lp_job_id(&out.stdout) else {
            return Ok(SendOutcome::Unconfirmed(
                "the spooler accepted the job but gave no job id to follow".into(),
            ));
        };
        let deadline = Instant::now() + confirm;
        loop {
            let args = vec!["-o".to_owned(), queue.to_owned()];
            match run("lpstat", &args, None, Duration::from_secs(2)) {
                Ok(o) if o.success && !lpstat_lists_job(&o.stdout, &job) => {
                    return Ok(SendOutcome::Delivered);
                }
                Ok(o) if o.success => {}
                _ => {
                    return Ok(SendOutcome::Unconfirmed(format!(
                        "spooler job {job} was accepted but its progress cannot be read"
                    )));
                }
            }
            if Instant::now() >= deadline {
                return Ok(SendOutcome::Unconfirmed(format!(
                    "spooler job {job} is still queued (printer offline or busy): it may print later"
                )));
            }
            std::thread::sleep(POLL);
        }
    }

    pub(super) fn list_queues(timeout: Duration) -> Vec<String> {
        match run("lpstat", &["-e".to_owned()], None, timeout) {
            Ok(o) if o.success => parse_lpstat_e(&o.stdout),
            _ => Vec::new(),
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use printers::common::base::job::{PrinterJobOptions, PrinterJobState};
    use printers::common::converters::Converter;
    use std::time::Instant;

    pub(super) fn submit(
        queue: &str,
        title: &str,
        data: &[u8],
        confirm: Duration,
    ) -> Result<SendOutcome, TransportError> {
        let Some(printer) = printers::get_printer_by_name(queue) else {
            return Err(TransportError::Unreachable(format!(
                "no printer named {queue} in Windows"
            )));
        };
        let options = PrinterJobOptions {
            name: Some(title),
            raw_properties: &[("document-format", "RAW")],
            converter: Converter::None,
        };
        // print() fails before StartDocPrinter succeeds, so an error means nothing queued.
        let job = printer
            .print(data, options)
            .map_err(|e| TransportError::Rejected(format!("the spooler refused the job: {}", e.message)))?;
        let deadline = Instant::now() + confirm;
        loop {
            match printer.get_job_history().into_iter().find(|j| j.id == job) {
                None => return Ok(SendOutcome::Delivered),
                Some(j) if j.state == PrinterJobState::CANCELLED => {
                    return Ok(SendOutcome::Unconfirmed(format!(
                        "spooler job {job} was cancelled or deleted while printing"
                    )));
                }
                Some(j) if j.state == PrinterJobState::COMPLETED => return Ok(SendOutcome::Delivered),
                Some(j) => {
                    if Instant::now() >= deadline {
                        return Ok(SendOutcome::Unconfirmed(format!(
                            "spooler job {job} is still in the queue ({:?}): it may print later",
                            j.state
                        )));
                    }
                }
            }
            std::thread::sleep(POLL);
        }
    }

    pub(super) fn list_queues(_timeout: Duration) -> Vec<String> {
        printers::get_printers().into_iter().map(|p| p.name).collect()
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;

    pub(super) fn submit(_: &str, _: &str, _: &[u8], _: Duration) -> Result<SendOutcome, TransportError> {
        Err(TransportError::Unsupported(
            "no spooler support on this platform".into(),
        ))
    }

    pub(super) fn list_queues(_: Duration) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lp_invocation_is_raw_and_shell_free() {
        let (prog, args) = lp_command("EPSON_TM_T20", "R01-000042\n");
        assert_eq!(prog, "lp");
        assert_eq!(args, ["-d", "EPSON_TM_T20", "-o", "raw", "-t", "R01-000042"]);
        assert_eq!(job_title("\u{1}\u{2}"), "receipt");
        assert_eq!(job_title(&"x".repeat(100)).len(), 64);
    }

    #[test]
    fn lp_and_lpstat_parsing() {
        assert_eq!(
            parse_lp_job_id("request id is EPSON-42 (1 file(s))\n").as_deref(),
            Some("EPSON-42")
        );
        assert_eq!(parse_lp_job_id("nothing"), None);
        let out = "EPSON-41  shop  1024  Fri 20 Nov 2026\nEPSON-42  shop  2048  Fri 20 Nov 2026\n";
        assert!(lpstat_lists_job(out, "EPSON-42"));
        assert!(!lpstat_lists_job(out, "EPSON-4"));
        assert_eq!(parse_lpstat_e("EPSON\nStar_TSP\n\n-bad\n"), ["EPSON", "Star_TSP"]);
    }

    #[test]
    fn queue_names() {
        assert!(check_queue_name("EPSON_TM-T20").is_ok());
        assert!(check_queue_name("").is_err());
        assert!(check_queue_name("-o").is_err());
        assert!(check_queue_name("a\nb").is_err());
        if cfg!(unix) {
            assert!(check_queue_name("has space").is_err());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_lp_when_installed() {
        let has_lp = std::process::Command::new("lp").arg("-h").output().is_ok();
        let transport = SpoolerTransport::new("pos_device_hw_no_such_queue");
        let r = transport.send(b"hello", Duration::from_secs(10)).await;
        if has_lp {
            // A missing queue is refused before anything is queued.
            assert!(matches!(r, Err(ref e) if e.sent() == 0), "{r:?}");
        } else {
            assert!(matches!(r, Err(TransportError::Unreachable(_))), "{r:?}");
        }
        let _ = list_queues(Duration::from_secs(2)).await;
    }
}
