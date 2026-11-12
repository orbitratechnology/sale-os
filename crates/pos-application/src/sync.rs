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

/// The cloud API, as the worker needs it (the HTTP adapter arrives with linking, E17-T08).
#[async_trait::async_trait]
pub trait SyncTransport: Send + Sync {
    async fn push(&self, request: PushRequest) -> Result<PushResponse, TransportError>;
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
    fn skew_over_ten_minutes_warns() {
        assert!(!clock_skew_warning(None));
        assert!(!clock_skew_warning(Some(CLOCK_SKEW_WARNING_MS)));
        assert!(clock_skew_warning(Some(CLOCK_SKEW_WARNING_MS + 1)));
        assert!(clock_skew_warning(Some(-CLOCK_SKEW_WARNING_MS - 1)));
    }
}
