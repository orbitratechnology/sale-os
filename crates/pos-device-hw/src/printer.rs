//! `HwPrinter`: an ESC/POS, Star Line Mode or plain-text receipt printer on any
//! [`Transport`], implementing [`ReceiptOutputPort`] and [`CashDrawerPort`].
//!
//! Jobs to one printer run one at a time. A job is one exchange on one link: optional
//! pre-flight status, the job bytes, optional post-job status, then close.
//!
//! **Status queries** (`DLE EOT 1/2/4`, plus `3` on error) are used only when the profile
//! allows them, the dialect is ESC/POS and the link is two-way. Because the printer answers
//! real-time queries in the order it reads its input, an answer to the post-job query also
//! proves that every job byte before it reached the printer.
//!
//! # Outcome rules
//!
//! The rule of thumb: `Failed` only when no job byte can have reached the paper (so a retry
//! cannot print twice); `Uncertain` whenever some may have; `Printed` only when everything
//! was delivered and nothing looked wrong. The print queue never resubmits `Uncertain` or a
//! timed-out submission (RCP-007).
//!
//! | Situation | Result |
//! |---|---|
//! | Connect refused or unreachable, port/file/queue not found, open timed out | `Failed(Disconnected)` |
//! | Port or device in use by another program | `Failed(Disconnected)` |
//! | Pre-flight status: paper out / cover open / offline or error | `Failed(PaperOut / CoverOpen / Offline)`, nothing sent |
//! | Pre-flight status: no answer | status unknown, job sent anyway |
//! | Connection lost during pre-flight | `Failed(Disconnected)`, nothing sent |
//! | Write fails or times out before any job byte was accepted | `Failed(Disconnected)` / `Failed(Offline)` |
//! | Write fails or times out after some job bytes were accepted | `Uncertain` |
//! | All bytes accepted, no status available on this link | `Printed` |
//! | All bytes accepted, post-job status fine | `Printed` |
//! | All bytes accepted, post-job status paper out / cover open / error | `Uncertain` (receipt may be cut short) |
//! | Pre-flight answered but post-job status did not (or the link dropped) | `Uncertain` |
//! | TCP: printer reset the connection while we drained after the job | `Uncertain` |
//! | Spooler refused the job (`lp` error, `StartDocPrinter` failed) | `Failed(Rejected)` or `Failed(Disconnected)` |
//! | Spooler accepted; job left the queue within the confirm bound | `Printed` |
//! | Spooler accepted; job still queued at the bound, or its progress unreadable | `Uncertain` |
//! | `lp` did not finish in time | `Uncertain` |
//! | The whole exchange exceeded the job budget after the job write began | `Uncertain` (before: `Failed(Disconnected)`) |
//!
//! Spooler caveats: CUPS reports cancelled and finished jobs alike once they leave the
//! queue; on Windows the `printers` crate cannot tell "job left the queue" from "queue could
//! not be read". Both count as printed.

use crate::escpos::{self, StatusBytes};
use crate::transport::{Link, SendOutcome, Transport, TransportError, read_until};
use pos_device_ports::{
    CashDrawerPort, CommandDialect, DeviceError, DeviceHealth, DrawerPin, PaperLevel, PrintFailure,
    PrintResult, PrinterCapabilities, PrinterStatus, ReceiptOutputPort,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::Instant;

/// Which level of the drawer sensor (kick connector pin 3, `DLE EOT 1` bit 2) means open.
/// It depends on the drawer's switch; `High` is the common wiring.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum DrawerOpenLevel {
    #[default]
    High,
    Low,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PrinterTimeouts {
    /// Connect / open the port.
    pub connect: Duration,
    /// The whole job write.
    pub write: Duration,
    /// Each pre-flight status query.
    pub status: Duration,
    /// The post-job status query (the printer answers once it has read the job).
    pub post_status: Duration,
    /// Waiting for the printer to close after a job (TCP).
    pub drain: Duration,
    /// Following a spooler job until it leaves the queue.
    pub spool_confirm: Duration,
    /// Everything for one job, after it got its turn. Keep it under the print queue's
    /// timeout so the adapter, not the caller, decides the outcome.
    pub job: Duration,
}

impl Default for PrinterTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(3),
            write: Duration::from_secs(5),
            status: Duration::from_millis(800),
            post_status: Duration::from_secs(4),
            drain: Duration::from_millis(1500),
            spool_confirm: Duration::from_secs(6),
            job: Duration::from_millis(9500),
        }
    }
}

/// What the printer said about itself (`GS I`).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct IdentifiedPrinter {
    /// `GS I 66`, e.g. `EPSON`.
    pub maker: Option<String>,
    /// `GS I 67`, e.g. `TM-T88V`.
    pub model: Option<String>,
    /// `GS I 1`, a vendor-specific model number.
    pub model_id: Option<u8>,
}

#[derive(Debug)]
pub struct HwPrinter {
    transport: Arc<dyn Transport>,
    caps: PrinterCapabilities,
    drawer_pin: DrawerPin,
    drawer_level: DrawerOpenLevel,
    timeouts: PrinterTimeouts,
    profile_id: Option<String>,
    /// One exchange at a time per printer.
    turn: tokio::sync::Mutex<()>,
}

impl HwPrinter {
    pub fn new(transport: Arc<dyn Transport>, caps: PrinterCapabilities) -> Self {
        Self {
            transport,
            caps,
            drawer_pin: DrawerPin::Pin2,
            drawer_level: DrawerOpenLevel::High,
            timeouts: PrinterTimeouts::default(),
            profile_id: None,
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// The pin [`HwPrinter::open_drawer`] uses.
    pub fn with_drawer_pin(mut self, pin: DrawerPin) -> Self {
        self.drawer_pin = pin;
        self
    }

    pub fn with_drawer_open_level(mut self, level: DrawerOpenLevel) -> Self {
        self.drawer_level = level;
        self
    }

    pub fn with_timeouts(mut self, timeouts: PrinterTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    pub fn with_profile_id(mut self, id: impl Into<String>) -> Self {
        self.profile_id = Some(id.into());
        self
    }

    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    pub fn profile_id(&self) -> Option<&str> {
        self.profile_id.as_deref()
    }

    pub fn timeouts(&self) -> PrinterTimeouts {
        self.timeouts
    }

    /// For `print_jobs.target`: `192.168.1.50:9100`, `COM3 @ 9600`, `/dev/usb/lp0`, or the
    /// spooler queue name.
    pub fn target(&self) -> String {
        self.transport.describe()
    }

    /// Opens the drawer on the configured pin.
    pub async fn open_drawer(&self) -> Result<(), DeviceError> {
        CashDrawerPort::open(self, self.drawer_pin).await
    }

    fn can_query(&self) -> bool {
        self.caps.status_query
            && self.caps.dialect == CommandDialect::EscPos
            && self.transport.bidirectional()
    }

    /// One query: the reply byte if it is a valid status byte.
    async fn ask(
        &self,
        link: &mut dyn Link,
        cmd: &[u8],
        timeout: Duration,
    ) -> Result<Option<u8>, TransportError> {
        link.write_all(cmd, timeout).await?;
        let reply = link.read(1, timeout).await?;
        Ok(reply.first().copied().filter(|b| escpos::is_status_byte(*b)))
    }

    /// `DLE EOT 1, 2, 4` (and `3` on error). `None`: the printer did not answer the first.
    async fn query(
        &self,
        link: &mut dyn Link,
        timeout: Duration,
    ) -> Result<Option<StatusBytes>, TransportError> {
        let Some(printer) = self.ask(link, &escpos::DLE_EOT_PRINTER, timeout).await? else {
            return Ok(None);
        };
        let mut bytes = StatusBytes {
            printer: Some(printer),
            ..Default::default()
        };
        bytes.offline = self.ask(link, &escpos::DLE_EOT_OFFLINE, timeout).await?;
        bytes.paper = self.ask(link, &escpos::DLE_EOT_PAPER, timeout).await?;
        if bytes.has_error() {
            bytes.error = self.ask(link, &escpos::DLE_EOT_ERROR, timeout).await?;
        }
        Ok(Some(bytes))
    }

    /// Status over a fresh link: `Ok(None)` when it cannot be asked or got no answer.
    async fn read_status(&self) -> Result<Option<PrinterStatus>, DeviceError> {
        if !self.caps.status_query || self.caps.dialect != CommandDialect::EscPos {
            return Ok(None);
        }
        let _turn = self.turn.lock().await;
        let mut link = self
            .transport
            .open(self.timeouts.connect)
            .await
            .map_err(|e| e.to_device_error())?;
        // Device files learn whether they answer on the first open.
        if !self.transport.bidirectional() {
            return Ok(None);
        }
        let result = self.query(link.as_mut(), self.timeouts.status).await;
        let _ = link.finish(Duration::from_millis(200)).await;
        Ok(result
            .map_err(|e| e.to_device_error())?
            .map(|b| b.to_status(self.drawer_level)))
    }

    /// The printer's state for the status bar.
    pub async fn health(&self) -> DeviceHealth {
        match self.read_status().await {
            Err(e) => DeviceHealth::Offline(e.to_string()),
            Ok(None) => DeviceHealth::Unknown,
            Ok(Some(st)) => match st.blocking_failure() {
                Some(PrintFailure::PaperOut) => DeviceHealth::Offline("out of paper".into()),
                Some(PrintFailure::CoverOpen) => DeviceHealth::Offline("cover open".into()),
                Some(_) => DeviceHealth::Offline(st.error.unwrap_or_else(|| "printer offline".into())),
                None if st.paper == PaperLevel::NearEnd => DeviceHealth::Warning("paper running low".into()),
                None => DeviceHealth::Ready,
            },
        }
    }

    /// Asks the printer who it is (`GS I 1`, `GS I 66`, `GS I 67`), with short timeouts.
    /// `None` when it does not answer (one-way link, not ESC/POS, or no `GS I` support).
    pub async fn identify(&self) -> Option<IdentifiedPrinter> {
        if self.caps.dialect != CommandDialect::EscPos {
            return None;
        }
        let _turn = self.turn.lock().await;
        let mut link = self.transport.open(self.timeouts.connect).await.ok()?;
        if !self.transport.bidirectional() {
            return None;
        }
        let found = identify_on(link.as_mut(), self.timeouts.status).await;
        let _ = link.finish(Duration::from_millis(200)).await;
        found
    }

    async fn run_job(&self, name: &str, bytes: &[u8], started: &AtomicBool) -> PrintResult {
        let mut link = match self.transport.open(self.timeouts.connect).await {
            Ok(l) => l,
            Err(e) => return failed_before_job(&e),
        };
        link.set_title(name);
        let query = self.can_query();
        // The pre-flight answered, so the post-job query is expected to answer too.
        let mut answered = false;
        if query {
            match self.query(link.as_mut(), self.timeouts.status).await {
                Ok(Some(b)) => {
                    answered = true;
                    let st = b.to_status(self.drawer_level);
                    if let Some(f) = st.blocking_failure() {
                        tracing::info!(target: "pos_device_hw", printer = %self.target(), failure = f.code(), "not printing");
                        return PrintResult::Failed(f);
                    }
                }
                Ok(None) => {
                    tracing::debug!(target: "pos_device_hw", printer = %self.target(), "no status answer; sending anyway");
                }
                Err(e) => return failed_before_job(&e),
            }
        }

        started.store(true, Ordering::SeqCst);
        if let Err(e) = link.write_all(bytes, self.timeouts.write).await {
            return after_job_error(&e);
        }

        if answered {
            match self.query(link.as_mut(), self.timeouts.post_status).await {
                Ok(Some(b)) => {
                    let st = b.to_status(self.drawer_level);
                    if let Some(f) = st.blocking_failure() {
                        return PrintResult::Uncertain(format!(
                            "the printer reported {} right after the job: the receipt may be incomplete",
                            f.code().replace('_', " ")
                        ));
                    }
                }
                Ok(None) => {
                    return PrintResult::Uncertain(
                        "the printer stopped answering after the job was sent: it may not have read all of it"
                            .into(),
                    );
                }
                Err(e) => {
                    return PrintResult::Uncertain(format!(
                        "the connection broke after the job was sent: {e}"
                    ));
                }
            }
        }

        match link.finish(self.timeouts.drain).await {
            Ok(SendOutcome::Delivered) => PrintResult::Printed,
            Ok(SendOutcome::Unconfirmed(reason)) => PrintResult::Uncertain(reason),
            Err(e) => after_job_error(&e),
        }
    }
}

/// Reads the three `GS I` answers on an open link.
pub(crate) async fn identify_on(link: &mut dyn Link, timeout: Duration) -> Option<IdentifiedPrinter> {
    let mut found = IdentifiedPrinter::default();
    if link.write_all(&escpos::GS_I_MODEL_ID, timeout).await.is_ok()
        && let Ok(r) = link.read(1, timeout).await
    {
        found.model_id = r.first().copied();
    }
    for (cmd, slot) in [
        (escpos::GS_I_MAKER, &mut found.maker),
        (escpos::GS_I_MODEL_NAME, &mut found.model),
    ] {
        if link.write_all(&cmd, timeout).await.is_err() {
            break;
        }
        let Ok(reply) = read_until(link, 0x00, 64, Instant::now() + timeout).await else {
            break;
        };
        *slot = escpos::parse_gs_i_text(&reply);
    }
    (found != IdentifiedPrinter::default()).then_some(found)
}

/// Nothing of the job was sent.
fn failed_before_job(e: &TransportError) -> PrintResult {
    PrintResult::Failed(match e {
        TransportError::Rejected(m) | TransportError::Unsupported(m) | TransportError::Invalid(m) => {
            PrintFailure::Rejected(m.clone())
        }
        _ => PrintFailure::Disconnected,
    })
}

/// The job write began: only "nothing accepted" is a failure.
fn after_job_error(e: &TransportError) -> PrintResult {
    if e.sent() > 0 {
        return PrintResult::Uncertain(format!("the printer did not confirm the job: {e}"));
    }
    match e {
        TransportError::Timeout { .. } => PrintResult::Failed(PrintFailure::Offline),
        _ => failed_before_job(e),
    }
}

#[async_trait::async_trait]
impl ReceiptOutputPort for HwPrinter {
    fn adapter(&self) -> &'static str {
        self.transport.adapter()
    }

    fn target(&self) -> Option<String> {
        Some(self.transport.describe())
    }

    fn capabilities(&self) -> PrinterCapabilities {
        self.caps.clone()
    }

    async fn submit(&self, job_name: &str, bytes: &[u8]) -> PrintResult {
        let _turn = self.turn.lock().await;
        let started = AtomicBool::new(false);
        let result =
            match tokio::time::timeout(self.timeouts.job, self.run_job(job_name, bytes, &started)).await {
                Ok(r) => r,
                Err(_) if started.load(Ordering::SeqCst) => PrintResult::Uncertain(format!(
                    "no confirmation within {} ms after the job was sent",
                    self.timeouts.job.as_millis()
                )),
                Err(_) => PrintResult::Failed(PrintFailure::Disconnected),
            };
        tracing::info!(
            target: "pos_device_hw",
            adapter = self.transport.adapter(),
            printer = %self.target(),
            job = job_name,
            bytes = bytes.len(),
            result = ?result,
            "print job"
        );
        result
    }

    async fn status(&self) -> Result<PrinterStatus, DeviceError> {
        Ok(self.read_status().await?.unwrap_or_else(PrinterStatus::unknown))
    }
}

#[async_trait::async_trait]
impl CashDrawerPort for HwPrinter {
    async fn open(&self, pin: DrawerPin) -> Result<(), DeviceError> {
        if !self.caps.drawer_kick {
            return Err(DeviceError::Unsupported(
                "this printer model has no drawer port".into(),
            ));
        }
        let kick = escpos::drawer_kick(self.caps.dialect, pin)
            .ok_or_else(|| DeviceError::Unsupported("plain-text printers cannot open a drawer".into()))?;
        let _turn = self.turn.lock().await;
        let mut link = self
            .transport
            .open(self.timeouts.connect)
            .await
            .map_err(|e| e.to_device_error())?;
        link.set_title("drawer");
        link.write_all(&kick, self.timeouts.write)
            .await
            .map_err(|e| e.to_device_error())?;
        match link.finish(self.timeouts.drain).await {
            Ok(_) => Ok(()),
            Err(e) => Err(e.to_device_error()),
        }
    }

    async fn is_open(&self) -> Result<Option<bool>, DeviceError> {
        if !self.caps.drawer_kick {
            return Ok(None);
        }
        Ok(self.read_status().await?.and_then(|s| s.drawer_open))
    }
}
