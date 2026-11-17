//! E14-T05: consolidated reports on the register. Fetched from the cloud and kept; shown
//! stale from what was kept while the cloud is out of reach (PRD §11.6); and every pull tells
//! the cloud how much this register still holds, which is what "complete" rests on.

mod common;

use pos_application::cloud_reports::{CloudReportService, Freshness};
use pos_application::sync::{SyncStore, SyncTransport, SyncWorker, TransportError};
use pos_domain::{BusinessDate, ErrorCode, HasErrorCode, Role, Timestamp};
use pos_local_db::SqliteStore;
use pos_sync::pull::ChangesResponse;
use pos_sync::reports::{Completeness, GroupBy, RegisterCompleteness, SalesReport, SalesReportRow};
use pos_sync::{PushRequest, PushResponse};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// A cloud that answers the report (or is offline), and notes what each pull reported.
struct Cloud {
    online: AtomicBool,
    unsent_seen: Mutex<Vec<Option<u64>>>,
    current: bool,
}

impl Cloud {
    fn new(current: bool) -> Arc<Self> {
        Arc::new(Self {
            online: AtomicBool::new(true),
            unsent_seen: Mutex::new(Vec::new()),
            current,
        })
    }
}

fn report(from: &str, to: &str, group_by: GroupBy, current: bool) -> SalesReport {
    let reg = |code: &str, current: bool| RegisterCompleteness {
        register_id: Uuid::now_v7(),
        code: code.into(),
        name: code.into(),
        last_ack_at: Some("2026-01-01T09:30:00Z".into()),
        pending_known: !current,
        current,
    };
    SalesReport {
        group_by,
        from: from.into(),
        to: to.into(),
        currency_code: "GBP".into(),
        currency_exponent: 2,
        rows: vec![SalesReportRow {
            key: from.into(),
            label: from.into(),
            sales_count: 3,
            gross_minor: 1200,
            tax_minor: 200,
            discount_minor: 0,
            returns_count: 1,
            refunds_minor: 100,
            net_minor: 1100,
        }],
        as_of: "2026-01-01T10:00:00Z".into(),
        completeness: Completeness {
            registers: vec![reg("R01", true), reg("R02", current)],
        },
    }
}

#[async_trait::async_trait]
impl SyncTransport for Cloud {
    async fn push(&self, _r: PushRequest) -> Result<PushResponse, TransportError> {
        Err(TransportError::Offline)
    }

    async fn changes(
        &self,
        cursor: Option<String>,
        _limit: u32,
        unsent: Option<u64>,
    ) -> Result<ChangesResponse, TransportError> {
        self.unsent_seen.lock().unwrap().push(unsent);
        Ok(ChangesResponse {
            changes: vec![],
            next_cursor: cursor.unwrap_or_else(|| "v1:0".into()),
            has_more: false,
        })
    }

    async fn sales_report(
        &self,
        from: &str,
        to: &str,
        group_by: GroupBy,
    ) -> Result<SalesReport, TransportError> {
        if self.online.load(Ordering::SeqCst) {
            Ok(report(from, to, group_by, self.current))
        } else {
            Err(TransportError::Offline)
        }
    }
}

async fn set_link(store: &SqliteStore, state: &str) {
    sqlx::query(
        "INSERT INTO sync_state (id, link_state) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET link_state = ?1",
    )
    .bind(state)
    .execute(store.db().writer())
    .await
    .unwrap();
}

fn day(s: &str) -> BusinessDate {
    s.parse().unwrap()
}

fn service(shop: &common::Shop, cloud: Arc<Cloud>) -> CloudReportService {
    CloudReportService {
        transport: cloud,
        sync: Arc::new(shop.store.clone()),
        cache: Arc::new(shop.store.clone()),
        clock: shop.clock.clone(),
    }
}

#[tokio::test]
async fn fetched_reports_are_kept_and_shown_stale_while_the_cloud_is_out_of_reach() {
    let shop = common::shop().await;
    let cloud = Cloud::new(false);
    let svc = service(&shop, cloud.clone());
    let (from, to) = (day("2026-01-01"), day("2026-01-07"));

    // Not linked: nothing to ask.
    let e = svc.sales(&shop.owner, from, to, GroupBy::Day).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::SyncNotLinked);
    set_link(&shop.store, "linked").await;

    // A cashier may not see reports at all, online or not.
    let e = svc
        .sales(&shop.as_role(Role::Cashier), from, to, GroupBy::Day)
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::PermissionDenied);

    // Online: R02 still holds data, so the figures are partial.
    let fetched = svc.sales(&shop.owner, from, to, GroupBy::Day).await.unwrap();
    assert_eq!(
        fetched.freshness,
        Freshness::Partial {
            unsynced: 1,
            last_synced: Some("2026-01-01T09:30:00Z".into())
        }
    );
    let fetched_at = fetched.fetched_at;

    // The cloud goes away; time passes. The same report comes back, stale, as it was.
    cloud.online.store(false, Ordering::SeqCst);
    shop.clock
        .set(Timestamp::from_millis(fetched_at.as_millis() + 3_600_000));
    let kept = svc.sales(&shop.owner, from, to, GroupBy::Day).await.unwrap();
    assert_eq!(
        kept.freshness,
        Freshness::Stale {
            reason: ErrorCode::SyncOffline
        }
    );
    assert_eq!(kept.fetched_at, fetched_at);
    assert_eq!(kept.report, fetched.report);

    // Nothing kept for another range or grouping: offline says so.
    for (f, t, by) in [
        (from, day("2026-01-08"), GroupBy::Day),
        (from, to, GroupBy::Tender),
    ] {
        let e = svc.sales(&shop.owner, f, t, by).await.unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::SyncOffline);
    }

    // Back online, fully synchronized now: the kept copy is replaced.
    let all = Cloud::new(true);
    let svc = service(&shop, all);
    let fresh = svc.sales(&shop.owner, from, to, GroupBy::Day).await.unwrap();
    assert_eq!(fresh.freshness, Freshness::FullySynchronized);
    let kept: (String, i64) = sqlx::query_as("SELECT report_json, fetched_at FROM cloud_report_cache")
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(kept.1, fresh.fetched_at.as_millis());
    assert!(kept.0.contains("\"current\":true") && !kept.0.contains("\"current\":false"));

    // Bad ranges are refused before anything is asked.
    let e = svc.sales(&shop.owner, to, from, GroupBy::Day).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
}

#[tokio::test]
async fn every_pull_tells_the_cloud_how_much_is_unsent() {
    let shop = common::shop().await;
    let s = &shop.store;
    set_link(s, "linked").await;
    let cloud = Cloud::new(true);
    let worker = SyncWorker::new(
        Arc::new(s.clone()),
        cloud.clone(),
        shop.clock.clone(),
        Arc::new(pos_application::sync::OsRandom),
    );
    let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox_events")
        .fetch_one(s.db().readers())
        .await
        .unwrap();
    assert!(outbox > 0, "setting up the workspace wrote events");
    worker.pull_once().await.unwrap();
    // Some pending, one refused: both count; accepted ones do not.
    sqlx::query(
        "UPDATE outbox_events SET status = CASE WHEN local_sequence = 1 THEN 'rejected' ELSE 'applied' END",
    )
    .execute(s.db().writer())
    .await
    .unwrap();
    worker.pull_once().await.unwrap();
    sqlx::query("UPDATE outbox_events SET status = 'applied'")
        .execute(s.db().writer())
        .await
        .unwrap();
    worker.pull_once().await.unwrap();
    assert_eq!(
        *cloud.unsent_seen.lock().unwrap(),
        [Some(u64::try_from(outbox).unwrap()), Some(1), Some(0)]
    );
    assert_eq!(s.pull_state().await.unwrap().unsent, 0);
}
