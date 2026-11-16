//! E17-T07: what registers pull. The change feed in pages and from a cursor, the bootstrap
//! snapshot in pages and resumed from a token, and the visibility property of the feed
//! (option (a) of 05 C0003): with 8 concurrent writers, a reader pulling all the time sees
//! every committed row and never skips one. PostgreSQL required (`TEST_DATABASE_URL`).

mod common;

use axum::http::{Method, StatusCode};
use common::{Tenant, call, end_of_day, fact_appliers, push, second_register, state_with, tenant};
use pos_sync::pull::{BootstrapPage, ChangesResponse};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

async fn get<T: serde::de::DeserializeOwned>(
    s: &cloud_api::AppState,
    t: &Tenant,
    path: &str,
) -> (StatusCode, Option<T>) {
    let (code, body) = call(s, Method::GET, path, Some(&t.device_token), None).await;
    (code, serde_json::from_str(&body).ok())
}

#[tokio::test]
async fn another_register_pulls_the_day_in_pages_and_resumes_from_its_cursor() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "Feed").await;
    let two = second_register(&s, &one).await;
    let day = end_of_day(&one);
    push(&s, &one, &day).await;
    let mut cursor = String::new();
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let (code, page) =
            get::<ChangesResponse>(&s, &two, &format!("/v1/sync/changes?limit=10&cursor={cursor}")).await;
        assert_eq!(code, StatusCode::OK);
        let page = page.unwrap();
        pages += 1;
        seen.extend(page.changes);
        cursor = page.next_cursor;
        if !page.has_more {
            break;
        }
    }
    // 22 master changes (business, location, register, staff, a rate, a category, 12
    // settings, 4 products) and 19 facts; the audit-free day has nothing else.
    assert_eq!(seen.len(), 41);
    assert_eq!(pages, 5);
    let seqs: Vec<i64> = seen.iter().map(|c| c.server_seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in feed order: {seqs:?}");
    for c in &seen {
        assert_eq!(c.origin_register_id, Some(one.register));
        pos_sync::parse_payload(&c.event_type, c.schema_version, &c.snapshot)
            .unwrap_or_else(|e| panic!("{}: {e}", c.event_type));
    }
    // Nothing new: the same cursor comes back.
    let (_, again) = get::<ChangesResponse>(&s, &two, &format!("/v1/sync/changes?cursor={cursor}")).await;
    let again = again.unwrap();
    assert!(again.changes.is_empty() && !again.has_more);
    assert_eq!(again.next_cursor, cursor);
    // A cursor this server never issued is refused.
    let (code, _) = get::<Value>(&s, &two, "/v1/sync/changes?cursor=v9:1").await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_bootstrap_pages_through_every_aggregate_and_resumes_from_a_token() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "Bootstrap").await;
    let two = second_register(&s, &one).await;
    push(&s, &one, &end_of_day(&one)).await;
    let (_, first) = get::<BootstrapPage>(&s, &two, "/v1/sync/bootstrap?size=5").await;
    let first = first.unwrap();
    let start = first.snapshot_cursor.clone();
    let mut entities = first.entities.clone();
    let mut token = first.next_page.clone();
    let mut tokens = Vec::new();
    while let Some(t) = token {
        tokens.push(t.clone());
        let (code, page) =
            get::<BootstrapPage>(&s, &two, &format!("/v1/sync/bootstrap?size=5&page={t}")).await;
        assert_eq!(code, StatusCode::OK);
        let page = page.unwrap();
        assert_eq!(
            page.snapshot_cursor, start,
            "every page names the same starting point"
        );
        entities.extend(page.entities);
        token = page.next_page;
    }
    // Business, location, register 1 (register 2 has no snapshot yet), a rate, a category,
    // 12 settings, the owner and 4 products; references first.
    assert_eq!(entities.len(), 22);
    let types: Vec<&str> = entities.iter().map(|e| e.aggregate_type.as_str()).collect();
    assert_eq!(&types[..3], ["business", "location", "register"]);
    assert_eq!(*types.last().unwrap(), "product");
    let ids: BTreeSet<(&str, &str)> = entities
        .iter()
        .map(|e| (e.aggregate_type.as_str(), e.aggregate_id.as_str()))
        .collect();
    assert_eq!(ids.len(), 22, "no aggregate twice");
    for e in &entities {
        assert!(e.version >= 1);
        pos_sync::parse_payload(&e.event_type, e.schema_version, &e.snapshot).unwrap();
    }
    // Resuming from a token gives the same rest.
    let resume = &tokens[2];
    let (_, page) = get::<BootstrapPage>(&s, &two, &format!("/v1/sync/bootstrap?size=5&page={resume}")).await;
    assert_eq!(page.unwrap().entities, entities[15..20]);
    // The feed from the snapshot's cursor holds what changed after it began.
    let (_, after) = get::<ChangesResponse>(&s, &two, &format!("/v1/sync/changes?cursor={start}")).await;
    assert!(after.unwrap().changes.is_empty());
    let (code, _) = get::<Value>(&s, &two, "/v1/sync/bootstrap?page=v1:0:42:").await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Option (a): 8 writers append (some roll back) while a reader pulls without pause. The
/// reader must end up with exactly the committed rows, each seen once, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 10)]
async fn the_feed_never_skips_a_row_under_concurrent_writers() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Visibility").await;
    let pool = s.db.clone().unwrap();
    let principal = pos_cloud_db::Principal {
        business_id: pos_domain::BusinessId::from_uuid(t.business),
    };
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (pool, done) = (pool.clone(), done.clone());
        tokio::spawn(async move {
            let mut cursor = 0;
            let mut seen = Vec::new();
            loop {
                let finished = done.load(std::sync::atomic::Ordering::SeqCst);
                let mut tx = pos_cloud_db::TenantTx::begin(&pool, &principal).await.unwrap();
                let rows = pos_cloud_db::sync::changes_after(&mut tx, cursor, 50)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                if let Some(last) = rows.last() {
                    cursor = last.server_seq;
                }
                let empty = rows.is_empty();
                seen.extend(rows.into_iter().map(|r| r.server_seq));
                if finished && empty {
                    return seen;
                }
            }
        })
    };
    let mut writers = tokio::task::JoinSet::new();
    for w in 0..8u64 {
        let pool = pool.clone();
        writers.spawn(async move {
            let mut committed = Vec::new();
            for i in 0..40u64 {
                let mut tx = pos_cloud_db::TenantTx::begin(&pool, &principal).await.unwrap();
                let snapshot = serde_json::json!({"writer": w, "i": i});
                let seq = pos_cloud_db::sync::append_change(
                    &mut tx,
                    &pos_cloud_db::sync::NewChange {
                        event_type: "audit.recorded",
                        schema_version: 1,
                        aggregate_type: "audit",
                        aggregate_id: &Uuid::now_v7().to_string(),
                        version: None,
                        kind: "fact",
                        origin_register: None,
                        origin_event_id: None,
                        snapshot: &snapshot,
                    },
                )
                .await
                .unwrap();
                // Hold the transaction open a little: the race a skip needs.
                tokio::time::sleep(std::time::Duration::from_millis((w * 7 + i * 3) % 5)).await;
                if (w + i) % 9 == 0 {
                    tx.rollback().await.unwrap();
                } else {
                    tx.commit().await.unwrap();
                    committed.push(seq);
                }
            }
            committed
        });
    }
    let mut committed = BTreeSet::new();
    while let Some(r) = writers.join_next().await {
        committed.extend(r.unwrap());
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let seen = reader.await.unwrap();
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "each row once, in order");
    let seen: BTreeSet<i64> = seen.into_iter().collect();
    assert_eq!(
        committed.len(),
        8 * 40
            - (0..8u64)
                .flat_map(|w| (0..40u64).map(move |i| (w + i) % 9 == 0))
                .filter(|r| *r)
                .count()
    );
    assert_eq!(seen, committed, "the reader saw exactly the committed rows");
}

/// E17-T07 acceptance bench, server side: serving a 50,000-product bootstrap in pages of
/// 2,000. Run with `cargo test -p cloud-api --test sync_pull --release -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "bench"]
async fn bench_serving_a_fifty_thousand_product_bootstrap() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let one = tenant(&s, "BigCatalogue").await;
    let pool = s.db.clone().unwrap();
    // 50,000 products with realistic snapshots, written directly (as the migration owner).
    sqlx::query(
        "INSERT INTO products (business_id, product_id, name, product_type, unit_code, tax_category_id, quick_select,
                               status, version, snapshot)
         SELECT $1, gen_random_uuid(), 'Product ' || n, 'stock', 'ea', gen_random_uuid(), false, 'active', 1,
                jsonb_build_object('id', gen_random_uuid(), 'name', 'Product ' || n, 'description', null,
                  'categoryId', null, 'productType', 'stock', 'unit', 'ea', 'taxCategoryId', gen_random_uuid(),
                  'quickSelect', false, 'archived', false, 'version', 1,
                  'variants', jsonb_build_array(jsonb_build_object('id', gen_random_uuid(), 'isDefault', true,
                    'sku', 'SKU-' || n, 'options', '{}'::jsonb, 'price', 100 + n % 900, 'cost', null,
                    'trackStock', true, 'lowStockThreshold', null, 'scanQty', 1000, 'archived', false,
                    'barcodes', jsonb_build_array(jsonb_build_object('id', gen_random_uuid(),
                      'value', lpad(n::text, 13, '2'), 'raw', lpad(n::text, 13, '2'), 'symbology', 'ean13',
                      'qualifier', 'primary', 'packQty', 1000)))))
           FROM generate_series(1, 50000) AS n",
    )
    .bind(one.business)
    .execute(&pool)
    .await
    .unwrap();
    let started = std::time::Instant::now();
    let (mut token, mut entities, mut bytes, mut pages) = (None::<String>, 0, 0, 0);
    loop {
        let path = match &token {
            Some(t) => format!("/v1/sync/bootstrap?page={t}"),
            None => "/v1/sync/bootstrap".to_owned(),
        };
        let (code, body) = call(&s, Method::GET, &path, Some(&one.device_token), None).await;
        assert_eq!(code, StatusCode::OK);
        bytes += body.len();
        let page: BootstrapPage = serde_json::from_str(&body).unwrap();
        entities += page.entities.len();
        pages += 1;
        token = page.next_page;
        if token.is_none() {
            break;
        }
    }
    eprintln!(
        "served {entities} aggregates in {pages} pages, {} kB, in {:?}",
        bytes / 1000,
        started.elapsed()
    );
    assert!(entities >= 50_000);
}
