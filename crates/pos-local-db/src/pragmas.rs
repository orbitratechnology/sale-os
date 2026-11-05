use crate::StoreError;
use sqlx::SqlitePool;

/// Effective connection settings, as reported by SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PragmaReport {
    pub journal_mode: String,
    /// 0 = OFF, 1 = NORMAL, 2 = FULL, 3 = EXTRA.
    pub synchronous: i64,
    pub foreign_keys: bool,
    pub busy_timeout_ms: i64,
}

pub(crate) async fn read(pool: &SqlitePool) -> Result<PragmaReport, StoreError> {
    let mut conn = pool.acquire().await.map_err(StoreError::from_sqlx)?;
    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    let busy_timeout_ms: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    Ok(PragmaReport {
        journal_mode: journal_mode.to_lowercase(),
        synchronous,
        foreign_keys: foreign_keys == 1,
        busy_timeout_ms,
    })
}
