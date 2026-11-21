//! The register's local SQLite store (docs/backlog/04-local-database.md).
//!
//! - [`LocalDb::open`] applies the ADR-007 durability policy, refuses network paths,
//!   takes a pre-migration backup, applies migrations, and guards against downgrades.
//! - One writer connection (all business writes are serialised) plus a small pool of
//!   read-only connections for search and reports.

mod audit_queries;
pub mod backup;
mod cart_queries;
mod cash;
mod cash_queries;
mod catalog_queries;
mod cloud_report_cache;
mod config_queries;
mod conflict_queries;
pub mod crypto;
pub mod diagnostics;
mod error;
mod export_queries;
pub mod full_export;
mod import_queries;
mod link_store;
mod migrate;
mod payment_queries;
mod pragmas;
mod queries;
mod receipt_queries;
mod report_queries;
mod return_queries;
mod sales_queries;
mod stock_queries;
mod store;
mod sync_store;
pub use sync_store::{ConflictSummary, RepairEvent, SyncStatusRecord};

pub use error::StoreError;
pub use migrate::{MIGRATOR, MigrationReport};
pub use pragmas::PragmaReport;
pub use stock_queries::StartupStockCheck;
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
    /// The SQLCipher key (ADR-023). `None` writes plain SQLite (tests, tools); the app
    /// always passes one.
    pub key: Option<crypto::DbKey>,
}

impl DbOptions {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            path: dir.join("pos.db"),
            backups_dir: dir.join("backups"),
            reader_connections: 4,
            key: None,
        }
    }

    /// The same, encrypted with `key`.
    pub fn with_key(mut self, key: crypto::DbKey) -> Self {
        self.key = Some(key);
        self
    }
}

/// Open handle to the local database.
#[derive(Debug, Clone)]
pub struct LocalDb {
    writer: SqlitePool,
    readers: SqlitePool,
    path: PathBuf,
    backups_dir: PathBuf,
    migration: MigrationReport,
    key: Option<crypto::DbKey>,
}

/// Flushes a file we just wrote to the disk. Opened for writing: on Windows,
/// `FlushFileBuffers` refuses a read-only handle ("Access is denied").
pub(crate) fn sync_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new().write(true).open(path)?.sync_all()
}

/// On Windows another program (the virus scanner, search indexing) may hold a file we have
/// just written for a moment: replacing or deleting it then fails with "Access is denied"
/// (error 5), a sharing violation (32) or a lock violation (33).
fn held_by_another_program(e: &std::io::Error) -> bool {
    cfg!(windows)
        && (e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32 | 33)))
}

/// Runs `op` until it succeeds, fails for another reason, or `budget` has passed while the file
/// was held; logs once after the first second of waiting. The final error names the file.
fn while_held<T>(
    file: &Path,
    budget: Duration,
    mut op: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let started = std::time::Instant::now();
    let mut wait = Duration::from_millis(20);
    let mut warned = false;
    loop {
        match op() {
            Err(e) if held_by_another_program(&e) => {
                let waited = started.elapsed();
                if waited >= budget {
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!(
                            "{e}: {} was still held after {} s; another program (often the virus scanner or search indexing) was using it",
                            file.display(),
                            waited.as_secs()
                        ),
                    ));
                }
                if !warned && waited >= Duration::from_secs(1) {
                    warned = true;
                    tracing::warn!(file = %file.display(), "another program is holding the file; waiting");
                }
                std::thread::sleep(wait.min(budget.saturating_sub(waited)));
                wait = (wait * 2).min(Duration::from_secs(1));
            }
            other => return other,
        }
    }
}

/// Routine work while the till is selling (backups, exports) waits on the thread, so it gives a
/// held file about five seconds.
const ROUTINE_WAIT: Duration = Duration::from_secs(5);
/// Steps that run while nothing else does (encrypting at start-up, putting a restored database
/// in place) happen once and nothing waits on them: give a busy scanner up to 30 seconds.
const ONE_OFF_WAIT: Duration = Duration::from_secs(30);

/// Renames `from` over `to`, waiting out a held file (see `while_held`).
pub(crate) fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    while_held(to, ROUTINE_WAIT, || std::fs::rename(from, to))
}

/// `replace_file` for one-off steps: waits up to 30 s.
pub(crate) fn replace_file_patient(from: &Path, to: &Path) -> std::io::Result<()> {
    while_held(to, ONE_OFF_WAIT, || std::fs::rename(from, to))
}

/// Deletes `path` if it exists, for one-off steps: waits up to 30 s for a held file.
pub(crate) fn remove_file_patient(path: &Path) -> std::io::Result<()> {
    while_held(path, ONE_OFF_WAIT, || match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    })
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

/// Before opening with a key: a plain database (from before encryption) and the plain backups
/// beside it are encrypted in place; an encrypted one must open with the key. Without a key,
/// an encrypted database cannot be opened at all.
async fn prepare_encryption(opts: &DbOptions) -> Result<(), StoreError> {
    use crypto::FileKind;
    let kind = crypto::file_kind(&opts.path);
    let Some(key) = &opts.key else {
        return match kind {
            FileKind::Encrypted => Err(StoreError::Locked),
            _ => Ok(()),
        };
    };
    match kind {
        FileKind::Missing => Ok(()),
        FileKind::Encrypted => {
            if crypto::key_opens(&opts.path, key).await {
                Ok(())
            } else {
                Err(StoreError::Locked)
            }
        }
        FileKind::Plain => {
            crypto::encrypt_in_place(&opts.path, key).await?;
            let mut converted = 0;
            for b in backup::list(&opts.backups_dir) {
                if crypto::file_kind(&b.path) == FileKind::Plain {
                    match crypto::encrypt_in_place(&b.path, key).await {
                        Ok(()) => converted += 1,
                        Err(e) => {
                            tracing::warn!(error = %e, file = %b.path.display(), "a backup could not be encrypted")
                        }
                    }
                }
            }
            tracing::warn!(
                backups = converted,
                "the database was not encrypted: it is now (ADR-023)"
            );
            Ok(())
        }
    }
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
        prepare_encryption(opts).await?;
        let existed = opts.path.exists();
        let base = crypto::keyed(connect_options(&opts.path), &opts.path, opts.key.as_ref());

        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            // E19-T02: on a full disk (or an I/O error) SQLite rolls the transaction back by
            // itself; sqlx's own ROLLBACK then fails and it goes on counting a transaction
            // that no longer exists, so every later BEGIN on this, the only writing
            // connection, failed until the app restarted. A connection returned in that state
            // is closed, and the pool opens a fresh one.
            .after_release(|conn, _meta| {
                Box::pin(async move {
                    use sqlx::Connection;
                    // Queued after the dropped transaction's rollback: by the time it answers,
                    // that rollback has run.
                    conn.ping().await?;
                    let sound = !conn.is_in_transaction();
                    if !sound {
                        tracing::warn!("closing the database writer left inside a failed transaction");
                    }
                    Ok(sound)
                })
            })
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
            backups_dir: opts.backups_dir.clone(),
            migration,
            key: opts.key.clone(),
        })
    }

    /// The key the database is encrypted with, if it is (ADR-023).
    pub fn key(&self) -> Option<&crypto::DbKey> {
        self.key.as_ref()
    }

    /// The options this database was opened with (to close and reopen it).
    pub fn options(&self) -> DbOptions {
        DbOptions {
            path: self.path.clone(),
            backups_dir: self.backups_dir.clone(),
            reader_connections: 1,
            key: self.key.clone(),
        }
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

    /// Where automatic and manual backups go (E19-T03).
    pub fn backups_dir(&self) -> &Path {
        &self.backups_dir
    }

    /// Makes a backup now (E19-T03). Uses a read-only connection: sales carry on meanwhile.
    pub async fn backup(&self, kind: backup::BackupKind) -> Result<backup::BackupInfo, StoreError> {
        backup::create(&self.readers, &self.backups_dir, kind, self.key.as_ref()).await
    }

    /// The backups in [`LocalDb::backups_dir`], newest first.
    pub fn backups(&self) -> Vec<backup::BackupInfo> {
        backup::list(&self.backups_dir)
    }

    /// Changes since a backup show here: the last outbox sequence and the audit row count.
    pub async fn change_marker(&self) -> Result<(i64, i64), StoreError> {
        sqlx::query_as(
            "SELECT (SELECT coalesce(max(local_sequence), 0) FROM outbox_events),
                    (SELECT count(*) FROM audit_events)",
        )
        .fetch_one(&self.readers)
        .await
        .map_err(StoreError::from_sqlx)
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
