//! E02-T03 acceptance tests for the single write path (ADR-008).

use pos_application::audit::{self, AuditEntry};
use pos_application::records::{BusinessRecord, LocationRecord, RegisterRecord, SettingRecord};
use pos_application::{Counter, LocalStore, NumberSlot, OutboxDraft, WriteContext, WriteError, WriteSet};
use pos_domain::{
    BusinessId, Currency, ErrorCode, HasErrorCode, IdGenerator, LocationId, RegisterId, Timestamp,
    UuidV7Generator,
};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const TABLES: &[&str] = &[
    "businesses",
    "locations",
    "registers",
    "business_settings",
    "register_counters",
    "audit_events",
    "outbox_events",
];

struct Fixture {
    _dir: TempDir,
    store: SqliteStore,
    ctx: WriteContext,
    ids: Arc<dyn IdGenerator>,
}

async fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let db = LocalDb::open(&DbOptions::in_dir(dir.path())).await.unwrap();
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidV7Generator);
    let ctx = WriteContext {
        business_id: BusinessId::generate(ids.as_ref()),
        location_id: LocationId::generate(ids.as_ref()),
        register_id: RegisterId::generate(ids.as_ref()),
        actor: None,
        occurred_at: Timestamp::from_millis(1_767_225_600_000),
    };
    Fixture {
        _dir: dir,
        store: SqliteStore::new(db),
        ctx,
        ids,
    }
}

fn workspace_set(f: &Fixture) -> WriteSet {
    let ids = f.ids.as_ref();
    let c = &f.ctx;
    let business = BusinessRecord {
        business_id: c.business_id,
        name: "Corner Shop".into(),
        legal_name: None,
        tax_registration_no: None,
        address: serde_json::json!({"line1": "1 High St"}),
        phone: None,
        email: None,
        logo_path: None,
        country_code: "GB".into(),
        timezone: "Europe/London".into(),
        currency: Currency::from_iso("GBP").unwrap(),
        prices_include_tax: true,
        version: 1,
        created_at: c.occurred_at,
        updated_at: c.occurred_at,
    };
    let location = LocationRecord {
        location_id: c.location_id,
        business_id: c.business_id,
        name: "Main".into(),
        address: serde_json::json!({}),
        archived: false,
        version: 1,
        created_at: c.occurred_at,
        updated_at: c.occurred_at,
    };
    let register = RegisterRecord {
        register_id: c.register_id,
        business_id: c.business_id,
        location_id: c.location_id,
        name: "Till 1".into(),
        code: "R01".into(),
        receipt_prefix: String::new(),
        retired: false,
        version: 1,
        created_at: c.occurred_at,
        updated_at: c.occurred_at,
    };
    let setting = setting(c, "inventory.allow_negative", 1);
    let business_event = OutboxDraft::business_updated(ids, &business).unwrap();
    let audit = AuditEntry::new(ids, audit::BUSINESS_CREATED, "business", c.business_id)
        .linked_to(business_event.event_id);
    WriteSet::builder(c.clone())
        .outbox(business_event)
        .outbox(OutboxDraft::location_upserted(ids, &location).unwrap())
        .outbox(OutboxDraft::register_upserted(ids, &register).unwrap())
        .outbox(OutboxDraft::setting_updated(ids, &setting).unwrap())
        .put_business(business, None)
        .put_location(location, None)
        .put_register(register, None)
        .put_setting(setting, None)
        .audit(audit)
        .build()
        .unwrap()
}

fn setting(c: &WriteContext, key: &str, version: u64) -> SettingRecord {
    SettingRecord {
        business_id: c.business_id,
        key: key.into(),
        value: serde_json::json!(true),
        version,
        updated_at: c.occurred_at,
    }
}

fn setting_set(f: &Fixture, key: &str, version: u64, expected: Option<u64>) -> WriteSet {
    let ids = f.ids.as_ref();
    let s = setting(&f.ctx, key, version);
    WriteSet::builder(f.ctx.clone())
        .outbox(OutboxDraft::setting_updated(ids, &s).unwrap())
        .put_setting(s, expected)
        .audit(AuditEntry::new(ids, audit::SETTINGS_UPDATED, "setting", key))
        .build()
        .unwrap()
}

async fn counts(store: &SqliteStore) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for t in TABLES {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {t}"))
            .fetch_one(store.db().readers())
            .await
            .unwrap();
        out.push((t.to_string(), n));
    }
    out
}

#[tokio::test]
async fn applies_everything_with_envelope_and_gapless_sequences() {
    let f = fixture().await;
    let applied = f.store.apply(workspace_set(&f)).await.unwrap();
    assert_eq!(
        applied.outbox.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );

    let rows: Vec<(String, String, String, i64, Option<i64>, String)> = sqlx::query_as(
        "SELECT event_type, business_id, register_id, local_sequence, base_version, status FROM outbox_events ORDER BY local_sequence",
    )
    .fetch_all(f.store.db().readers())
    .await
    .unwrap();
    let types: Vec<_> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        types,
        [
            "business.updated",
            "location.upserted",
            "register.upserted",
            "setting.updated"
        ]
    );
    for r in &rows {
        assert_eq!(r.1, f.ctx.business_id.to_string());
        assert_eq!(r.2, f.ctx.register_id.to_string());
        assert_eq!(r.4, Some(0), "creates carry base_version 0");
        assert_eq!(r.5, "pending");
    }

    // IDs round-trip through SQLite TEXT (E03-T01 open item).
    let stored: String = sqlx::query_scalar("SELECT business_id FROM businesses")
        .fetch_one(f.store.db().readers())
        .await
        .unwrap();
    assert_eq!(stored.parse::<BusinessId>().unwrap(), f.ctx.business_id);

    // Audit row is linked to its event and carries the context.
    let (action, link): (String, Option<String>) =
        sqlx::query_as("SELECT action, outbox_event_id FROM audit_events")
            .fetch_one(f.store.db().readers())
            .await
            .unwrap();
    assert_eq!(action, "business.created");
    assert_eq!(link.unwrap(), applied.outbox[0].0.to_string());
}

#[tokio::test]
async fn a_failure_before_any_statement_leaves_nothing_behind() {
    let f = fixture().await;
    let before = counts(&f.store).await;
    let mut statement = 0;
    loop {
        let faulty = f.store.clone().with_fault_before_statement(statement);
        match faulty.apply(workspace_set(&f)).await {
            Err(WriteError::FaultInjected { statement: s }) => {
                assert_eq!(s, statement);
                assert_eq!(
                    counts(&f.store).await,
                    before,
                    "partial write when failing at statement {statement}"
                );
                statement += 1;
            }
            Ok(_) => break,
            Err(other) => panic!("unexpected error at statement {statement}: {other}"),
        }
        assert!(statement < 100, "runaway");
    }
    // 4 records × 2 + 4 events × 2 + 1 audit + commit = 18 injection points, all exercised.
    assert_eq!(statement, 18);
    assert_ne!(counts(&f.store).await, before);
}

#[tokio::test]
async fn versions_are_checked_inside_the_transaction() {
    let f = fixture().await;
    f.store.apply(workspace_set(&f)).await.unwrap();
    let before = counts(&f.store).await;

    // Creating again: the record exists at version 1.
    let err = f
        .store
        .apply(setting_set(&f, "inventory.allow_negative", 1, None))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            WriteError::VersionConflict {
                expected: None,
                actual: Some(1),
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(err.error_code(), ErrorCode::ConflictVersion);

    // Editing from a stale version.
    let err = f
        .store
        .apply(setting_set(&f, "inventory.allow_negative", 3, Some(2)))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        WriteError::VersionConflict {
            expected: Some(2),
            actual: Some(1),
            ..
        }
    ));
    assert_eq!(counts(&f.store).await, before, "conflicts write nothing");

    // Editing from the current version.
    f.store
        .apply(setting_set(&f, "inventory.allow_negative", 2, Some(1)))
        .await
        .unwrap();
    let v: i64 =
        sqlx::query_scalar("SELECT version FROM business_settings WHERE key = 'inventory.allow_negative'")
            .fetch_one(f.store.db().readers())
            .await
            .unwrap();
    assert_eq!(v, 2);
}

#[tokio::test]
async fn numbers_are_allocated_per_register_inside_the_transaction() {
    let f = fixture().await;
    f.store.apply(workspace_set(&f)).await.unwrap();
    for expected in 1..=3u64 {
        let ws = WriteSet::builder(f.ctx.clone())
            .allocate_number(Counter::Receipt, NumberSlot("receipt"))
            .audit(AuditEntry::new(
                f.ids.as_ref(),
                audit::SALE_COMPLETED,
                "sale",
                "x",
            ))
            .build()
            .unwrap();
        let applied = f.store.apply(ws).await.unwrap();
        assert_eq!(applied.numbers[&NumberSlot("receipt")], expected);
    }
    // A failed apply does not consume a number (no gaps).
    let ws = WriteSet::builder(f.ctx.clone())
        .allocate_number(Counter::Receipt, NumberSlot("receipt"))
        .audit(AuditEntry::new(
            f.ids.as_ref(),
            audit::SALE_COMPLETED,
            "sale",
            "x",
        ))
        .build()
        .unwrap();
    assert!(
        f.store
            .clone()
            .with_fault_before_statement(1)
            .apply(ws.clone())
            .await
            .is_err()
    );
    assert_eq!(
        f.store.apply(ws).await.unwrap().numbers[&NumberSlot("receipt")],
        4
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_applies_produce_gapless_unique_sequences() {
    let f = fixture().await;
    f.store.apply(workspace_set(&f)).await.unwrap();
    let per_task = std::env::var("POS_APPLY_STRESS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(250usize);
    let tasks = 8;
    let f = Arc::new(f);
    let mut handles = Vec::new();
    for t in 0..tasks {
        let f = f.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..per_task {
                f.store
                    .apply(setting_set(&f, &format!("stress.{t}.{i}"), 1, None))
                    .await
                    .unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let total = 4 + (tasks * per_task) as i64;
    let (count, min, max, distinct): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*), min(local_sequence), max(local_sequence), count(DISTINCT local_sequence) FROM outbox_events",
    )
    .fetch_one(f.store.db().readers())
    .await
    .unwrap();
    assert_eq!(
        (count, min, max, distinct),
        (total, 1, total, total),
        "sequences must be 1..=N without gaps"
    );
    let counter: i64 = sqlx::query_scalar("SELECT value FROM register_counters WHERE name = 'outbox_seq'")
        .fetch_one(f.store.db().readers())
        .await
        .unwrap();
    assert_eq!(counter, total);
}

#[tokio::test]
async fn change_notices_follow_commits_only() {
    let f = fixture().await;
    let mut rx = f.store.subscribe();
    assert!(
        f.store
            .clone()
            .with_fault_before_statement(0)
            .apply(workspace_set(&f))
            .await
            .is_err()
    );
    assert!(rx.try_recv().is_err(), "no notice for a failed apply");
    f.store.apply(workspace_set(&f)).await.unwrap();
    let notice = rx.try_recv().unwrap();
    assert!(notice.outbox_appended);
    assert_eq!(notice.aggregates.len(), 4);
}

#[tokio::test]
async fn a_cancelled_caller_cannot_leave_a_partial_write() {
    let f = fixture().await;
    let before = counts(&f.store).await;
    // Drop the apply future almost immediately.
    let _ = tokio::time::timeout(Duration::from_micros(1), f.store.apply(workspace_set(&f))).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = counts(&f.store).await;
    let all_or_nothing = after == before || after.iter().all(|(t, n)| *n > 0 || t == "register_counters");
    assert!(all_or_nothing, "partial state after cancellation: {after:?}");
    // The writer connection is still usable afterwards.
    f.store
        .apply(setting_set(&f, "after.cancel", 1, None))
        .await
        .unwrap_or_else(|e| {
            // If the cancelled apply did commit, the workspace exists; otherwise we need it first.
            panic!("writer unusable after cancellation: {e}")
        });
}

#[tokio::test]
async fn apply_latency_budget() {
    let f = fixture().await;
    f.store.apply(workspace_set(&f)).await.unwrap();
    // A 20-record set approximates a 20-line sale in statement count until E09-T08 lands.
    let mut samples = Vec::new();
    for i in 0..100 {
        let mut b = WriteSet::builder(f.ctx.clone());
        for j in 0..20 {
            let s = setting(&f.ctx, &format!("bench.{i}.{j}"), 1);
            b = b
                .outbox(OutboxDraft::setting_updated(f.ids.as_ref(), &s).unwrap())
                .put_setting(s, None);
        }
        let ws = b
            .audit(AuditEntry::new(
                f.ids.as_ref(),
                audit::SETTINGS_UPDATED,
                "setting",
                "bench",
            ))
            .build()
            .unwrap();
        let t = Instant::now();
        f.store.apply(ws).await.unwrap();
        samples.push(t.elapsed());
    }
    samples.sort();
    let p95 = samples[94];
    eprintln!("apply p95 for 20 records + 20 events: {p95:?}");
    // E02-T03 budget is 50 ms on CI hardware; debug builds get 4x headroom.
    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(200)
    } else {
        Duration::from_millis(50)
    };
    assert!(p95 < budget, "p95 {p95:?} exceeds {budget:?}");
}
