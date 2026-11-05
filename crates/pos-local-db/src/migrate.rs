//! Migration runner with pre-migration backup, failure recovery and a downgrade guard
//! (E02-T02). Migrations are append-only: once merged to `main`, a file is never edited.

use crate::StoreError;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The embedded migrations shipped with this build.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// What [`crate::LocalDb::open`] did to the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub from_version: i64,
    pub to_version: i64,
    pub applied: Vec<i64>,
    /// Backup taken before applying migrations to a database that already held data.
    pub backup: Option<PathBuf>,
}

pub(crate) struct MigrationFailure {
    pub error: StoreError,
    pub backup: Option<PathBuf>,
}

async fn applied_versions(pool: &SqlitePool) -> Result<Vec<i64>, StoreError> {
    let has_table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .map_err(StoreError::from_sqlx)?;
    if has_table == 0 {
        return Ok(Vec::new());
    }
    sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1 ORDER BY version")
        .fetch_all(pool)
        .await
        .map_err(StoreError::from_sqlx)
}

pub(crate) async fn current_version(pool: &SqlitePool) -> Result<i64, StoreError> {
    Ok(applied_versions(pool).await?.into_iter().max().unwrap_or(0))
}

pub(crate) async fn run(
    writer: &SqlitePool,
    migrator: &Migrator,
    db_path: &Path,
    backups_dir: &Path,
    existed: bool,
) -> Result<MigrationReport, MigrationFailure> {
    let fail = |error: StoreError| MigrationFailure { error, backup: None };
    let applied = applied_versions(writer).await.map_err(fail)?;
    let known: BTreeSet<i64> = migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| m.version)
        .collect();
    let from_version = applied.iter().copied().max().unwrap_or(0);
    let app_version = known.iter().copied().max().unwrap_or(0);

    // Downgrade guard: refuse before touching anything.
    if applied.iter().any(|v| !known.contains(v)) {
        return Err(fail(StoreError::SchemaTooNew {
            db_version: from_version,
            app_version,
        }));
    }

    let pending: Vec<i64> = known.iter().copied().filter(|v| !applied.contains(v)).collect();
    if pending.is_empty() {
        return Ok(MigrationReport {
            from_version,
            to_version: from_version,
            applied: vec![],
            backup: None,
        });
    }

    let backup = if existed && !applied.is_empty() {
        Some(
            backup_before_migration(writer, db_path, backups_dir, from_version, app_version)
                .await
                .map_err(fail)?,
        )
    } else {
        None
    };

    tracing::info!(
        from_version,
        to_version = app_version,
        ?pending,
        "applying migrations"
    );
    if let Err(e) = migrator.run(writer).await {
        return Err(MigrationFailure {
            error: StoreError::Migration(e.to_string()),
            backup,
        });
    }
    Ok(MigrationReport {
        from_version,
        to_version: app_version,
        applied: pending,
        backup,
    })
}

async fn backup_before_migration(
    writer: &SqlitePool,
    db_path: &Path,
    backups_dir: &Path,
    from: i64,
    to: i64,
) -> Result<PathBuf, StoreError> {
    std::fs::create_dir_all(backups_dir).map_err(|e| StoreError::io("create backups directory", e))?;
    let stem = db_path.file_stem().and_then(|s| s.to_str()).unwrap_or("pos");
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let target = backups_dir.join(format!("{stem}.pre-migrate-v{from}-v{to}-{millis}.db"));
    // VACUUM INTO writes a consistent, compacted copy including committed WAL content.
    sqlx::query("VACUUM INTO ?")
        .bind(target.to_string_lossy().into_owned())
        .execute(writer)
        .await
        .map_err(StoreError::from_sqlx)?;
    tracing::info!(backup = %target.display(), "pre-migration backup written");
    Ok(target)
}

/// Called after the writer pool is closed. Restores the pre-migration backup so a failed
/// upgrade leaves the original database as it was; removes a database that this run created.
pub(crate) fn recover_after_failure(db_path: &Path, existed: bool, backup: Option<&Path>, err: &StoreError) {
    let sidecars = [sidecar(db_path, "-wal"), sidecar(db_path, "-shm")];
    if let Some(backup) = backup {
        tracing::error!(error = %err, backup = %backup.display(), "migration failed; restoring backup");
        for f in &sidecars {
            let _ = std::fs::remove_file(f);
        }
        if let Err(e) = std::fs::copy(backup, db_path) {
            tracing::error!(error = %e, "restoring pre-migration backup failed; the backup file is kept");
        }
    } else if !existed {
        tracing::error!(error = %err, "migration of a new database failed; removing it");
        let _ = std::fs::remove_file(db_path);
        for f in &sidecars {
            let _ = std::fs::remove_file(f);
        }
    }
}

fn sidecar(db_path: &Path, suffix: &str) -> PathBuf {
    let mut s = db_path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}
