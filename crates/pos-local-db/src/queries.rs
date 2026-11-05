//! Read-port implementations over the read-only connection pool.

use crate::SqliteStore;
use pos_application::queries::{QueryError, StaffQueries};
use pos_application::records::{StaffRecord, ThrottleRecord};
use pos_domain::{Timestamp, UserId};

type StaffRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    bool,
    String,
    i64,
    i64,
    i64,
);

const STAFF_COLUMNS: &str =
    "user_id, business_id, display_name, role, pin_hash, pin_updated_at, must_change_pin,
    status, version, created_at, updated_at";

fn q(e: impl std::fmt::Display) -> QueryError {
    QueryError(e.to_string())
}

fn staff_from(r: StaffRow) -> Result<StaffRecord, QueryError> {
    Ok(StaffRecord {
        user_id: r.0.parse().map_err(q)?,
        business_id: r.1.parse().map_err(q)?,
        display_name: r.2,
        role: r.3.parse().map_err(q)?,
        pin_hash: r.4,
        pin_updated_at: Timestamp::from_millis(r.5),
        must_change_pin: r.6,
        active: r.7 == "active",
        version: u64::try_from(r.8).map_err(q)?,
        created_at: Timestamp::from_millis(r.9),
        updated_at: Timestamp::from_millis(r.10),
    })
}

#[async_trait::async_trait]
impl StaffQueries for SqliteStore {
    async fn staff(&self, user_id: UserId) -> Result<Option<StaffRecord>, QueryError> {
        let row: Option<StaffRow> = sqlx::query_as(&format!(
            "SELECT {STAFF_COLUMNS} FROM staff_users WHERE user_id = ?"
        ))
        .bind(user_id.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(staff_from).transpose()
    }

    async fn active_staff(&self) -> Result<Vec<StaffRecord>, QueryError> {
        let rows: Vec<StaffRow> = sqlx::query_as(&format!(
            "SELECT {STAFF_COLUMNS} FROM staff_users WHERE status = 'active'
             ORDER BY CASE role WHEN 'owner' THEN 0 WHEN 'manager' THEN 1 ELSE 2 END, display_name COLLATE NOCASE"
        ))
        .fetch_all(self.db().readers())
        .await
        .map_err(q)?;
        rows.into_iter().map(staff_from).collect()
    }

    async fn throttle(&self, user_id: UserId) -> Result<Option<ThrottleRecord>, QueryError> {
        let row: Option<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT failed_attempts, locked_until, last_failed_at FROM staff_login_throttle WHERE user_id = ?",
        )
        .bind(user_id.to_string())
        .fetch_optional(self.db().readers())
        .await
        .map_err(q)?;
        row.map(|(failed, locked, last)| {
            Ok(ThrottleRecord {
                user_id,
                failed_attempts: u32::try_from(failed).map_err(q)?,
                locked_until: locked.map(Timestamp::from_millis),
                last_failed_at: last.map(Timestamp::from_millis),
            })
        })
        .transpose()
    }
}
