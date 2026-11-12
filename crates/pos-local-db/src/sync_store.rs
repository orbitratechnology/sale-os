//! The outbox and sync bookkeeping for the push worker (E17-T03). Recording a push writes
//! only local-only tables (`outbox_events` status columns, `sync_conflicts`, `sync_state`), in
//! one short transaction, so it never waits behind or holds up a sale for long.

use crate::SqliteStore;
use pos_application::queries::QueryError;
use pos_application::sync::{PushRecord, PushState, SyncStore};
use pos_domain::Timestamp;
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
