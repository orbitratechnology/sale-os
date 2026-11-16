//! E17-T05: fact appliers. The end-of-day scenario's real outbox (from `pos-local-db`) is
//! replayed into the cloud and its books checked against the day's known totals; then the
//! cross-register cases of PRD §11.5. PostgreSQL required (`TEST_DATABASE_URL`).

mod common;

use axum::http::StatusCode;
use common::{
    Tenant, business_count, end_of_day, fact_appliers, push, receipt_event, return_event, sale_event,
    second_register, state_with, tenant,
};
use pos_sync::EventEnvelope;
use serde_json::{Value, json};
use uuid::Uuid;

fn statuses(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap().to_owned())
        .collect()
}

async fn sum(s: &cloud_api::AppState, t: &Tenant, sql: &str) -> i64 {
    business_count(s, t, sql).await
}

#[tokio::test]
async fn the_end_of_day_replays_into_the_cloud_books() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "EndOfDay").await;
    let day = end_of_day(&t);
    let (code, body) = push(&s, &t, &day).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let bad: Vec<&Value> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["status"] != "applied")
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
    assert_eq!(body["ackSequence"], day.len());

    // The day's figures (see the table in pos-local-db/tests/end_of_day.rs).
    let q = |sql: &'static str| sum(&s, &t, sql);
    assert_eq!(
        q("SELECT count(*) FROM sales WHERE business_id = $1 AND mode = 'live'").await,
        12
    );
    assert_eq!(
        q("SELECT sum(total_minor)::bigint FROM sales WHERE business_id = $1").await,
        3016
    );
    assert_eq!(
        q("SELECT sum(tax_minor)::bigint FROM sales WHERE business_id = $1").await,
        504
    );
    assert_eq!(
        q("SELECT sum(discount_minor)::bigint FROM sales WHERE business_id = $1").await,
        74
    );
    assert_eq!(
        q("SELECT sum(amount_minor)::bigint FROM tenders WHERE business_id = $1 AND kind = 'cash'").await,
        1986
    );
    assert_eq!(
        q("SELECT sum(amount_minor)::bigint FROM tenders WHERE business_id = $1 AND kind = 'card_external'")
            .await,
        780
    );
    assert_eq!(q("SELECT count(*) FROM returns WHERE business_id = $1").await, 2);
    assert_eq!(
        q("SELECT sum(amount_minor)::bigint FROM refunds WHERE business_id = $1").await,
        370
    );
    assert_eq!(
        q("SELECT count(*) FROM inventory_documents WHERE business_id = $1").await,
        3
    );
    // The rollup agrees with the facts.
    assert_eq!(
        q("SELECT sum(sales_count)::bigint FROM daily_sales_rollup WHERE business_id = $1").await,
        12
    );
    assert_eq!(
        q("SELECT sum(sales_total_minor)::bigint FROM daily_sales_rollup WHERE business_id = $1").await,
        3016
    );
    assert_eq!(
        q("SELECT sum(tax_minor)::bigint FROM daily_sales_rollup WHERE business_id = $1").await,
        504
    );
    assert_eq!(
        q("SELECT sum(returns_count)::bigint FROM daily_sales_rollup WHERE business_id = $1").await,
        2
    );
    assert_eq!(
        q("SELECT sum(refunds_total_minor)::bigint FROM daily_sales_rollup WHERE business_id = $1").await,
        370
    );
    // The drawer: 0.36 short.
    assert_eq!(
        q("SELECT variance_minor FROM cash_session_closings WHERE business_id = $1").await,
        -36
    );
    // Every stock movement of the day, and balances that are their sums.
    let movements: usize = day
        .iter()
        .map(|e| {
            e.payload
                .get("stockMovements")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum();
    assert_eq!(
        q("SELECT count(*) FROM stock_movements WHERE business_id = $1").await,
        movements as i64
    );
    assert_eq!(
        q(
            "SELECT count(*) FROM stock_balances b WHERE business_id = $1 AND on_hand_milli <>
             (SELECT sum(delta_milli) FROM stock_movements m WHERE m.business_id = b.business_id
                AND m.location_id = b.location_id AND m.variant_id = b.variant_id AND m.bucket = b.bucket)"
        )
        .await,
        0
    );
    // Gum was sold with none in stock: flagged, not refused.
    assert_eq!(
        q("SELECT count(*) FROM sync_flags WHERE business_id = $1 AND kind = 'negative_stock'").await,
        1
    );
    // Facts go to the other registers' feed: 12 sales, 2 returns, 3 documents, 2 drawer events.
    assert_eq!(
        q("SELECT count(*) FROM change_log WHERE business_id = $1 AND change_kind = 'fact'").await,
        19
    );

    // Sending the day again changes nothing.
    let (_, again) = push(&s, &t, &day).await;
    assert!(statuses(&again).iter().all(|st| st == "duplicate"));
    assert_eq!(q("SELECT count(*) FROM sales WHERE business_id = $1").await, 12);
    assert_eq!(
        q("SELECT sum(sales_count)::bigint FROM daily_sales_rollup WHERE business_id = $1").await,
        12
    );
}

#[tokio::test]
async fn a_tampered_total_is_rejected_as_a_snapshot_mismatch() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Tamper").await;
    let mut day = end_of_day(&t);
    let first_sale = day.iter().position(|e| e.event_type == "sale.completed").unwrap();
    day[first_sale].payload["total"] = json!(1);
    let (_, body) = push(&s, &t, &day).await;
    let r = &body["results"][first_sale];
    assert_eq!(
        (r["status"].as_str(), r["code"].as_str()),
        (Some("rejected"), Some("SNAPSHOT_MISMATCH"))
    );
    // The rest of the day still goes through; the books just lack that sale.
    assert_eq!(body["ackSequence"], day.len());
    assert_eq!(
        business_count(&s, &t, "SELECT count(*) FROM sales WHERE business_id = $1").await,
        11
    );
}

#[tokio::test]
async fn facts_cannot_be_updated_or_deleted_by_the_app_role() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Immutable").await;
    push(&s, &t, &end_of_day(&t)).await;
    let principal = pos_cloud_db::Principal {
        business_id: pos_domain::BusinessId::from_uuid(t.business),
    };
    for table in [
        "sales",
        "sale_lines",
        "tenders",
        "returns",
        "return_lines",
        "refunds",
        "inventory_documents",
        "stock_movements",
        "cash_sessions",
        "cash_session_closings",
        "audit_events",
        "sync_flags",
    ] {
        for sql in [
            format!("UPDATE {table} SET business_id = business_id"),
            format!("DELETE FROM {table}"),
        ] {
            let mut tx = pos_cloud_db::TenantTx::begin(s.db.as_ref().unwrap(), &principal)
                .await
                .unwrap();
            let err = sqlx::query(&sql).execute(tx.test_conn()).await.unwrap_err();
            assert!(err.to_string().contains("permission denied"), "{sql}: {err}");
        }
    }
}

#[tokio::test]
async fn two_registers_selling_the_last_unit_both_apply_and_the_balance_is_flagged() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "LastUnit").await;
    let two = second_register(&s, &one).await;
    let cola = Uuid::now_v7();
    // Register 1 receives one and sells it; register 2, offline, sold one too.
    let (sale_one, ..) = sale_event(&one, 2, cola, 1000, 120, "live");
    let (_, b1) = push(&s, &one, &[receipt_event(&one, 1, cola, 1000), sale_one]).await;
    assert_eq!(statuses(&b1), ["applied", "applied"]);
    let (sale_two, ..) = sale_event(&two, 1, cola, 1000, 120, "live");
    let (_, b2) = push(&s, &two, &[sale_two]).await;
    assert_eq!(statuses(&b2), ["applied"]);
    let principal = pos_cloud_db::Principal {
        business_id: pos_domain::BusinessId::from_uuid(one.business),
    };
    let mut tx = pos_cloud_db::TenantTx::begin(s.db.as_ref().unwrap(), &principal)
        .await
        .unwrap();
    assert_eq!(
        pos_cloud_db::facts::balance(&mut tx, one.location, cola, "sellable")
            .await
            .unwrap(),
        -1000
    );
    tx.rollback().await.unwrap();
    let flags: Vec<Value> = sqlx::query_scalar(
        "SELECT detail FROM sync_flags WHERE business_id = $1 AND kind = 'negative_stock'",
    )
    .bind(one.business)
    .fetch_all(s.db.as_ref().unwrap())
    .await
    .unwrap();
    assert_eq!(flags.len(), 1, "{flags:?}");
    assert_eq!(flags[0]["registerId"], json!(two.register));
    assert_eq!(flags[0]["onHandMilli"], -1000);
}

#[tokio::test]
async fn a_return_before_its_sale_waits_and_an_over_return_is_flagged() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "Returns").await;
    let two = second_register(&s, &one).await;
    let cola = Uuid::now_v7();
    let (sale, sale_id, line_id) = sale_event(&one, 1, cola, 1000, 120, "live");
    // Register 2 takes the item back before register 1's sale has reached the cloud.
    let early = return_event(&two, 1, sale_id, line_id, cola, 1000, 120);
    let (_, b) = push(&s, &two, std::slice::from_ref(&early)).await;
    assert_eq!(statuses(&b), ["transient"]);
    assert_eq!(b["results"][0]["code"], "ORIGINAL_SALE_PENDING");
    assert_eq!(b["ackSequence"], 0);
    // Register 1 syncs its sale and, offline, also took the item back.
    let late = return_event(&one, 2, sale_id, line_id, cola, 1000, 120);
    let (_, b) = push(&s, &one, &[sale, late]).await;
    assert_eq!(
        statuses(&b),
        ["applied", "applied"],
        "a sale and its return in one push"
    );
    // Register 2's return now goes through: two returns of one unit sold, flagged.
    let (_, b) = push(&s, &two, &[early]).await;
    assert_eq!(statuses(&b), ["applied"]);
    let over: Vec<Value> =
        sqlx::query_scalar("SELECT detail FROM sync_flags WHERE business_id = $1 AND kind = 'over_return'")
            .bind(one.business)
            .fetch_all(s.db.as_ref().unwrap())
            .await
            .unwrap();
    assert_eq!(over.len(), 1, "{over:?}");
    assert_eq!(
        (over[0]["soldMilli"].as_i64(), over[0]["returnedMilli"].as_i64()),
        (Some(1000), Some(2000))
    );
}

#[tokio::test]
async fn sandbox_sales_are_kept_but_not_counted_or_shared() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Sandbox").await;
    let (practice, ..) = sale_event(&t, 1, Uuid::now_v7(), 1000, 500, "sandbox");
    let (_, b) = push(&s, &t, &[practice]).await;
    assert_eq!(statuses(&b), ["applied"]);
    assert!(b["results"][0].get("serverSeq").is_none());
    let q = |sql: &'static str| business_count(&s, &t, sql);
    assert_eq!(q("SELECT count(*) FROM sales WHERE business_id = $1").await, 1);
    assert_eq!(
        q("SELECT count(*) FROM daily_sales_rollup WHERE business_id = $1").await,
        0
    );
    assert_eq!(
        q("SELECT count(*) FROM change_log WHERE business_id = $1").await,
        0
    );
}

#[tokio::test]
async fn a_fact_for_another_register_or_location_is_rejected() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Place").await;
    let other = Tenant {
        location: Uuid::now_v7(),
        ..t.clone()
    };
    // Envelope and payload agree on a location this register is not at.
    let (mut elsewhere, ..) = sale_event(&other, 1, Uuid::now_v7(), 1000, 100, "live");
    elsewhere.location_id = other.location;
    let (mut foreign, ..) = sale_event(&t, 2, Uuid::now_v7(), 1000, 100, "live");
    foreign.payload["registerId"] = json!(Uuid::now_v7());
    let events: Vec<EventEnvelope> = vec![elsewhere, foreign];
    let (_, b) = push(&s, &t, &events).await;
    assert_eq!(statuses(&b), ["rejected", "rejected"]);
    assert!(
        b["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["code"] == "EVENT_INVALID")
    );
    assert_eq!(
        business_count(&s, &t, "SELECT count(*) FROM sales WHERE business_id = $1").await,
        0
    );
}

#[tokio::test]
async fn audit_entries_are_kept_with_their_register() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Audit").await;
    let (_, b) = push(&s, &t, &[common::audit_event(&t, 1, "auth.login_failed")]).await;
    assert_eq!(statuses(&b), ["applied"]);
    let row: (Uuid, String) =
        sqlx::query_as("SELECT register_id, action FROM audit_events WHERE business_id = $1")
            .bind(t.business)
            .fetch_one(s.db.as_ref().unwrap())
            .await
            .unwrap();
    assert_eq!(row, (t.register, "auth.login_failed".into()));
}
