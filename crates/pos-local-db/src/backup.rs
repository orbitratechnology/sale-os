//! Local backups and restore (E19-T03, ADR-007, PRD §13 and §18).
//!
//! A backup is a consistent, compacted copy made with `VACUUM INTO` from a read-only
//! connection (sales are never blocked), next to a small JSON description: when, why, the
//! schema version, the last outbox sequence and how many sales it holds. Each kind keeps its
//! own number of copies (ADR-007: hourly 24, daily 14).
//!
//! A restore replaces the database file while it is closed. The current database is kept
//! first as a `pre-restore` backup, so a restore can itself be undone.

use crate::crypto::DbKey;
use crate::{DbOptions, MIGRATOR, StoreError};
use serde::{Deserialize, Serialize};
use sqlx::{ConnectOptions, Connection};
use std::path::{Path, PathBuf};

/// Why a backup was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    /// Every hour there were changes.
    Hourly,
    /// Once a day.
    Daily,
    /// When the app closes normally.
    Shutdown,
    /// Before the schema is upgraded.
    PreMigration,
    /// The owner asked for one.
    Manual,
    /// The database as it was just before a restore replaced it.
    PreRestore,
}

impl BackupKind {
    pub const ALL: [BackupKind; 6] = [
        Self::Hourly,
        Self::Daily,
        Self::Shutdown,
        Self::PreMigration,
        Self::Manual,
        Self::PreRestore,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Shutdown => "shutdown",
            Self::PreMigration => "pre_migration",
            Self::Manual => "manual",
            Self::PreRestore => "pre_restore",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// How many copies of this kind are kept (ADR-007 for hourly and daily).
    pub const fn keep(self) -> usize {
        match self {
            Self::Hourly => 24,
            Self::Daily => 14,
            Self::Shutdown | Self::PreRestore => 5,
            Self::PreMigration => 10,
            Self::Manual => 20,
        }
    }
}

/// The description written next to each backup (`<backup>.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupMeta {
    pub kind: BackupKind,
    /// Milliseconds since the epoch.
    pub created_at: i64,
    pub schema_version: i64,
    /// The last outbox sequence in the copy (0 when empty).
    pub outbox_sequence: i64,
    pub sales: i64,
    pub app_version: String,
}

/// A backup on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupInfo {
    /// The file name, which is also how the UI names it (never a path).
    pub id: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub meta: BackupMeta,
}

/// What a full check of a backup found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verification {
    /// `None` when `integrity_check` says ok.
    pub problem: Option<String>,
    pub schema_version: i64,
    /// Written by a newer app than this one: it cannot be restored here.
    pub too_new: bool,
}

impl Verification {
    pub fn restorable(&self) -> bool {
        self.problem.is_none() && !self.too_new
    }
}

/// What a restore did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreReport {
    pub restored: BackupMeta,
    /// The database as it was, kept as a backup (`None` if there was none).
    pub kept: Option<BackupInfo>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn io(context: &'static str) -> impl Fn(std::io::Error) -> StoreError {
    move |e| StoreError::io(context, e)
}

fn meta_path(backup: &Path) -> PathBuf {
    let mut p = backup.as_os_str().to_owned();
    p.push(".json");
    PathBuf::from(p)
}

/// A read-only connection to a database file that is not the live one.
async fn open_file(path: &Path, key: Option<&DbKey>) -> Result<sqlx::SqliteConnection, StoreError> {
    // An encrypted file the key does not open reads as "not a database": say what it is.
    if crate::crypto::file_kind(path) == crate::crypto::FileKind::Encrypted {
        match key {
            Some(k) if crate::crypto::key_opens(path, k).await => {}
            _ => return Err(StoreError::Locked),
        }
    }
    crate::crypto::keyed(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .read_only(true),
        path,
        key,
    )
    .connect()
    .await
    .map_err(StoreError::from_sqlx)
}

/// A private copy of a database file that someone else may have written (a backup on a USB
/// stick), removed when dropped. RUSTSEC-2026-0459 (SQLite before 3.54) can leak process
/// memory when a database is opened with a crafted sibling `-wal` file. A backup is one
/// self-contained file (`VACUUM INTO`), so only that file is copied, into a new folder of our
/// own: a `-wal` placed next to the original is never read.
struct Staged {
    dir: PathBuf,
    file: PathBuf,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stage(path: &Path) -> Result<Staged, StoreError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sale-os-stage-{}-{}-{}",
        std::process::id(),
        now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    // `create_dir`, not `create_dir_all`: the folder must be new.
    std::fs::create_dir(&dir).map_err(io("stage backup"))?;
    let staged = Staged {
        file: dir.join("backup.db"),
        dir,
    };
    std::fs::copy(path, &staged.file).map_err(io("stage backup"))?;
    Ok(staged)
}

/// Reads what a database file holds, for its description.
async fn describe(
    path: &Path,
    kind: BackupKind,
    created_at: i64,
    key: Option<&DbKey>,
) -> Result<BackupMeta, StoreError> {
    let mut c = open_file(path, key).await?;
    // A scalar from `table`, or 0 if the file has no such table (an empty or very old one).
    async fn scalar(c: &mut sqlx::SqliteConnection, table: &str, sql: &str) -> Result<i64, StoreError> {
        let exists: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(table)
                .fetch_one(&mut *c)
                .await
                .map_err(StoreError::from_sqlx)?;
        if exists == 0 {
            return Ok(0);
        }
        sqlx::query_scalar(sql)
            .fetch_one(&mut *c)
            .await
            .map_err(StoreError::from_sqlx)
    }
    let schema_version = scalar(
        &mut c,
        "_sqlx_migrations",
        "SELECT coalesce(max(version), 0) FROM _sqlx_migrations WHERE success = 1",
    )
    .await?;
    let outbox_sequence = scalar(
        &mut c,
        "outbox_events",
        "SELECT coalesce(max(local_sequence), 0) FROM outbox_events",
    )
    .await?;
    let sales = scalar(&mut c, "sales", "SELECT count(*) FROM sales").await?;
    c.close().await.map_err(StoreError::from_sqlx)?;
    Ok(BackupMeta {
        kind,
        created_at,
        schema_version,
        outbox_sequence,
        sales,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

fn write_meta(backup: &Path, meta: &BackupMeta) -> Result<(), StoreError> {
    let text = serde_json::to_string_pretty(meta).map_err(|e| StoreError::Io {
        context: "describe backup",
        message: e.to_string(),
    })?;
    std::fs::write(meta_path(backup), text).map_err(io("write backup description"))
}

fn info(path: PathBuf, meta: BackupMeta) -> BackupInfo {
    BackupInfo {
        id: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size_bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        path,
        meta,
    }
}

/// Makes a backup of the database at `db_path` into `dir` with `VACUUM INTO`, describes it
/// and prunes that kind down to its retention.
pub(crate) async fn create(
    readers: &sqlx::SqlitePool,
    dir: &Path,
    kind: BackupKind,
    key: Option<&DbKey>,
) -> Result<BackupInfo, StoreError> {
    std::fs::create_dir_all(dir).map_err(io("create backups directory"))?;
    let created_at = now_ms();
    let path = dir.join(format!("pos.{}.{created_at}.db", kind.as_str()));
    // Written under a temporary name, so a crash part-way never leaves a file that looks
    // like a backup.
    let partial = dir.join(format!("pos.{}.{created_at}.db.partial", kind.as_str()));
    let _ = std::fs::remove_file(&partial);
    sqlx::query("VACUUM INTO ?")
        .bind(partial.to_string_lossy().into_owned())
        .execute(readers)
        .await
        .map_err(StoreError::from_sqlx)?;
    std::fs::File::open(&partial)
        .and_then(|f| f.sync_all())
        .map_err(io("sync backup"))?;
    std::fs::rename(&partial, &path).map_err(io("name backup"))?;
    let meta = describe(&path, kind, created_at, key).await?;
    write_meta(&path, &meta)?;
    prune(dir, kind)?;
    tracing::info!(kind = kind.as_str(), file = %path.display(), "backup written");
    Ok(info(path, meta))
}

/// The backups in `dir`, newest first. Files without a description (older pre-migration
/// copies) are listed from their name.
pub fn list(dir: &Path) -> Vec<BackupInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<BackupInfo> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "db"))
        .filter_map(|p| {
            let meta = std::fs::read_to_string(meta_path(&p))
                .ok()
                .and_then(|t| serde_json::from_str::<BackupMeta>(&t).ok())
                .or_else(|| legacy_meta(&p))?;
            Some(info(p, meta))
        })
        .collect();
    out.sort_by(|a, b| b.meta.created_at.cmp(&a.meta.created_at).then(b.id.cmp(&a.id)));
    out
}

/// `pos.pre-migrate-v3-v4-1700000000000.db`, written before descriptions existed.
fn legacy_meta(p: &Path) -> Option<BackupMeta> {
    let name = p.file_name()?.to_str()?;
    let rest = name.strip_prefix("pos.pre-migrate-v")?.strip_suffix(".db")?;
    let mut parts = rest.splitn(3, '-');
    let from: i64 = parts.next()?.parse().ok()?;
    let _to = parts.next()?;
    let created_at = parts.next()?.parse().ok()?;
    Some(BackupMeta {
        kind: BackupKind::PreMigration,
        created_at,
        schema_version: from,
        outbox_sequence: 0,
        sales: 0,
        app_version: String::new(),
    })
}

/// Keeps the newest `kind.keep()` backups of that kind.
fn prune(dir: &Path, kind: BackupKind) -> Result<(), StoreError> {
    for old in list(dir)
        .into_iter()
        .filter(|b| b.meta.kind == kind)
        .skip(kind.keep())
    {
        std::fs::remove_file(&old.path).map_err(io("remove old backup"))?;
        let _ = std::fs::remove_file(meta_path(&old.path));
    }
    Ok(())
}

/// Copies a backup (and its description) into another folder, such as a USB drive, and
/// prunes that folder the same way.
pub fn copy_to(backup: &BackupInfo, folder: &Path) -> Result<BackupInfo, StoreError> {
    std::fs::create_dir_all(folder).map_err(io("create the second backup folder"))?;
    let target = folder.join(&backup.id);
    let partial = folder.join(format!("{}.partial", backup.id));
    std::fs::copy(&backup.path, &partial).map_err(io("copy backup"))?;
    std::fs::File::open(&partial)
        .and_then(|f| f.sync_all())
        .map_err(io("sync backup copy"))?;
    std::fs::rename(&partial, &target).map_err(io("name backup copy"))?;
    write_meta(&target, &backup.meta)?;
    prune(folder, backup.meta.kind)?;
    Ok(info(target, backup.meta.clone()))
}

/// A full `integrity_check` of a backup file, and whether this app can open it. The file is
/// checked as a private copy (see [`Staged`]).
/// An encrypted backup that `key` does not open is refused with [`StoreError::Locked`].
pub async fn verify(path: &Path, key: Option<&DbKey>) -> Result<Verification, StoreError> {
    let staged = stage(path)?;
    check(&staged.file, key).await
}

/// [`verify`] for a file of our own (no stranger's `-wal` can be beside it).
async fn check(path: &Path, key: Option<&DbKey>) -> Result<Verification, StoreError> {
    let head = MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0);
    let mut c = open_file(path, key).await?;
    let check: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut c)
        .await
        .map_err(StoreError::from_sqlx)?;
    c.close().await.map_err(StoreError::from_sqlx)?;
    let meta = describe(path, BackupKind::Manual, 0, key).await?;
    Ok(Verification {
        problem: (check != ["ok"]).then(|| check.join("; ")),
        schema_version: meta.schema_version,
        too_new: meta.schema_version > head,
    })
}

/// How a database file is linked to the cloud: `(link_state, device_id)` from its
/// `sync_state` (`("unlinked", None)` if it has none).
pub async fn link_of(path: &Path, key: Option<&DbKey>) -> Result<(String, Option<String>), StoreError> {
    let staged = stage(path)?;
    let mut c = open_file(&staged.file, key).await?;
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT link_state, device_id FROM sync_state WHERE id = 1")
            .fetch_optional(&mut c)
            .await
            .map_err(StoreError::from_sqlx)?;
    c.close().await.map_err(StoreError::from_sqlx)?;
    Ok(row.unwrap_or_else(|| ("unlinked".into(), None)))
}

/// The device id the open database is linked with, if any.
pub async fn device_of(db: &crate::LocalDb) -> Result<Option<String>, StoreError> {
    sqlx::query_scalar("SELECT device_id FROM sync_state WHERE id = 1")
        .fetch_optional(db.readers())
        .await
        .map(Option::flatten)
        .map_err(StoreError::from_sqlx)
}

/// After restoring a linked register: its events after what the backup knew the cloud had are
/// to come back from the cloud before anything new is synced, and the cloud's master data
/// after them (E19-T03).
pub async fn begin_replay(db: &crate::LocalDb) -> Result<u64, StoreError> {
    let after: i64 = sqlx::query_scalar(
        "UPDATE sync_state SET replay_after = last_ack_sequence WHERE id = 1 RETURNING last_ack_sequence",
    )
    .fetch_one(db.writer())
    .await
    .map_err(StoreError::from_sqlx)?;
    Ok(u64::try_from(after).unwrap_or(0))
}

/// Replaces the database at `opts.path` with `backup`. **The database must be closed.**
///
/// The backup is checked first. The current database is kept as a `pre-restore` backup
/// (a consistent copy if it opens; otherwise its files as they are). The new file is put in
/// place under a temporary name and renamed, so a crash leaves either database whole. An
/// older backup is upgraded the next time the database is opened.
pub async fn restore(opts: &DbOptions, backup: &Path) -> Result<RestoreReport, StoreError> {
    let dir = opts
        .path
        .parent()
        .ok_or_else(|| StoreError::io("restore", std::io::Error::other("database path has no folder")))?;
    // Copied in first, then checked and described as our own file: what is checked is what is
    // restored, and nothing beside the original (a stranger's `-wal`) is ever opened.
    let incoming = dir.join("pos.db.restoring");
    let siblings = |p: &Path| {
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut s = p.as_os_str().to_owned();
            s.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(s));
        }
    };
    siblings(&incoming);
    std::fs::copy(backup, &incoming).map_err(io("copy backup into place"))?;
    let checked = async {
        let v = check(&incoming, opts.key.as_ref()).await?;
        if let Some(problem) = v.problem {
            return Err(StoreError::Corrupt(format!(
                "the backup failed its integrity check: {problem}"
            )));
        }
        if v.too_new {
            return Err(StoreError::SchemaTooNew {
                db_version: v.schema_version,
                app_version: MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0),
            });
        }
        describe(&incoming, BackupKind::Manual, now_ms(), opts.key.as_ref()).await
    }
    .await;
    let restored = match checked {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&incoming);
            siblings(&incoming);
            return Err(e);
        }
    };
    siblings(&incoming);
    let kept = if opts.path.exists() {
        Some(keep_current(opts).await?)
    } else {
        None
    };
    std::fs::File::open(&incoming)
        .and_then(|f| f.sync_all())
        .map_err(io("sync restored database"))?;
    for suffix in ["-wal", "-shm"] {
        let mut p = opts.path.as_os_str().to_owned();
        p.push(suffix);
        let p = PathBuf::from(p);
        if p.exists() {
            std::fs::remove_file(&p).map_err(io("remove old journal"))?;
        }
    }
    std::fs::rename(&incoming, &opts.path).map_err(io("put restored database in place"))?;
    tracing::warn!(
        backup = %backup.display(),
        outbox_sequence = restored.outbox_sequence,
        "database restored from a backup"
    );
    Ok(RestoreReport { restored, kept })
}

/// Keeps the current database as a `pre-restore` backup before it is replaced.
async fn keep_current(opts: &DbOptions) -> Result<BackupInfo, StoreError> {
    std::fs::create_dir_all(&opts.backups_dir).map_err(io("create backups directory"))?;
    let created_at = now_ms();
    let target = opts
        .backups_dir
        .join(format!("pos.{}.{created_at}.db", BackupKind::PreRestore.as_str()));
    // A consistent copy (WAL included) if the database opens; its files as they are if not
    // (it may be the damaged one being replaced).
    let copied = async {
        let mut c = crate::crypto::keyed(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&opts.path),
            &opts.path,
            opts.key.as_ref(),
        )
        .connect()
        .await?;
        sqlx::query("VACUUM INTO ?")
            .bind(target.to_string_lossy().into_owned())
            .execute(&mut c)
            .await?;
        c.close().await
    }
    .await;
    if let Err(e) = copied {
        tracing::warn!(error = %e, "current database does not open; keeping its files as they are");
        let _ = std::fs::remove_file(&target);
        std::fs::copy(&opts.path, &target).map_err(io("keep current database"))?;
        for suffix in ["-wal", "-shm"] {
            let mut from = opts.path.as_os_str().to_owned();
            from.push(suffix);
            let mut to = target.as_os_str().to_owned();
            to.push(suffix);
            if Path::new(&from).exists() {
                std::fs::copy(&from, &to).map_err(io("keep current journal"))?;
            }
        }
    }
    let meta = describe(&target, BackupKind::PreRestore, created_at, opts.key.as_ref())
        .await
        .unwrap_or(BackupMeta {
            kind: BackupKind::PreRestore,
            created_at,
            schema_version: 0,
            outbox_sequence: 0,
            sales: 0,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        });
    write_meta(&target, &meta)?;
    prune(&opts.backups_dir, BackupKind::PreRestore)?;
    Ok(info(target, meta))
}

#[cfg(test)]
mod tests {
    use super::stage;

    #[test]
    fn a_staged_copy_is_private_and_removed() {
        let dir = tempfile::TempDir::new().unwrap();
        let original = dir.path().join("b.db");
        std::fs::write(&original, b"abc").unwrap();
        let mut wal = original.as_os_str().to_owned();
        wal.push("-wal");
        std::fs::write(&wal, b"stranger").unwrap();
        let staged = stage(&original).unwrap();
        let folder = staged.dir.clone();
        assert_eq!(std::fs::read(&staged.file).unwrap(), b"abc");
        let names: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["backup.db"], "only the database file is copied");
        drop(staged);
        assert!(!folder.exists());
    }
}
