//! Tenant-scoped transactions (E16-T02, ADR-016). Every query against tenant data runs in a
//! [`TenantTx`]: it switches to the `pos_app` role (no `BYPASSRLS`, owns nothing) and sets
//! `app.business_id` as its first statements, so PostgreSQL's row-level security limits it to
//! one business whatever the query says. Repositories take a `TenantTx`, never a pool, so
//! the setting cannot be skipped:
//!
//! ```compile_fail
//! # async fn f(pool: &sqlx::PgPool) {
//! // A repository cannot be called with a bare pool.
//! let _ = pos_cloud_db::repo::business(pool).await;
//! # }
//! ```

use pos_domain::BusinessId;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

/// Who is acting: the business their token is for. Built only from a verified token (the
/// API layer), never from a request body.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub business_id: BusinessId,
}

/// A transaction limited to one business by row-level security.
pub struct TenantTx<'p> {
    tx: Transaction<'p, Postgres>,
    business_id: BusinessId,
}

impl<'p> TenantTx<'p> {
    /// Begins a transaction as `pos_app` with `app.business_id` set to the principal's.
    pub async fn begin(pool: &'p PgPool, principal: &Principal) -> Result<Self, sqlx::Error> {
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL ROLE pos_app").execute(&mut *tx).await?;
        sqlx::query("SELECT set_config('app.business_id', $1, true)")
            .bind(principal.business_id.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(Self {
            tx,
            business_id: principal.business_id,
        })
    }

    pub fn business_id(&self) -> BusinessId {
        self.business_id
    }

    /// The connection, for repositories in this crate only.
    pub(crate) fn conn(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    /// The connection, for tests that prove raw SQL is limited too. Not for production code.
    #[doc(hidden)]
    pub fn test_conn(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    /// Marks a point to undo back to (one at a time): sync ingestion undoes a rejected
    /// event's partial writes but still records the rejection in the same transaction.
    pub async fn savepoint(&mut self) -> Result<(), sqlx::Error> {
        sqlx::query("SAVEPOINT tenant_sp")
            .execute(&mut *self.tx)
            .await
            .map(|_| ())
    }

    /// Undoes everything since [`savepoint`](Self::savepoint), keeping the transaction.
    pub async fn rollback_to_savepoint(&mut self) -> Result<(), sqlx::Error> {
        sqlx::query("ROLLBACK TO SAVEPOINT tenant_sp")
            .execute(&mut *self.tx)
            .await
            .map(|_| ())
    }

    pub async fn release_savepoint(&mut self) -> Result<(), sqlx::Error> {
        sqlx::query("RELEASE SAVEPOINT tenant_sp")
            .execute(&mut *self.tx)
            .await
            .map(|_| ())
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.tx.commit().await
    }

    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.tx.rollback().await
    }
}

/// A transaction for one owner across the businesses they belong to: `app.account_id` is
/// set (from a verified user token only), which read-only policies use to show their
/// memberships, businesses, registers and devices. It cannot write tenant rows.
pub struct AccountTx<'p> {
    tx: Transaction<'p, Postgres>,
}

impl<'p> AccountTx<'p> {
    pub async fn begin(pool: &'p PgPool, account_id: uuid::Uuid) -> Result<Self, sqlx::Error> {
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL ROLE pos_app").execute(&mut *tx).await?;
        sqlx::query("SELECT set_config('app.account_id', $1, true)")
            .bind(account_id.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(Self { tx })
    }

    pub(crate) fn conn(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.tx.commit().await
    }
}
