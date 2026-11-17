//! E16-T06: `GET /v1/catalog/products`, the owner's read-only product list. By name, archived
//! products included; a query matches part of a name or a SKU, or a whole barcode; pages
//! with a total. PostgreSQL required (`TEST_DATABASE_URL`).

mod common;

use axum::http::{Method, StatusCode};
use common::{call, fact_appliers, product_payload, state_with, tenant};
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn the_owner_lists_and_searches_the_cloud_catalogue() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let t = tenant(&s, "Catalogue").await;
    let products = [
        ("Cola 330ml", Some("COLA-330"), Some("5012345678900"), false),
        ("Bread, white", Some("BRD-1"), None, false),
        ("Old cola", None, None, true),
        ("100% juice", None, None, false),
    ];
    for (name, sku, barcode, archived) in products {
        let body = json!({"businessId": t.business, "expectedVersion": 0,
                          "product": product_payload(Uuid::now_v7(), Uuid::now_v7(), 0, name, sku, barcode, archived)});
        let (code, text) = call(
            &s,
            Method::POST,
            "/v1/catalog/products",
            Some(&t.user_token),
            Some(body),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{text}");
    }
    let list = |q: &str| {
        let s = s.clone();
        let (token, biz) = (t.user_token.clone(), t.business);
        let q = q.to_owned();
        async move {
            let (code, text) = call(
                &s,
                Method::GET,
                &format!("/v1/catalog/products?businessId={biz}{q}"),
                Some(&token),
                None,
            )
            .await;
            assert_eq!(code, StatusCode::OK, "{text}");
            let v: Value = serde_json::from_str(&text).unwrap();
            let names: Vec<String> = v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["product"]["name"].as_str().unwrap().to_owned())
                .collect();
            (names, v["total"].as_i64().unwrap())
        }
    };

    // Everything, by name, archived included.
    let (all, total) = list("").await;
    assert_eq!(all, ["100% juice", "Bread, white", "Cola 330ml", "Old cola"]);
    assert_eq!(total, 4);
    // Part of a name (case-insensitive), part of a SKU, a whole barcode.
    assert_eq!(
        list("&query=COLA").await,
        (vec!["Cola 330ml".into(), "Old cola".into()], 2)
    );
    assert_eq!(list("&query=brd").await.0, ["Bread, white"]);
    assert_eq!(list("&query=5012345678900").await.0, ["Cola 330ml"]);
    assert_eq!(
        list("&query=50123").await.0,
        Vec::<String>::new(),
        "barcodes match whole"
    );
    // `%` and `_` are plain characters, not wildcards.
    assert_eq!(list("&query=100%25").await.0, ["100% juice"]);
    assert_eq!(list("&query=_").await.0, Vec::<String>::new());
    // Pages, each with the total.
    assert_eq!(
        list("&pageSize=3").await,
        (
            vec!["100% juice".into(), "Bread, white".into(), "Cola 330ml".into()],
            4
        )
    );
    assert_eq!(list("&pageSize=3&page=2").await, (vec!["Old cola".into()], 4));
    assert_eq!(list("&pageSize=3&page=9").await, (vec![], 4));

    // Another owner sees nothing of it.
    let other = tenant(&s, "Elsewhere").await;
    let (code, _) = call(
        &s,
        Method::GET,
        &format!("/v1/catalog/products?businessId={}", t.business),
        Some(&other.user_token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    // A register's token is not an owner's.
    let (code, _) = call(
        &s,
        Method::GET,
        &format!("/v1/catalog/products?businessId={}", t.business),
        Some(&t.device_token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
}
