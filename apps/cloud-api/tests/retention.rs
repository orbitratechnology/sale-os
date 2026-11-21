//! E19-T10: deleting a cloud business. The request stops it at once (tills revoked, owner
//! reports refused) and can be cancelled; after the grace period the job removes every row of
//! it from every table, and no other business loses anything.

mod common;

use axum::http::{Method, StatusCode};
use common::{call, end_of_day, fact_appliers, push, tenant};
use serde_json::json;
use uuid::Uuid;

/// Rows of `business` in every table with `business_id` (as the test's superuser).
async fn rows_of(pool: &sqlx::PgPool, business: Uuid) -> Vec<(String, i64)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT c.table_name::text FROM information_schema.columns c
           JOIN information_schema.tables x USING (table_schema, table_name)
          WHERE c.table_schema = current_schema() AND c.column_name = 'business_id'
            AND x.table_type = 'BASE TABLE' ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut out = Vec::new();
    for t in tables {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM \"{t}\" WHERE business_id = $1"))
            .bind(business)
            .fetch_one(pool)
            .await
            .unwrap();
        out.push((t, n));
    }
    out
}

#[tokio::test]
async fn a_deleted_business_stops_at_once_and_is_gone_after_the_grace_period() {
    let Some(s) = common::state_with(fact_appliers()).await else {
        return;
    };
    let pool = s.db.clone().unwrap();
    let (a, b) = (tenant(&s, "A").await, tenant(&s, "B").await);
    for t in [&a, &b] {
        let (code, _) = push(&s, t, &end_of_day(t)).await;
        assert_eq!(code, StatusCode::OK);
    }
    let b_before = rows_of(&pool, b.business).await;
    assert!(
        rows_of(&pool, a.business)
            .await
            .iter()
            .filter(|(_, n)| *n > 0)
            .count()
            > 10
    );

    // The day's push renamed the business: the owner types the name it has now.
    let a_name: String = sqlx::query_scalar("SELECT name FROM businesses WHERE business_id = $1")
        .bind(a.business)
        .fetch_one(&pool)
        .await
        .unwrap();
    // The request needs the name typed again.
    let uri = format!("/v1/businesses/{}/deletion", a.business);
    let (code, _) = call(
        &s,
        Method::POST,
        &uri,
        Some(&a.user_token),
        Some(json!({"confirmName": "A"})),
    )
    .await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
    // Another owner cannot ask.
    let (code, _) = call(
        &s,
        Method::POST,
        &uri,
        Some(&b.user_token),
        Some(json!({"confirmName": a_name})),
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, body) = call(
        &s,
        Method::POST,
        &uri,
        Some(&a.user_token),
        Some(json!({"confirmName": a_name})),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    let due: serde_json::Value = serde_json::from_str(&body).unwrap();
    let due = chrono::DateTime::parse_from_rfc3339(due["deletionDueAt"].as_str().unwrap()).unwrap();
    let days = (due.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_days();
    assert!((29..=30).contains(&days), "default grace 30 days, got {days}");

    // Stopped at once: the till is revoked, owner reports refuse the business.
    let (code, body) = call(&s, Method::GET, "/v1/sync/ack", Some(&a.device_token), None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "{body}");
    assert!(body.contains("SYNC_DEVICE_REVOKED"));
    let (code, _) = call(
        &s,
        Method::GET,
        &format!(
            "/v1/reports/sales?from=2026-01-01&to=2026-12-31&groupBy=day&businessId={}",
            a.business
        ),
        Some(&a.user_token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);

    // Cancelled, then asked again.
    let (code, _) = call(&s, Method::DELETE, &uri, Some(&a.user_token), None).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, _) = call(
        &s,
        Method::POST,
        &uri,
        Some(&a.user_token),
        Some(json!({"confirmName": a_name})),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED);

    // Not due yet: nothing is removed.
    let removed = pos_cloud_db::retention::purge_due(&pool, chrono::Utc::now())
        .await
        .unwrap();
    assert!(!removed.contains(&a.business));
    assert!(rows_of(&pool, a.business).await.iter().any(|(_, n)| *n > 0));

    // Due: every row of A goes, from every table; B keeps everything.
    let later = chrono::Utc::now() + chrono::Duration::days(31);
    let removed = pos_cloud_db::retention::purge_due(&pool, later).await.unwrap();
    assert!(removed.contains(&a.business));
    let left: Vec<_> = rows_of(&pool, a.business)
        .await
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .collect();
    assert!(left.is_empty(), "rows left: {left:?}");
    assert_eq!(
        rows_of(&pool, b.business).await,
        b_before,
        "the other business is untouched"
    );
    // The owner's account stays (account deletion is separate); the business is no longer listed.
    let (code, body) = call(&s, Method::GET, "/v1/businesses", Some(&a.user_token), None).await;
    assert_eq!(code, StatusCode::OK);
    assert!(!body.contains(&a.business.to_string()));
}
