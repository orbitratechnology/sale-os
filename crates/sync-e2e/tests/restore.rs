//! E19-T03: restoring a backup on a linked register. Its sales after the backup come back from
//! the cloud with their original receipt numbers, numbering carries on after them, the cloud
//! holds each sale once, and master data made since returns with the bootstrap.

mod common;

use common::{cloud, register};
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::replay::ReplayService;
use pos_application::sync::{Next, OsRandom, SyncStore, SyncWorker};
use pos_domain::cart::AddVia;
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, UuidV7Generator};
use pos_local_db::backup::{self, BackupKind};
use pos_local_db::{LocalDb, SqliteStore};
use std::sync::Arc;

async fn receipts(store: &SqliteStore) -> Vec<String> {
    sqlx::query_scalar("SELECT receipt_number FROM sales ORDER BY receipt_seq")
        .fetch_all(store.db().readers())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_linked_till_restores_and_gets_its_later_sales_back_from_the_cloud() {
    let Some(cloud) = cloud().await else { return };
    let (email, password) = cloud.owner().await;
    let r = register(&cloud, "1234").await;
    let tea = r.product("Tea", 250).await;
    r.go_live().await;
    let owner = r.sign_in(&email, &password).await;
    let linked = r.link.create(&r.owner, &owner, false).await.unwrap();
    let carts = r.carts();
    for _ in 0..3 {
        r.sell(&carts, tea, 1).await;
    }
    assert_eq!(r.sync().await, Next::Idle);

    let made = r.store.db().backup(BackupKind::Manual).await.unwrap();
    assert_eq!(made.meta.sales, 3);

    // After the backup: two more sales and a new product, all in the cloud.
    for _ in 0..2 {
        r.sell(&carts, tea, 2).await;
    }
    r.product("Scones", 300).await;
    assert_eq!(r.sync().await, Next::Idle);
    let before = receipts(&r.store).await;
    assert_eq!(before.len(), 5);

    // The restore, as the app does it: close, replace, reopen, schedule the replay.
    let db = r.store.db().clone();
    let opts = db.options();
    let (state, device) = backup::link_of(&made.path, db.key()).await.unwrap();
    assert_eq!(state, "linked");
    assert_eq!(device, backup::device_of(&db).await.unwrap());
    db.close().await;
    backup::restore(&opts, &made.path).await.unwrap();
    let reopened = LocalDb::open(&opts).await.unwrap();
    backup::begin_replay(&reopened).await.unwrap();
    let store = SqliteStore::new(reopened);
    assert_eq!(receipts(&store).await.len(), 3);

    // Until the replay is done, a sale cannot complete (its number may be taken)...
    let shared = Arc::new(store.clone());
    let restored_carts = Arc::new(pos_application::cart::CartService::new(
        shared.clone(),
        shared.clone(),
        shared.clone(),
    ));
    let (view, _) = restored_carts
        .add_variant(&r.owner, tea, None, AddVia::Search)
        .await
        .unwrap();
    let cart = restored_carts.current(&r.owner).await.unwrap().cart;
    let checkout = CheckoutService {
        carts: restored_carts.clone(),
        store: shared.clone(),
        setup: shared.clone(),
        stock: shared.clone(),
        receipts: shared.clone(),
    };
    let complete = |rev| CompleteSale {
        cart_id: cart.id,
        revision: rev,
        tenders: vec![TenderInput {
            kind: TenderKind::Cash,
            amount: view.priced.total,
            external_reference: None,
        }],
    };
    let e = checkout
        .complete_sale(&r.owner, complete(cart.revision))
        .await
        .unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::SyncReplayPending, "{e:?}");

    // ...and the sync loop brings the rest back: the replay, then a fresh bootstrap.
    let worker = SyncWorker::new(
        shared.clone(),
        r.session.clone(),
        r.clock.clone(),
        Arc::new(OsRandom),
    );
    let replayer = ReplayService {
        store: shared.clone(),
        setup: shared.clone(),
        receipts: shared.clone(),
        returns: shared.clone(),
        ids: Arc::new(UuidV7Generator),
    };
    let mut rounds = 0;
    while store.pull_state().await.unwrap().replay_after.is_some() {
        worker.replay_step(&replayer).await.unwrap();
        rounds += 1;
        assert!(rounds < 20, "the replay does not finish");
    }
    while store.pull_state().await.unwrap().bootstrap_page.is_some() {
        assert_ne!(
            worker.bootstrap_step().await.unwrap().next,
            Next::RetryAfter(std::time::Duration::ZERO)
        );
        rounds += 1;
        assert!(rounds < 40, "the bootstrap does not finish");
    }

    // The till is as it was: the same five receipts, the new product, nothing waiting.
    assert_eq!(receipts(&store).await, before);
    let scones: i64 = sqlx::query_scalar("SELECT count(*) FROM products WHERE name = 'Scones'")
        .fetch_one(store.db().readers())
        .await
        .unwrap();
    assert_eq!(scones, 1);
    let state = store.push_state().await.unwrap();
    assert_eq!(state.last_ack_sequence, state.max_sequence);
    let waiting: i64 =
        sqlx::query_scalar("SELECT count(*) FROM outbox_events WHERE status NOT IN ('applied', 'duplicate')")
            .fetch_one(store.db().readers())
            .await
            .unwrap();
    assert_eq!(waiting, 0);
    // Stock counts every sale: 3 + 2×2 teas.
    let sold: i64 = sqlx::query_scalar(
        "SELECT -sum(quantity_delta_milli) FROM stock_movements WHERE movement_type = 'sale'",
    )
    .fetch_one(store.db().readers())
    .await
    .unwrap();
    assert_eq!(sold, 7_000);

    // The next sale is the sixth, and the cloud holds each sale once.
    let cart = restored_carts.current(&r.owner).await.unwrap().cart;
    let sixth = checkout
        .complete_sale(&r.owner, complete(cart.revision))
        .await
        .unwrap();
    assert!(!before.contains(&sixth.receipt_number));
    assert!(
        sixth.receipt_number.ends_with("000006"),
        "{}",
        sixth.receipt_number
    );
    loop {
        if worker.push_once().await.unwrap().next != Next::Immediately {
            break;
        }
    }
    let in_cloud: (i64, i64) =
        sqlx::query_as("SELECT count(*), count(DISTINCT receipt_number) FROM sales WHERE business_id = $1")
            .bind(linked.business_id)
            .fetch_one(&cloud.pool)
            .await
            .unwrap();
    assert_eq!(in_cloud, (6, 6));
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'sale.completed' AND json_extract(metadata_json, '$.restoredFromCloud')",
    )
    .fetch_one(store.db().readers())
    .await
    .unwrap();
    assert_eq!(audited, 2);
}
