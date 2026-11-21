//! E19-T09 (PRD §10.1): below the cloud's minimum app version, every call is answered
//! `426 SYNC_UPGRADE_REQUIRED`. The till keeps selling and queuing; sync stops and says
//! "update required"; after the update everything is pushed and nothing is lost.

mod common;

use common::{cloud, register};
use pos_application::sync::{Next, OsRandom, SyncStore, SyncWorker};
use pos_sync_http::{CloudClient, DeviceSession};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outdated_till_keeps_selling_and_loses_nothing_until_it_updates() {
    let Some(cloud) = cloud().await else { return };
    let (email, password) = cloud.owner().await;
    let r = register(&cloud, "1234").await;
    let tea = r.product("Tea", 250).await;
    r.go_live().await;
    let owner = r.sign_in(&email, &password).await;
    let linked = r.link.create(&r.owner, &owner, false).await.unwrap();
    let carts = r.carts();
    for _ in 0..2 {
        r.sell(&carts, tea, 1).await;
    }
    assert_eq!(r.sync().await, Next::Idle);

    // The cloud now needs 9.0.0; this till is 0.1.0.
    let mut config = cloud_api::Config::for_tests();
    config.min_app_version = "9.0.0".into();
    let strict = cloud.serve_with(config).await;
    let outdated = SyncWorker::new(
        Arc::new(r.store.clone()),
        Arc::new(DeviceSession::new(
            CloudClient::new(&strict, "0.1.0").unwrap(),
            r.credentials.clone(),
        )),
        r.clock.clone(),
        Arc::new(OsRandom),
    );

    // Selling goes on.
    for _ in 0..3 {
        r.sell(&carts, tea, 1).await;
    }
    let before = r.store.push_state().await.unwrap();
    assert!(before.max_sequence > before.last_ack_sequence);
    let report = outdated.push_once().await.unwrap();
    assert_eq!(report.next, Next::Stopped("SYNC_UPGRADE_REQUIRED"));
    let after = r.store.push_state().await.unwrap();
    assert_eq!(
        after.last_ack_sequence, before.last_ack_sequence,
        "nothing acknowledged"
    );
    assert_eq!(
        r.local("SELECT count(*) FROM sync_state WHERE last_error_code = 'SYNC_UPGRADE_REQUIRED'")
            .await,
        1,
        "the status bar shows Update required"
    );
    assert_eq!(r.local("SELECT count(*) FROM sales").await, 5);

    // The problem names the minimum; health and the policy stay open.
    let http = reqwest::Client::new();
    let res = http
        .get(format!("{strict}/v1/sync/ack"))
        .header("x-app-version", "0.1.0")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 426);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["code"], "SYNC_UPGRADE_REQUIRED");
    assert_eq!(body["details"]["minVersion"], "9.0.0");
    for open in ["/v1/health", "/v1/version-policy"] {
        let res = http
            .get(format!("{strict}{open}"))
            .header("x-app-version", "0.1.0")
            .send()
            .await
            .unwrap();
        assert_ne!(res.status().as_u16(), 426, "{open}");
    }

    // After the update (the cloud the till can talk to again): all five sales arrive, once.
    assert_eq!(r.sync().await, Next::Idle);
    let state = r.store.push_state().await.unwrap();
    assert_eq!(state.last_ack_sequence, state.max_sequence);
    let in_cloud: i64 = sqlx::query_scalar("SELECT count(*) FROM sales WHERE business_id = $1")
        .bind(linked.business_id)
        .fetch_one(&cloud.pool)
        .await
        .unwrap();
    assert_eq!(in_cloud, 5);
}
