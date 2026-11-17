//! Conflict reads (E18).

use crate::SqliteStore;
use pos_application::conflict::{ConflictQueries, ConflictRecord};
use pos_application::queries::QueryError;
use pos_application::sync::master_table;
use pos_domain::Timestamp;
use sqlx::Row;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

const COLUMNS: &str = "c.conflict_id, c.aggregate_type, c.aggregate_id, c.local_payload_json, c.server_payload_json,
   c.local_base_version, c.server_version, coalesce(json_extract(o.server_result_json, '$.code'), 'VERSION'), c.created_at
   FROM sync_conflicts c JOIN outbox_events o ON o.event_id = c.outbox_event_id";

fn record(r: &sqlx::sqlite::SqliteRow) -> Result<ConflictRecord, QueryError> {
    let json = |i: usize| -> Result<serde_json::Value, QueryError> {
        serde_json::from_str(&r.try_get::<String, _>(i).map_err(q)?).map_err(q)
    };
    let version = |i: usize| -> Result<u64, QueryError> {
        u64::try_from(r.try_get::<i64, _>(i).map_err(q)?).map_err(q)
    };
    Ok(ConflictRecord {
        conflict_id: r.try_get(0).map_err(q)?,
        aggregate_type: r.try_get(1).map_err(q)?,
        aggregate_id: r.try_get(2).map_err(q)?,
        local: json(3)?,
        server: json(4)?,
        local_base_version: version(5)?,
        server_version: version(6)?,
        reason: r.try_get(7).map_err(q)?,
        created_at: Timestamp::from_millis(r.try_get(8).map_err(q)?),
    })
}

#[async_trait::async_trait]
impl ConflictQueries for SqliteStore {
    async fn open_conflicts(&self) -> Result<Vec<ConflictRecord>, QueryError> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} WHERE c.status = 'open' ORDER BY c.created_at"
        ))
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?
        .iter()
        .map(record)
        .collect()
    }

    async fn open_conflict(&self, conflict_id: &str) -> Result<Option<ConflictRecord>, QueryError> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} WHERE c.status = 'open' AND c.conflict_id = ?"
        ))
        .bind(conflict_id)
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?
        .as_ref()
        .map(record)
        .transpose()
    }

    async fn local_version(
        &self,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> Result<Option<u64>, QueryError> {
        let Some((table, column)) = master_table(aggregate_type) else {
            return Ok(None);
        };
        let v: Option<i64> = sqlx::query_scalar(&format!("SELECT version FROM {table} WHERE {column} = ?"))
            .bind(aggregate_id)
            .fetch_optional(self.db().readers())
            .await
            .map_err(q)?;
        Ok(v.and_then(|v| u64::try_from(v).ok()))
    }
}
