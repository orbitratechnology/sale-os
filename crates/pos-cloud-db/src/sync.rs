//! Sync ingestion storage (E17-T04, ADR-010): the per-register ack row and its lock, the
//! event inbox (idempotency) and the change feed. Every call runs in a [`TenantTx`].

use crate::tenant::TenantTx;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Locks the register's sync row for this transaction (creating it at 0 the first time) and
/// returns its ack. Two pushes from one register therefore apply one event at a time.
pub async fn lock_register(tx: &mut TenantTx<'_>, register_id: Uuid) -> Result<i64, sqlx::Error> {
    let business_id = *tx.business_id().as_uuid();
    sqlx::query(
        "INSERT INTO register_sync_state (business_id, register_id) VALUES ($1, $2)
         ON CONFLICT (business_id, register_id) DO NOTHING",
    )
    .bind(business_id)
    .bind(register_id)
    .execute(tx.conn())
    .await?;
    sqlx::query_scalar(
        "SELECT ack_sequence FROM register_sync_state WHERE business_id = $1 AND register_id = $2 FOR UPDATE",
    )
    .bind(business_id)
    .bind(register_id)
    .fetch_one(tx.conn())
    .await
}

/// The register's ack (0 before its first push), without locking.
pub async fn ack(tx: &mut TenantTx<'_>, register_id: Uuid) -> Result<i64, sqlx::Error> {
    let ack: Option<i64> = sqlx::query_scalar(
        "SELECT ack_sequence FROM register_sync_state WHERE business_id = $1 AND register_id = $2",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(register_id)
    .fetch_optional(tx.conn())
    .await?;
    Ok(ack.unwrap_or(0))
}

/// Moves the ack (only ever forwards: the caller holds the lock and checked `ack + 1`).
pub async fn set_ack(tx: &mut TenantTx<'_>, register_id: Uuid, sequence: i64) -> Result<(), sqlx::Error> {
    let done = sqlx::query(
        "UPDATE register_sync_state SET ack_sequence = $3, last_push_at = now()
          WHERE business_id = $1 AND register_id = $2 AND ack_sequence < $3",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(register_id)
    .bind(sequence)
    .execute(tx.conn())
    .await?;
    if done.rows_affected() == 1 {
        Ok(())
    } else {
        Err(sqlx::Error::Protocol(format!(
            "ack for register {register_id} would not move forward to {sequence}"
        )))
    }
}

/// What the inbox holds for an event already handled.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct InboxEntry {
    pub register_id: Uuid,
    pub local_sequence: i64,
    pub payload_sha256: Vec<u8>,
    pub result_status: String,
    pub result: serde_json::Value,
}

pub async fn inbox_entry(tx: &mut TenantTx<'_>, event_id: Uuid) -> Result<Option<InboxEntry>, sqlx::Error> {
    sqlx::query_as(
        "SELECT register_id, local_sequence, payload_sha256, result_status, result
           FROM event_inbox WHERE business_id = $1 AND event_id = $2",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(event_id)
    .fetch_optional(tx.conn())
    .await
}

/// A handled event, recorded in the same transaction as its outcome.
pub struct NewInboxEntry<'a> {
    pub event_id: Uuid,
    pub register_id: Uuid,
    pub local_sequence: i64,
    pub event_type: &'a str,
    pub schema_version: i32,
    pub occurred_at: DateTime<Utc>,
    pub payload: &'a serde_json::Value,
    pub payload_sha256: &'a [u8],
    pub result_status: &'a str,
    pub result: &'a serde_json::Value,
    pub aggregate_type: &'a str,
    pub aggregate_id: &'a str,
    pub base_version: Option<i64>,
}

pub async fn insert_inbox(tx: &mut TenantTx<'_>, e: &NewInboxEntry<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO event_inbox (business_id, event_id, register_id, local_sequence, event_type, schema_version,
                                  occurred_at, payload, payload_sha256, result_status, result, aggregate_type,
                                  aggregate_id, base_version)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(e.event_id)
    .bind(e.register_id)
    .bind(e.local_sequence)
    .bind(e.event_type)
    .bind(e.schema_version)
    .bind(e.occurred_at)
    .bind(e.payload)
    .bind(e.payload_sha256)
    .bind(e.result_status)
    .bind(e.result)
    .bind(e.aggregate_type)
    .bind(e.aggregate_id)
    .bind(e.base_version)
    .execute(tx.conn())
    .await
    .map(|_| ())
}

/// One row of the change feed.
pub struct NewChange<'a> {
    /// The event type and schema version the snapshot follows.
    pub event_type: &'a str,
    pub schema_version: i32,
    pub aggregate_type: &'a str,
    pub aggregate_id: &'a str,
    pub version: Option<i64>,
    /// `upsert`, `archive` or `fact`.
    pub kind: &'a str,
    pub origin_register: Option<Uuid>,
    pub origin_event_id: Option<Uuid>,
    pub snapshot: &'a serde_json::Value,
}

/// Appends to the change feed; returns its `server_seq`.
///
/// Visibility (E17-T07, option (a) of 05 C0003): appenders of one business take a
/// transaction-scoped advisory lock *before* drawing the next `server_seq`, and keep it until
/// they commit. So one business's rows commit in `server_seq` order, and a reader that has
/// seen row `n` will never later find a row below `n` appear: pulling `server_seq > cursor`
/// never skips a row.
pub async fn append_change(tx: &mut TenantTx<'_>, c: &NewChange<'_>) -> Result<i64, sqlx::Error> {
    let b = *tx.business_id().as_uuid();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('feed:' || $1::text, 0))")
        .bind(b)
        .execute(tx.conn())
        .await?;
    sqlx::query_scalar(
        "INSERT INTO change_log (business_id, aggregate_type, aggregate_id, version, change_kind,
                                 origin_register, origin_event_id, snapshot, event_type, schema_version)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING server_seq",
    )
    .bind(b)
    .bind(c.aggregate_type)
    .bind(c.aggregate_id)
    .bind(c.version)
    .bind(c.kind)
    .bind(c.origin_register)
    .bind(c.origin_event_id)
    .bind(c.snapshot)
    .bind(c.event_type)
    .bind(c.schema_version)
    .fetch_one(tx.conn())
    .await
}

/// One row of the feed, as a register pulls it.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct ChangeRow {
    pub server_seq: i64,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub version: Option<i64>,
    pub change_kind: String,
    pub origin_register: Option<Uuid>,
    pub event_type: String,
    pub schema_version: i32,
    pub snapshot: serde_json::Value,
}

/// Up to `limit` rows after `after`, in order.
pub async fn changes_after(
    tx: &mut TenantTx<'_>,
    after: i64,
    limit: i64,
) -> Result<Vec<ChangeRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT server_seq, aggregate_type, aggregate_id, version, change_kind, origin_register, event_type,
                schema_version, snapshot
           FROM change_log
          WHERE business_id = $1 AND server_seq > $2 AND event_type IS NOT NULL
          ORDER BY server_seq LIMIT $3",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(after)
    .bind(limit)
    .fetch_all(tx.conn())
    .await
}

/// The feed's current end for this business (0 when empty).
pub async fn feed_position(tx: &mut TenantTx<'_>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT coalesce(max(server_seq), 0) FROM change_log WHERE business_id = $1")
        .bind(*tx.business_id().as_uuid())
        .fetch_one(tx.conn())
        .await
}

/// How many feed rows came from one event (tests: exactly-once application).
pub async fn changes_from_event(tx: &mut TenantTx<'_>, event_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM change_log WHERE business_id = $1 AND origin_event_id = $2")
        .bind(*tx.business_id().as_uuid())
        .bind(event_id)
        .fetch_one(tx.conn())
        .await
}

/// One of a register's own events, as the inbox holds it (E19-T03).
pub struct OwnEvent {
    pub event_id: Uuid,
    pub local_sequence: i64,
    pub event_type: String,
    pub schema_version: i32,
    pub occurred_at: DateTime<Utc>,
    pub payload: serde_json::Value,
    pub result_status: String,
    pub aggregate_type: Option<String>,
    pub aggregate_id: Option<String>,
    pub base_version: Option<i64>,
}

/// A register's own events after `after`, in sequence order (at most `limit`).
pub async fn own_events(
    tx: &mut TenantTx<'_>,
    register_id: Uuid,
    after: i64,
    limit: i64,
) -> Result<Vec<OwnEvent>, sqlx::Error> {
    type Row = (
        Uuid,
        i64,
        String,
        i32,
        DateTime<Utc>,
        serde_json::Value,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT event_id, local_sequence, event_type, schema_version, occurred_at, payload, result_status,
                aggregate_type, aggregate_id, base_version
           FROM event_inbox
          WHERE business_id = $1 AND register_id = $2 AND local_sequence > $3
          ORDER BY local_sequence
          LIMIT $4",
    )
    .bind(*tx.business_id().as_uuid())
    .bind(register_id)
    .bind(after)
    .bind(limit)
    .fetch_all(tx.conn())
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                event_id,
                local_sequence,
                event_type,
                schema_version,
                occurred_at,
                payload,
                result_status,
                at,
                ai,
                bv,
            )| {
                OwnEvent {
                    event_id,
                    local_sequence,
                    event_type,
                    schema_version,
                    occurred_at,
                    payload,
                    result_status,
                    aggregate_type: at,
                    aggregate_id: ai,
                    base_version: bv,
                }
            },
        )
        .collect())
}
