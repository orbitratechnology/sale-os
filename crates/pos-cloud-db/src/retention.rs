//! Deleting a cloud business (E19-T10). A request marks it `deleted` and revokes its tills;
//! after the grace period [`purge_due`] removes every row of it. The purge runs as
//! `pos_retention`, which sees only the business named in `app.purge_business_id`.

use crate::TenantTx;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Marks the business for deletion on `due`, and revokes every till and refresh token.
/// Returns false if it is not active (already requested).
pub async fn request_deletion(tx: &mut TenantTx<'_>, due: DateTime<Utc>) -> Result<bool, sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    let n = sqlx::query(
        "UPDATE businesses SET status = 'deleted', deletion_requested_at = now(), deletion_due_at = $2,
           updated_at = now()
         WHERE business_id = $1 AND status = 'active'",
    )
    .bind(id)
    .bind(due)
    .execute(tx.conn())
    .await?
    .rows_affected();
    if n == 0 {
        return Ok(false);
    }
    sqlx::query("UPDATE devices SET status = 'revoked', revoked_at = now() WHERE business_id = $1 AND status = 'active'")
        .bind(id)
        .execute(tx.conn())
        .await?;
    sqlx::query("UPDATE device_refresh_tokens SET used_at = coalesce(used_at, now()) WHERE business_id = $1")
        .bind(id)
        .execute(tx.conn())
        .await?;
    Ok(true)
}

/// Cancels a pending deletion. The tills stay revoked: they link again.
pub async fn cancel_deletion(tx: &mut TenantTx<'_>) -> Result<bool, sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    let n = sqlx::query(
        "UPDATE businesses SET status = 'active', deletion_requested_at = NULL, deletion_due_at = NULL,
           updated_at = now()
         WHERE business_id = $1 AND status = 'deleted' AND deletion_due_at > now()",
    )
    .bind(id)
    .execute(tx.conn())
    .await?
    .rows_affected();
    Ok(n == 1)
}

/// When the business is due to be deleted, if it is pending deletion.
pub async fn deletion_due(tx: &mut TenantTx<'_>) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    let id = *tx.business_id().as_uuid();
    sqlx::query_scalar("SELECT deletion_due_at FROM businesses WHERE business_id = $1 AND status = 'deleted'")
        .bind(id)
        .fetch_optional(tx.conn())
        .await
        .map(Option::flatten)
}

/// Removes every business whose deletion is due by `now`, each in its own transaction:
/// every row in every table with `business_id`. Returns the ids removed.
pub async fn purge_due(pool: &PgPool, now: DateTime<Utc>) -> Result<Vec<Uuid>, sqlx::Error> {
    let due: Vec<Uuid> = {
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL ROLE pos_retention")
            .execute(&mut *tx)
            .await?;
        let ids = sqlx::query_scalar("SELECT business_id FROM businesses WHERE deletion_due_at <= $1")
            .bind(now)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        ids
    };
    for id in &due {
        purge_one(pool, *id).await?;
    }
    Ok(due)
}

async fn purge_one(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL ROLE pos_retention")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('app.purge_business_id', $1, true)")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT c.table_name::text FROM information_schema.columns c
           JOIN information_schema.tables x USING (table_schema, table_name)
          WHERE c.table_schema = current_schema() AND c.column_name = 'business_id'
            AND x.table_type = 'BASE TABLE' ORDER BY 1",
    )
    .fetch_all(&mut *tx)
    .await?;
    // Children before parents: delete what can be deleted, pass after pass, until every
    // table is empty for this business (a foreign key refuses a parent until its children go).
    let mut left: Vec<String> = tables;
    for _ in 0..16 {
        let mut still = Vec::new();
        for t in &left {
            sqlx::query("SAVEPOINT purge").execute(&mut *tx).await?;
            let q = format!(
                "DELETE FROM \"{}\" WHERE business_id = $1",
                t.replace('"', "\"\"")
            );
            match sqlx::query(&q).bind(id).execute(&mut *tx).await {
                Ok(_) => {
                    sqlx::query("RELEASE SAVEPOINT purge").execute(&mut *tx).await?;
                }
                Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23503") => {
                    sqlx::query("ROLLBACK TO SAVEPOINT purge")
                        .execute(&mut *tx)
                        .await?;
                    still.push(t.clone());
                }
                Err(e) => return Err(e),
            }
        }
        if still.is_empty() {
            return tx.commit().await;
        }
        left = still;
    }
    Err(sqlx::Error::Protocol(format!(
        "could not delete business {id}: rows remain in {}",
        left.join(", ")
    )))
}
