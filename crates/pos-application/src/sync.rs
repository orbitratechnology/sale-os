//! The push side of sync (E17-T03, SYN-004/006/007, ADR-010): the transport and store ports
//! and the [`SyncWorker`] that drains the outbox.
//!
//! One call to [`SyncWorker::push_once`] sends the next batch (up to 200 events / 2 MB, in
//! `local_sequence` order from `last_ack_sequence + 1`), records the server's per-event
//! results and moves the local ack to the server's `ackSequence`, never past it. It returns
//! what to do next; the desktop loop sleeps, waits for a trigger or goes again accordingly.
//! Pushing only reads the outbox and writes sync bookkeeping, so it never holds up a sale.

use crate::queries::QueryError;
use pos_domain::{Clock, Timestamp};
use pos_sync::envelope::{MAX_PUSH_BYTES, MAX_PUSH_EVENTS};
use pos_sync::pull::{BootstrapPage, ChangesResponse};
use pos_sync::{EventEnvelope, EventResult, PushRequest, PushResponse, ResultStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// How the cloud could not be reached, or refused the request as a whole.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("offline")]
    Offline,
    #[error("timed out")]
    Timeout,
    /// The access token was refused: refresh it and try again.
    #[error("unauthorized")]
    Unauthorized,
    /// The owner disconnected this register (ADR-013).
    #[error("device revoked")]
    DeviceRevoked,
    #[error("upgrade required (minimum {min_version})")]
    UpgradeRequired { min_version: String },
    #[error("server error {status}")]
    Server { status: u16, code: Option<String> },
    #[error("unreadable response: {0}")]
    Decode(String),
}

impl TransportError {
    /// Worth retrying with backoff.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Offline | Self::Timeout | Self::Unauthorized | Self::Decode(_) => true,
            Self::Server { status, .. } => *status >= 500 || *status == 429,
            Self::DeviceRevoked | Self::UpgradeRequired { .. } => false,
        }
    }

    /// The code stored in `sync_state.last_error_code`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Offline => "OFFLINE",
            Self::Timeout => "TIMEOUT",
            Self::Unauthorized => "AUTH_REQUIRED",
            Self::DeviceRevoked => "SYNC_DEVICE_REVOKED",
            Self::UpgradeRequired { .. } => "SYNC_UPGRADE_REQUIRED",
            Self::Server { .. } => "SERVER_ERROR",
            Self::Decode(_) => "DECODE_ERROR",
        }
    }
}

/// A transport failure, as the link wizard reports it.
pub fn link_error(e: &TransportError) -> crate::link::LinkError {
    use crate::link::LinkError;
    match e {
        TransportError::Offline | TransportError::Timeout | TransportError::Server { .. } => {
            LinkError::Offline
        }
        TransportError::UpgradeRequired { .. } => LinkError::UpgradeRequired,
        TransportError::Unauthorized | TransportError::DeviceRevoked => {
            LinkError::Other("the cloud refused this register's credentials".into())
        }
        TransportError::Decode(d) => LinkError::Other(format!("unreadable reply from the cloud: {d}")),
    }
}

/// The cloud API, as the worker needs it (the HTTP adapter arrives with linking, E17-T08).
#[async_trait::async_trait]
pub trait SyncTransport: Send + Sync {
    async fn push(&self, request: PushRequest) -> Result<PushResponse, TransportError>;

    /// `GET /v1/sync/changes` (E17-T07). Test doubles that only push need not answer.
    async fn changes(&self, cursor: Option<String>, limit: u32) -> Result<ChangesResponse, TransportError> {
        let _ = (cursor, limit);
        Err(TransportError::Offline)
    }

    /// `GET /v1/sync/bootstrap` (E17-T07).
    async fn bootstrap(&self, page: Option<String>) -> Result<BootstrapPage, TransportError> {
        let _ = page;
        Err(TransportError::Offline)
    }
}

/// Where the local side stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushState {
    pub linked: bool,
    pub register_id: uuid::Uuid,
    pub last_ack_sequence: u64,
    /// The highest sequence in the outbox (0 when empty).
    pub max_sequence: u64,
}

/// What one push did, for the store to record in one transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct PushRecord {
    /// The server's ack, clamped to the outbox (the server cannot have more than we sent).
    pub ack_sequence: u64,
    pub results: Vec<EventResult>,
    pub at: Timestamp,
    /// Server clock minus ours, from the response's `serverTime` (ADR-006).
    pub clock_skew_ms: Option<i64>,
    /// A code that blocks progress (a transient result or an unacknowledged rejection), or
    /// `None` when the push went through.
    pub blocking_code: Option<String>,
}

/// The outbox and sync bookkeeping (implemented by `pos-local-db`).
#[async_trait::async_trait]
pub trait SyncStore: Send + Sync {
    async fn push_state(&self) -> Result<PushState, QueryError>;
    /// Events after `after`, in order, within the limits (always at least one if any).
    async fn outbox_batch(
        &self,
        after: u64,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<Vec<EventEnvelope>, QueryError>;
    /// Records results and the ack in one transaction: outbox statuses, conflicts, the ack,
    /// the clock skew and the last error.
    async fn record_push(&self, record: &PushRecord) -> Result<(), QueryError>;
    /// Records a failed attempt (nothing else changes).
    async fn record_push_failure(&self, code: &str, at: Timestamp) -> Result<(), QueryError>;

    /// Where the pull stands (E17-T07).
    async fn pull_state(&self) -> Result<PullState, QueryError>;
    /// Applies a page of the change feed and moves the cursor, in one transaction. Changes
    /// already applied (by `serverSeq`) are skipped, so a page can be applied twice.
    async fn apply_changes(&self, page: &ChangesResponse, at: Timestamp)
    -> Result<InboundReport, QueryError>;
    /// Applies a bootstrap page and records where to resume (or, after the last page, the
    /// cursor to pull from), in one transaction.
    async fn apply_bootstrap(&self, page: &BootstrapPage, at: Timestamp)
    -> Result<InboundReport, QueryError>;
}

/// Where the pull stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullState {
    pub linked: bool,
    /// `None` before the first pull.
    pub cursor: Option<String>,
    /// Set while a bootstrap is under way: the next page to fetch.
    pub bootstrap_page: Option<String>,
}

/// What a pulled page did (`inbound_changes.outcome`, docs/backlog/04 0013).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InboundReport {
    pub applied: u32,
    /// Not newer than what this register holds.
    pub skipped_older: u32,
    /// Made by this register.
    pub skipped_own: u32,
    /// The aggregate has an edit not yet acknowledged here: the push result settles it.
    pub held: u32,
    /// Already applied by an earlier pull.
    pub already: u32,
}

impl InboundReport {
    pub fn add(&mut self, o: InboundOutcome) {
        match o {
            InboundOutcome::Applied => self.applied += 1,
            InboundOutcome::SkippedOlder => self.skipped_older += 1,
            InboundOutcome::SkippedOwn => self.skipped_own += 1,
            InboundOutcome::ConflictPending => self.held += 1,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InboundOutcome {
    Applied,
    SkippedOlder,
    SkippedOwn,
    ConflictPending,
}

impl InboundOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::SkippedOlder => "skipped_older",
            Self::SkippedOwn => "skipped_own",
            Self::ConflictPending => "conflict_pending",
        }
    }
}

/// Why a pulled snapshot cannot be applied here.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InboundError(pub String);

/// The local table and id column of a master aggregate type.
pub fn master_table(aggregate_type: &str) -> Option<(&'static str, &'static str)> {
    Some(match aggregate_type {
        "business" => ("businesses", "business_id"),
        "location" => ("locations", "location_id"),
        "register" => ("registers", "register_id"),
        "setting" => ("business_settings", "key"),
        "staff_user" => ("staff_users", "user_id"),
        "tax_rate" => ("tax_rates", "tax_rate_id"),
        "tax_category" => ("tax_categories", "tax_category_id"),
        "category" => ("categories", "category_id"),
        "product" => ("products", "product_id"),
        _ => return None,
    })
}

/// A pulled master-data snapshot as the local write that applies it: the same `Put*` op a
/// local edit uses, expecting the version held here (`None` if new), so a local edit made in
/// the meantime makes it fail instead of being overwritten. The snapshot is checked against
/// the contract first; it must belong to `business`.
pub fn inbound_master(
    event_type: &str,
    schema_version: u32,
    snapshot: &serde_json::Value,
    local_version: Option<u64>,
    business: pos_domain::BusinessId,
) -> Result<crate::WriteOp, InboundError> {
    use crate::WriteOp;
    let bad = |e: &dyn std::fmt::Display| InboundError(format!("{event_type}: {e}"));
    pos_sync::parse_payload(event_type, schema_version, snapshot).map_err(|e| bad(&e))?;
    let (_, value) = pos_sync::upcast::to_current(event_type, schema_version, snapshot.clone());
    fn de<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, serde_json::Error> {
        serde_json::from_value(v)
    }
    let expected_version = local_version;
    let (op, owner) = match event_type {
        "business.updated" => {
            let record: crate::records::BusinessRecord = de(value).map_err(|e| bad(&e))?;
            let owner = record.business_id;
            (
                WriteOp::PutBusiness {
                    record,
                    expected_version,
                },
                owner,
            )
        }
        "location.upserted" => {
            let record: crate::records::LocationRecord = de(value).map_err(|e| bad(&e))?;
            let owner = record.business_id;
            (
                WriteOp::PutLocation {
                    record,
                    expected_version,
                },
                owner,
            )
        }
        "register.upserted" => {
            let record: crate::records::RegisterRecord = de(value).map_err(|e| bad(&e))?;
            let owner = record.business_id;
            (
                WriteOp::PutRegister {
                    record,
                    expected_version,
                },
                owner,
            )
        }
        "setting.updated" => {
            let record: crate::records::SettingRecord = de(value).map_err(|e| bad(&e))?;
            let owner = record.business_id;
            (
                WriteOp::PutSetting {
                    record,
                    expected_version,
                },
                owner,
            )
        }
        "staff.upserted" => {
            let record: crate::records::StaffRecord = de(value).map_err(|e| bad(&e))?;
            let owner = record.business_id;
            (
                WriteOp::PutStaff {
                    record,
                    expected_version,
                },
                owner,
            )
        }
        "tax_rate.upserted" => {
            let record: crate::records::TaxRateRecord = de(value).map_err(|e| bad(&e))?;
            let owner = record.business_id;
            (
                WriteOp::PutTaxRate {
                    record,
                    expected_version,
                },
                owner,
            )
        }
        "tax_category.upserted" => {
            let record: crate::records::TaxCategoryRecord = de(value).map_err(|e| bad(&e))?;
            let owner = record.business_id;
            (
                WriteOp::PutTaxCategory {
                    record,
                    expected_version,
                },
                owner,
            )
        }
        "category.upserted" => {
            let record: crate::records::CategoryRecord = de(value).map_err(|e| bad(&e))?;
            let owner = record.business_id;
            (
                WriteOp::PutCategory {
                    record,
                    expected_version,
                },
                owner,
            )
        }
        "product.upserted" => {
            let product: pos_domain::catalog::Product = de(value).map_err(|e| bad(&e))?;
            (
                WriteOp::PutProduct {
                    product,
                    business_id: business,
                    expected_version,
                },
                business,
            )
        }
        other => return Err(InboundError(format!("{other} is not master data"))),
    };
    if owner != business {
        return Err(InboundError(format!("{event_type} is for another business")));
    }
    Ok(op)
}

/// The stock movements a pulled fact carries (a sale, a return or a stock document from
/// another register); other facts move no stock.
pub fn inbound_movements(
    event_type: &str,
    schema_version: u32,
    snapshot: &serde_json::Value,
) -> Result<Vec<pos_sync::events::StockMovementV1>, InboundError> {
    use pos_sync::Payload;
    let payload = pos_sync::parse_payload(event_type, schema_version, snapshot)
        .map_err(|e| InboundError(format!("{event_type}: {e}")))?;
    Ok(match payload {
        Payload::SaleCompletedV1(s) => s.stock_movements,
        Payload::ReturnCompletedV1(r) => r.stock_movements,
        Payload::DocumentPostedV1(d) => d.stock_movements,
        _ => Vec::new(),
    })
}

/// Full-jitter exponential backoff: attempt `n` (0-based) sleeps a random time in
/// `[0, min(cap, base·2ⁿ)]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub base: Duration,
    pub cap: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(2),
            cap: Duration::from_secs(300),
        }
    }
}

impl Backoff {
    /// The upper bound for attempt `n`.
    pub fn bound(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt.min(20)).unwrap_or(u32::MAX);
        self.base.saturating_mul(factor).min(self.cap)
    }

    /// The delay for attempt `n`, with `random` drawing uniformly from `0..=max`.
    pub fn delay(&self, attempt: u32, random: &dyn Random) -> Duration {
        let max = u64::try_from(self.bound(attempt).as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(random.up_to(max))
    }
}

/// Randomness for jitter (a fake in tests).
pub trait Random: Send + Sync {
    /// Uniform in `0..=max`.
    fn up_to(&self, max: u64) -> u64;
}

/// Clock skew beyond this sets `clockSkewWarning` (ADR-006).
pub const CLOCK_SKEW_WARNING_MS: i64 = 10 * 60 * 1000;

pub fn clock_skew_warning(skew_ms: Option<i64>) -> bool {
    skew_ms.is_some_and(|s| s.abs() > CLOCK_SKEW_WARNING_MS)
}

/// What the loop should do after a push.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// More is waiting: push again now.
    Immediately,
    /// Nothing to send (or not linked): wait for a commit, a retry or the periodic tick.
    Idle,
    /// It failed: try again after this long, or sooner on `sync_retry_now`.
    RetryAfter(Duration),
    /// Pushing cannot continue until something changes (revoked, upgrade needed).
    Stopped(&'static str),
}

/// What one push did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushReport {
    pub sent: usize,
    pub ack_before: u64,
    pub ack_after: u64,
    pub next: Next,
}

pub struct SyncWorker {
    pub store: Arc<dyn SyncStore>,
    pub transport: Arc<dyn SyncTransport>,
    pub clock: Arc<dyn Clock>,
    pub random: Arc<dyn Random>,
    pub backoff: Backoff,
    failures: AtomicU32,
}

impl SyncWorker {
    pub fn new(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SyncTransport>,
        clock: Arc<dyn Clock>,
        random: Arc<dyn Random>,
    ) -> Self {
        Self {
            store,
            transport,
            clock,
            random,
            backoff: Backoff::default(),
            failures: AtomicU32::new(0),
        }
    }

    /// `sync_retry_now`: the next failure starts the backoff from the beginning.
    pub fn reset_backoff(&self) {
        self.failures.store(0, Ordering::Relaxed);
    }

    fn retry_later(&self) -> Next {
        let n = self.failures.fetch_add(1, Ordering::Relaxed);
        Next::RetryAfter(self.backoff.delay(n, self.random.as_ref()))
    }

    /// Sends the next batch and records what came back.
    pub async fn push_once(&self) -> Result<PushReport, QueryError> {
        let state = self.store.push_state().await?;
        let idle = |next| PushReport {
            sent: 0,
            ack_before: state.last_ack_sequence,
            ack_after: state.last_ack_sequence,
            next,
        };
        if !state.linked {
            return Ok(idle(Next::Idle));
        }
        let batch = self
            .store
            .outbox_batch(state.last_ack_sequence, MAX_PUSH_EVENTS, MAX_PUSH_BYTES)
            .await?;
        if batch.is_empty() {
            self.reset_backoff();
            return Ok(idle(Next::Idle));
        }
        let sent = batch.len();
        let last_sent = batch.last().map_or(0, |e| e.local_sequence);
        let started = self.clock.now();
        let response = self
            .transport
            .push(PushRequest {
                register_id: state.register_id,
                events: batch,
            })
            .await;
        let finished = self.clock.now();
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                self.store.record_push_failure(e.code(), finished).await?;
                let next = if e.is_transient() {
                    self.retry_later()
                } else {
                    Next::Stopped(e.code())
                };
                return Ok(PushReport { sent, ..idle(next) });
            }
        };

        // Never past the server's ack; and never past what exists here.
        let ack = response.ack_sequence.min(state.max_sequence);
        let skew = Timestamp::parse_rfc3339(&response.server_time).map(|server| {
            let midpoint = started.as_millis() + (finished.as_millis() - started.as_millis()) / 2;
            server.as_millis() - midpoint
        });
        let blocking = response.results.iter().find_map(|r| match r.status {
            ResultStatus::Transient => Some(r.code.clone().unwrap_or_else(|| "TRANSIENT".into())),
            ResultStatus::Rejected if r.local_sequence > ack => {
                Some(r.code.clone().unwrap_or_else(|| "REJECTED".into()))
            }
            _ => None,
        });
        let out_of_order = response
            .results
            .iter()
            .any(|r| r.status == ResultStatus::OutOfOrder);
        self.store
            .record_push(&PushRecord {
                ack_sequence: ack,
                results: response.results,
                at: finished,
                clock_skew_ms: skew,
                blocking_code: blocking.clone(),
            })
            .await?;

        let progressed = ack > state.last_ack_sequence || (out_of_order && ack != state.last_ack_sequence);
        let next = if blocking.is_some() || (out_of_order && !progressed) {
            self.retry_later()
        } else if ack >= last_sent && ack >= state.max_sequence {
            self.reset_backoff();
            Next::Idle
        } else if progressed {
            self.reset_backoff();
            Next::Immediately
        } else {
            self.retry_later()
        };
        Ok(PushReport {
            sent,
            ack_before: state.last_ack_sequence,
            ack_after: ack,
            next,
        })
    }
    /// Pulls the next page of the change feed and applies it (E17-T07).
    pub async fn pull_once(&self) -> Result<PullReport, QueryError> {
        let state = self.store.pull_state().await?;
        if !state.linked || state.bootstrap_page.is_some() {
            return Ok(PullReport::idle());
        }
        match self.transport.changes(state.cursor.clone(), PULL_LIMIT).await {
            Err(e) => {
                self.store.record_push_failure(e.code(), self.clock.now()).await?;
                let next = if e.is_transient() {
                    self.retry_later()
                } else {
                    Next::Stopped(e.code())
                };
                Ok(PullReport {
                    next,
                    ..PullReport::idle()
                })
            }
            Ok(page) => {
                let report = self.store.apply_changes(&page, self.clock.now()).await?;
                Ok(PullReport {
                    received: page.changes.len(),
                    report,
                    next: if page.has_more {
                        Next::Immediately
                    } else {
                        Next::Idle
                    },
                })
            }
        }
    }

    /// Fetches and applies the next bootstrap page, resuming where an interrupted bootstrap
    /// stopped. `Next::Idle` once the snapshot is complete (the cursor is then set).
    pub async fn bootstrap_step(&self) -> Result<PullReport, QueryError> {
        let state = self.store.pull_state().await?;
        match self.transport.bootstrap(state.bootstrap_page.clone()).await {
            Err(e) => {
                self.store.record_push_failure(e.code(), self.clock.now()).await?;
                let next = if e.is_transient() {
                    self.retry_later()
                } else {
                    Next::Stopped(e.code())
                };
                Ok(PullReport {
                    next,
                    ..PullReport::idle()
                })
            }
            Ok(page) => {
                let report = self.store.apply_bootstrap(&page, self.clock.now()).await?;
                Ok(PullReport {
                    received: page.entities.len(),
                    report,
                    next: if page.next_page.is_some() {
                        Next::Immediately
                    } else {
                        Next::Idle
                    },
                })
            }
        }
    }
}

/// Changes per pull.
pub const PULL_LIMIT: u32 = 500;

/// What one pull or bootstrap page did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullReport {
    pub received: usize,
    pub report: InboundReport,
    pub next: Next,
}

impl PullReport {
    fn idle() -> Self {
        Self {
            received: 0,
            report: InboundReport::default(),
            next: Next::Idle,
        }
    }
}

/// Jitter from the operating system's random source.
pub struct OsRandom;

impl Random for OsRandom {
    fn up_to(&self, max: u64) -> u64 {
        use argon2::password_hash::rand_core::{OsRng, RngCore};
        match max.checked_add(1) {
            Some(n) => OsRng.next_u64() % n,
            None => OsRng.next_u64(),
        }
    }
}

/// The one-line sync state the status bar shows (E17-T09, PRD §5.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SyncIndicator {
    NotLinked,
    /// Everything acknowledged.
    Synced,
    /// Events waiting, and nothing wrong.
    Pending,
    /// Events waiting and the cloud cannot be reached.
    Offline,
    /// Conflicts or rejected events need a person.
    Attention,
    /// The owner disconnected this register.
    Revoked,
    /// The cloud needs a newer app.
    UpdateRequired,
}

impl SyncIndicator {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotLinked => "not_linked",
            Self::Synced => "synced",
            Self::Pending => "pending",
            Self::Offline => "offline",
            Self::Attention => "attention",
            Self::Revoked => "revoked",
            Self::UpdateRequired => "update_required",
        }
    }
}

/// What the status bar says, from the stored sync state.
pub fn indicator(
    link_state: &str,
    pending: u64,
    needs_attention: u64,
    last_error_code: Option<&str>,
) -> SyncIndicator {
    match link_state {
        "revoked" => return SyncIndicator::Revoked,
        "linked" => {}
        _ => return SyncIndicator::NotLinked,
    }
    match last_error_code {
        Some("SYNC_UPGRADE_REQUIRED") => return SyncIndicator::UpdateRequired,
        Some("SYNC_DEVICE_REVOKED") => return SyncIndicator::Revoked,
        _ => {}
    }
    if needs_attention > 0 {
        return SyncIndicator::Attention;
    }
    let unreachable = matches!(
        last_error_code,
        Some("OFFLINE" | "TIMEOUT" | "SERVER_ERROR" | "DECODE_ERROR" | "AUTH_REQUIRED" | "SERVER_BUSY")
    );
    match (pending, unreachable) {
        (0, _) => SyncIndicator::Synced,
        (_, true) => SyncIndicator::Offline,
        (_, false) => SyncIndicator::Pending,
    }
}

/// A sale's mark (AT-13): `local` (no cloud link), `pending` (saved here, waiting),
/// `synced` (the cloud has it) or `attention` (the cloud refused it).
pub fn sale_sync(link_state: &str, outbox_status: Option<&str>) -> &'static str {
    match outbox_status {
        Some("applied" | "duplicate") => "synced",
        Some("rejected" | "conflict") => "attention",
        _ if link_state == "linked" => "pending",
        _ => "local",
    }
}

/// An event payload as it may be shown or sent to support: PIN hashes removed (the hash is
/// not the PIN, but it is never anyone else's business).
pub fn redact_payload(mut v: serde_json::Value) -> serde_json::Value {
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if k == "pinHash" {
                        *x = serde_json::Value::String("[REDACTED]".into());
                    } else {
                        walk(x);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Max;
    impl Random for Max {
        fn up_to(&self, max: u64) -> u64 {
            max
        }
    }

    #[test]
    fn backoff_doubles_from_two_seconds_to_a_five_minute_cap() {
        let b = Backoff::default();
        let secs: Vec<u64> = (0..12).map(|n| b.delay(n, &Max).as_secs()).collect();
        assert_eq!(secs, [2, 4, 8, 16, 32, 64, 128, 256, 300, 300, 300, 300]);
        assert_eq!(b.bound(1000), Duration::from_secs(300));
    }

    #[test]
    fn transient_transport_errors_are_retried_and_the_rest_stop() {
        assert!(TransportError::Offline.is_transient());
        assert!(
            TransportError::Server {
                status: 503,
                code: None
            }
            .is_transient()
        );
        assert!(
            TransportError::Server {
                status: 429,
                code: None
            }
            .is_transient()
        );
        assert!(
            !TransportError::Server {
                status: 400,
                code: None
            }
            .is_transient()
        );
        assert!(!TransportError::DeviceRevoked.is_transient());
        assert!(
            !TransportError::UpgradeRequired {
                min_version: "1.2.0".into()
            }
            .is_transient()
        );
    }

    #[test]
    fn the_status_bar_states() {
        use SyncIndicator::*;
        assert_eq!(indicator("unlinked", 5, 0, None), NotLinked);
        assert_eq!(indicator("linking", 5, 0, None), NotLinked);
        assert_eq!(indicator("revoked", 5, 0, None), Revoked);
        assert_eq!(indicator("linked", 0, 0, None), Synced);
        assert_eq!(indicator("linked", 3, 0, None), Pending);
        assert_eq!(indicator("linked", 3, 0, Some("TIMEOUT")), Offline);
        assert_eq!(indicator("linked", 0, 0, Some("OFFLINE")), Synced);
        assert_eq!(indicator("linked", 3, 1, Some("OFFLINE")), Attention);
        assert_eq!(
            indicator("linked", 3, 1, Some("SYNC_UPGRADE_REQUIRED")),
            UpdateRequired
        );
        assert_eq!(indicator("linked", 3, 0, Some("SYNC_DEVICE_REVOKED")), Revoked);
    }

    #[test]
    fn a_sale_is_marked_by_its_event() {
        assert_eq!(sale_sync("unlinked", Some("pending")), "local");
        assert_eq!(sale_sync("linked", Some("pending")), "pending");
        assert_eq!(sale_sync("linked", Some("in_flight")), "pending");
        assert_eq!(sale_sync("linked", Some("applied")), "synced");
        assert_eq!(sale_sync("revoked", Some("duplicate")), "synced");
        assert_eq!(sale_sync("linked", Some("rejected")), "attention");
    }

    #[test]
    fn pin_hashes_never_leave_in_a_payload() {
        let v = serde_json::json!({"userId": "u", "pinHash": "$argon2id$...", "nested": [{"pinHash": "x", "name": "y"}]});
        let r = redact_payload(v);
        assert_eq!(r["pinHash"], "[REDACTED]");
        assert_eq!(r["nested"][0]["pinHash"], "[REDACTED]");
        assert_eq!(r["nested"][0]["name"], "y");
    }

    #[test]
    fn skew_over_ten_minutes_warns() {
        assert!(!clock_skew_warning(None));
        assert!(!clock_skew_warning(Some(CLOCK_SKEW_WARNING_MS)));
        assert!(clock_skew_warning(Some(CLOCK_SKEW_WARNING_MS + 1)));
        assert!(clock_skew_warning(Some(-CLOCK_SKEW_WARNING_MS - 1)));
    }
}
