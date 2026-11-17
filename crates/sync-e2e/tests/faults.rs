//! E17-T10 / AT-06: 200 sales made offline, then the register reconnects through a faulty
//! network. Whatever the fault, the cloud ends up with exactly those 200 sales, their tender
//! totals and stock movements, each event applied once. The smoke subset runs on every PR;
//! the full matrix (`--include-ignored`) nightly.

mod common;

use common::proxy::{Action, Schedule};
use common::{Cloud, Register, cloud, register_at};
use pos_application::sync::{Next, SyncStore, SyncTransport, SyncWorker, TransportError};
use pos_sync::{PushRequest, PushResponse};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const SALES: usize = 200;

/// A register behind a proxy with `schedule`, 200 sales made offline, then linked (the link
/// itself is not faulted).
async fn offline_register(cloud: &Cloud, schedule: &Arc<Schedule>) -> (Register, uuid::Uuid) {
    *schedule.only.lock().unwrap() = "/v1/sync/".into();
    let upstream = cloud.url.trim_start_matches("http://").to_owned();
    let url = common::proxy::start(upstream, schedule.clone()).await;
    let r = register_at(&url, "1234").await;
    let items = [
        r.product("Cola", 120).await,
        r.product("Bread", 250).await,
        r.product("Gum", 75).await,
    ];
    r.go_live().await;
    let carts = r.carts();
    for i in 0..SALES {
        r.sell(&carts, items[i % 3], 1 + (i % 4) as i64).await;
    }
    let (email, password) = cloud.owner().await;
    let owner = r.sign_in(&email, &password).await;
    let linked = r.link.create(&r.owner, &owner, false).await.unwrap();
    (r, linked.business_id)
}

/// Pushes until everything is acknowledged, retrying at once whatever the worker says (the
/// backoff delays are tested elsewhere).
async fn converge(w: &SyncWorker, store: &pos_local_db::SqliteStore) -> usize {
    for round in 1..=2_000 {
        match w.push_once().await.unwrap().next {
            Next::Stopped(code) => panic!("sync stopped: {code}"),
            _ => {
                let s = store.push_state().await.unwrap();
                if s.last_ack_sequence == s.max_sequence {
                    return round;
                }
            }
        }
    }
    panic!("did not converge");
}

/// AT-06: the cloud holds exactly what the register did, once.
async fn assert_exactly_once(cloud: &Cloud, r: &Register, business: uuid::Uuid) {
    let local = |sql: &'static str| r.local(sql);
    let local_sales = local("SELECT count(*) FROM sales WHERE mode = 'live'").await;
    let local_tendered = local("SELECT sum(amount_minor) FROM tenders").await;
    let local_moves = local("SELECT count(*) FROM stock_movements WHERE source_type = 'sale'").await;
    let local_moved =
        local("SELECT sum(quantity_delta_milli) FROM stock_movements WHERE source_type = 'sale'").await;
    let local_events = local("SELECT count(*) FROM outbox_events").await;
    assert_eq!(local_sales, SALES as i64);
    let q = |sql: &'static str| async move {
        sqlx::query_scalar::<_, i64>(sql)
            .bind(business)
            .fetch_one(&cloud.pool)
            .await
            .unwrap()
    };
    assert_eq!(
        q("SELECT count(*) FROM sales WHERE business_id = $1").await,
        local_sales,
        "sales"
    );
    assert_eq!(
        q("SELECT sum(amount_minor)::bigint FROM tenders WHERE business_id = $1").await,
        local_tendered,
        "tenders"
    );
    assert_eq!(
        q("SELECT count(*) FROM stock_movements WHERE business_id = $1 AND source_type = 'sale'").await,
        local_moves,
        "stock movements"
    );
    assert_eq!(
        q("SELECT sum(delta_milli)::bigint FROM stock_movements WHERE business_id = $1 AND source_type = 'sale'").await,
        local_moved
    );
    // One inbox row per event, and no event applied twice (one feed row at most each).
    assert_eq!(
        q("SELECT count(*) FROM event_inbox WHERE business_id = $1").await,
        local_events,
        "inbox"
    );
    assert_eq!(
        q(
            "SELECT count(*) FROM (SELECT origin_event_id FROM change_log WHERE business_id = $1
             GROUP BY origin_event_id HAVING count(*) > 1) d"
        )
        .await,
        0,
        "an event applied twice"
    );
    assert_eq!(
        q("SELECT sum(sales_count)::bigint FROM daily_sales_rollup WHERE business_id = $1").await,
        local_sales
    );
}

async fn scenario(fault: Action, every: usize) {
    let Some(cloud) = cloud().await else { return };
    let schedule = Schedule::new(fault, every);
    let (r, business) = offline_register(&cloud, &schedule).await;
    let rounds = converge(&r.worker, &r.store).await;
    assert!(
        schedule.faulted.load(Ordering::SeqCst) > 0 || fault == Action::Pass,
        "the fault never fired"
    );
    eprintln!(
        "{fault:?} every {every}: converged in {rounds} pushes, {} faulted",
        schedule.faulted.load(Ordering::SeqCst)
    );
    assert_exactly_once(&cloud, &r, business).await;
}

// ---------- smoke (every PR) ----------

/// The server applied the push, the answer was lost: the retry must not apply it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at06_dropped_responses() {
    scenario(Action::DropResponse, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at06_connection_reset_mid_body() {
    scenario(Action::ResetMidBody, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at06_duplicated_requests() {
    scenario(Action::Duplicate, 1).await;
}

// ---------- the full matrix (nightly) ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly matrix"]
async fn at06_dropped_requests() {
    scenario(Action::Refuse, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly matrix"]
async fn at06_service_unavailable() {
    scenario(Action::Unavailable, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly matrix"]
async fn at06_internal_server_errors() {
    scenario(Action::ServerError, 3).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly matrix"]
async fn at06_slow_network() {
    scenario(Action::Delay(300), 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly matrix"]
async fn at06_every_request_lost_once_in_a_while() {
    scenario(Action::DropResponse, 1).await;
}

/// Sends each batch's second half before its first half: the cloud refuses the second half
/// (`out_of_order`) and the register resends it.
struct Reorder(Arc<pos_sync_http::DeviceSession>);

#[async_trait::async_trait]
impl SyncTransport for Reorder {
    async fn push(&self, req: PushRequest) -> Result<PushResponse, TransportError> {
        if req.events.len() < 2 {
            return self.0.push(req).await;
        }
        let mid = req.events.len() / 2;
        let (first, second) = req.events.split_at(mid);
        let later = PushRequest {
            register_id: req.register_id,
            events: second.to_vec(),
        };
        let early = self.0.push(later).await?;
        assert!(
            early.results.iter().all(|r| matches!(
                r.status,
                pos_sync::ResultStatus::OutOfOrder | pos_sync::ResultStatus::Duplicate
            )),
            "a later half went in before the earlier one"
        );
        self.0
            .push(PushRequest {
                register_id: req.register_id,
                events: first.to_vec(),
            })
            .await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly matrix"]
async fn at06_reordered_batches() {
    let Some(cloud) = cloud().await else { return };
    let schedule = Schedule::new(Action::Pass, 1);
    let (r, business) = offline_register(&cloud, &schedule).await;
    let w = SyncWorker::new(
        Arc::new(r.store.clone()),
        Arc::new(Reorder(r.session.clone())),
        r.clock.clone(),
        Arc::new(pos_application::sync::OsRandom),
    );
    converge(&w, &r.store).await;
    assert_exactly_once(&cloud, &r, business).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly matrix"]
async fn at06_flapping_connection() {
    let Some(cloud) = cloud().await else { return };
    let schedule = Schedule::new(Action::Pass, 1);
    let (r, business) = offline_register(&cloud, &schedule).await;
    let flap = {
        let s = schedule.clone();
        tokio::spawn(async move {
            loop {
                s.offline.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                s.offline.store(false, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(90)).await;
            }
        })
    };
    converge(&r.worker, &r.store).await;
    flap.abort();
    assert!(schedule.faulted.load(Ordering::SeqCst) > 0);
    assert_exactly_once(&cloud, &r, business).await;
}

/// The app is killed while a push is in flight (the server may or may not have applied it),
/// then restarts: a new session from the stored credentials converges without duplicates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_killed_mid_push_converges_after_restart() {
    let Some(cloud) = cloud().await else { return };
    let schedule = Schedule::new(Action::Delay(500), 1);
    let (r, business) = offline_register(&cloud, &schedule).await;
    let worker = Arc::new(SyncWorker::new(
        Arc::new(r.store.clone()),
        r.session.clone(),
        r.clock.clone(),
        Arc::new(pos_application::sync::OsRandom),
    ));
    let push = {
        let w = worker.clone();
        tokio::spawn(async move { w.push_once().await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    push.abort();
    let _ = push.await;
    // Give the in-flight request time to reach the server and be applied there.
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Restart: a fresh session (no access token in memory) from the same credentials.
    let client = pos_sync_http::CloudClient::new(&cloud.url, "0.1.0").unwrap();
    let session = Arc::new(pos_sync_http::DeviceSession::new(client, r.credentials.clone()));
    let restarted = SyncWorker::new(
        Arc::new(r.store.clone()),
        session,
        r.clock.clone(),
        Arc::new(pos_application::sync::OsRandom),
    );
    converge(&restarted, &r.store).await;
    assert_exactly_once(&cloud, &r, business).await;
}

/// Online/offline flapping while the till sells: no sale waits on the network.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flapping_during_checkout_never_blocks_a_sale() {
    let Some(cloud) = cloud().await else { return };
    let schedule = Schedule::new(Action::Delay(50), 1);
    let (r, business) = offline_register(&cloud, &schedule).await;
    let r = Arc::new(r);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let background = {
        let (r, s, stop) = (r.clone(), schedule.clone(), stop.clone());
        tokio::spawn(async move {
            let mut i = 0u32;
            while !stop.load(Ordering::SeqCst) {
                s.offline.store(i.is_multiple_of(2), Ordering::SeqCst);
                let _ = r.worker.push_once().await;
                i += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            s.offline.store(false, Ordering::SeqCst);
        })
    };
    let carts = r.carts();
    let cola: String = sqlx::query_scalar("SELECT variant_id FROM product_variants LIMIT 1")
        .fetch_one(r.store.db().readers())
        .await
        .unwrap();
    let mut slowest = Duration::ZERO;
    for _ in 0..40 {
        let t = Instant::now();
        r.sell(&carts, cola.parse().unwrap(), 1).await;
        slowest = slowest.max(t.elapsed());
    }
    stop.store(true, Ordering::SeqCst);
    background.await.unwrap();
    eprintln!("slowest sale while the network flapped: {slowest:?}");
    assert!(slowest < Duration::from_secs(2), "a sale waited {slowest:?}");
    converge(&r.worker, &r.store).await;
    // 240 sales now: the 200 and the 40.
    let in_cloud: i64 = sqlx::query_scalar("SELECT count(*) FROM sales WHERE business_id = $1")
        .bind(business)
        .fetch_one(&cloud.pool)
        .await
        .unwrap();
    assert_eq!(in_cloud, SALES as i64 + 40);
}
