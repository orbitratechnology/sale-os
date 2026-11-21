//! E17-T06: master-data appliers with version checks (ADR-011), uniqueness across registers,
//! and the cloud's own product edits. PostgreSQL required (`TEST_DATABASE_URL`).

mod common;

use axum::http::{Method, StatusCode};
use common::{
    Tenant, business_count, call, end_of_day, fact_appliers, product_event, product_payload, push,
    second_register, state_with, tenant,
};
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

async fn product_row(s: &cloud_api::AppState, t: &Tenant, id: Uuid) -> (i64, String) {
    sqlx::query_as("SELECT version, name FROM products WHERE business_id = $1 AND product_id = $2")
        .bind(t.business)
        .bind(id)
        .fetch_one(s.db.as_ref().unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn edits_on_the_current_version_apply_and_move_it_on() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Versions").await;
    let (id, variant) = (Uuid::now_v7(), Uuid::now_v7());
    let events: Vec<_> = (0..4)
        .map(|base| {
            let p = product_payload(
                id,
                variant,
                base,
                &format!("Cola v{}", base + 1),
                Some("COLA"),
                Some("5000112637922"),
                false,
            );
            product_event(&t, base + 1, base, p)
        })
        .collect();
    let (_, b) = push(&s, &t, &events).await;
    assert_eq!(statuses(&b), ["applied"; 4]);
    // Base 3 with the cloud at 3: applied, now 4.
    assert_eq!(b["results"][3]["serverVersion"], 4);
    assert_eq!(product_row(&s, &t, id).await, (4, "Cola v4".into()));
    assert_eq!(
        business_count(
            &s,
            &t,
            "SELECT count(*) FROM change_log WHERE business_id = $1 AND aggregate_type = 'product'"
        )
        .await,
        4
    );
}

#[tokio::test]
async fn an_edit_on_an_old_version_conflicts_and_changes_nothing() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "Stale").await;
    let two = second_register(&s, &one).await;
    let (id, variant) = (Uuid::now_v7(), Uuid::now_v7());
    let events: Vec<_> = (0..4)
        .map(|base| {
            product_event(
                &one,
                base + 1,
                base,
                product_payload(id, variant, base, "Cola", None, None, false),
            )
        })
        .collect();
    push(&s, &one, &events).await;
    let feed_before =
        business_count(&s, &one, "SELECT count(*) FROM change_log WHERE business_id = $1").await;
    // Register 2 edited version 3 offline; the cloud is at 4.
    let stale = product_event(
        &two,
        1,
        3,
        product_payload(id, variant, 3, "Cola (renamed offline)", None, None, false),
    );
    let (_, b) = push(&s, &two, &[stale]).await;
    assert_eq!(statuses(&b), ["conflict"]);
    let r = &b["results"][0];
    assert_eq!(
        (r["code"].as_str(), r["serverVersion"].as_u64()),
        (Some("VERSION"), Some(4))
    );
    assert_eq!(r["serverSnapshot"]["name"], "Cola");
    // Acknowledged (the register settles it), and the cloud record is untouched.
    assert_eq!(b["ackSequence"], 1);
    assert_eq!(product_row(&s, &one, id).await, (4, "Cola".into()));
    assert_eq!(
        business_count(&s, &one, "SELECT count(*) FROM change_log WHERE business_id = $1").await,
        feed_before
    );
}

#[tokio::test]
async fn the_same_barcode_or_sku_on_two_products_conflicts_for_the_second_register() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "Barcodes").await;
    let two = second_register(&s, &one).await;
    let (a, b_id) = (Uuid::now_v7(), Uuid::now_v7());
    let a_v = Uuid::now_v7();
    let (_, r) = push(
        &s,
        &one,
        &[product_event(
            &one,
            1,
            0,
            product_payload(a, a_v, 0, "Cola", Some("COLA-1"), Some("4006381333931"), false),
        )],
    )
    .await;
    assert_eq!(statuses(&r), ["applied"]);
    // Offline, register 2 gave another product the same barcode, then the same SKU.
    let same_barcode = product_payload(
        b_id,
        Uuid::now_v7(),
        0,
        "Lemonade",
        None,
        Some("4006381333931"),
        false,
    );
    let same_sku = product_payload(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        "Water",
        Some("cola-1"),
        None,
        false,
    );
    let (_, r) = push(
        &s,
        &two,
        &[
            product_event(&two, 1, 0, same_barcode),
            product_event(&two, 2, 0, same_sku),
        ],
    )
    .await;
    assert_eq!(statuses(&r), ["conflict", "conflict"]);
    assert_eq!(r["results"][0]["code"], "BARCODE_TAKEN");
    assert_eq!(r["results"][1]["code"], "SKU_TAKEN");
    for c in r["results"].as_array().unwrap() {
        // The owning product, so the register can offer to reassign or merge.
        assert_eq!(c["serverSnapshot"]["id"], json!(a));
        assert_eq!(c["serverVersion"], 1);
    }
    assert_eq!(
        business_count(&s, &one, "SELECT count(*) FROM products WHERE business_id = $1").await,
        1
    );
    // Archiving the first product frees the barcode; moving it within a product is no clash.
    let archived = product_payload(a, a_v, 1, "Cola", Some("COLA-1"), Some("4006381333931"), true);
    push(&s, &one, &[product_event(&one, 2, 1, archived)]).await;
    let retry = product_payload(
        b_id,
        Uuid::now_v7(),
        0,
        "Lemonade",
        None,
        Some("4006381333931"),
        false,
    );
    let (_, r) = push(&s, &two, &[product_event(&two, 3, 0, retry)]).await;
    assert_eq!(statuses(&r), ["applied"]);
}

#[tokio::test]
async fn staff_changes_and_pin_hashes_reach_the_cloud() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Staff").await;
    let user = Uuid::now_v7();
    let staff = |base: u64, hash: &str, active: bool| {
        let p = json!({"userId": user, "businessId": t.business, "displayName": "Ade", "role": "cashier",
                       "pinHash": hash, "pinUpdatedAt": 1, "mustChangePin": false, "active": active,
                       "version": base + 1, "createdAt": 1, "updatedAt": 2});
        pos_sync::EventEnvelope {
            event_id: Uuid::now_v7(),
            business_id: t.business,
            location_id: t.location,
            register_id: t.register,
            local_sequence: base + 1,
            event_type: "staff.upserted".into(),
            schema_version: 1,
            aggregate_type: "staff_user".into(),
            aggregate_id: user.to_string(),
            base_version: Some(base),
            occurred_at: "2026-11-05T09:14:03.120Z".into(),
            payload: p,
        }
    };
    let events = [
        staff(0, "$argon2id$v=19$m=256,t=1,p=1$c2FsdHNhbHQ$first", true),
        staff(1, "$argon2id$v=19$m=256,t=1,p=1$c2FsdHNhbHQ$second", true),
        staff(2, "$argon2id$v=19$m=256,t=1,p=1$c2FsdHNhbHQ$second", false),
    ];
    let (_, b) = push(&s, &t, &events).await;
    assert_eq!(statuses(&b), ["applied"; 3]);
    let row: (String, String, i64) = sqlx::query_as(
        "SELECT pin_hash, status, version FROM staff_users WHERE business_id = $1 AND user_id = $2",
    )
    .bind(t.business)
    .bind(user)
    .fetch_one(s.db.as_ref().unwrap())
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "$argon2id$v=19$m=256,t=1,p=1$c2FsdHNhbHQ$second".into(),
            "inactive".into(),
            3
        )
    );
}

#[tokio::test]
async fn the_end_of_day_master_data_lands_on_the_rows_the_link_created() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Linked").await;
    let (_, b) = push(&s, &t, &end_of_day(&t)).await;
    assert!(statuses(&b).iter().all(|st| st == "applied"), "{b}");
    let q = |sql: &'static str| business_count(&s, &t, sql);
    // The link made the business, location and register at version 0; the register's own
    // first snapshots took them to 1.
    assert_eq!(
        q("SELECT version FROM businesses WHERE business_id = $1 AND snapshot IS NOT NULL").await,
        1
    );
    assert_eq!(
        q("SELECT count(*) FROM locations WHERE business_id = $1 AND version = 1 AND snapshot IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        q("SELECT count(*) FROM registers WHERE business_id = $1 AND version = 1 AND snapshot IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        q("SELECT count(*) FROM business_settings WHERE business_id = $1").await,
        18,
        "every registered setting (18 since tax.registered, D-02)"
    );
    assert_eq!(
        q("SELECT count(*) FROM staff_users WHERE business_id = $1").await,
        1
    );
    assert_eq!(
        q("SELECT count(*) FROM tax_rates WHERE business_id = $1").await,
        1
    );
    assert_eq!(
        q("SELECT count(*) FROM tax_categories WHERE business_id = $1").await,
        1
    );
    assert_eq!(q("SELECT count(*) FROM products WHERE business_id = $1").await, 4);
}

#[tokio::test]
async fn malformed_master_events_are_rejected() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "BadMaster").await;
    let (id, v) = (Uuid::now_v7(), Uuid::now_v7());
    // The payload's version does not follow the base.
    let mut skipped = product_event(&t, 1, 0, product_payload(id, v, 0, "Cola", None, None, false));
    skipped.payload["version"] = json!(5);
    // Describes another register.
    let other_register = json!({"registerId": Uuid::now_v7(), "businessId": t.business, "locationId": t.location,
        "name": "Till 9", "code": "R09", "receiptPrefix": "", "retired": false, "version": 1, "createdAt": 1, "updatedAt": 1});
    let mut foreign = product_event(&t, 2, 0, product_payload(id, v, 0, "Cola", None, None, false));
    foreign.event_type = "register.upserted".into();
    foreign.aggregate_type = "register".into();
    foreign.aggregate_id = other_register["registerId"].as_str().unwrap().into();
    foreign.payload = other_register;
    let (_, b) = push(&s, &t, &[skipped, foreign]).await;
    assert_eq!(statuses(&b), ["rejected", "rejected"], "{b}");
    assert!(
        b["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["code"] == "EVENT_INVALID")
    );
}

#[tokio::test]
async fn the_cloud_edits_products_through_the_same_rules() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "CloudEdit").await;
    let (id, v) = (Uuid::now_v7(), Uuid::now_v7());
    let save = |base: u64, name: &str, barcode: Option<&str>| {
        json!({"businessId": t.business, "expectedVersion": base,
               "product": product_payload(id, v, base, name, None, barcode, false)})
    };
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/catalog/products",
        Some(&t.user_token),
        Some(save(0, "Tea", Some("5012345678900"))),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["version"], 1);
    // An edit from version 0 again: the cloud is at 1.
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/catalog/products",
        Some(&t.user_token),
        Some(save(0, "Tea!", None)),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert!(body.contains("CONFLICT_VERSION"), "{body}");
    // A register's product with that barcode now conflicts with the cloud's.
    let (_, r) = push(
        &s,
        &t,
        &[product_event(
            &t,
            1,
            0,
            product_payload(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                "Other",
                None,
                Some("5012345678900"),
                false,
            ),
        )],
    )
    .await;
    assert_eq!(r["results"][0]["code"], "BARCODE_TAKEN");
    // The change feed records the cloud as the origin.
    let origins: i64 = business_count(
        &s,
        &t,
        "SELECT count(*) FROM change_log WHERE business_id = $1 AND aggregate_type = 'product' AND origin_register IS NULL",
    )
    .await;
    assert_eq!(origins, 1);
    // Another owner cannot edit this business's catalogue.
    let stranger = tenant(&s, "Stranger").await;
    let (code, _) = call(
        &s,
        Method::POST,
        "/v1/catalog/products",
        Some(&stranger.user_token),
        Some(save(1, "Mine", None)),
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_registers_racing_for_one_barcode_get_one_winner() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "Race").await;
    let two = second_register(&s, &one).await;
    for round in 0..5 {
        let code = format!("40063813339{round:02}");
        let mut set = tokio::task::JoinSet::new();
        for (t, seq) in [(one.clone(), round + 1), (two.clone(), round + 1)] {
            let (s, code) = (s.clone(), code.clone());
            set.spawn(async move {
                let p = product_payload(
                    Uuid::now_v7(),
                    Uuid::now_v7(),
                    0,
                    "Racer",
                    None,
                    Some(&code),
                    false,
                );
                let (_, b) = push(&s, &t, &[product_event(&t, seq, 0, p)]).await;
                b["results"][0]["status"].as_str().unwrap().to_owned()
            });
        }
        let mut outcomes = Vec::new();
        while let Some(r) = set.join_next().await {
            outcomes.push(r.unwrap());
        }
        outcomes.sort();
        assert_eq!(outcomes, ["applied", "conflict"], "round {round}");
    }
}
