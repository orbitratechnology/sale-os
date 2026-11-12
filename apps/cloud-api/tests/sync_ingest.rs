//! E17-T04: the ingestion framework (`POST /v1/sync/events`, `GET /v1/sync/ack`) with test
//! appliers. PostgreSQL required (`TEST_DATABASE_URL`); skipped without it.

mod common;

use axum::http::{Method, StatusCode};
use cloud_api::sync::Appliers;
use common::{
    Tenant, assert_matches_spec, audit_event, call, count, push, setting_event, state_with, tenant,
    test_appliers,
};
use pos_sync::EventEnvelope;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const FEED_ROWS: &str = "SELECT count(*) FROM change_log WHERE origin_event_id = $1";
const INBOX_ROWS: &str = "SELECT count(*) FROM event_inbox WHERE event_id = $1";

/// The result statuses, after checking the response against the contract.
fn statuses(body: &Value) -> Vec<String> {
    assert_matches_spec("PushResponse", body);
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap().to_owned())
        .collect()
}

async fn ack_of(s: &cloud_api::AppState, t: &Tenant) -> u64 {
    let (code, body) = call(s, Method::GET, "/v1/sync/ack", Some(&t.device_token), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_matches_spec("AckResponse", &v);
    assert_eq!(v["registerId"], json!(t.register));
    v["ackSequence"].as_u64().unwrap()
}

#[tokio::test]
async fn the_same_batch_five_times_applies_once_with_identical_results() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Idem").await;
    let batch: Vec<EventEnvelope> = (1..=3).map(|n| audit_event(&t, n, "ok")).collect();
    for e in &batch {
        assert_matches_spec("EventEnvelope", &serde_json::to_value(e).unwrap());
    }
    let (code, first) = push(&s, &t, &batch).await;
    assert_eq!(code, StatusCode::OK, "{first}");
    assert_eq!(statuses(&first), ["applied"; 3]);
    assert_eq!(first["ackSequence"], 3);
    for _ in 0..4 {
        let (_, again) = push(&s, &t, &batch).await;
        assert_eq!(statuses(&again), ["duplicate"; 3]);
        assert_eq!(again["ackSequence"], 3);
        for (a, b) in first["results"]
            .as_array()
            .unwrap()
            .iter()
            .zip(again["results"].as_array().unwrap())
        {
            assert_eq!(b["status"], "duplicate");
            assert_eq!(b["originalStatus"], "applied");
            assert_eq!(b["serverSeq"], a["serverSeq"], "the original outcome comes back");
            assert_eq!(b["eventId"], a["eventId"]);
        }
    }
    for e in &batch {
        assert_eq!(count(&s, FEED_ROWS, e.event_id).await, 1);
        assert_eq!(count(&s, INBOX_ROWS, e.event_id).await, 1);
    }
    assert_eq!(ack_of(&s, &t).await, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pushes_of_one_batch_apply_each_event_exactly_once() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Race").await;
    let batch: Vec<EventEnvelope> = (1..=5).map(|n| audit_event(&t, n, "ok")).collect();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..5 {
        let (s, t, batch) = (s.clone(), t.clone(), batch.clone());
        set.spawn(async move { push(&s, &t, &batch).await });
    }
    let mut applied: BTreeMap<String, usize> = BTreeMap::new();
    let mut seqs: BTreeMap<String, Value> = BTreeMap::new();
    while let Some(r) = set.join_next().await {
        let (code, body) = r.unwrap();
        assert_eq!(code, StatusCode::OK, "{body}");
        for r in body["results"].as_array().unwrap() {
            let id = r["eventId"].as_str().unwrap().to_owned();
            match r["status"].as_str().unwrap() {
                "applied" => *applied.entry(id.clone()).or_default() += 1,
                "duplicate" => assert_eq!(r["originalStatus"], "applied"),
                // A push that overtook another may see a gap ahead of it; it stops there.
                "out_of_order" => continue,
                other => panic!("unexpected {other}: {r}"),
            }
            let seq = r["serverSeq"].clone();
            assert_eq!(seqs.entry(id).or_insert_with(|| seq.clone()), &seq);
        }
    }
    assert_eq!(applied.len(), 5, "{applied:?}");
    assert!(applied.values().all(|n| *n == 1), "{applied:?}");
    for e in &batch {
        assert_eq!(count(&s, FEED_ROWS, e.event_id).await, 1);
    }
    assert_eq!(ack_of(&s, &t).await, 5);
}

#[tokio::test]
async fn an_event_is_acknowledged_only_after_its_commit() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Commit").await;
    // The applier writes, then fails: nothing of it stays, and the ack does not move.
    let failing = audit_event(&t, 1, "transient");
    let (_, body) = push(&s, &t, std::slice::from_ref(&failing)).await;
    assert_eq!(statuses(&body), ["transient"]);
    assert_eq!(body["results"][0]["code"], "SERVER_BUSY");
    assert_eq!(body["ackSequence"], 0);
    assert_eq!(count(&s, FEED_ROWS, failing.event_id).await, 0);
    assert_eq!(count(&s, INBOX_ROWS, failing.event_id).await, 0);
    // The response is lost after the commit (the process died): the retry is a duplicate
    // carrying the original outcome.
    let e = audit_event(&t, 1, "ok");
    let (_, lost) = push(&s, &t, std::slice::from_ref(&e)).await;
    let (_, retry) = push(&s, &t, std::slice::from_ref(&e)).await;
    assert_eq!(retry["results"][0]["status"], "duplicate");
    assert_eq!(retry["results"][0]["originalStatus"], "applied");
    assert_eq!(retry["results"][0]["serverSeq"], lost["results"][0]["serverSeq"]);
    assert_eq!(count(&s, FEED_ROWS, e.event_id).await, 1);
}

#[tokio::test]
async fn a_reused_event_id_with_another_payload_is_rejected() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Reuse").await;
    let e = audit_event(&t, 1, "ok");
    push(&s, &t, std::slice::from_ref(&e)).await;
    let mut forged = audit_event(&t, 2, "ok");
    forged.event_id = e.event_id;
    let (_, body) = push(&s, &t, &[forged]).await;
    assert_eq!(statuses(&body), ["rejected"]);
    assert_eq!(body["results"][0]["code"], "EVENT_ID_REUSED");
    assert_eq!(body["ackSequence"], 1);
    assert_eq!(count(&s, FEED_ROWS, e.event_id).await, 1);
}

#[tokio::test]
async fn a_gap_in_the_sequence_stops_the_batch_with_the_expected_value() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Gap").await;
    let (_, body) = push(&s, &t, &[audit_event(&t, 2, "ok")]).await;
    assert_eq!(statuses(&body), ["out_of_order"]);
    assert_eq!(body["results"][0]["expectedSequence"], 1);
    assert_eq!(body["ackSequence"], 0);
    let batch = [1, 2, 4, 5].map(|n| audit_event(&t, n, "ok"));
    let (_, body) = push(&s, &t, &batch).await;
    assert_eq!(statuses(&body), ["applied", "applied", "out_of_order"]);
    assert_eq!(body["results"][2]["expectedSequence"], 3);
    assert_eq!(body["ackSequence"], 2);
    assert_eq!(count(&s, FEED_ROWS, batch[3].event_id).await, 0);
}

#[tokio::test]
async fn an_unknown_type_or_newer_version_is_rejected_and_kept_by_the_client() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Version").await;
    let mut unknown = audit_event(&t, 1, "ok");
    unknown.event_type = "sale.teleported".into();
    let mut newer = audit_event(&t, 1, "ok");
    newer.schema_version = 99;
    for e in [unknown, newer] {
        let (_, body) = push(&s, &t, &[e.clone(), audit_event(&t, 2, "ok")]).await;
        assert_eq!(statuses(&body), ["rejected"], "the batch stops there");
        assert_eq!(body["results"][0]["code"], "EVENT_VERSION_UNSUPPORTED");
        // Not recorded and not acknowledged: the client keeps it and sends it again later.
        assert_eq!(body["ackSequence"], 0);
        assert_eq!(count(&s, INBOX_ROWS, e.event_id).await, 0);
    }
}

#[tokio::test]
async fn an_invalid_event_is_rejected_recorded_and_acknowledged() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Invalid").await;
    let mut bad = audit_event(&t, 1, "ok");
    bad.payload.as_object_mut().unwrap().remove("action");
    let mut based = audit_event(&t, 2, "ok");
    based.base_version = Some(3);
    let (_, body) = push(&s, &t, &[bad.clone(), based, audit_event(&t, 3, "ok")]).await;
    assert_eq!(statuses(&body), ["rejected", "rejected", "applied"]);
    assert_eq!(body["results"][0]["code"], "EVENT_INVALID");
    assert!(
        body["results"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("baseVersion")
    );
    assert_eq!(body["ackSequence"], 3);
    let (_, again) = push(&s, &t, &[bad]).await;
    assert_eq!(again["results"][0]["status"], "duplicate");
    assert_eq!(again["results"][0]["originalStatus"], "rejected");
    assert_eq!(again["results"][0]["code"], "EVENT_INVALID");
}

#[tokio::test]
async fn an_applier_rejection_undoes_its_writes_but_is_recorded() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Reject").await;
    let e = audit_event(&t, 1, "reject");
    let (_, body) = push(&s, &t, std::slice::from_ref(&e)).await;
    assert_eq!(statuses(&body), ["rejected"]);
    assert_eq!(body["ackSequence"], 1);
    assert_eq!(count(&s, FEED_ROWS, e.event_id).await, 0);
    assert_eq!(count(&s, INBOX_ROWS, e.event_id).await, 1);
}

#[tokio::test]
async fn a_conflict_is_recorded_with_the_server_snapshot() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Conflict").await;
    let (_, body) = push(&s, &t, &[setting_event(&t, 1, 0), setting_event(&t, 2, 3)]).await;
    assert_eq!(statuses(&body), ["applied", "conflict"]);
    assert_eq!(body["results"][0]["serverVersion"], 1);
    let c = &body["results"][1];
    assert_eq!(
        (c["code"].as_str(), c["serverVersion"].as_u64()),
        (Some("VERSION"), Some(0))
    );
    assert_eq!(c["serverSnapshot"]["value"], "Server text");
    assert_eq!(body["ackSequence"], 2);
}

#[tokio::test]
async fn a_type_without_an_applier_is_transient() {
    let Some(s) = state_with(Appliers::standard()).await else {
        return;
    };
    let t = tenant(&s, "NoApplier").await;
    let (_, body) = push(&s, &t, &[audit_event(&t, 1, "ok")]).await;
    assert_eq!(statuses(&body), ["transient"]);
    assert_eq!(body["results"][0]["code"], "APPLIER_UNAVAILABLE");
    assert_eq!(body["ackSequence"], 0);
}

#[tokio::test]
async fn pushes_are_limited_to_the_tokens_register_and_the_size_limits() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let (a, b) = (tenant(&s, "A").await, tenant(&s, "B").await);
    // The request names another register, or an event names another business.
    let body = json!({"registerId": b.register, "events": []});
    let (code, _) = call(
        &s,
        Method::POST,
        "/v1/sync/events",
        Some(&a.device_token),
        Some(body),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let mut foreign = audit_event(&a, 1, "ok");
    foreign.business_id = b.business;
    let (code, _) = push(&s, &a, &[foreign]).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    // More than 200 events.
    let many: Vec<EventEnvelope> = (1..=201).map(|n| audit_event(&a, n, "ok")).collect();
    let (code, body) = push(&s, &a, &many).await;
    assert_eq!(code, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    // An owner token is not a device token.
    let (code, _) = call(&s, Method::GET, "/v1/sync/ack", Some(&a.user_token), None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    // Nothing reached either business.
    assert_eq!(ack_of(&s, &a).await, 0);
    assert_eq!(ack_of(&s, &b).await, 0);
}

#[tokio::test]
async fn the_inbox_and_change_feed_are_append_only_for_the_app_role() {
    let Some(s) = state_with(test_appliers()).await else {
        return;
    };
    let t = tenant(&s, "AppendOnly").await;
    push(&s, &t, &[audit_event(&t, 1, "ok")]).await;
    let principal = pos_cloud_db::Principal {
        business_id: pos_domain::BusinessId::from_uuid(t.business),
    };
    for sql in [
        "UPDATE event_inbox SET result_status = 'applied'",
        "DELETE FROM event_inbox",
        "UPDATE change_log SET snapshot = '{}'",
        "DELETE FROM change_log",
        "DELETE FROM register_sync_state",
    ] {
        let mut tx = pos_cloud_db::TenantTx::begin(s.db.as_ref().unwrap(), &principal)
            .await
            .unwrap();
        let err = sqlx::query(sql).execute(tx.test_conn()).await.unwrap_err();
        assert!(err.to_string().contains("permission denied"), "{sql}: {err}");
    }
}
