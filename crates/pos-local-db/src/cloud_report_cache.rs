//! The consolidated reports kept on this register (E14-T05, `cloud_report_cache`).

use crate::SqliteStore;
use pos_application::cloud_reports::CloudReportCache;
use pos_application::queries::QueryError;
use pos_domain::Timestamp;

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

#[async_trait::async_trait]
impl CloudReportCache for SqliteStore {
    async fn cached_report(&self, key: &str) -> Result<Option<(String, Timestamp)>, QueryError> {
        let row: Option<(String, i64)> =
            sqlx::query_as("SELECT report_json, fetched_at FROM cloud_report_cache WHERE cache_key = ?")
                .bind(key)
                .fetch_optional(self.db().readers())
                .await
                .map_err(q)?;
        Ok(row.map(|(json, at)| (json, Timestamp::from_millis(at))))
    }

    async fn keep_report(&self, key: &str, json: &str, at: Timestamp) -> Result<(), QueryError> {
        sqlx::query(
            "INSERT INTO cloud_report_cache (cache_key, report_json, fetched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (cache_key) DO UPDATE SET report_json = ?2, fetched_at = ?3",
        )
        .bind(key)
        .bind(json)
        .bind(at.as_millis())
        .execute(self.db().writer())
        .await
        .map_err(q)?;
        Ok(())
    }
}
