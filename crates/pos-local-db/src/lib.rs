//! The register's local SQLite store (docs/backlog/04-local-database.md).
//!
//! - [`LocalDb::open`] applies the ADR-007 durability policy, refuses network paths,
//!   takes a pre-migration backup, applies migrations, and guards against downgrades.
//! - One writer connection (all business writes are serialised) plus a small pool of
//!   read-only connections for search and reports.

mod error;
mod migrate;
mod pragmas;
mod store;

pub use error::StoreError;
pub use migrate::{MIGRATOR, MigrationReport};
pub use pragmas::PragmaReport;
pub use store::SqliteStore;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How to open the local database.
#[derive(Debug, Clone)]
pub struct DbOptions {
    /// Database file, normally `<app data>/pos.db`.
    pub path: PathBuf,
    /// Directory for automatic backups, normally `<app data>/backups`.
    pub backups_dir: PathBuf,
    /// Number of read-only connections (default 4).
    pub reader_connections: u32,
}

impl DbOptions {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            path: dir.join("pos.db"),
            backups_dir: dir.join("backups"),
            reader_connections: 4,
        }
    }
}

/// Open handle to the local database.
#[derive(Debug, Clone)]
pub struct LocalDb {
    writer: SqlitePool,
    readers: SqlitePool,
    path: PathBuf,
    migration: MigrationReport,
}

pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

pub(crate) fn connect_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Wal)
        // FULL, not NORMAL: in WAL mode NORMAL may lose the last commits on power loss,
        // and a completed sale must survive (ADR-007).
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT)
}

impl LocalDb {
    /// Opens (creating if needed) and migrates the database with the embedded migrations.
    pub async fn open(opts: &DbOptions) -> Result<Self, StoreError> {
        Self::open_with_migrator(opts, &MIGRATOR).await
    }

    /// Like [`LocalDb::open`] with an explicit migrator (used by migration tests).
    pub async fn open_with_migrator(opts: &DbOptions, migrator: &Migrator) -> Result<Self, StoreError> {
        refuse_network_path(&opts.path)?;
        if let Some(parent) = opts.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::io("create database directory", e))?;
        }
        let existed = opts.path.exists();
        let base = connect_options(&opts.path);

        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .connect_with(base.clone().create_if_missing(true))
            .await
            .map_err(StoreError::from_sqlx)?;

        let migration = match migrate::run(&writer, migrator, &opts.path, &opts.backups_dir, existed).await {
            Ok(report) => report,
            Err(failure) => {
                writer.close().await;
                if !matches!(failure.error, StoreError::SchemaTooNew { .. }) {
                    migrate::recover_after_failure(
                        &opts.path,
                        existed,
                        failure.backup.as_deref(),
                        &failure.error,
                    );
                }
                return Err(failure.error);
            }
        };

        let readers = SqlitePoolOptions::new()
            .max_connections(opts.reader_connections.max(1))
            .connect_with(base.read_only(true))
            .await
            .map_err(StoreError::from_sqlx)?;

        tracing::info!(
            path = %opts.path.display(),
            schema_version = migration.to_version,
            applied = migration.applied.len(),
            "local database ready"
        );
        Ok(Self {
            writer,
            readers,
            path: opts.path.clone(),
            migration,
        })
    }

    /// The single writer connection. All business writes go through `LocalStore::apply`
    /// (E02-T03), which uses this pool.
    pub fn writer(&self) -> &SqlitePool {
        &self.writer
    }

    /// Read-only connections for queries.
    pub fn readers(&self) -> &SqlitePool {
        &self.readers
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What happened during [`LocalDb::open`].
    pub fn migration_report(&self) -> &MigrationReport {
        &self.migration
    }

    /// Current schema version (highest applied migration).
    pub async fn schema_version(&self) -> Result<i64, StoreError> {
        migrate::current_version(&self.readers).await
    }

    /// The register this installation is, once set up (`device_identity`, docs/backlog/04).
    pub async fn device_register_id(&self) -> Result<Option<String>, StoreError> {
        sqlx::query_scalar("SELECT register_id FROM device_identity WHERE id = 1")
            .fetch_optional(&self.readers)
            .await
            .map(Option::flatten)
            .map_err(StoreError::from_sqlx)
    }

    /// Effective pragmas on a writer and a reader connection (diagnostics and tests).
    pub async fn pragma_report(&self) -> Result<(PragmaReport, PragmaReport), StoreError> {
        Ok((
            pragmas::read(&self.writer).await?,
            pragmas::read(&self.readers).await?,
        ))
    }

    /// Checkpoints the WAL and closes all connections.
    pub async fn close(self) {
        if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.writer)
            .await
        {
            tracing::warn!(error = %e, "checkpoint on close failed");
        }
        self.readers.close().await;
        self.writer.close().await;
    }
}

/// A live SQLite database must never sit on a network share (PRD §9.1): locking and fsync
/// semantics there are unreliable and corruption is likely.
fn refuse_network_path(path: &Path) -> Result<(), StoreError> {
    let s = path.to_string_lossy();
    let unc = s.starts_with(r"\\") || s.starts_with("//");
    if unc {
        return Err(StoreError::NetworkPathRefused(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unc_paths_are_refused() {
        for p in [
            r"\\server\share\pos.db",
            "//server/share/pos.db",
            r"\\?\UNC\server\share\pos.db",
        ] {
            assert!(
                matches!(
                    refuse_network_path(Path::new(p)),
                    Err(StoreError::NetworkPathRefused(_))
                ),
                "{p}"
            );
        }
        assert!(refuse_network_path(Path::new("/home/u/.local/share/pos.db")).is_ok());
        assert!(refuse_network_path(Path::new(r"C:\Users\u\AppData\Local\pos.db")).is_ok());
    }
}
