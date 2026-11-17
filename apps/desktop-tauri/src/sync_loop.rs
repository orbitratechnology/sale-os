//! Runs sync in the background: each cycle pushes the outbox (E17-T03), then pulls the
//! change feed, or continues an unfinished bootstrap (E17-T07). It runs after every commit
//! that adds to the outbox, every 30 seconds (which also notices connectivity coming back and
//! picks up other registers' changes), after a backoff delay, and at once on `sync_retry_now`.
//! Pushing reads the outbox and writes sync bookkeeping; a pull is one short transaction per
//! page; so a sale never waits long for it.

use pos_application::link::CredentialStore;
use pos_application::queries::QueryError;
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
    pub fn start(
        store: SqliteStore,
        transport: Arc<dyn SyncTransport>,
        clock: Arc<dyn Clock>,
        credentials: Option<Arc<dyn CredentialStore>>,
    ) -> Self {
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
        tokio::spawn(run(worker.clone(), wake.clone(), store, credentials));
        Self { worker, wake }
    }

    /// `sync_retry_now`: push now, with the backoff reset.
    pub fn retry_now(&self) {
        self.worker.reset_backoff();
        self.wake.notify_one();
    }
}

async fn run(
    worker: Arc<SyncWorker>,
    wake: Arc<Notify>,
    store: SqliteStore,
    credentials: Option<Arc<dyn CredentialStore>>,
) {
    loop {
        let next = match cycle(&worker).await {
            Ok(next) => next,
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
                if code == "SYNC_DEVICE_REVOKED" {
                    // The owner disconnected this register: it keeps selling locally, stops
                    // syncing, and forgets its credentials (E16-T04).
                    if let Err(e) = pos_application::link::LinkStore::mark_revoked(&store).await {
                        tracing::error!(error = %e, "could not record the revocation");
                    }
                    if let Some(c) = &credentials
                        && let Err(e) = c.clear().await
                    {
                        tracing::error!(error = %e, "could not clear the device credentials");
                    }
                }
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

/// One push, then one pull (or bootstrap page); what to do next is the more urgent of the two.
async fn cycle(worker: &SyncWorker) -> Result<Next, QueryError> {
    let push = worker.push_once().await?.next;
    if push == Next::Immediately {
        return Ok(Next::Immediately);
    }
    let pull = if worker.store.pull_state().await?.bootstrap_page.is_some() {
        worker.bootstrap_step().await?.next
    } else {
        worker.pull_once().await?.next
    };
    Ok(match (push, pull) {
        (Next::Stopped(code), _) | (_, Next::Stopped(code)) => Next::Stopped(code),
        (_, Next::Immediately) => Next::Immediately,
        (Next::RetryAfter(a), Next::RetryAfter(b)) => Next::RetryAfter(a.min(b)),
        (Next::RetryAfter(d), _) | (_, Next::RetryAfter(d)) => Next::RetryAfter(d),
        _ => Next::Idle,
    })
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
