//! ADR-023: the database and its backups are encrypted at rest (SQLCipher). Without the key a
//! file is unreadable and says so (`StoreError::Locked`); a database from before encryption,
//! and its backups, are converted on the first open with a key; a backup encrypted with
//! another computer's key is refused untouched.

mod common;

use pos_local_db::backup::{self, BackupKind};
use pos_local_db::crypto::{self, DbKey, FileKind};
use pos_local_db::{DbOptions, LocalDb, StoreError};
use std::path::Path;

const SECRET: &str = "Very Secret Tea Co";

async fn write_name(db: &LocalDb) {
    sqlx::query("CREATE TABLE IF NOT EXISTS probe (v TEXT); INSERT INTO probe VALUES (?)")
        .bind(SECRET)
        .execute(db.writer())
        .await
        .unwrap();
}

async fn read_name(db: &LocalDb) -> String {
    sqlx::query_scalar("SELECT v FROM probe")
        .fetch_one(db.readers())
        .await
        .unwrap()
}

fn shows(path: &Path, text: &str) -> bool {
    let bytes = std::fs::read(path).unwrap();
    bytes.windows(text.len()).any(|w| w == text.as_bytes())
}

#[tokio::test]
async fn encrypted_at_rest_with_backups_and_nothing_opens_without_the_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let key = DbKey::generate().unwrap();
    let opts = DbOptions::in_dir(dir.path()).with_key(key.clone());
    let db = LocalDb::open(&opts).await.unwrap();
    write_name(&db).await;
    let made = db.backup(BackupKind::Manual).await.unwrap();
    assert!(backup::verify(&made.path, Some(&key)).await.unwrap().restorable());
    db.clone().close().await;

    for f in [&opts.path, &made.path] {
        assert_eq!(crypto::file_kind(f), FileKind::Encrypted, "{}", f.display());
        assert!(!shows(f, SECRET), "{} shows the data", f.display());
    }
    // No key, or the wrong one: locked, and said so.
    for wrong in [None, Some(DbKey::generate().unwrap())] {
        let mut o = DbOptions::in_dir(dir.path());
        o.key = wrong.clone();
        assert!(matches!(LocalDb::open(&o).await, Err(StoreError::Locked)));
        assert!(matches!(
            backup::verify(&made.path, wrong.as_ref()).await,
            Err(StoreError::Locked)
        ));
    }
    // The recovery key, as written down, opens it.
    let written = key.recovery_key().to_lowercase().replace('-', " ");
    let back =
        LocalDb::open(&DbOptions::in_dir(dir.path()).with_key(DbKey::from_recovery_key(&written).unwrap()))
            .await
            .unwrap();
    assert_eq!(read_name(&back).await, SECRET);
    back.close().await;
}

#[tokio::test]
async fn a_database_from_before_encryption_and_its_backups_are_converted() {
    let dir = tempfile::TempDir::new().unwrap();
    let plain = DbOptions::in_dir(dir.path());
    let db = LocalDb::open(&plain).await.unwrap();
    write_name(&db).await;
    let old = db.backup(BackupKind::Daily).await.unwrap();
    db.clone().close().await;
    assert_eq!(crypto::file_kind(&plain.path), FileKind::Plain);
    assert!(shows(&old.path, SECRET));

    let key = DbKey::generate().unwrap();
    let db = LocalDb::open(&plain.clone().with_key(key.clone())).await.unwrap();
    assert_eq!(read_name(&db).await, SECRET, "nothing lost");
    let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(db.readers())
        .await
        .unwrap();
    assert!(tables > 10, "the migration history came across");
    db.clone().close().await;
    for f in [&plain.path, &old.path] {
        assert_eq!(crypto::file_kind(f), FileKind::Encrypted, "{}", f.display());
        assert!(!shows(f, SECRET), "{} still shows the data", f.display());
    }
    assert!(backup::verify(&old.path, Some(&key)).await.unwrap().restorable());
    // The old backup's description still reads.
    assert_eq!(backup::list(&plain.backups_dir)[0].id, old.id);
}

#[tokio::test]
async fn restore_takes_this_computers_backups_and_refuses_another_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let key = DbKey::generate().unwrap();
    let opts = DbOptions::in_dir(dir.path()).with_key(key.clone());
    let db = LocalDb::open(&opts).await.unwrap();
    write_name(&db).await;
    let mine = db.backup(BackupKind::Manual).await.unwrap();
    db.clone().close().await;

    // A backup from another computer (another key) on the stick: refused, nothing replaced.
    let other_dir = tempfile::TempDir::new().unwrap();
    let other = LocalDb::open(&DbOptions::in_dir(other_dir.path()).with_key(DbKey::generate().unwrap()))
        .await
        .unwrap();
    let theirs = other.backup(BackupKind::Manual).await.unwrap();
    other.close().await;
    assert!(matches!(
        backup::restore(&opts, &theirs.path).await,
        Err(StoreError::Locked)
    ));
    assert!(!dir.path().join("pos.db.restoring").exists());

    // This computer's own: restored, still encrypted, opens with the key.
    let report = backup::restore(&opts, &mine.path).await.unwrap();
    assert_eq!(report.kept.unwrap().meta.kind, BackupKind::PreRestore);
    assert_eq!(crypto::file_kind(&opts.path), FileKind::Encrypted);
    let back = LocalDb::open(&opts).await.unwrap();
    assert_eq!(read_name(&back).await, SECRET);
    back.close().await;
}
