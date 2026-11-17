//! E16-T03: nothing secret reaches the logs. Its own test binary: `tracing` caches which log
//! lines are enabled per process, so other tests logging first could hide lines from the
//! capturing subscriber and let this pass for the wrong reason.

use axum::body::Body;
use axum::http::{Request, header};
use cloud_api::{AppState, Config, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn post(state: &AppState, path: &str, body: Value, ip: &str) -> Value {
    let res = router(state.clone())
        .oneshot(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-forwarded-for", ip)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn state(url: &str) -> AppState {
    let pool = pos_cloud_db::connect_lazy(url).unwrap();
    pos_cloud_db::migrate(&pool).await.unwrap();
    let mut cfg = Config::for_tests();
    cfg.trust_proxy = true;
    AppState::new(cfg, Some(pool))
}

#[test]
fn logs_never_contain_passwords_or_tokens() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let e = format!("owner-{}@example.com", uuid::Uuid::now_v7().simple());
    let secret = "unusual-secret-passphrase-42";
    let mut token = String::new();
    let lines = pos_observability::capture_json_lines(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let s = state(&url).await;
            post(
                &s,
                "/v1/auth/signup",
                json!({"email": e, "password": secret}),
                "10.3.0.1",
            )
            .await;
            post(
                &s,
                "/v1/auth/login",
                json!({"email": e, "password": "not-the-password-x"}),
                "10.3.0.2",
            )
            .await;
            let body = post(
                &s,
                "/v1/auth/login",
                json!({"email": e, "password": secret}),
                "10.3.0.3",
            )
            .await;
            token = body["userAccessToken"].as_str().unwrap().to_owned();
        });
    });
    assert!(
        lines.iter().any(|l| l.to_string().contains("owner signed in")),
        "the sign-in was logged: {lines:?}"
    );
    let all = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    assert!(!token.is_empty());
    for needle in [secret, "not-the-password-x", token.as_str(), &e] {
        assert!(!all.contains(needle), "{needle:?} reached the logs");
    }
}
