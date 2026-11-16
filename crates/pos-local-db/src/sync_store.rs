//! The outbox and sync bookkeeping for the push worker (E17-T03). Recording a push writes
//! only local-only tables (`outbox_events` status columns, `sync_conflicts`, `sync_state`), in
//! one short transaction, so it never waits behind or holds up a sale for long.

use crate::SqliteStore;
use pos_application::queries::QueryError;
use pos_application::sync::{
    InboundOutcome, InboundReport, PullState, PushRecord, PushState, SyncStore, inbound_master,
    inbound_movements, master_table,
};
use pos_application::{AppliedWriteSet, WriteContext};
use pos_domain::Timestamp;
use pos_sync::pull::{BootstrapPage, ChangeKind, ChangesResponse};
use pos_sync::{EventEnvelope, EventResult, ResultStatus};
use sqlx::Row;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

/// Bytes an envelope adds beyond its payload, for the batch size limit (ids, type, times and
/// JSON punctuation; generous).
const ENVELOPE_OVERHEAD: usize = 512;

fn seq(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

#[async_trait::async_trait]
impl SyncStore for SqliteStore {
    async fn push_state(&self) -> Result<PushState, QueryError> {
        let r = self.db().readers();
        let register: Option<String> =
            sqlx::query_scalar("SELECT register_id FROM device_identity WHERE id = 1")
                .fetch_optional(r)
                .await
                .map_err(q)?;
        let state: Option<(String, i64)> =
            sqlx::query_as("SELECT link_state, last_ack_sequence FROM sync_state WHERE id = 1")
                .fetch_optional(r)
                .await
                .map_err(q)?;
        let max: i64 = sqlx::query_scalar("SELECT coalesce(max(local_sequence), 0) FROM outbox_events")
            .fetch_one(r)
            .await
            .map_err(q)?;
        let (link, ack) = state.unwrap_or_else(|| ("unlinked".into(), 0));
        Ok(PushState {
            linked: link == "linked" && register.is_some(),
            register_id: register
                .as_deref()
                .unwrap_or_default()
                .parse()
                .unwrap_or_default(),
            last_ack_sequence: u64::try_from(ack).unwrap_or(0),
            max_sequence: u64::try_from(max).unwrap_or(0),
        })
    }

    async fn outbox_batch(
        &self,
        after: u64,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<Vec<EventEnvelope>, QueryError> {
        let rows = sqlx::query(
            "SELECT event_id, business_id, location_id, register_id, local_sequence, event_type, schema_version,
                    aggregate_type, aggregate_id, base_version, occurred_at, payload_json
               FROM outbox_events WHERE local_sequence > ? ORDER BY local_sequence LIMIT ?",
        )
        .bind(seq(after))
        .bind(i64::try_from(max_events).unwrap_or(i64::MAX))
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        let mut out = Vec::with_capacity(rows.len());
        let mut bytes = 0;
        for r in rows {
            let payload: String = r.try_get(11).map_err(q)?;
            bytes += payload.len() + ENVELOPE_OVERHEAD;
            if bytes > max_bytes && !out.is_empty() {
                break;
            }
            let id = |i: usize| -> Result<uuid::Uuid, QueryError> {
                r.try_get::<String, _>(i).map_err(q)?.parse().map_err(q)
            };
            out.push(EventEnvelope {
                event_id: id(0)?,
                business_id: id(1)?,
                location_id: id(2)?,
                register_id: id(3)?,
                local_sequence: u64::try_from(r.try_get::<i64, _>(4).map_err(q)?).map_err(q)?,
                event_type: r.try_get(5).map_err(q)?,
                schema_version: u32::try_from(r.try_get::<i64, _>(6).map_err(q)?).map_err(q)?,
                aggregate_type: r.try_get(7).map_err(q)?,
                aggregate_id: r.try_get(8).map_err(q)?,
                base_version: r
                    .try_get::<Option<i64>, _>(9)
                    .map_err(q)?
                    .map(u64::try_from)
                    .transpose()
                    .map_err(q)?,
                occurred_at: Timestamp::from_millis(r.try_get(10).map_err(q)?).to_rfc3339(),
                payload: serde_json::from_str(&payload).map_err(q)?,
            });
        }
        Ok(out)
    }

    async fn record_push(&self, record: &PushRecord) -> Result<(), QueryError> {
        let at = record.at.as_millis();
        // Prepared outside the transaction, applied as one statement: the writer is held
        // for as short a time as possible.
        let mut updates = Vec::with_capacity(record.results.len());
        let mut conflicts = Vec::new();
        for r in &record.results {
            let (status, conflict) = outcome(r, record.ack_sequence);
            if conflict {
                conflicts.push(r);
            }
            if status.is_some() || matches!(r.status, ResultStatus::Transient | ResultStatus::Rejected) {
                updates.push(serde_json::json!({
                    "id": r.event_id.to_string(),
                    "status": status,
                    // The repair queue and blocked events show why; handled events need none.
                    "code": if status.is_none() || status == Some("rejected") { r.code.clone() } else { None },
                    "result": r,
                }));
            }
        }
        let updates = serde_json::Value::Array(updates).to_string();
        let mut tx = self.db().writer().begin().await.map_err(q)?;
        sqlx::query(
            "UPDATE outbox_events SET
                    status = coalesce(json_extract(u.value, '$.status'), outbox_events.status),
                    handled_at = CASE WHEN json_extract(u.value, '$.status') IS NULL THEN outbox_events.handled_at ELSE ?1 END,
                    attempt_count = outbox_events.attempt_count + 1,
                    last_error_code = json_extract(u.value, '$.code'),
                    server_result_json = json_extract(u.value, '$.result')
               FROM json_each(?2) u
              WHERE outbox_events.event_id = json_extract(u.value, '$.id')",
        )
        .bind(at)
        .bind(&updates)
        .execute(&mut *tx)
        .await
        .map_err(q)?;
        for r in conflicts {
            record_conflict(&mut tx, r, at).await?;
        }
        sqlx::query(
            "INSERT INTO sync_state (id, last_ack_sequence, last_push_ok_at, server_clock_skew_ms, last_error_code)
             VALUES (1, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET last_ack_sequence = excluded.last_ack_sequence,
               last_push_ok_at = excluded.last_push_ok_at,
               server_clock_skew_ms = coalesce(excluded.server_clock_skew_ms, sync_state.server_clock_skew_ms),
               last_error_code = excluded.last_error_code, last_error_message = NULL",
        )
        .bind(seq(record.ack_sequence))
        .bind(at)
        .bind(record.clock_skew_ms)
        .bind(&record.blocking_code)
        .execute(&mut *tx)
        .await
        .map_err(q)?;
        tx.commit().await.map_err(q)
    }

    async fn pull_state(&self) -> Result<PullState, QueryError> {
        let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT link_state, last_pull_cursor, bootstrap_page FROM sync_state WHERE id = 1",
        )
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        let (link, cursor, bootstrap_page) = row.unwrap_or_else(|| ("unlinked".into(), None, None));
        Ok(PullState {
            linked: link == "linked",
            cursor,
            bootstrap_page,
        })
    }

    async fn apply_changes(
        &self,
        page: &ChangesResponse,
        at: Timestamp,
    ) -> Result<InboundReport, QueryError> {
        let mut tx = self
            .db()
            .writer()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(q)?;
        let here = Here::load(&mut tx, at).await?;
        let mut report = InboundReport::default();
        for c in &page.changes {
            let seen: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM inbound_changes WHERE server_seq = ?)")
                    .bind(c.server_seq)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(q)?;
            if seen {
                report.already += 1;
                continue;
            }
            let outcome = if c.origin_register_id == Some(here.register) {
                InboundOutcome::SkippedOwn
            } else if c.kind == ChangeKind::Fact {
                let origin = c
                    .origin_register_id
                    .map_or_else(|| "cloud".to_owned(), |r| r.to_string());
                apply_fact(&mut tx, &c.event_type, c.schema_version, &c.snapshot, &origin, at).await?
            } else {
                let version = c
                    .version
                    .ok_or_else(|| QueryError(format!("{} has no version", c.event_type)))?;
                apply_master(
                    &mut tx,
                    &here,
                    &c.aggregate_type,
                    &c.aggregate_id,
                    &c.event_type,
                    c.schema_version,
                    version,
                    &c.snapshot,
                )
                .await?
            };
            report.add(outcome);
            sqlx::query(
                "INSERT INTO inbound_changes (server_seq, aggregate_type, aggregate_id, version, applied_at, outcome)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(c.server_seq)
            .bind(&c.aggregate_type)
            .bind(&c.aggregate_id)
            .bind(c.version.map(seq))
            .bind(at.as_millis())
            .bind(outcome.as_str())
            .execute(&mut *tx)
            .await
            .map_err(q)?;
        }
        sqlx::query(
            "INSERT INTO sync_state (id, last_pull_cursor, last_pull_ok_at) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET last_pull_cursor = ?1, last_pull_ok_at = ?2",
        )
        .bind(&page.next_cursor)
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(q)?;
        tx.commit().await.map_err(q)?;
        Ok(report)
    }

    async fn apply_bootstrap(
        &self,
        page: &BootstrapPage,
        at: Timestamp,
    ) -> Result<InboundReport, QueryError> {
        let mut tx = self
            .db()
            .writer()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(q)?;
        let here = Here::load(&mut tx, at).await?;
        let mut report = InboundReport::default();
        for e in &page.entities {
            let outcome = apply_master(
                &mut tx,
                &here,
                &e.aggregate_type,
                &e.aggregate_id,
                &e.event_type,
                e.schema_version,
                e.version,
                &e.snapshot,
            )
            .await?;
            report.add(outcome);
        }
        // Resume from the next page; after the last, pull the feed from the snapshot's start.
        match &page.next_page {
            Some(next) => sqlx::query(
                "INSERT INTO sync_state (id, bootstrap_page) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET bootstrap_page = ?1",
            )
            .bind(next),
            None => sqlx::query(
                "INSERT INTO sync_state (id, bootstrap_page, last_pull_cursor, last_pull_ok_at) VALUES (1, NULL, ?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET bootstrap_page = NULL, last_pull_cursor = ?1, last_pull_ok_at = ?2",
            )
            .bind(&page.snapshot_cursor)
            .bind(at.as_millis()),
        }
        .execute(&mut *tx)
        .await
        .map_err(q)?;
        tx.commit().await.map_err(q)?;
        Ok(report)
    }

    async fn record_push_failure(&self, code: &str, _at: Timestamp) -> Result<(), QueryError> {
        sqlx::query(
            "INSERT INTO sync_state (id, last_error_code) VALUES (1, ?)
             ON CONFLICT (id) DO UPDATE SET last_error_code = excluded.last_error_code, last_error_message = NULL",
        )
        .bind(code)
        .execute(self.db().writer())
        .await
        .map(|_| ())
        .map_err(q)
    }
}

/// The outbox status a result leads to (`None`: unchanged), and whether it is a conflict.
fn outcome(r: &EventResult, ack: u64) -> (Option<&'static str>, bool) {
    // A duplicate stands for its first outcome.
    let outcome = match (r.status, r.original_status) {
        (ResultStatus::Duplicate, Some(original)) => original,
        (status, _) => status,
    };
    let status = match outcome {
        ResultStatus::Applied if r.status == ResultStatus::Duplicate => Some("duplicate"),
        ResultStatus::Applied => Some("applied"),
        ResultStatus::Conflict => Some("conflict"),
        // Rejected and acknowledged: the repair queue. Not acknowledged (an unsupported
        // version, a reused id): it stays pending and is sent again.
        ResultStatus::Rejected if r.local_sequence <= ack => Some("rejected"),
        _ => None,
    };
    (status, outcome == ResultStatus::Conflict)
}

/// A conflict row for the conflict screens (E18), once per event.
async fn record_conflict(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    r: &EventResult,
    at: i64,
) -> Result<(), QueryError> {
    let id = r.event_id.to_string();
    let server =
        serde_json::to_string(r.server_snapshot.as_ref().unwrap_or(&serde_json::Value::Null)).map_err(q)?;
    sqlx::query(
        "INSERT INTO sync_conflicts (conflict_id, aggregate_type, aggregate_id, outbox_event_id, local_payload_json,
                                     server_payload_json, local_base_version, server_version, created_at)
         SELECT ?1, o.aggregate_type, o.aggregate_id, o.event_id, o.payload_json, ?2, coalesce(o.base_version, 0), ?3, ?4
           FROM outbox_events o
          WHERE o.event_id = ?5
            AND NOT EXISTS (SELECT 1 FROM sync_conflicts c WHERE c.outbox_event_id = o.event_id)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(server)
    .bind(r.server_version.map_or(0, |v| i64::try_from(v).unwrap_or(i64::MAX)))
    .bind(at)
    .bind(&id)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(q)
}

/// This register, for applying pulled changes.
pub(crate) struct Here {
    pub(crate) register: uuid::Uuid,
    pub(crate) ctx: WriteContext,
    /// Events up to here are acknowledged; later ones are edits the cloud has not seen.
    pub(crate) ack: i64,
}

impl Here {
    async fn load(tx: &mut sqlx::SqliteConnection, at: Timestamp) -> Result<Self, QueryError> {
        let (business, register, location): (String, String, String) = sqlx::query_as(
            "SELECT d.business_id, d.register_id, r.location_id FROM device_identity d
               JOIN registers r ON r.register_id = d.register_id WHERE d.id = 1",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(q)?;
        let ack: Option<i64> = sqlx::query_scalar("SELECT last_ack_sequence FROM sync_state WHERE id = 1")
            .fetch_optional(&mut *tx)
            .await
            .map_err(q)?;
        Ok(Self {
            register: register.parse().map_err(q)?,
            ctx: WriteContext {
                business_id: business.parse().map_err(q)?,
                location_id: location.parse().map_err(q)?,
                register_id: register.parse().map_err(q)?,
                actor: None,
                occurred_at: at,
            },
            ack: ack.unwrap_or(0),
        })
    }
}

/// A master-data change: applied when newer than the local copy and the local copy has no
/// unacknowledged edit (docs/backlog/04, inbound application rule); otherwise held or skipped.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_master(
    tx: &mut sqlx::SqliteConnection,
    here: &Here,
    aggregate_type: &str,
    aggregate_id: &str,
    event_type: &str,
    schema_version: u32,
    version: u64,
    snapshot: &serde_json::Value,
) -> Result<InboundOutcome, QueryError> {
    let (table, column) = master_table(aggregate_type)
        .ok_or_else(|| QueryError(format!("unknown aggregate type {aggregate_type}")))?;
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM outbox_events WHERE aggregate_type = ? AND aggregate_id = ? AND local_sequence > ?)",
    )
    .bind(aggregate_type)
    .bind(aggregate_id)
    .bind(here.ack)
    .fetch_one(&mut *tx)
    .await
    .map_err(q)?;
    if pending {
        return Ok(InboundOutcome::ConflictPending);
    }
    let local: Option<i64> = sqlx::query_scalar(&format!("SELECT version FROM {table} WHERE {column} = ?"))
        .bind(aggregate_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(q)?;
    let local = local.map(|v| u64::try_from(v).unwrap_or(0));
    if local.is_some_and(|l| version <= l) {
        return Ok(InboundOutcome::SkippedOlder);
    }
    let op = inbound_master(event_type, schema_version, snapshot, local, here.ctx.business_id).map_err(q)?;
    // A local rule the cloud does not know (say, a barcode a local product holds) refuses it:
    // undo just this change and hold it for the conflict screens (E18).
    sqlx::query("SAVEPOINT inbound")
        .execute(&mut *tx)
        .await
        .map_err(q)?;
    let mut applied = AppliedWriteSet::default();
    let mut st = crate::store::Statements::unlimited();
    match crate::store::run_ops(
        &mut *tx,
        &here.ctx,
        std::slice::from_ref(&op),
        &mut st,
        &mut applied,
    )
    .await
    {
        Ok(()) => {
            sqlx::query("RELEASE inbound")
                .execute(&mut *tx)
                .await
                .map_err(q)?;
            Ok(InboundOutcome::Applied)
        }
        Err(e) => {
            tracing::warn!(error = %e, aggregate_type, "a pulled change could not be applied here; held");
            sqlx::query("ROLLBACK TO inbound")
                .execute(&mut *tx)
                .await
                .map_err(q)?;
            sqlx::query("RELEASE inbound")
                .execute(&mut *tx)
                .await
                .map_err(q)?;
            Ok(InboundOutcome::ConflictPending)
        }
    }
}

/// Another register's fact: its stock movements join the ledger once each, and move the
/// balances (docs/backlog/04: "Pulled facts from other registers").
async fn apply_fact(
    tx: &mut sqlx::SqliteConnection,
    event_type: &str,
    schema_version: u32,
    snapshot: &serde_json::Value,
    origin: &str,
    at: Timestamp,
) -> Result<InboundOutcome, QueryError> {
    use pos_sync::events::MovementSource;
    for m in inbound_movements(event_type, schema_version, snapshot).map_err(q)? {
        let (source_type, source_id, line) = match &m.source {
            MovementSource::Sale { sale_id, line_id } => ("sale", *sale_id, Some(*line_id)),
            MovementSource::Return { return_id, line_id } => ("return", *return_id, Some(*line_id)),
            MovementSource::InventoryDocument { document_id } => ("inventory_document", *document_id, None),
        };
        let tag = |v: &dyn erased::Tag| v.tag();
        let business: String = sqlx::query_scalar("SELECT business_id FROM device_identity WHERE id = 1")
            .fetch_one(&mut *tx)
            .await
            .map_err(q)?;
        let inserted = sqlx::query(
            "INSERT INTO stock_movements (movement_id, business_id, location_id, variant_id, bucket, movement_type,
               quantity_delta_milli, unit_cost_minor, source_type, source_id, source_line_id, register_id,
               actor_user_id, occurred_at, business_date, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (movement_id) DO NOTHING",
        )
        .bind(m.id.to_string())
        .bind(&business)
        .bind(m.location_id.to_string())
        .bind(m.variant_id.to_string())
        .bind(tag(&m.bucket))
        .bind(tag(&m.movement_type))
        .bind(m.delta)
        .bind(m.unit_cost)
        .bind(source_type)
        .bind(source_id.to_string())
        .bind(line.map(|l| l.to_string()))
        .bind(origin)
        .bind(m.actor.to_string())
        .bind(m.occurred_at)
        .bind(&m.business_date)
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(q)?
        .rows_affected();
        if inserted == 1 {
            sqlx::query(
                "INSERT INTO stock_balances (business_id, location_id, variant_id, bucket, on_hand_milli,
                   last_movement_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (location_id, variant_id, bucket) DO UPDATE SET
                   on_hand_milli = on_hand_milli + ?5,
                   last_movement_at = max(last_movement_at, ?6), updated_at = ?7",
            )
            .bind(&business)
            .bind(m.location_id.to_string())
            .bind(m.variant_id.to_string())
            .bind(tag(&m.bucket))
            .bind(m.delta)
            .bind(m.occurred_at)
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(q)?;
        }
    }
    Ok(InboundOutcome::Applied)
}

/// The wire name of an enum value (`"return_restock"`).
mod erased {
    pub trait Tag {
        fn tag(&self) -> String;
    }
    impl<T: serde::Serialize> Tag for T {
        fn tag(&self) -> String {
            match serde_json::to_value(self) {
                Ok(serde_json::Value::String(s)) => s,
                Ok(other) => other.to_string(),
                Err(_) => String::new(),
            }
        }
    }
}

/// `sync_state` columns the status reads.
type StatusRow = (String, i64, Option<i64>, Option<i64>, Option<String>, Option<i64>);

/// What the sync status shows (E17-T08/T09).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStatusRecord {
    pub link_state: String,
    pub pending: u64,
    pub last_push_ok_at: Option<i64>,
    pub last_pull_ok_at: Option<i64>,
    pub last_error_code: Option<String>,
    pub clock_skew_ms: Option<i64>,
    pub register_code: Option<String>,
}

impl SqliteStore {
    pub async fn sync_status(&self) -> Result<SyncStatusRecord, QueryError> {
        let r = self.db().readers();
        let row: Option<StatusRow> = sqlx::query_as(
            "SELECT link_state, last_ack_sequence, last_push_ok_at, last_pull_ok_at, last_error_code, server_clock_skew_ms
               FROM sync_state WHERE id = 1",
        )
        .fetch_optional(r)
        .await
        .map_err(q)?;
        let (link_state, ack, push, pull, error, skew) =
            row.unwrap_or_else(|| ("unlinked".into(), 0, None, None, None, None));
        let max: i64 = sqlx::query_scalar("SELECT coalesce(max(local_sequence), 0) FROM outbox_events")
            .fetch_one(r)
            .await
            .map_err(q)?;
        let code: Option<String> = sqlx::query_scalar(
            "SELECT g.code FROM device_identity d JOIN registers g ON g.register_id = d.register_id WHERE d.id = 1",
        )
        .fetch_optional(r)
        .await
        .map_err(q)?;
        Ok(SyncStatusRecord {
            link_state,
            pending: u64::try_from(max - ack).unwrap_or(0),
            last_push_ok_at: push,
            last_pull_ok_at: pull,
            last_error_code: error,
            clock_skew_ms: skew,
            register_code: code,
        })
    }
}
