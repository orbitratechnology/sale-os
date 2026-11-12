//! E17-T03: the push worker against a fake server that follows the real server's rules
//! (E17-T04): batches, acks, duplicates, conflicts, rejections, gaps, transient failures,
//! backoff and clock skew.

mod common;

use pos_application::sync::{
    Next, PushReport, Random, SyncStore, SyncTransport, SyncWorker, TransportError, clock_skew_warning,
};
use pos_domain::{Clock, Timestamp};
use pos_local_db::SqliteStore;
use pos_sync::{EventResult, PushRequest, PushResponse, ResultStatus, codes};
use pos_testkit::FixedClock;
use proptest::prelude::*;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

// ---------- fixtures ----------

/// Marks the register as linked (E17-T08 will do this for real).
async fn link(store: &SqliteStore) {
    sqlx::query(
        "INSERT INTO sync_state (id, link_state, device_id) VALUES (1, 'linked', 'device')
         ON CONFLICT (id) DO UPDATE SET link_state = 'linked'",
    )
    .execute(store.db().writer())
    .await
    .unwrap();
}

/// Appends `n` audit events to the outbox after the current last sequence.
async fn add_events(store: &SqliteStore, n: u32) {
    let mut tx = store.db().writer().begin().await.unwrap();
    for _ in 0..n {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO outbox_events (event_id, business_id, location_id, register_id, local_sequence, event_type,
                    schema_version, aggregate_type, aggregate_id, occurred_at, payload_json, created_at)
             SELECT ?, r.business_id, r.location_id, r.register_id,
                    (SELECT coalesce(max(local_sequence), 0) + 1 FROM outbox_events),
                    'audit.recorded', 1, 'audit', ?, 1767225600000,
                    json_object('auditId', ?, 'action', 'test', 'entityType', 'test', 'entityId', 'x', 'metadata', json_object()),
                    1767225600000
               FROM device_identity d JOIN registers r ON r.register_id = d.register_id",
        )
        .bind(&id)
        .bind(&id)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    // Keep the register's sequence counter in step, as the store's own writes do.
    sqlx::query(
        "UPDATE register_counters SET value = (SELECT max(local_sequence) FROM outbox_events)
          WHERE name = 'outbox_seq'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn local_ack(store: &SqliteStore) -> u64 {
    store.push_state().await.unwrap().last_ack_sequence
}

async fn statuses(store: &SqliteStore) -> HashMap<String, i64> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*) FROM outbox_events GROUP BY status")
            .fetch_all(store.db().readers())
            .await
            .unwrap();
    rows.into_iter().collect()
}

/// Jitter at its upper bound, remembering each bound asked for.
#[derive(Default)]
struct MaxRandom(Mutex<Vec<u64>>);
impl Random for MaxRandom {
    fn up_to(&self, max: u64) -> u64 {
        self.0.lock().unwrap().push(max);
        max
    }
}

/// A server with the real ingestion rules, plus switches for failures and outcomes.
struct FakeServer {
    clock: Arc<FixedClock>,
    s: Mutex<ServerState>,
}

#[derive(Default)]
struct ServerState {
    ack: u64,
    next_server_seq: i64,
    inbox: HashMap<Uuid, EventResult>,
    fail_next: VecDeque<TransportError>,
    conflict: BTreeSet<u64>,
    reject: BTreeSet<u64>,
    unsupported: BTreeSet<u64>,
    clock_offset_ms: i64,
    batches: Vec<usize>,
    /// Simulated round trip.
    latency_ms: u64,
}

impl FakeServer {
    fn new(clock: Arc<FixedClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            s: Mutex::new(ServerState::default()),
        })
    }
}

#[async_trait::async_trait]
impl SyncTransport for FakeServer {
    async fn push(&self, req: PushRequest) -> Result<PushResponse, TransportError> {
        let latency = self.s.lock().unwrap().latency_ms;
        if latency > 0 {
            tokio::time::sleep(Duration::from_millis(latency)).await;
        }
        let mut s = self.s.lock().unwrap();
        if let Some(e) = s.fail_next.pop_front() {
            return Err(e);
        }
        s.batches.push(req.events.len());
        let mut results = Vec::new();
        for e in &req.events {
            let base = EventResult::new(e.event_id, e.local_sequence, ResultStatus::Applied);
            if let Some(first) = s.inbox.get(&e.event_id) {
                let mut r = first.clone();
                r.original_status = Some(r.status);
                r.status = ResultStatus::Duplicate;
                results.push(r);
                continue;
            }
            if e.local_sequence != s.ack + 1 {
                let mut r = EventResult::new(e.event_id, e.local_sequence, ResultStatus::OutOfOrder);
                r.expected_sequence = Some(s.ack + 1);
                results.push(r);
                break;
            }
            if s.unsupported.contains(&e.local_sequence) {
                results.push(
                    EventResult::new(e.event_id, e.local_sequence, ResultStatus::Rejected)
                        .with_code(codes::EVENT_VERSION_UNSUPPORTED, None),
                );
                break;
            }
            let r = if s.conflict.contains(&e.local_sequence) {
                let mut r = EventResult::new(e.event_id, e.local_sequence, ResultStatus::Conflict)
                    .with_code(codes::VERSION, None);
                r.server_version = Some(7);
                r.server_snapshot = Some(serde_json::json!({"name": "Server"}));
                r
            } else if s.reject.contains(&e.local_sequence) {
                EventResult::new(e.event_id, e.local_sequence, ResultStatus::Rejected)
                    .with_code(codes::EVENT_INVALID, Some("bad".into()))
            } else {
                s.next_server_seq += 1;
                let mut r = base;
                r.server_seq = Some(s.next_server_seq);
                r
            };
            s.inbox.insert(e.event_id, r.clone());
            s.ack = e.local_sequence;
            results.push(r);
        }
        let server_time = Timestamp::from_millis(self.clock.now().as_millis() + s.clock_offset_ms);
        Ok(PushResponse {
            ack_sequence: s.ack,
            server_time: server_time.to_rfc3339(),
            results,
        })
    }
}

struct Rig {
    shop: common::Shop,
    server: Arc<FakeServer>,
    random: Arc<MaxRandom>,
    worker: SyncWorker,
}

async fn rig() -> Rig {
    let shop = common::shop().await;
    link(&shop.store).await;
    let server = FakeServer::new(shop.clock.clone());
    let random = Arc::new(MaxRandom::default());
    let worker = SyncWorker::new(
        Arc::new(shop.store.clone()),
        server.clone(),
        shop.clock.clone(),
        random.clone(),
    );
    Rig {
        shop,
        server,
        random,
        worker,
    }
}

/// Pushes until the worker has nothing more to do now; returns the reports.
async fn drain(w: &SyncWorker) -> Vec<PushReport> {
    let mut out = Vec::new();
    loop {
        let r = w.push_once().await.unwrap();
        let again = r.next == Next::Immediately;
        out.push(r);
        if !again {
            return out;
        }
    }
}

// ---------- tests ----------

#[tokio::test]
async fn an_unlinked_register_pushes_nothing() {
    let shop = common::shop().await;
    let server = FakeServer::new(shop.clock.clone());
    let w = SyncWorker::new(
        Arc::new(shop.store.clone()),
        server.clone(),
        shop.clock.clone(),
        Arc::new(MaxRandom::default()),
    );
    let r = w.push_once().await.unwrap();
    assert_eq!((r.sent, r.next), (0, Next::Idle));
    assert!(server.s.lock().unwrap().batches.is_empty());
}

#[tokio::test]
async fn the_outbox_drains_in_order_in_batches_of_two_hundred() {
    let r = rig().await;
    let setup_events = r.shop.store.push_state().await.unwrap().max_sequence;
    add_events(&r.shop.store, 450).await;
    let max = r.shop.store.push_state().await.unwrap().max_sequence;
    assert_eq!(max, setup_events + 450);
    let reports = drain(&r.worker).await;
    let batches = r.server.s.lock().unwrap().batches.clone();
    assert!(batches.iter().all(|n| *n <= 200), "{batches:?}");
    assert_eq!(batches.iter().sum::<usize>() as u64, max);
    assert_eq!(reports.last().unwrap().next, Next::Idle);
    assert_eq!(local_ack(&r.shop.store).await, max);
    assert_eq!(
        statuses(&r.shop.store).await,
        HashMap::from([("applied".into(), max as i64)])
    );
    // Nothing left: the next push sends nothing.
    let again = r.worker.push_once().await.unwrap();
    assert_eq!((again.sent, again.next), (0, Next::Idle));
}

#[tokio::test]
async fn a_transient_failure_changes_nothing_and_backs_off_with_growing_delays() {
    let r = rig().await;
    let before = local_ack(&r.shop.store).await;
    let statuses_before = statuses(&r.shop.store).await;
    for _ in 0..10 {
        r.server
            .s
            .lock()
            .unwrap()
            .fail_next
            .push_back(TransportError::Server {
                status: 503,
                code: None,
            });
    }
    r.server
        .s
        .lock()
        .unwrap()
        .fail_next
        .push_back(TransportError::Timeout);
    let mut delays = Vec::new();
    for _ in 0..11 {
        match r.worker.push_once().await.unwrap().next {
            Next::RetryAfter(d) => delays.push(d.as_secs()),
            other => panic!("expected a retry, got {other:?}"),
        }
    }
    assert_eq!(delays, [2, 4, 8, 16, 32, 64, 128, 256, 300, 300, 300]);
    assert_eq!(local_ack(&r.shop.store).await, before);
    assert_eq!(statuses(&r.shop.store).await, statuses_before);
    let code: Option<String> = sqlx::query_scalar("SELECT last_error_code FROM sync_state WHERE id = 1")
        .fetch_one(r.shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(code.as_deref(), Some("TIMEOUT"));
    // Then it gets through, and the backoff starts again from the beginning.
    assert_eq!(drain(&r.worker).await.last().unwrap().next, Next::Idle);
    r.server
        .s
        .lock()
        .unwrap()
        .fail_next
        .push_back(TransportError::Offline);
    add_events(&r.shop.store, 1).await;
    assert_eq!(
        r.worker.push_once().await.unwrap().next,
        Next::RetryAfter(Duration::from_secs(2))
    );
    // retry-now resets too.
    r.server
        .s
        .lock()
        .unwrap()
        .fail_next
        .push_back(TransportError::Offline);
    r.worker.reset_backoff();
    assert_eq!(
        r.worker.push_once().await.unwrap().next,
        Next::RetryAfter(Duration::from_secs(2))
    );
    assert_eq!(r.random.0.lock().unwrap()[0], 2000);
}

#[tokio::test]
async fn revocation_and_upgrade_stop_the_worker() {
    let r = rig().await;
    r.server
        .s
        .lock()
        .unwrap()
        .fail_next
        .push_back(TransportError::DeviceRevoked);
    assert_eq!(
        r.worker.push_once().await.unwrap().next,
        Next::Stopped("SYNC_DEVICE_REVOKED")
    );
    r.server
        .s
        .lock()
        .unwrap()
        .fail_next
        .push_back(TransportError::UpgradeRequired {
            min_version: "9.0.0".into(),
        });
    assert_eq!(
        r.worker.push_once().await.unwrap().next,
        Next::Stopped("SYNC_UPGRADE_REQUIRED")
    );
}

#[tokio::test]
async fn conflicts_and_rejections_are_recorded_once_and_the_rest_flows() {
    let r = rig().await;
    let base = r.shop.store.push_state().await.unwrap().max_sequence;
    add_events(&r.shop.store, 4).await;
    {
        let mut s = r.server.s.lock().unwrap();
        s.conflict.insert(base + 1);
        s.reject.insert(base + 2);
    }
    drain(&r.worker).await;
    let st = statuses(&r.shop.store).await;
    assert_eq!(st.get("conflict"), Some(&1));
    assert_eq!(st.get("rejected"), Some(&1));
    assert_eq!(local_ack(&r.shop.store).await, base + 4);
    // The client lost its records (say, a crash before commit): the server's duplicates
    // restore the same outcome, and the conflict row is not doubled.
    sqlx::query("UPDATE sync_state SET last_ack_sequence = ?")
        .bind(i64::try_from(base).unwrap())
        .execute(r.shop.store.db().writer())
        .await
        .unwrap();
    drain(&r.worker).await;
    let conflicts: Vec<(String, i64, String)> =
        sqlx::query_as("SELECT status, server_version, server_payload_json FROM sync_conflicts")
            .fetch_all(r.shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(conflicts, [("open".into(), 7, r#"{"name":"Server"}"#.into())]);
    let st = statuses(&r.shop.store).await;
    assert_eq!((st.get("conflict"), st.get("rejected")), (Some(&1), Some(&1)));
    let code: Option<String> =
        sqlx::query_scalar("SELECT last_error_code FROM outbox_events WHERE status = 'rejected'")
            .fetch_one(r.shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(code.as_deref(), Some("EVENT_INVALID"));
}

#[tokio::test]
async fn an_unsupported_event_blocks_and_stays_pending_until_the_server_accepts_it() {
    let r = rig().await;
    let base = r.shop.store.push_state().await.unwrap().max_sequence;
    add_events(&r.shop.store, 3).await;
    r.server.s.lock().unwrap().unsupported.insert(base + 2);
    let reports = drain(&r.worker).await;
    assert!(matches!(reports.last().unwrap().next, Next::RetryAfter(_)));
    assert_eq!(local_ack(&r.shop.store).await, base + 1);
    let (status, code): (String, Option<String>) =
        sqlx::query_as("SELECT status, last_error_code FROM outbox_events WHERE local_sequence = ?")
            .bind(i64::try_from(base + 2).unwrap())
            .fetch_one(r.shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(
        (status.as_str(), code.as_deref()),
        ("pending", Some("EVENT_VERSION_UNSUPPORTED"))
    );
    // The server is upgraded: it goes through.
    r.server.s.lock().unwrap().unsupported.clear();
    assert_eq!(drain(&r.worker).await.last().unwrap().next, Next::Idle);
    assert_eq!(local_ack(&r.shop.store).await, base + 3);
}

#[tokio::test]
async fn a_server_behind_the_client_rewinds_the_ack_and_resends() {
    let r = rig().await;
    add_events(&r.shop.store, 10).await;
    drain(&r.worker).await;
    let max = local_ack(&r.shop.store).await;
    // The server lost the last three (restored from a backup).
    {
        let mut s = r.server.s.lock().unwrap();
        s.ack = max - 3;
        s.inbox.retain(|_, v| v.local_sequence <= max - 3);
    }
    add_events(&r.shop.store, 1).await;
    let first = r.worker.push_once().await.unwrap();
    assert_eq!(first.ack_after, max - 3, "rewound to the server's ack");
    assert_eq!(first.next, Next::Immediately);
    drain(&r.worker).await;
    assert_eq!(local_ack(&r.shop.store).await, max + 1);
}

#[tokio::test]
async fn clock_skew_is_recorded_and_over_ten_minutes_warns() {
    let r = rig().await;
    r.server.s.lock().unwrap().clock_offset_ms = 11 * 60 * 1000;
    drain(&r.worker).await;
    let skew: Option<i64> = sqlx::query_scalar("SELECT server_clock_skew_ms FROM sync_state WHERE id = 1")
        .fetch_one(r.shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(skew, Some(660_000));
    assert!(clock_skew_warning(skew));
}

#[tokio::test]
async fn ten_thousand_pending_events_drain_quickly() {
    let r = rig().await;
    add_events(&r.shop.store, 10_000).await;
    let started = std::time::Instant::now();
    let reports = drain(&r.worker).await;
    let took = started.elapsed();
    eprintln!(
        "10,000 events in {} pushes: {took:?} (local side, in-memory server)",
        reports.len()
    );
    assert_eq!(
        local_ack(&r.shop.store).await,
        r.shop.store.push_state().await.unwrap().max_sequence
    );
    assert!(took < Duration::from_secs(60), "{took:?}");
}

/// A server answering with arbitrary (but well-formed) acks and statuses.
struct ArbitraryServer {
    script: Mutex<VecDeque<(u64, Vec<u8>)>>,
    last_ack: Mutex<u64>,
}

#[async_trait::async_trait]
impl SyncTransport for ArbitraryServer {
    async fn push(&self, req: PushRequest) -> Result<PushResponse, TransportError> {
        let (ack, picks) = self.script.lock().unwrap().pop_front().unwrap_or((0, vec![]));
        *self.last_ack.lock().unwrap() = ack;
        let all = [
            ResultStatus::Applied,
            ResultStatus::Duplicate,
            ResultStatus::Conflict,
            ResultStatus::Rejected,
            ResultStatus::OutOfOrder,
            ResultStatus::Transient,
        ];
        let results = req
            .events
            .iter()
            .zip(picks.iter().cycle())
            .map(|(e, p)| {
                let mut r = EventResult::new(e.event_id, e.local_sequence, all[usize::from(*p) % all.len()]);
                if r.status == ResultStatus::Duplicate {
                    r.original_status = Some(ResultStatus::Applied);
                }
                if r.status == ResultStatus::Conflict {
                    r.server_version = Some(1);
                    r.server_snapshot = Some(serde_json::json!({}));
                }
                r
            })
            .collect();
        Ok(PushResponse {
            ack_sequence: ack,
            server_time: "2026-11-05T09:20:00.004Z".into(),
            results,
        })
    }
}

#[test]
fn the_local_ack_never_exceeds_the_servers() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let shop = rt.block_on(async {
        let shop = common::shop().await;
        link(&shop.store).await;
        add_events(&shop.store, 40).await;
        shop
    });
    let max = rt.block_on(shop.store.push_state()).unwrap().max_sequence;
    let steps = proptest::collection::vec((0u64..=max + 20, proptest::collection::vec(0u8..6, 1..8)), 1..8);
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    });
    runner
        .run(&steps, |script| {
            rt.block_on(async {
                sqlx::query("UPDATE sync_state SET last_ack_sequence = 0")
                    .execute(shop.store.db().writer())
                    .await
                    .unwrap();
                let server = Arc::new(ArbitraryServer {
                    script: Mutex::new(script.clone().into()),
                    last_ack: Mutex::new(0),
                });
                let w = SyncWorker::new(
                    Arc::new(shop.store.clone()),
                    server.clone(),
                    shop.clock.clone(),
                    Arc::new(MaxRandom::default()),
                );
                for _ in 0..script.len() {
                    let report = w.push_once().await.unwrap();
                    let server_ack = *server.last_ack.lock().unwrap();
                    let local = local_ack(&shop.store).await;
                    prop_assert!(local <= server_ack, "local {local} > server {server_ack}");
                    prop_assert_eq!(local, server_ack.min(max));
                    prop_assert_eq!(report.ack_after, local);
                }
                Ok(())
            })
        })
        .unwrap();
}

/// Commit latency with and without the worker pushing a large backlog at the same time
/// (E17-T03: pushing never blocks checkout). A bench, not a pass/fail gate in CI:
/// `cargo test -p pos-local-db --test sync_push --release -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "bench"]
async fn bench_commit_latency_while_pushing() {
    use pos_application::config::ConfigService;
    /// p95 of `n` edits of one category: a one-transaction write with an outbox event whose
    /// cost does not grow as the bench runs.
    async fn p95(shop: &common::Shop, n: usize) -> Duration {
        let config = ConfigService {
            store: Arc::new(shop.store.clone()),
            config: Arc::new(shop.store.clone()),
        };
        let mut c = config
            .save_category(
                &shop.owner,
                None,
                &format!("Bench {}", Uuid::now_v7()),
                None,
                None,
            )
            .await
            .unwrap();
        let mut times = Vec::with_capacity(n);
        for i in 0..n {
            let t = std::time::Instant::now();
            c = config
                .save_category(
                    &shop.owner,
                    Some(c.category_id),
                    &format!("Bench {i} {}", Uuid::now_v7()),
                    None,
                    Some(c.version),
                )
                .await
                .unwrap();
            times.push(t.elapsed());
        }
        times.sort();
        times[n * 95 / 100]
    }
    for latency_ms in [0, 30] {
        let r = rig().await;
        r.server.s.lock().unwrap().latency_ms = latency_ms;
        let alone = p95(&r.shop, 300).await;
        add_events(&r.shop.store, 20_000).await;
        // Control: the same database with the backlog, nothing pushing.
        let backlog = p95(&r.shop, 300).await;
        let worker = Arc::new(r.worker);
        let pushing = {
            let w = worker.clone();
            tokio::spawn(async move { while w.push_once().await.unwrap().next == Next::Immediately {} })
        };
        let busy = p95(&r.shop, 300).await;
        let still_pushing = !pushing.is_finished();
        pushing.abort();
        eprintln!(
            "server round trip {latency_ms} ms: commit p95 alone {alone:?}, with the backlog {backlog:?}, while pushing {busy:?} (still pushing at the end: {still_pushing})"
        );
    }
}
