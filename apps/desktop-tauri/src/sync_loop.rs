//! Runs the push worker in the background (E17-T03): after every commit that adds to the
//! outbox, every 30 seconds while there is pending work (which also notices connectivity
//! coming back), after a backoff delay, and at once on `sync_retry_now`. It only reads the
//! outbox and writes sync bookkeeping, so a sale never waits for it.

use pos_application::sync::{Next, OsRandom, SyncTransport, SyncWorker, TransportError};
use pos_domain::Clock;
use pos_local_db::SqliteStore;
use pos_sync::{PushRequest, PushResponse};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// While events are pending but nothing else happens, look again this often.
const TICK: Duration = Duration::from_secs(30);

pub(crate) struct SyncRunner {
    worker: Arc<SyncWorker>,
    wake: Arc<Notify>,
}

impl SyncRunner {
    /// Starts the loop on the current runtime.
    pub fn start(store: SqliteStore, transport: Arc<dyn SyncTransport>, clock: Arc<dyn Clock>) -> Self {
        let worker = Arc::new(SyncWorker::new(
            Arc::new(store.clone()),
            transport,
            clock,
            Arc::new(OsRandom),
        ));
        let wake = Arc::new(Notify::new());
        // Commits that append to the outbox wake the worker.
        let mut notices = store.subscribe();
        let w = wake.clone();
        tokio::spawn(async move {
            loop {
                match notices.recv().await {
                    Ok(n) if n.outbox_appended => w.notify_one(),
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => w.notify_one(),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        tokio::spawn(run(worker.clone(), wake.clone()));
        Self { worker, wake }
    }

    /// `sync_retry_now`: push now, with the backoff reset.
    pub fn retry_now(&self) {
        self.worker.reset_backoff();
        self.wake.notify_one();
    }
}

async fn run(worker: Arc<SyncWorker>, wake: Arc<Notify>) {
    loop {
        let next = match worker.push_once().await {
            Ok(report) => report.next,
            Err(e) => {
                tracing::warn!(error = %e, "sync push could not read or record the outbox");
                Next::RetryAfter(TICK)
            }
        };
        let wait = match next {
            Next::Immediately => continue,
            Next::Idle => Some(TICK),
            Next::RetryAfter(d) => Some(d),
            Next::Stopped(code) => {
                tracing::warn!(code, "sync stopped until the register is relinked or updated");
                None
            }
        };
        match wait {
            Some(d) => {
                let _ = tokio::time::timeout(d, wake.notified()).await;
            }
            None => wake.notified().await,
        }
    }
}

/// The transport until linking (E17-T08) provides the HTTP one. An unlinked register never
/// calls it.
pub(crate) struct NotLinked;

#[async_trait::async_trait]
impl SyncTransport for NotLinked {
    async fn push(&self, _request: PushRequest) -> Result<PushResponse, TransportError> {
        Err(TransportError::Offline)
    }
}
