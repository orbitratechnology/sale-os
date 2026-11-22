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
//! **Automatic status back** (`GS a`, ASB): with [`HwPrinter::start_status_watch`] the
//! printer pushes its state as it changes, over a status link kept open between jobs, so
//! "out of paper" shows at once instead of at the next poll. A job takes the link over for
//! its turn; ASB blocks that arrive during a job's `DLE EOT` exchange are told apart from
//! the replies by their bit pattern. Printers that never push are polled as before.
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

use crate::escpos::{self, StatusBytes, StatusDemux};
use crate::transport::{Link, SendOutcome, Transport, TransportError, read_until, remaining};
use pos_device_ports::{
    CashDrawerPort, CommandDialect, DeviceError, DeviceHealth, DrawerPin, PaperLevel, PrintFailure,
    PrintResult, PrinterCapabilities, PrinterStatus, PushedStatus, ReceiptOutputPort,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::watch;
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

/// Timings of automatic status back (see [`HwPrinter::start_status_watch`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AsbTimings {
    /// How long to wait for the status block a printer sends when ASB is switched on.
    pub first: Duration,
    /// After this long without a block, ASB is switched on again, which makes a live
    /// printer report at once: the slow liveness check.
    pub liveness: Duration,
    /// The longest a print job waits for the status link to step aside.
    pub slice: Duration,
    /// The longest pause between attempts to reach a printer that cannot be reached.
    pub retry_max: Duration,
}

impl Default for AsbTimings {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(2),
            liveness: Duration::from_secs(60),
            slice: Duration::from_millis(100),
            retry_max: Duration::from_secs(30),
        }
    }
}

/// Where the printer's state comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StatusSource {
    /// Not known yet: the status link is waiting for the printer's first block.
    Probing,
    /// The printer pushes its status (ASB); asking is not needed.
    Pushed,
    /// The printer is asked (`DLE EOT`) when the status is wanted: it does not push, or
    /// its link or profile cannot.
    Polled,
}

const PROBING: u8 = 0;
const PUSHED: u8 = 1;
const POLLED: u8 = 2;

/// The status link between jobs, kept open while the printer pushes its status.
struct IdleLink(Box<dyn Link>);

impl std::fmt::Debug for IdleLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IdleLink")
    }
}

/// How long closing the status link may take before a job opens its own.
const IDLE_CLOSE: Duration = Duration::from_millis(300);

#[derive(Debug)]
pub struct HwPrinter {
    transport: Arc<dyn Transport>,
    caps: PrinterCapabilities,
    drawer_pin: DrawerPin,
    drawer_level: DrawerOpenLevel,
    timeouts: PrinterTimeouts,
    profile_id: Option<String>,
    /// One exchange at a time per printer. Between exchanges it holds the status link, if
    /// one is open; whoever takes the turn closes it first.
    turn: tokio::sync::Mutex<Option<IdleLink>>,
    /// [`StatusSource`] as `PROBING`, `PUSHED` or `POLLED`.
    source: AtomicU8,
    watching: AtomicBool,
    pushed: watch::Sender<PushedStatus>,
    asb: AsbTimings,
    /// Jobs in a row whose status query got no answer. Many ESC/POS clones never answer
    /// `DLE EOT`; after two silent jobs the printer is treated as one-way (no 0.8 s wait per
    /// receipt), and asked again every [`STATUS_REPROBE_EVERY`] jobs in case it was busy.
    silent_jobs: std::sync::atomic::AtomicU32,
}

/// How often a printer that stopped answering status queries is asked again.
const STATUS_REPROBE_EVERY: u32 = 20;

impl HwPrinter {
    pub fn new(transport: Arc<dyn Transport>, caps: PrinterCapabilities) -> Self {
        Self {
            transport,
            caps,
            drawer_pin: DrawerPin::Pin2,
            drawer_level: DrawerOpenLevel::High,
            timeouts: PrinterTimeouts::default(),
            profile_id: None,
            turn: tokio::sync::Mutex::new(None),
            source: AtomicU8::new(PROBING),
            watching: AtomicBool::new(false),
            pushed: watch::channel(PushedStatus::None).0,
            asb: AsbTimings::default(),
            silent_jobs: std::sync::atomic::AtomicU32::new(0),
        }
    }

    pub fn with_asb_timings(mut self, timings: AsbTimings) -> Self {
        self.asb = timings;
        self
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

    /// Whether to ask this job: not while the printer is known to stay silent, except for a
    /// periodic re-check.
    fn should_query(&self) -> bool {
        let silent = self.silent_jobs.load(Ordering::SeqCst);
        self.can_query() && (silent < 2 || silent.is_multiple_of(STATUS_REPROBE_EVERY))
    }

    /// Takes the turn for an exchange, closing the status link if it is open: printers
    /// often serve one connection at a time, and serial ports open once.
    async fn take_turn(&self) -> tokio::sync::MutexGuard<'_, Option<IdleLink>> {
        let mut turn = self.turn.lock().await;
        if let Some(IdleLink(link)) = turn.take() {
            let _ = link.finish(IDLE_CLOSE).await;
        }
        turn
    }

    /// One query: the next `DLE EOT` reply. ASB blocks that arrive meanwhile are told
    /// apart by their bit pattern and published, never taken for the reply.
    async fn ask(
        &self,
        link: &mut dyn Link,
        demux: &mut StatusDemux,
        cmd: &[u8],
        timeout: Duration,
    ) -> Result<Option<u8>, TransportError> {
        link.write_all(cmd, timeout).await?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(reply) = demux.next_reply() {
                return Ok(Some(reply));
            }
            let left = remaining(deadline);
            if left.is_zero() {
                return Ok(None);
            }
            // No answer in time is not a broken link: the printer does not do real-time status.
            let bytes = match link.read(8, left).await {
                Ok(b) => b,
                Err(TransportError::Timeout { .. }) => return Ok(None),
                Err(e) => return Err(e),
            };
            if bytes.is_empty() {
                return Ok(None);
            }
            for block in demux.feed(&bytes) {
                self.on_asb(block);
            }
        }
    }

    /// `DLE EOT 1, 2, 4` (and `3` on error). `None`: the printer did not answer the first.
    async fn query(
        &self,
        link: &mut dyn Link,
        demux: &mut StatusDemux,
        timeout: Duration,
    ) -> Result<Option<StatusBytes>, TransportError> {
        let Some(printer) = self.ask(link, demux, &escpos::DLE_EOT_PRINTER, timeout).await? else {
            return Ok(None);
        };
        let mut bytes = StatusBytes {
            printer: Some(printer),
            ..Default::default()
        };
        bytes.offline = self.ask(link, demux, &escpos::DLE_EOT_OFFLINE, timeout).await?;
        bytes.paper = self.ask(link, demux, &escpos::DLE_EOT_PAPER, timeout).await?;
        if bytes.has_error() {
            bytes.error = self.ask(link, demux, &escpos::DLE_EOT_ERROR, timeout).await?;
        }
        Ok(Some(bytes))
    }

    /// The printer's state: what it last pushed while it pushes, otherwise asked over a
    /// fresh link. `Ok(None)` when it cannot be asked or got no answer.
    async fn read_status(&self) -> Result<Option<PrinterStatus>, DeviceError> {
        if !self.caps.status_query || self.caps.dialect != CommandDialect::EscPos {
            return Ok(None);
        }
        if self.status_source() == StatusSource::Pushed {
            match &*self.pushed.borrow() {
                PushedStatus::Status(st) => return Ok(Some(st.clone())),
                PushedStatus::Lost(why) => return Err(DeviceError::Disconnected(why.clone())),
                PushedStatus::None => {}
            }
        }
        let _turn = self.take_turn().await;
        let mut link = self
            .transport
            .open(self.timeouts.connect)
            .await
            .map_err(|e| e.to_device_error())?;
        // Device files learn whether they answer on the first open.
        if !self.transport.bidirectional() {
            return Ok(None);
        }
        let mut demux = StatusDemux::default();
        let result = self.query(link.as_mut(), &mut demux, self.timeouts.status).await;
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
        let _turn = self.take_turn().await;
        let mut link = self.transport.open(self.timeouts.connect).await.ok()?;
        if !self.transport.bidirectional() {
            return None;
        }
        let found = identify_on(link.as_mut(), self.timeouts.status).await;
        let _ = link.finish(Duration::from_millis(200)).await;
        found
    }

    // ---------- automatic status back (ASB) ----------

    /// Where the printer's state comes from right now.
    pub fn status_source(&self) -> StatusSource {
        match self.source.load(Ordering::SeqCst) {
            PUSHED => StatusSource::Pushed,
            POLLED => StatusSource::Polled,
            _ => StatusSource::Probing,
        }
    }

    fn set_source(&self, source: u8) {
        let before = self.source.swap(source, Ordering::SeqCst);
        if before != source {
            let now = match source {
                PUSHED => "pushed by the printer (automatic status back)",
                POLLED => "asked when needed (DLE EOT)",
                _ => "being probed",
            };
            tracing::info!(target: "pos_device_hw", printer = %self.target(), "printer status is {now}");
        }
    }

    /// Whether this printer and link could push status at all: an ESC/POS printer whose
    /// profile answers status, on a link that is not one-way. (The spooler never is;
    /// device files find out when first opened.)
    fn can_push(&self) -> bool {
        self.caps.status_query && self.caps.dialect == CommandDialect::EscPos
    }

    /// An ASB block arrived (on the status link, or between replies during a job).
    fn on_asb(&self, block: [u8; 4]) {
        let st = escpos::asb_to_status(block, self.drawer_level);
        if self.source.load(Ordering::SeqCst) != POLLED {
            self.set_source(PUSHED);
        }
        self.pushed.send_if_modified(|current| {
            let next = PushedStatus::Status(st);
            if *current == next {
                false
            } else {
                tracing::debug!(target: "pos_device_hw", printer = %self.target(), status = ?next, "status pushed");
                *current = next;
                true
            }
        });
    }

    fn publish_lost(&self, why: String) {
        self.pushed.send_if_modified(|current| {
            let next = PushedStatus::Lost(why);
            if *current == next {
                false
            } else {
                *current = next;
                true
            }
        });
    }

    /// Lets the printer push its status (ESC/POS `GS a`, automatic status back) instead
    /// of being asked every few seconds. A background task keeps a status link open between
    /// jobs, switches ASB on, and publishes every block on [`HwPrinter::status_updates`]
    /// as it arrives. Jobs take the link over when they run (the turn), and the task opens
    /// it again afterwards.
    ///
    /// A printer that sends no block after ASB is switched on, twice, is left to polling
    /// ([`StatusSource::Polled`]), as are profiles without status and one-way links. Every
    /// [`AsbTimings::liveness`] of silence ASB is switched on again, which a live printer
    /// answers at once; one that does not, or cannot be reached, is reported lost.
    ///
    /// Needs a Tokio runtime; without one (or when the printer cannot push) nothing starts.
    pub fn start_status_watch(self: &Arc<Self>) {
        if !self.can_push() || tokio::runtime::Handle::try_current().is_err() {
            self.set_source(POLLED);
            return;
        }
        if self.watching.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(watch_status(weak));
    }

    /// One turn of the status task: (re)open the link if needed, read what arrived, and
    /// decide on silence.
    async fn watch_step(&self, w: &mut Watcher) -> Step {
        let mut turn = self.turn.lock().await;
        if turn.is_none() {
            let mut link = match self.transport.open(self.timeouts.connect).await {
                Ok(l) => l,
                Err(e) => return self.unreachable(w, e.to_string()),
            };
            if !self.transport.bidirectional() {
                // A one-way device file: nothing will ever come back.
                self.set_source(POLLED);
                return Step::Stop;
            }
            if let Err(e) = link
                .write_all(&escpos::gs_a(escpos::ASB_ALL), self.timeouts.write)
                .await
            {
                return self.unreachable(w, e.to_string());
            }
            w.demux = StatusDemux::default();
            w.awaiting = Some(Instant::now() + self.asb.first);
            *turn = Some(IdleLink(link));
        }
        let Some(IdleLink(link)) = turn.as_mut() else {
            return Step::Continue;
        };
        match link.read(64, self.asb.slice).await {
            Ok(bytes) => {
                if let Some(block) = w.demux.feed(&bytes).pop() {
                    w.awaiting = None;
                    w.last_heard = Instant::now();
                    w.quiet = 0;
                    w.failures = 0;
                    self.on_asb(block);
                }
            }
            Err(e) => {
                // The printer closed the link (some drop idle connections) or it vanished.
                *turn = None;
                drop(turn);
                tracing::debug!(target: "pos_device_hw", printer = %self.target(), error = %e, "status link closed");
                if w.awaiting.take().is_some() {
                    return self.quiet(w);
                }
                return Step::Sleep(Duration::from_millis(500));
            }
        }
        let now = Instant::now();
        if w.awaiting.is_some_and(|deadline| now >= deadline) {
            w.awaiting = None;
            if let Some(IdleLink(link)) = turn.take() {
                let _ = link.finish(IDLE_CLOSE).await;
            }
            drop(turn);
            return self.quiet(w);
        }
        if w.awaiting.is_none() && now.saturating_duration_since(w.last_heard) >= self.asb.liveness {
            // The slow liveness check: switching ASB on again makes a live printer report.
            if let Err(e) = link
                .write_all(&escpos::gs_a(escpos::ASB_ALL), self.timeouts.write)
                .await
            {
                *turn = None;
                drop(turn);
                return self.unreachable(w, e.to_string());
            }
            w.awaiting = Some(now + self.asb.first);
        }
        Step::Continue
    }

    /// The link opened but no block came in time.
    fn quiet(&self, w: &mut Watcher) -> Step {
        w.quiet += 1;
        if self.status_source() == StatusSource::Pushed {
            self.publish_lost("the printer stopped reporting its status".into());
            if w.quiet < 5 {
                return Step::Sleep(backoff(w.quiet, self.asb.retry_max));
            }
        } else if w.quiet < 2 {
            return Step::Continue;
        }
        tracing::info!(target: "pos_device_hw", printer = %self.target(), "no automatic status back from this printer");
        self.set_source(POLLED);
        self.pushed.send_replace(PushedStatus::None);
        Step::Stop
    }

    /// The printer could not be reached: lost while it was pushing, then retried.
    fn unreachable(&self, w: &mut Watcher, why: String) -> Step {
        w.failures += 1;
        w.awaiting = None;
        if self.status_source() == StatusSource::Pushed {
            self.publish_lost(why);
        }
        Step::Sleep(backoff(w.failures, self.asb.retry_max))
    }

    async fn run_job(&self, name: &str, bytes: &[u8], started: &AtomicBool) -> PrintResult {
        let mut demux = StatusDemux::default();
        let mut link = match self.transport.open(self.timeouts.connect).await {
            Ok(l) => l,
            Err(e) => return failed_before_job(&e),
        };
        link.set_title(name);
        let query = self.should_query();
        if !query && self.can_query() {
            // Known silent: count the skipped job towards the periodic re-check.
            self.silent_jobs.fetch_add(1, Ordering::SeqCst);
        }
        // The pre-flight answered, so the post-job query is expected to answer too.
        let mut answered = false;
        if query {
            match self.query(link.as_mut(), &mut demux, self.timeouts.status).await {
                Ok(Some(b)) => {
                    answered = true;
                    self.silent_jobs.store(0, Ordering::SeqCst);
                    let st = b.to_status(self.drawer_level);
                    if let Some(f) = st.blocking_failure() {
                        tracing::info!(target: "pos_device_hw", printer = %self.target(), failure = f.code(), "not printing");
                        return PrintResult::Failed(f);
                    }
                }
                Ok(None) => {
                    self.silent_jobs.fetch_add(1, Ordering::SeqCst);
                    tracing::debug!(target: "pos_device_hw", printer = %self.target(), "no status answer; sending anyway");
                }
                // The link died while asking (some printers close an idle connection quickly).
                // No job byte was sent, so a fresh connection without the query is safe.
                Err(e) if e.sent() == 0 => {
                    tracing::debug!(target: "pos_device_hw", printer = %self.target(), error = %e, "link lost during status; reconnecting");
                    self.silent_jobs.fetch_add(1, Ordering::SeqCst);
                    link = match self.transport.open(self.timeouts.connect).await {
                        Ok(l) => l,
                        Err(e) => return failed_before_job(&e),
                    };
                    link.set_title(name);
                }
                Err(e) => return failed_before_job(&e),
            }
        }

        started.store(true, Ordering::SeqCst);
        if let Err(e) = link.write_all(bytes, self.timeouts.write).await {
            return after_job_error(&e);
        }

        if answered {
            match self
                .query(link.as_mut(), &mut demux, self.timeouts.post_status)
                .await
            {
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

/// What the status task carries between turns.
struct Watcher {
    demux: StatusDemux,
    /// ASB was switched on: a block is due by then.
    awaiting: Option<Instant>,
    last_heard: Instant,
    /// Links in a row that opened but brought no block.
    quiet: u32,
    /// Opens in a row that failed.
    failures: u32,
}

enum Step {
    Continue,
    Sleep(Duration),
    Stop,
}

/// 0.5 s, 1 s, 2 s… up to `max`.
fn backoff(attempt: u32, max: Duration) -> Duration {
    let ms = 250u64.saturating_mul(1u64 << attempt.min(16));
    Duration::from_millis(ms).min(max)
}

/// The status task. It holds the printer only weakly, so it ends soon after the printer is
/// dropped (replaced in Settings → Devices), and the status link closes with it.
async fn watch_status(printer: Weak<HwPrinter>) {
    let mut w = Watcher {
        demux: StatusDemux::default(),
        awaiting: None,
        last_heard: Instant::now(),
        quiet: 0,
        failures: 0,
    };
    loop {
        let Some(p) = printer.upgrade() else { return };
        let step = p.watch_step(&mut w).await;
        drop(p);
        match step {
            Step::Continue => tokio::task::yield_now().await,
            Step::Sleep(d) => tokio::time::sleep(d).await,
            Step::Stop => return,
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
        let _turn = self.take_turn().await;
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

    fn pushes_status(&self) -> bool {
        self.status_source() == StatusSource::Pushed
    }

    fn status_updates(&self) -> Option<watch::Receiver<PushedStatus>> {
        self.can_push().then(|| self.pushed.subscribe())
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
        let _turn = self.take_turn().await;
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
