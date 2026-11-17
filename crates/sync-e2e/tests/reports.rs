//! E14-T05 end to end: two registers sell, sync over HTTP, and either one reads the
//! consolidated report from the cloud, complete or not by what each last reported.
//! PostgreSQL required (`TEST_DATABASE_URL`).

mod common;

use common::{cloud, register};
use pos_application::cloud_reports::{CloudReportService, Freshness};
use pos_application::sync::Next;
use pos_sync::reports::GroupBy;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_registers_sales_add_up_in_the_cloud_and_completeness_follows_their_syncs() {
    let Some(cloud) = cloud().await else { return };
    let (email, password) = cloud.owner().await;
    let one = register(&cloud, "1234").await;
    let cola = one.product("Cola", 120).await;
    let owner = one.sign_in(&email, &password).await;
    let linked = one.link.create(&one.owner, &owner, false).await.unwrap();
    one.go_live().await;
    let carts = one.carts();
    one.sell(&carts, cola, 1).await;
    one.sell(&carts, cola, 2).await;
    assert_eq!(one.sync().await, Next::Idle);

    let mut two = register(&cloud, "9999").await;
    let (owner2, _) = two.link.start(&two.owner, &email, &password).await.unwrap();
    two.link
        .join(&two.owner, &owner2, linked.business_id)
        .await
        .unwrap();
    two.become_joined_owner().await;
    two.go_live().await;
    two.sell(&two.carts(), cola, 1).await;
    assert_eq!(two.sync().await, Next::Idle);
    one.sync().await;

    let day: String = sqlx::query_scalar("SELECT business_date FROM sales LIMIT 1")
        .fetch_one(one.store.db().readers())
        .await
        .unwrap();
    let day = day.parse().unwrap();
    let reports = |r: &common::Register| CloudReportService {
        transport: r.session.clone(),
        sync: Arc::new(r.store.clone()),
        cache: Arc::new(r.store.clone()),
        clock: r.clock.clone(),
    };

    // Both synced just now holding nothing: fully synchronized, every sale counted.
    let by_register = reports(&one)
        .sales(&one.owner, day, day, GroupBy::Register)
        .await
        .unwrap();
    assert_eq!(by_register.freshness, Freshness::FullySynchronized);
    let counts: Vec<_> = by_register
        .report
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.sales_count, r.gross_minor))
        .collect();
    assert_eq!(counts, [("R01", 2, 360), ("R02", 1, 120)]);
    let by_tender = reports(&two)
        .sales(&two.owner, day, day, GroupBy::Tender)
        .await
        .unwrap();
    assert_eq!(
        by_tender
            .report
            .rows
            .iter()
            .map(|r| (r.key.as_str(), r.gross_minor))
            .collect::<Vec<_>>(),
        [("cash", 480)]
    );

    // Register 1 sells again; its next pull says it holds an unsent sale.
    one.sell(&carts, cola, 1).await;
    one.worker.pull_once().await.unwrap();
    let partial = reports(&two)
        .sales(&two.owner, day, day, GroupBy::Day)
        .await
        .unwrap();
    assert!(
        matches!(
            partial.freshness,
            Freshness::Partial {
                unsynced: 1,
                last_synced: Some(_)
            }
        ),
        "{:?}",
        partial.freshness
    );
    assert_eq!(
        partial.report.rows[0].sales_count, 3,
        "the unsent sale is not in yet"
    );
    let r01 = &partial.report.completeness.registers[0];
    assert!(r01.code == "R01" && r01.pending_known && !r01.current);

    // Once it is sent, the cloud has everything again.
    assert_eq!(one.sync().await, Next::Idle);
    let full = reports(&two)
        .sales(&two.owner, day, day, GroupBy::Day)
        .await
        .unwrap();
    assert_eq!(full.freshness, Freshness::FullySynchronized);
    assert_eq!(full.report.rows[0].sales_count, 4);
}
