//! The diagnostics bundle (E19-T07, PRD §14 Observability): what support needs to see why a
//! till misbehaves, in one ZIP the owner can send, and nothing more.
//!
//! `diagnostics.json` holds the database's state: schema version, `integrity_check`, stock
//! projection drift, the sync state and outbox/inbox counts, print jobs, the business
//! settings, and the last 100 audit actions **as identifiers only** (no metadata, no names).
//! `logs/` holds the last 7 days of logs, already redacted when they were written; the caller
//! passes them in (masked once more).
//!
//! Never in it: PIN hashes, tokens, event payloads, sale lines, customer data. The sync state
//! leaves out the error message (it can quote a server reply).

use crate::full_export::Zip;
use crate::{LocalDb, StoreError};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::{BufWriter, Write};
use std::path::Path;

/// What the app knows about itself (the database cannot tell).
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppFacts {
    pub app_version: String,
    pub os: String,
    pub webview_version: Option<String>,
    pub encrypted: bool,
    pub created_at_ms: i64,
}

/// One log file, by name, already redacted.
pub struct LogFile {
    pub name: String,
    pub text: String,
}

/// What a bundle held, for the audit row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleSummary {
    pub files: usize,
    pub integrity_ok: bool,
    pub drift: usize,
}

async fn rows(db: &LocalDb, sql: &str) -> Result<Vec<Value>, StoreError> {
    // Each query returns one JSON object per row (json_object), so no column is guessed at.
    let v: Vec<String> = sqlx::query_scalar(sql)
        .fetch_all(db.readers())
        .await
        .map_err(StoreError::from_sqlx)?;
    Ok(v.into_iter()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect())
}

/// `PRAGMA integrity_check` on a connection of its own. A pooled reader can hold FTS5 state
/// from before the last write, and then reports the search index as malformed when it is not.
pub async fn integrity_check(db: &LocalDb) -> Result<Vec<String>, StoreError> {
    use sqlx::{ConnectOptions, Connection};
    let mut c = crate::crypto::keyed(
        crate::connect_options(db.path()).read_only(true),
        db.path(),
        db.key(),
    )
    .connect()
    .await
    .map_err(StoreError::from_sqlx)?;
    let rows: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut c)
        .await
        .map_err(StoreError::from_sqlx)?;
    c.close().await.map_err(StoreError::from_sqlx)?;
    Ok(rows)
}

/// The database part of the bundle.
pub async fn database_facts(db: &LocalDb) -> Result<Value, StoreError> {
    let integrity = integrity_check(db).await?;
    let schema_version = db.schema_version().await?;
    let drift = rows(
        db,
        "WITH m AS (SELECT location_id, variant_id, bucket, sum(quantity_delta_milli) q
                    FROM stock_movements GROUP BY 1, 2, 3)
         SELECT json_object('variantId', m.variant_id, 'bucket', m.bucket, 'movementsMilli', m.q,
                            'balanceMilli', coalesce(b.on_hand_milli, 0))
         FROM m LEFT JOIN stock_balances b USING (location_id, variant_id, bucket)
         WHERE coalesce(b.on_hand_milli, 0) <> m.q LIMIT 100",
    )
    .await
    .unwrap_or_default();
    Ok(json!({
        "schemaVersion": schema_version,
        "integrityCheck": integrity,
        "stockDrift": drift,
        "sync": rows(db, "SELECT json_object('linkState', link_state, 'deviceId', device_id,
              'lastAckSequence', last_ack_sequence, 'lastPushOkAt', last_push_ok_at,
              'lastPullOkAt', last_pull_ok_at, 'lastErrorCode', last_error_code,
              'serverClockSkewMs', server_clock_skew_ms, 'replayAfter', replay_after,
              'bootstrapping', bootstrap_page IS NOT NULL)
              FROM sync_state").await?,
        "outbox": rows(db, "SELECT json_object('status', status, 'events', count(*), 'lastSequence', max(local_sequence))
              FROM outbox_events GROUP BY status").await?,
        "outboxByType": rows(db, "SELECT json_object('eventType', event_type, 'events', count(*))
              FROM outbox_events GROUP BY event_type").await?,
        "inbound": rows(db, "SELECT json_object('outcome', outcome, 'changes', count(*))
              FROM inbound_changes GROUP BY outcome").await?,
        "conflicts": rows(db, "SELECT json_object('status', status, 'conflicts', count(*))
              FROM sync_conflicts GROUP BY status").await.unwrap_or_default(),
        "printJobs": rows(db, "SELECT json_object('status', status, 'jobs', count(*))
              FROM print_jobs GROUP BY status").await?,
        "printJobsNotDone": rows(db, "SELECT json_object('jobId', job_id, 'status', status,
              'createdAt', created_at) FROM print_jobs
              WHERE status IN ('queued', 'submitted', 'failed', 'unknown')
              ORDER BY created_at DESC LIMIT 20").await?,
        "counts": rows(db, "SELECT json_object(
              'products', (SELECT count(*) FROM products),
              'variants', (SELECT count(*) FROM product_variants),
              'sales', (SELECT count(*) FROM sales),
              'returns', (SELECT count(*) FROM returns),
              'staff', (SELECT count(*) FROM staff_users),
              'openCarts', (SELECT count(*) FROM carts WHERE status = 'open'),
              'parkedCarts', (SELECT count(*) FROM carts WHERE status = 'parked'))").await?,
        "settings": rows(db, "SELECT json_object('key', key, 'value', json(value_json))
              FROM business_settings ORDER BY key").await?,
        "recentAudit": rows(db, "SELECT json_object('auditId', audit_id, 'action', action,
              'entityType', entity_type, 'entityId', entity_id, 'actorUserId', actor_user_id,
              'occurredAt', occurred_at)
              FROM audit_events ORDER BY occurred_at DESC, audit_id DESC LIMIT 100").await?,
    }))
}

/// Writes the bundle to `path` (under a temporary name, renamed when whole).
pub async fn write(
    db: &LocalDb,
    path: &Path,
    app: &AppFacts,
    logs: &[LogFile],
) -> Result<BundleSummary, StoreError> {
    let facts = database_facts(db).await?;
    let integrity_ok = facts["integrityCheck"] == json!(["ok"]);
    let drift = facts["stockDrift"].as_array().map_or(0, Vec::len);
    let partial = path.with_extension("zip.partial");
    let io = |e: std::io::Error| StoreError::io("diagnostics", e);
    let file = std::fs::File::create(&partial).map_err(io)?;
    let mut zip = Zip::new(BufWriter::new(file));
    let body = serde_json::to_vec_pretty(&json!({ "app": app, "database": facts }))
        .map_err(|e| StoreError::io("diagnostics", e))?;
    zip.start("diagnostics.json")?;
    zip.write_all(&body).map_err(io)?;
    zip.finish_entry()?;
    for l in logs {
        zip.start(&format!("logs/{}", l.name))?;
        zip.write_all(l.text.as_bytes()).map_err(io)?;
        zip.finish_entry()?;
    }
    let mut out = zip.close()?;
    out.flush().map_err(io)?;
    drop(out);
    crate::sync_file(&partial).map_err(io)?;
    crate::replace_file(&partial, path).map_err(io)?;
    Ok(BundleSummary {
        files: 1 + logs.len(),
        integrity_ok,
        drift,
    })
}
