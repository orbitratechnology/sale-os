//! E19-T03: local backups and restore. A backup is a whole, checked copy; restoring one puts
//! the database back to that moment and keeps the replaced one as a `pre-restore` backup; a
//! damaged backup, or one from a newer app, is refused without touching the database.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_domain::cart::AddVia;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{Money, VariantId};
use pos_local_db::backup::{self, BackupKind};
use pos_local_db::{DbOptions, LocalDb, StoreError};
use std::sync::Arc;

async fn product(shop: &common::Shop) -> VariantId {
    let a = Arc::new(shop.store.clone());
    CatalogService {
        store: a.clone(),
        catalog: a,
    }
    .create(
        &shop.owner,
        ProductInput {
            name: "Tea".into(),
            description: None,
            category_id: None,
            product_type: ProductType::Stock,
            unit: Unit::Ea,
            tax_category_id: shop.tax_category,
            quick_select: false,
            variants: vec![VariantInput::simple(Money::from_minor(250))],
        },
    )
    .await
    .unwrap()
    .variants[0]
        .id
}

async fn sell(shop: &common::Shop, tea: VariantId) -> String {
    let a = Arc::new(shop.store.clone());
    let carts = Arc::new(CartService::new(a.clone(), a.clone(), a.clone()));
    carts
        .add_variant(&shop.owner, tea, None, AddVia::Search)
        .await
        .unwrap();
    let view = carts.current(&shop.owner).await.unwrap();
    CheckoutService {
        carts,
        store: a.clone(),
        setup: a.clone(),
        stock: a.clone(),
        receipts: a,
    }
    .complete_sale(
        &shop.owner,
        CompleteSale {
            cart_id: view.cart.id,
            revision: view.cart.revision,
            tenders: vec![TenderInput {
                kind: TenderKind::Cash,
                amount: view.priced.total,
                external_reference: None,
            }],
        },
    )
    .await
    .unwrap()
    .receipt_number
}

async fn sales(db: &LocalDb) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sales")
        .fetch_one(db.readers())
        .await
        .unwrap()
}

#[tokio::test]
async fn back_up_restore_and_keep_what_was_replaced() {
    let shop = common::shop().await;
    let db = shop.store.db().clone();
    let tea = product(&shop).await;
    sell(&shop, tea).await;
    sell(&shop, tea).await;

    // A backup is whole and described.
    let b = db.backup(BackupKind::Manual).await.unwrap();
    assert_eq!((b.meta.kind, b.meta.sales), (BackupKind::Manual, 2));
    assert!(b.meta.outbox_sequence > 0 && b.size_bytes > 0);
    assert!(!b.path.with_extension("db.partial").exists());
    let v = backup::verify(&b.path, None).await.unwrap();
    assert!(v.restorable(), "{v:?}");
    assert_eq!(db.backups().first().map(|x| x.id.clone()), Some(b.id.clone()));

    // Two more sales, then the restore.
    sell(&shop, tea).await;
    sell(&shop, tea).await;
    let opts = DbOptions::in_dir(db.path().parent().unwrap());
    db.clone().close().await;
    let report = backup::restore(&opts, &b.path).await.unwrap();
    assert_eq!(report.restored.sales, 2);
    let kept = report.kept.unwrap();
    assert_eq!((kept.meta.kind, kept.meta.sales), (BackupKind::PreRestore, 4));

    // The database is as it was at the backup; the replaced one can be restored in turn.
    let back = LocalDb::open(&opts).await.unwrap();
    assert_eq!(sales(&back).await, 2);
    let check: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(back.readers())
        .await
        .unwrap();
    assert_eq!(check, "ok");
    back.clone().close().await;
    backup::restore(&opts, &kept.path).await.unwrap();
    let again = LocalDb::open(&opts).await.unwrap();
    assert_eq!(sales(&again).await, 4);
    again.close().await;
}

#[tokio::test]
async fn a_damaged_or_newer_backup_is_refused_and_nothing_changes() {
    let shop = common::shop().await;
    let db = shop.store.db().clone();
    let tea = product(&shop).await;
    sell(&shop, tea).await;
    let good = db.backup(BackupKind::Manual).await.unwrap();

    // Damaged: bytes in the middle of the file overwritten.
    let damaged = good.path.with_file_name("damaged.db");
    let mut bytes = std::fs::read(&good.path).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 4096] {
        *b = 0x5a;
    }
    std::fs::write(&damaged, &bytes).unwrap();
    let v = backup::verify(&damaged, None).await;
    assert!(
        v.as_ref().map_or(true, |v| !v.restorable()),
        "a damaged file passed: {v:?}"
    );

    // Newer: a migration this app does not know.
    let newer = good.path.with_file_name("newer.db");
    std::fs::copy(&good.path, &newer).unwrap();
    {
        use sqlx::{ConnectOptions, Connection};
        let mut c = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&newer)
            .connect()
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
             VALUES (9999, 'future', 1, x'00', 0)",
        )
        .execute(&mut c)
        .await
        .unwrap();
        c.close().await.unwrap();
    }
    assert!(backup::verify(&newer, None).await.unwrap().too_new);

    let opts = DbOptions::in_dir(db.path().parent().unwrap());
    sell(&shop, tea).await;
    db.clone().close().await;
    let e = backup::restore(&opts, &newer).await.unwrap_err();
    assert!(matches!(e, StoreError::SchemaTooNew { .. }), "{e:?}");
    assert!(backup::restore(&opts, &damaged).await.is_err());
    let still = LocalDb::open(&opts).await.unwrap();
    assert_eq!(sales(&still).await, 2, "the live database was not touched");
    // No pre-restore copy either: nothing was replaced.
    assert!(
        still
            .backups()
            .iter()
            .all(|b| b.meta.kind != BackupKind::PreRestore)
    );
    still.close().await;
}

#[tokio::test]
async fn retention_per_kind_and_a_second_folder() {
    let shop = common::shop().await;
    let db = shop.store.db().clone();
    for _ in 0..26 {
        db.backup(BackupKind::Hourly).await.unwrap();
        // Distinct millisecond timestamps in the names.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let daily = db.backup(BackupKind::Daily).await.unwrap();
    let all = db.backups();
    assert_eq!(
        all.iter().filter(|b| b.meta.kind == BackupKind::Hourly).count(),
        24
    );
    assert_eq!(all.iter().filter(|b| b.meta.kind == BackupKind::Daily).count(), 1);
    assert_eq!(all[0].id, daily.id, "newest first");

    let usb = tempfile::TempDir::new().unwrap();
    let copied = backup::copy_to(&daily, usb.path()).unwrap();
    assert_eq!(copied.meta, daily.meta);
    assert!(backup::verify(&copied.path, None).await.unwrap().restorable());
    assert_eq!(backup::list(usb.path()).len(), 1);
}

/// RUSTSEC-2026-0459: a backup is never opened beside a stranger's `-wal`. One placed next to
/// a backup on the stick is ignored by the check and the restore, and left untouched.
#[tokio::test]
async fn a_wal_file_beside_a_backup_is_never_opened() {
    let shop = common::shop().await;
    let db = shop.store.db().clone();
    let tea = product(&shop).await;
    sell(&shop, tea).await;
    let made = db.backup(BackupKind::Manual).await.unwrap();
    let usb = tempfile::TempDir::new().unwrap();
    let copied = backup::copy_to(&made, usb.path()).unwrap();
    let mut wal = copied.path.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::write(&wal, vec![0xA5u8; 64 * 1024]).unwrap();

    assert!(backup::verify(&copied.path, None).await.unwrap().restorable());
    assert_eq!(backup::link_of(&copied.path, None).await.unwrap().0, "unlinked");
    let opts = DbOptions::in_dir(db.path().parent().unwrap());
    db.clone().close().await;
    let report = backup::restore(&opts, &copied.path).await.unwrap();
    assert_eq!(report.restored.sales, 1);
    assert_eq!(
        std::fs::read(&wal).unwrap(),
        vec![0xA5u8; 64 * 1024],
        "left as it was"
    );
    let back = LocalDb::open(&opts).await.unwrap();
    assert_eq!(sales(&back).await, 1);
    back.close().await;
}
