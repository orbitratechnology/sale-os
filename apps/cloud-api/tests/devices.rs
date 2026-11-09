//! E16-T04: businesses, device registration, refresh rotation and revocation against
//! PostgreSQL (`TEST_DATABASE_URL`; skipped without it).

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::get;
use axum::{Json, Router};
use cloud_api::devices::DevicePrincipal;
use cloud_api::{AppState, Config, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn state() -> Option<AppState> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return None;
    };
    let pool = pos_cloud_db::connect_lazy(&url).unwrap();
    pos_cloud_db::migrate(&pool).await.unwrap();
    let mut cfg = Config::for_tests();
    cfg.trust_proxy = true;
    Some(AppState::new(cfg, Some(pool)))
}

async fn call(
    s: &AppState,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        // A fresh client address per call keeps the sign-in rate limit out of the way.
        .header("x-forwarded-for", format!("10.9.{}.{}", rand_byte(), rand_byte()));
    if let Some(t) = token {
        r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => r
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => r.body(Body::empty()),
    }
    .unwrap();
    let app = router(s.clone()).merge(
        Router::new()
            .route("/v1/whoami", get(whoami))
            .with_state(s.clone()),
    );
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn rand_byte() -> u8 {
    Uuid::now_v7().as_bytes()[15]
}

/// A route that needs a device: proves what a device token can still do.
async fn whoami(d: DevicePrincipal) -> Json<Value> {
    Json(json!({"deviceId": d.device_id, "registerId": d.register_id}))
}

/// Signs up and signs in a new owner; returns the user token.
async fn owner(s: &AppState) -> String {
    let email = format!("owner-{}@example.com", Uuid::now_v7().simple());
    let creds = json!({"email": email, "password": "correct horse battery staple"});
    assert_eq!(
        call(s, Method::POST, "/v1/auth/signup", None, Some(creds.clone()))
            .await
            .0,
        StatusCode::CREATED
    );
    let (code, body) = call(s, Method::POST, "/v1/auth/login", None, Some(creds)).await;
    assert_eq!(code, StatusCode::OK);
    body["userAccessToken"].as_str().unwrap().to_owned()
}

fn business_body(id: Uuid, name: &str) -> Value {
    json!({"businessId": id, "name": name, "countryCode": "GB", "timezone": "Europe/London",
           "currencyCode": "GBP", "currencyExponent": 2, "pricesIncludeTax": true})
}

fn register_body(business: Uuid, register: Uuid, location: Uuid, replace: bool) -> Value {
    json!({"businessId": business, "registerId": register, "registerName": "Till 1", "registerCode": "R01",
           "locationId": location, "locationName": "Main", "installationId": Uuid::now_v7(),
           "appVersion": "0.1.0", "osInfo": "Linux", "replaceExisting": replace})
}

#[tokio::test]
async fn a_business_is_created_once_by_its_owner() {
    let Some(s) = state().await else { return };
    let (a, b) = (owner(&s).await, owner(&s).await);
    let id = Uuid::now_v7();
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/businesses",
        Some(&a),
        Some(business_body(id, "Corner Shop")),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{body}");
    assert_eq!(
        (body["businessId"].as_str(), body["currencyExponent"].as_i64()),
        (Some(id.to_string().as_str()), Some(2))
    );
    // The same owner again (a retry): the same business, not a second one.
    let (code, again) = call(
        &s,
        Method::POST,
        "/v1/businesses",
        Some(&a),
        Some(business_body(id, "Corner Shop")),
    )
    .await;
    assert_eq!((code, again["version"].as_i64()), (StatusCode::OK, Some(1)));
    // Another owner with that id: refused, nothing about it revealed.
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/businesses",
        Some(&b),
        Some(business_body(id, "Mine now")),
    )
    .await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("SYNC_BUSINESS_ID_TAKEN"))
    );
    assert!(body.get("name").is_none());
    // Each owner lists only their own.
    let (_, list) = call(&s, Method::GET, "/v1/businesses", Some(&a), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, list) = call(&s, Method::GET, "/v1/businesses", Some(&b), None).await;
    assert_eq!(list, json!([]));
    // A wrong exponent and no token.
    let mut bad = business_body(Uuid::now_v7(), "X");
    bad["currencyExponent"] = json!(3);
    assert_eq!(
        call(&s, Method::POST, "/v1/businesses", Some(&a), Some(bad))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/businesses",
        None,
        Some(business_body(Uuid::now_v7(), "X")),
    )
    .await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("AUTH_REQUIRED"))
    );
}

#[tokio::test]
async fn devices_register_rotate_and_are_revoked() {
    let Some(s) = state().await else { return };
    let a = owner(&s).await;
    let (business, register, location) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    call(
        &s,
        Method::POST,
        "/v1/businesses",
        Some(&a),
        Some(business_body(business, "Shop")),
    )
    .await;
    let (code, first) = call(
        &s,
        Method::POST,
        "/v1/devices/register",
        Some(&a),
        Some(register_body(business, register, location, false)),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{first}");
    assert_eq!(first["expiresIn"], 900);
    let (first_access, first_refresh) = (
        first["accessToken"].as_str().unwrap(),
        first["refreshToken"].as_str().unwrap(),
    );
    let (code, me) = call(&s, Method::GET, "/v1/whoami", Some(first_access), None).await;
    assert_eq!(
        (code, me["registerId"].as_str()),
        (StatusCode::OK, Some(register.to_string().as_str()))
    );
    // A user token is not a device token.
    assert_eq!(
        call(&s, Method::GET, "/v1/whoami", Some(&a), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Only the refresh token's hash is stored.
    let pool = s.db.clone().unwrap();
    let stored: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT token_hash FROM device_refresh_tokens WHERE business_id = $1")
            .bind(business)
            .fetch_all(&pool)
            .await
            .unwrap();
    use sha2::Digest;
    assert_eq!(
        stored,
        vec![sha2::Sha256::digest(first_refresh.as_bytes()).to_vec()]
    );

    // A second installation on the same register: needs the owner's confirmation...
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/devices/register",
        Some(&a),
        Some(register_body(business, register, location, false)),
    )
    .await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("SYNC_REGISTER_IN_USE"))
    );
    // ...then replaces it, and the first device is cut off at once (its token is still unexpired).
    let (code, second) = call(
        &s,
        Method::POST,
        "/v1/devices/register",
        Some(&a),
        Some(register_body(business, register, location, true)),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    let (code, body) = call(&s, Method::GET, "/v1/whoami", Some(first_access), None).await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("SYNC_DEVICE_REVOKED"))
    );
    assert_eq!(body["detail"], "This register was disconnected by the owner");
    let (code, _) = call(
        &s,
        Method::POST,
        "/v1/auth/device/refresh",
        None,
        Some(json!({"refreshToken": first_refresh})),
    )
    .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);

    // Rotation: a refresh gives new tokens; the old refresh token is spent.
    let r0 = second["refreshToken"].as_str().unwrap().to_owned();
    let (code, rotated) = call(
        &s,
        Method::POST,
        "/v1/auth/device/refresh",
        None,
        Some(json!({"refreshToken": r0})),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{rotated}");
    let (a1, r1) = (
        rotated["accessToken"].as_str().unwrap().to_owned(),
        rotated["refreshToken"].as_str().unwrap().to_owned(),
    );
    assert_ne!(r1, r0);
    assert_eq!(
        call(&s, Method::GET, "/v1/whoami", Some(&a1), None).await.0,
        StatusCode::OK
    );
    // The spent token again: someone has a copy, so the device is revoked.
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/auth/device/refresh",
        None,
        Some(json!({"refreshToken": r0})),
    )
    .await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("SYNC_DEVICE_REVOKED"))
    );
    assert_eq!(
        call(&s, Method::GET, "/v1/whoami", Some(&a1), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (code, _) = call(
        &s,
        Method::POST,
        "/v1/auth/device/refresh",
        None,
        Some(json!({"refreshToken": r1})),
    )
    .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "the whole family is spent");
    // An unknown token: sign in again.
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/auth/device/refresh",
        None,
        Some(json!({"refreshToken": "nonsense"})),
    )
    .await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("AUTH_REQUIRED"))
    );

    // The owner sees both devices, both revoked now.
    let (_, devices) = call(&s, Method::GET, "/v1/devices", Some(&a), None).await;
    let statuses: Vec<&str> = devices
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, vec!["revoked", "revoked"]);
}

#[tokio::test]
async fn two_refreshes_with_one_token_cannot_both_win() {
    let Some(s) = state().await else { return };
    let a = owner(&s).await;
    let (business, register) = (Uuid::now_v7(), Uuid::now_v7());
    call(
        &s,
        Method::POST,
        "/v1/businesses",
        Some(&a),
        Some(business_body(business, "Shop")),
    )
    .await;
    let (_, creds) = call(
        &s,
        Method::POST,
        "/v1/devices/register",
        Some(&a),
        Some(register_body(business, register, Uuid::now_v7(), false)),
    )
    .await;
    let token = creds["refreshToken"].as_str().unwrap().to_owned();
    let body = json!({"refreshToken": token});
    let (x, y) = tokio::join!(
        call(
            &s,
            Method::POST,
            "/v1/auth/device/refresh",
            None,
            Some(body.clone())
        ),
        call(
            &s,
            Method::POST,
            "/v1/auth/device/refresh",
            None,
            Some(body.clone())
        ),
    );
    let ok = [x.0, y.0].iter().filter(|c| **c == StatusCode::OK).count();
    assert_eq!(ok, 1, "{x:?} {y:?}");
}

#[tokio::test]
async fn owners_reach_only_their_own_businesses_and_devices() {
    let Some(s) = state().await else { return };
    let (a, b) = (owner(&s).await, owner(&s).await);
    let (business, register) = (Uuid::now_v7(), Uuid::now_v7());
    call(
        &s,
        Method::POST,
        "/v1/businesses",
        Some(&a),
        Some(business_body(business, "A's shop")),
    )
    .await;
    let (_, creds) = call(
        &s,
        Method::POST,
        "/v1/devices/register",
        Some(&a),
        Some(register_body(business, register, Uuid::now_v7(), false)),
    )
    .await;
    let (device, access) = (
        creds["deviceId"].as_str().unwrap().to_owned(),
        creds["accessToken"].as_str().unwrap().to_owned(),
    );
    // B cannot register a device in A's business, list A's devices, or revoke A's device.
    let (code, body) = call(
        &s,
        Method::POST,
        "/v1/devices/register",
        Some(&b),
        Some(register_body(business, Uuid::now_v7(), Uuid::now_v7(), true)),
    )
    .await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("NOT_FOUND"))
    );
    assert_eq!(
        call(&s, Method::GET, "/v1/devices", Some(&b), None).await.1,
        json!([])
    );
    let (code, _) = call(
        &s,
        Method::POST,
        &format!("/v1/devices/{device}/revoke"),
        Some(&b),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(
        call(&s, Method::GET, "/v1/whoami", Some(&access), None).await.0,
        StatusCode::OK
    );
    // A revokes it: cut off immediately.
    let (code, _) = call(
        &s,
        Method::POST,
        &format!("/v1/devices/{device}/revoke"),
        Some(&a),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, body) = call(&s, Method::GET, "/v1/whoami", Some(&access), None).await;
    assert_eq!(
        (code, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("SYNC_DEVICE_REVOKED"))
    );
}
