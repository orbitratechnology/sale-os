//! E14-T05: the consolidated sales report. Two registers' facts summed by day, register and
//! tender; per-register completeness from what each register last reported; who may read
//! it. PostgreSQL required (`TEST_DATABASE_URL`).

mod common;

use axum::http::{Method, StatusCode};
use cloud_api::AppState;
use common::{
    Tenant, assert_matches_spec, call, end_of_day, fact_appliers, push, return_event, sale_event,
    second_register, state_with, tenant,
};
use pos_sync::reports::SalesReport;
use serde_json::Value;
use uuid::Uuid;

async fn report(s: &AppState, token: &str, query: &str) -> (StatusCode, String) {
    call(
        s,
        Method::GET,
        &format!("/v1/reports/sales?{query}"),
        Some(token),
        None,
    )
    .await
}

async fn ok(s: &AppState, token: &str, query: &str) -> SalesReport {
    let (code, body) = report(s, token, query).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_matches_spec("SalesReport", &v);
    serde_json::from_value(v).unwrap()
}

/// The register reports what it still holds, as its sync loop does with every pull.
async fn pull_reporting(s: &AppState, t: &Tenant, unsent: i64) {
    let (code, body) = call(
        s,
        Method::GET,
        &format!("/v1/sync/changes?limit=1&unsent={unsent}"),
        Some(&t.device_token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
}

async fn age(s: &AppState, t: &Tenant, minutes: i32) {
    sqlx::query(
        "UPDATE register_sync_state
            SET last_push_at = last_push_at - make_interval(mins => $3),
                unsent_reported_at = unsent_reported_at - make_interval(mins => $3)
          WHERE business_id = $1 AND register_id = $2",
    )
    .bind(t.business)
    .bind(t.register)
    .bind(minutes)
    .execute(s.db.as_ref().unwrap())
    .await
    .unwrap();
}

#[tokio::test]
async fn two_registers_add_up_by_day_register_and_tender() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "Reports").await;
    let two = second_register(&s, &one).await;
    // R01: the end-of-day scenario (2026-01-01; 12 live sales 30.16, 2 returns 3.70).
    let (code, body) = push(&s, &one, &end_of_day(&one)).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    // R02, on 2026-11-05: two cash sales (5.00 and 2.50), one partly returned (1.00); and a
    // sandbox sale, which counts nowhere.
    let variant = Uuid::now_v7();
    let (a, sale_a, line_a) = sale_event(&two, 1, variant, 2000, 500, "live");
    let (b, _, _) = sale_event(&two, 2, variant, 1000, 250, "live");
    let (c, _, _) = sale_event(&two, 3, variant, 1000, 999, "sandbox");
    let r = return_event(&two, 4, sale_a, line_a, variant, 1000, 100);
    let (code, body) = push(&s, &two, &[a, b, c, r]).await;
    assert_eq!(code, StatusCode::OK, "{body}");

    let year = "from=2026-01-01&to=2026-12-31&businessId=".to_owned() + &one.business.to_string();
    let days = ok(&s, &one.user_token, &format!("{year}&groupBy=day")).await;
    assert_eq!((days.currency_code.as_str(), days.currency_exponent), ("GBP", 2));
    let summary: Vec<_> = days
        .rows
        .iter()
        .map(|r| {
            (
                r.key.as_str(),
                r.sales_count,
                r.gross_minor,
                r.returns_count,
                r.refunds_minor,
                r.net_minor,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("2026-01-01", 12, 3016, 2, 370, 2646),
            ("2026-11-05", 2, 750, 1, 100, 650)
        ]
    );
    assert_eq!((days.rows[0].tax_minor, days.rows[0].discount_minor), (504, 74));

    let registers = ok(&s, &one.user_token, &format!("{year}&groupBy=register")).await;
    let by: Vec<_> = registers
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.sales_count, r.net_minor))
        .collect();
    assert_eq!(by, [("R01", 12, 2646), ("R02", 2, 650)]);

    let tenders = ok(&s, &one.user_token, &format!("{year}&groupBy=tender")).await;
    let cash = tenders.rows.iter().find(|r| r.key == "cash").unwrap();
    let card = tenders.rows.iter().find(|r| r.key == "card_external").unwrap();
    // R01's cash 19.86 and card 7.80, R02's cash 7.50; the refunds (3.70 + 1.00) by the
    // kind they were paid back with, adding up to the day rows'.
    assert_eq!((cash.gross_minor, card.gross_minor), (1986 + 750, 780));
    assert_eq!(cash.refunds_minor + card.refunds_minor, 370 + 100);
    assert_eq!(tenders.rows.iter().map(|r| r.refunds_minor).sum::<i64>(), 470);

    // One day only, and the device's own view (no businessId needed) agrees.
    let one_day = ok(
        &s,
        &two.device_token,
        "from=2026-11-05&to=2026-11-05&groupBy=register",
    )
    .await;
    assert_eq!(one_day.rows.len(), 1);
    assert_eq!(one_day.rows[0].net_minor, 650);
}

#[tokio::test]
async fn completeness_follows_what_each_register_last_reported() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "Complete").await;
    let two = second_register(&s, &one).await;
    let q = "from=2026-11-05&to=2026-11-05&groupBy=day";
    let regs = |r: &SalesReport| {
        r.completeness
            .registers
            .iter()
            .map(|c| (c.code.clone(), c.pending_known, c.current))
            .collect::<Vec<_>>()
    };

    // Nothing reported yet: neither register can be vouched for.
    let r = ok(&s, &one.device_token, q).await;
    assert_eq!(
        regs(&r),
        [("R01".into(), false, false), ("R02".into(), false, false)]
    );
    assert!(r.completeness.registers.iter().all(|c| c.last_ack_at.is_none()));
    assert!(!r.completeness.fully_synchronized());

    // Both pull holding nothing: fully synchronized.
    pull_reporting(&s, &one, 0).await;
    pull_reporting(&s, &two, 0).await;
    let r = ok(&s, &one.device_token, q).await;
    assert!(r.completeness.fully_synchronized(), "{:?}", r.completeness);
    assert!(r.completeness.registers.iter().all(|c| c.last_ack_at.is_some()));

    // R02 says it holds 3 events the cloud has not accepted.
    pull_reporting(&s, &two, 3).await;
    let r = ok(&s, &one.device_token, q).await;
    assert_eq!(
        regs(&r),
        [("R01".into(), false, true), ("R02".into(), true, false)]
    );
    assert_eq!(r.completeness.unsynced(), 1);

    // R02 catches up, but R01 has not been heard from for 6 minutes.
    pull_reporting(&s, &two, 0).await;
    age(&s, &one, 6).await;
    let r = ok(&s, &two.device_token, q).await;
    assert_eq!(
        regs(&r),
        [("R01".into(), false, false), ("R02".into(), false, true)]
    );
    let r01 = &r.completeness.registers[0];
    assert_eq!(r.completeness.last_synced(), r01.last_ack_at.as_deref());

    // A revoked register drops out of the count.
    let (code, body) = call(
        &s,
        Method::POST,
        &format!("/v1/devices/{}/revoke", one.device),
        Some(&one.user_token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    let r = ok(&s, &two.device_token, q).await;
    assert_eq!(regs(&r), [("R02".into(), false, true)]);
    assert!(r.completeness.fully_synchronized());
    // And it may no longer read the report.
    let (code, body) = report(&s, &one.device_token, q).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert!(body.contains("SYNC_DEVICE_REVOKED"), "{body}");
}

#[tokio::test]
async fn only_the_business_may_read_it() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let mine = tenant(&s, "Mine").await;
    let theirs = tenant(&s, "Theirs").await;
    let q = |b: Uuid| format!("from=2026-11-05&to=2026-11-05&groupBy=day&businessId={b}");
    // Another owner, or another business's register: not found, whatever the business.
    let (code, _) = report(&s, &theirs.user_token, &q(mine.business)).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, _) = report(&s, &theirs.device_token, &q(mine.business)).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, _) = report(&s, &mine.user_token, &q(Uuid::now_v7())).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    // No token, or a bad one.
    let (code, _) = call(
        &s,
        Method::GET,
        &format!("/v1/reports/sales?{}", q(mine.business)),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    let (code, _) = report(&s, "not-a-token", &q(mine.business)).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    // An account must name the business; ranges must make sense.
    for bad in [
        "from=2026-11-05&to=2026-11-05&groupBy=day".to_owned(),
        format!(
            "from=2026-11-06&to=2026-11-05&groupBy=day&businessId={}",
            mine.business
        ),
        format!(
            "from=2025-01-01&to=2026-11-05&groupBy=day&businessId={}",
            mine.business
        ),
        format!(
            "from=05/11/2026&to=2026-11-05&groupBy=day&businessId={}",
            mine.business
        ),
    ] {
        let (code, body) = report(&s, &mine.user_token, &bad).await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {body}");
    }
    let (code, _) = report(
        &s,
        &mine.user_token,
        &format!("{}&groupBy=week", q(mine.business).replace("&groupBy=day", "")),
    )
    .await;
    assert!(code.is_client_error());
    // A negative count is refused.
    let (code, _) = call(
        &s,
        Method::GET,
        "/v1/sync/changes?unsent=-1",
        Some(&mine.device_token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
}
