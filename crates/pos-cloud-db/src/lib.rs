//! Cloud PostgreSQL access (docs/backlog/05-cloud-database.md).
//!
//! The connection helper, the migration runner, tenant-scoped transactions ([`TenantTx`],
//! row-level security; E16-T02) and the repositories that use them.

pub mod accounts;
pub mod devices;
pub mod repo;
pub mod tenant;

pub use tenant::{AccountTx, Principal, TenantTx};

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

/// Embedded cloud migrations, applied by `cloud-api migrate` as the owner role.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Connects a pool. Connections are established lazily so the API can start (and report
/// itself unhealthy) while the database is unavailable.
pub fn connect_lazy(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy(database_url)
}

/// Applies pending migrations.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

/// Cheap liveness probe of the database.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
        .map(|_| ())
}
