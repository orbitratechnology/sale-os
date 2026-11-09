//! E16-T03: owner sign-up and sign-in against PostgreSQL (`TEST_DATABASE_URL`; skipped
//! without it).

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use axum::{Json, Router};
use cloud_api::auth::UserPrincipal;
use cloud_api::auth::identity::{IdentityError, IdentityProvider, PasswordIdentity};
use cloud_api::auth::password::Hasher;
use cloud_api::{AppState, Config, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

fn url() -> Option<String> {
    let u = std::env::var("TEST_DATABASE_URL").ok();
    if u.is_none() {
        eprintln!("skipping: TEST_DATABASE_URL not set");
    }
    u
}

async fn state(url: &str) -> AppState {
    let pool = pos_cloud_db::connect_lazy(url).unwrap();
    pos_cloud_db::migrate(&pool).await.unwrap();
    let mut cfg = Config::for_tests();
    cfg.trust_proxy = true;
    AppState::new(cfg, Some(pool))
}

fn email() -> String {
    format!("owner-{}@example.com", uuid::Uuid::now_v7().simple())
}

async fn post(state: &AppState, path: &str, body: Value, ip: &str) -> (StatusCode, Option<String>, Value) {
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
    let status = res.status();
    let retry = res
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        retry,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

const GOOD: &str = "correct horse battery staple";

#[tokio::test]
async fn signup_then_login_gives_a_15_minute_user_token() {
    let Some(url) = url() else { return };
    let s = state(&url).await;
    let e = email();
    let (code, _, body) = post(
        &s,
        "/v1/auth/signup",
        json!({"email": e, "password": GOOD}),
        "10.0.0.1",
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{body}");
    let account = body["accountId"].as_str().unwrap().to_owned();
    // The same email in other case: taken.
    let (code, _, body) = post(
        &s,
        "/v1/auth/signup",
        json!({"email": e.to_uppercase(), "password": GOOD}),
        "10.0.0.1",
    )
    .await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("AUTH_EMAIL_TAKEN"))
    );
    // Weak passwords and bad emails: 422 with what to fix.
    for (email, password, why) in [
        (email(), "short", "use at least 10 characters"),
        (
            email(),
            "1234567890",
            "this password is too common; choose another",
        ),
        ("not-an-email".to_owned(), GOOD, "that is not an email address"),
    ] {
        let (code, _, body) = post(
            &s,
            "/v1/auth/signup",
            json!({"email": email, "password": password}),
            "10.0.0.2",
        )
        .await;
        assert_eq!(
            (code, body["code"].as_str(), body["detail"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("VALIDATION_FAILED"),
                Some(why)
            )
        );
    }
    let (code, _, body) = post(
        &s,
        "/v1/auth/login",
        json!({"email": e, "password": GOOD}),
        "10.0.0.3",
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["expiresIn"], 900);
    let token = body["userAccessToken"].as_str().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let claims = s.auth.signer.verify(token, "user", now).unwrap();
    assert_eq!(claims.sub, account);
    assert_eq!(claims.exp - claims.iat, 900);

    // The token opens routes that need an owner; anything else does not.
    let app = Router::new()
        .route(
            "/me",
            get(|p: UserPrincipal| async move { Json(json!({"accountId": p.account_id})) }),
        )
        .with_state(s.clone());
    let call = |auth: Option<String>| {
        let app = app.clone();
        async move {
            let mut r = Request::get("/me");
            if let Some(a) = auth {
                r = r.header(header::AUTHORIZATION, a);
            }
            let res = app.oneshot(r.body(Body::empty()).unwrap()).await.unwrap();
            let status = res.status();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }
    };
    let (code, body) = call(Some(format!("Bearer {token}"))).await;
    assert_eq!(
        (code, body["accountId"].as_str()),
        (StatusCode::OK, Some(account.as_str()))
    );
    for bad in [
        None,
        Some("Bearer nonsense".to_owned()),
        Some(format!("Basic {token}")),
    ] {
        let (code, body) = call(bad).await;
        assert_eq!(
            (code, body["code"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("AUTH_REQUIRED"))
        );
    }
    // A device token is not a user token.
    let mut device = claims.clone();
    device.kind = "device".into();
    let (code, _) = call(Some(format!("Bearer {}", s.auth.signer.sign(&device)))).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_password_and_unknown_email_look_the_same() {
    let Some(url) = url() else { return };
    let s = state(&url).await;
    let e = email();
    post(
        &s,
        "/v1/auth/signup",
        json!({"email": e, "password": GOOD}),
        "10.1.0.1",
    )
    .await;
    let wrong = post(
        &s,
        "/v1/auth/login",
        json!({"email": e, "password": "wrong horse battery staple"}),
        "10.1.0.2",
    )
    .await;
    let unknown = post(
        &s,
        "/v1/auth/login",
        json!({"email": email(), "password": GOOD}),
        "10.1.0.3",
    )
    .await;
    assert_eq!(wrong.0, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.2["code"], "AUTH_CREDENTIALS_INVALID");
    assert_eq!(
        (wrong.0, wrong.2.to_string()),
        (unknown.0, unknown.2.to_string()),
        "identical status and body"
    );
}

#[tokio::test]
async fn wrong_password_and_unknown_email_take_the_same_time() {
    let Some(url) = url() else { return };
    // A realistic cost (OWASP's minimum for Argon2id, 19 MiB × 2): if an unknown email
    // skipped the hash it would answer measurably faster.
    let pool = pos_cloud_db::connect_lazy(&url).unwrap();
    pos_cloud_db::migrate(&pool).await.unwrap();
    let id = PasswordIdentity::new(pool, Arc::new(Hasher::new(19_456, 2)));
    let e = email();
    id.signup(&e, GOOD).await.unwrap();
    let unknown = email();
    let (mut wrong_total, mut unknown_total) = (Duration::ZERO, Duration::ZERO);
    let runs = 100;
    for _ in 0..runs {
        let t = Instant::now();
        assert!(matches!(
            id.login(&e, "wrong horse battery staple").await,
            Err(IdentityError::InvalidCredentials)
        ));
        wrong_total += t.elapsed();
        let t = Instant::now();
        assert!(matches!(
            id.login(&unknown, GOOD).await,
            Err(IdentityError::InvalidCredentials)
        ));
        unknown_total += t.elapsed();
    }
    let (w, u) = (wrong_total / runs, unknown_total / runs);
    eprintln!("mean wrong password {w:?}, unknown email {u:?}");
    let diff = w.abs_diff(u);
    assert!(diff <= Duration::from_millis(10), "wrong {w:?} vs unknown {u:?}");
    assert!(
        w >= Duration::from_millis(5),
        "the hash is really being computed ({w:?})"
    );
}

#[tokio::test]
async fn sign_in_attempts_are_rate_limited_per_email_and_per_ip() {
    let Some(url) = url() else { return };
    let s = state(&url).await;
    let e = email();
    // Five attempts on one email (from different addresses) are answered; the sixth waits.
    for i in 0..5 {
        let (code, _, _) = post(
            &s,
            "/v1/auth/login",
            json!({"email": e, "password": "wrong horse battery"}),
            &format!("10.2.0.{i}"),
        )
        .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
    }
    let (code, retry, body) = post(
        &s,
        "/v1/auth/login",
        json!({"email": e, "password": GOOD}),
        "10.2.0.9",
    )
    .await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("AUTH_RATE_LIMITED"))
    );
    assert_eq!(retry.as_deref(), Some("60"));
    // One address trying many emails is limited too.
    for _ in 0..5 {
        post(
            &s,
            "/v1/auth/login",
            json!({"email": email(), "password": GOOD}),
            "10.2.1.1",
        )
        .await;
    }
    let (code, _, _) = post(
        &s,
        "/v1/auth/login",
        json!({"email": email(), "password": GOOD}),
        "10.2.1.1",
    )
    .await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
    // Sign-ups from one address as well.
    for _ in 0..5 {
        post(
            &s,
            "/v1/auth/signup",
            json!({"email": email(), "password": GOOD}),
            "10.2.2.1",
        )
        .await;
    }
    let (code, _, _) = post(
        &s,
        "/v1/auth/signup",
        json!({"email": email(), "password": GOOD}),
        "10.2.2.1",
    )
    .await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
}
