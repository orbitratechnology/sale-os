//! E16-T01 acceptance: the API starts, answers health, and reuses `pos-domain`.
//! The Postgres-backed test runs when `TEST_DATABASE_URL` is set (CI provides a service
//! container); otherwise it is skipped with a message.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cloud_api::{AppState, Config, router};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

async fn call(state: AppState, req: Request<Body>) -> (StatusCode, serde_json::Value, String) {
    let res = router(state).oneshot(req).await.unwrap();
    let status = res.status();
    let ctype = res
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        ctype,
    )
}

fn state(config: Config) -> AppState {
    AppState {
        config: Arc::new(config),
        db: None,
    }
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn health_without_database() {
    let (status, body, _) = call(state(Config::for_tests()), get("/v1/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["database"], "not_configured");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn health_reports_unavailable_database_as_503() {
    let mut cfg = Config::for_tests();
    cfg.database_url = Some("postgres://nobody@127.0.0.1:1/none".into());
    let db = Some(pos_cloud_db::connect_lazy(cfg.database_url.as_deref().unwrap()).unwrap());
    let (status, body, _) = call(
        AppState {
            config: Arc::new(cfg),
            db,
        },
        get("/v1/health"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["database"], "unavailable");
}

#[tokio::test]
async fn version_policy() {
    let (status, body, _) = call(state(Config::for_tests()), get("/v1/version-policy")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["minSupported"], "0.1.0");
    assert!(body.get("updateUrl").is_none());
}

#[tokio::test]
async fn debug_route_uses_the_shared_domain_crate() {
    let req = post_json(
        "/v1/_debug/money-add",
        serde_json::json!({"aMinor": "1999", "bMinor": "1", "currency": "USD"}),
    );
    let (status, body, _) = call(state(Config::for_tests()), req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sumMinor"], "2000");
    assert_eq!(body["formatted"], "20.00");
}

#[tokio::test]
async fn debug_route_errors_are_problem_json_with_code() {
    let req = post_json(
        "/v1/_debug/money-add",
        serde_json::json!({"aMinor": "1", "bMinor": "1", "currency": "usd"}),
    );
    let (status, body, ctype) = call(state(Config::for_tests()), req).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(ctype, "application/problem+json");
    assert_eq!(body["code"], "VALIDATION_FAILED");
    assert_eq!(body["type"], "https://errors.sale-os.dev/VALIDATION_FAILED");
}

#[tokio::test]
async fn debug_routes_absent_unless_enabled() {
    let mut cfg = Config::for_tests();
    cfg.debug_routes = false;
    let req = post_json(
        "/v1/_debug/money-add",
        serde_json::json!({"aMinor": "1", "bMinor": "1", "currency": "USD"}),
    );
    let (status, _, _) = call(state(cfg), req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn migrates_and_reports_healthy_with_postgres() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let pool = pos_cloud_db::connect_lazy(&url).unwrap();
    pos_cloud_db::migrate(&pool).await.unwrap();
    pos_cloud_db::migrate(&pool)
        .await
        .expect("migrations are idempotent");
    let family: String = sqlx::query_scalar("SELECT value FROM service_meta WHERE key = 'schema_family'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(family, "sale-os-cloud");
    let mut cfg = Config::for_tests();
    cfg.database_url = Some(url);
    let (status, body, _) = call(
        AppState {
            config: Arc::new(cfg),
            db: Some(pool),
        },
        get("/v1/health"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["database"], "ok");
}
