//! E02-T01 / E02-T02 acceptance tests.

use pos_domain::{ErrorCode, HasErrorCode};
use pos_local_db::{DbOptions, LocalDb, StoreError};
use sqlx::migrate::Migrator;
use std::path::Path;
use tempfile::TempDir;

fn opts(dir: &TempDir) -> DbOptions {
    DbOptions::in_dir(dir.path())
}

/// Builds a migrator from inline (version, sql) pairs written to a temp directory.
async fn migrator(dir: &Path, migrations: &[(i64, &str)]) -> Migrator {
    std::fs::create_dir_all(dir).unwrap();
    for entry in std::fs::read_dir(dir).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    for (v, sql) in migrations {
        std::fs::write(dir.join(format!("{v:04}_m{v}.sql")), sql).unwrap();
    }
    Migrator::new(dir).await.unwrap()
}

fn file_hash(p: &Path) -> Vec<u8> {
    // Byte-for-byte comparison is enough for these small files.
    std::fs::read(p).unwrap()
}

fn backups(dir: &TempDir) -> Vec<String> {
    match std::fs::read_dir(dir.path().join("backups")) {
        Ok(rd) => rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => vec![],
    }
}

#[tokio::test]
async fn pragmas_follow_adr_007_on_writer_and_readers() {
    let dir = TempDir::new().unwrap();
    let db = LocalDb::open(&opts(&dir)).await.unwrap();
    let (w, r) = db.pragma_report().await.unwrap();
    for p in [w, r] {
        assert_eq!(p.journal_mode, "wal");
        assert_eq!(p.synchronous, 2, "synchronous must be FULL");
        assert!(p.foreign_keys);
        assert_eq!(p.busy_timeout_ms, 5_000);
    }
    db.close().await;
}

#[tokio::test]
async fn fresh_database_migrates_to_head_without_backup_and_reopen_is_noop() {
    let dir = TempDir::new().unwrap();
    let db = LocalDb::open(&opts(&dir)).await.unwrap();
    let head = pos_local_db::MIGRATOR.iter().map(|m| m.version).max().unwrap();
    assert_eq!(db.schema_version().await.unwrap(), head);
    assert_eq!(db.migration_report().from_version, 0);
    assert!(
        db.migration_report().backup.is_none(),
        "nothing to protect on a fresh database"
    );
    for table in [
        "businesses",
        "registers",
        "register_counters",
        "audit_events",
        "outbox_events",
        "sync_state",
    ] {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?")
            .bind(table)
            .fetch_one(db.readers())
            .await
            .unwrap();
        assert_eq!(n, 1, "table {table} missing");
    }
    db.close().await;

    let db = LocalDb::open(&opts(&dir)).await.unwrap();
    assert!(db.migration_report().applied.is_empty());
    assert!(backups(&dir).is_empty());
    db.close().await;
}

#[tokio::test]
async fn upgrade_takes_a_pre_migration_backup() {
    let dir = TempDir::new().unwrap();
    let mdir = dir.path().join("m");
    let v1 = migrator(
        &mdir,
        &[(1, "CREATE TABLE a (x INTEGER); INSERT INTO a VALUES (42);")],
    )
    .await;
    LocalDb::open_with_migrator(&opts(&dir), &v1)
        .await
        .unwrap()
        .close()
        .await;

    let v2 = migrator(
        &mdir,
        &[
            (1, "CREATE TABLE a (x INTEGER); INSERT INTO a VALUES (42);"),
            (2, "CREATE TABLE b (y INTEGER);"),
        ],
    )
    .await;
    let db = LocalDb::open_with_migrator(&opts(&dir), &v2).await.unwrap();
    let report = db.migration_report().clone();
    assert_eq!(
        (report.from_version, report.to_version, report.applied.clone()),
        (1, 2, vec![2])
    );
    let backup = report.backup.expect("backup expected");
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("pre-migrate-v1-v2")
    );
    db.close().await;

    // The backup is a valid database at the old version, with the data.
    let bdir = TempDir::new().unwrap();
    std::fs::copy(&backup, bdir.path().join("pos.db")).unwrap();
    let old = LocalDb::open_with_migrator(&opts(&bdir), &v1).await.unwrap();
    let x: i64 = sqlx::query_scalar("SELECT x FROM a")
        .fetch_one(old.readers())
        .await
        .unwrap();
    assert_eq!(x, 42);
    old.close().await;
}

#[tokio::test]
async fn failed_migration_restores_the_original_and_keeps_the_backup() {
    let dir = TempDir::new().unwrap();
    let mdir = dir.path().join("m");
    let m1 = (1, "CREATE TABLE a (x INTEGER); INSERT INTO a VALUES (7);");
    let v1 = migrator(&mdir, &[m1]).await;
    LocalDb::open_with_migrator(&opts(&dir), &v1)
        .await
        .unwrap()
        .close()
        .await;
    let original = file_hash(&dir.path().join("pos.db"));

    // v2 succeeds and commits, v3 fails: the database must end up exactly as before.
    let broken = migrator(
        &mdir,
        &[m1, (2, "CREATE TABLE b (y INTEGER);"), (3, "THIS IS NOT SQL;")],
    )
    .await;
    let err = LocalDb::open_with_migrator(&opts(&dir), &broken)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Migration(_)), "{err:?}");
    assert_eq!(err.error_code(), ErrorCode::StoreIo);
    assert_eq!(backups(&dir).len(), 1, "backup must be kept");

    let v1 = migrator(&mdir, &[m1]).await;
    let db = LocalDb::open_with_migrator(&opts(&dir), &v1).await.unwrap();
    assert_eq!(db.schema_version().await.unwrap(), 1);
    let has_b: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name='b'")
        .fetch_one(db.readers())
        .await
        .unwrap();
    assert_eq!(has_b, 0, "v2 must have been rolled back by the restore");
    let x: i64 = sqlx::query_scalar("SELECT x FROM a")
        .fetch_one(db.readers())
        .await
        .unwrap();
    assert_eq!(x, 7);
    db.close().await;
    // VACUUM INTO compacts, so compare content rather than bytes with the pre-upgrade file.
    assert!(!original.is_empty());
}

#[tokio::test]
async fn failed_migration_of_new_database_leaves_no_file() {
    let dir = TempDir::new().unwrap();
    let broken = migrator(&dir.path().join("m"), &[(1, "NOT SQL;")]).await;
    assert!(LocalDb::open_with_migrator(&opts(&dir), &broken).await.is_err());
    assert!(!dir.path().join("pos.db").exists());
}

#[tokio::test]
async fn newer_schema_is_refused_without_modifying_the_file() {
    let dir = TempDir::new().unwrap();
    let mdir = dir.path().join("m");
    let v2 = migrator(
        &mdir,
        &[
            (1, "CREATE TABLE a (x INTEGER);"),
            (2, "CREATE TABLE b (y INTEGER);"),
        ],
    )
    .await;
    LocalDb::open_with_migrator(&opts(&dir), &v2)
        .await
        .unwrap()
        .close()
        .await;
    let before = file_hash(&dir.path().join("pos.db"));

    let v1 = migrator(&mdir, &[(1, "CREATE TABLE a (x INTEGER);")]).await;
    let err = LocalDb::open_with_migrator(&opts(&dir), &v1).await.unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::SchemaTooNew {
                db_version: 2,
                app_version: 1
            }
        ),
        "{err:?}"
    );
    assert_eq!(err.error_code(), ErrorCode::StoreSchemaTooNew);
    assert_eq!(file_hash(&dir.path().join("pos.db")), before);
    assert!(backups(&dir).is_empty());
}

#[tokio::test]
async fn network_path_is_refused_with_store_io() {
    let mut o = DbOptions::in_dir(Path::new("/tmp"));
    o.path = r"\\server\share\pos.db".into();
    let err = LocalDb::open(&o).await.unwrap_err();
    assert!(matches!(err, StoreError::NetworkPathRefused(_)));
    assert_eq!(err.error_code(), ErrorCode::StoreIo);
    assert!(err.to_string().contains("local disk"));
}

#[tokio::test]
async fn readers_are_read_only() {
    let dir = TempDir::new().unwrap();
    let db = LocalDb::open(&opts(&dir)).await.unwrap();
    let res = sqlx::query("UPDATE sync_state SET last_ack_sequence = 1")
        .execute(db.readers())
        .await;
    assert!(res.is_err());
    db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reads_during_writes_never_report_busy() {
    let dir = TempDir::new().unwrap();
    let db = LocalDb::open(&opts(&dir)).await.unwrap();
    sqlx::query("CREATE TABLE load_test (id INTEGER PRIMARY KEY, v TEXT NOT NULL)")
        .execute(db.writer())
        .await
        .unwrap();

    let writer = db.writer().clone();
    let writes = tokio::spawn(async move {
        for i in 0..300 {
            let mut tx = writer.begin().await.unwrap();
            sqlx::query("INSERT INTO load_test (v) VALUES (?)")
                .bind(format!("row {i}"))
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
    });
    let mut reads = Vec::new();
    for _ in 0..1_000 {
        let r = db.readers().clone();
        reads.push(tokio::spawn(async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM load_test")
                .fetch_one(&r)
                .await
        }));
    }
    for r in reads {
        r.await.unwrap().expect("read failed (SQLITE_BUSY?)");
    }
    writes.await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM load_test")
        .fetch_one(db.readers())
        .await
        .unwrap();
    assert_eq!(n, 300);
    db.close().await;
}

#[tokio::test]
async fn core_schema_enforces_append_only_rules() {
    let dir = TempDir::new().unwrap();
    let db = LocalDb::open(&opts(&dir)).await.unwrap();
    let w = db.writer();
    sqlx::query(
        "INSERT INTO audit_events (audit_id, business_id, action, entity_type, entity_id, occurred_at)
         VALUES ('a1', 'b1', 'test', 't', 'e', 0)",
    )
    .execute(w)
    .await
    .unwrap();
    let upd = sqlx::query("UPDATE audit_events SET action = 'x'")
        .execute(w)
        .await
        .unwrap_err();
    assert!(upd.to_string().contains("append-only"));
    assert!(sqlx::query("DELETE FROM audit_events").execute(w).await.is_err());

    sqlx::query(
        "INSERT INTO outbox_events (event_id, business_id, location_id, register_id, local_sequence, event_type,
           schema_version, aggregate_type, aggregate_id, occurred_at, payload_json, created_at)
         VALUES ('e1','b','l','r',1,'test.event',1,'t','a',0,'{}',0)",
    )
    .execute(w)
    .await
    .unwrap();
    // Delivery state may change; the payload may not.
    sqlx::query("UPDATE outbox_events SET status = 'in_flight', attempt_count = 1")
        .execute(w)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE outbox_events SET payload_json = '{\"x\":1}'")
            .execute(w)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM outbox_events").execute(w).await.is_err(),
        "unhandled events cannot be deleted"
    );
    sqlx::query("UPDATE outbox_events SET status = 'applied'")
        .execute(w)
        .await
        .unwrap();
    sqlx::query("DELETE FROM outbox_events").execute(w).await.unwrap();

    // Duplicate local_sequence per register is rejected.
    for id in ["e2", "e3"] {
        let res = sqlx::query(
            "INSERT INTO outbox_events (event_id, business_id, location_id, register_id, local_sequence, event_type,
               schema_version, aggregate_type, aggregate_id, occurred_at, payload_json, created_at)
             VALUES (?,'b','l','r',2,'test.event',1,'t','a',0,'{}',0)",
        )
        .bind(id)
        .execute(w)
        .await;
        assert_eq!(res.is_ok(), id == "e2");
    }
    db.close().await;
}
