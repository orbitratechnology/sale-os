//! E16-T05 / AT-12: one business can never read or change another's data, at the SQL level
//! (every table with `business_id`) and the HTTP level (every secured route in the OpenAPI
//! contract, called with business A's token and business B's ids). PostgreSQL required
//! (`TEST_DATABASE_URL`); skipped without it.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use cloud_api::{AppState, Config, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
) -> (StatusCode, String) {
    let mut r = Request::builder().method(method).uri(path).header(
        "x-forwarded-for",
        format!(
            "10.12.{}.{}",
            Uuid::now_v7().as_bytes()[14],
            Uuid::now_v7().as_bytes()[15]
        ),
    );
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
    let res = router(s.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A business with an owner, a register and a linked device.
struct Tenant {
    user_token: String,
    business: Uuid,
    name: String,
    register: Uuid,
    location: Uuid,
    device: Uuid,
    device_token: String,
    refresh_token: String,
}

async fn tenant(s: &AppState, name: &str) -> Tenant {
    let email = format!("owner-{}@example.com", Uuid::now_v7().simple());
    let creds = json!({"email": email, "password": "correct horse battery staple"});
    call(s, Method::POST, "/v1/auth/signup", None, Some(creds.clone())).await;
    let (_, login) = call(s, Method::POST, "/v1/auth/login", None, Some(creds)).await;
    let user_token = serde_json::from_str::<Value>(&login).unwrap()["userAccessToken"]
        .as_str()
        .unwrap()
        .to_owned();
    let (business, register, location) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let name = format!("{name} {}", Uuid::now_v7().simple());
    let (code, _) = call(
        s,
        Method::POST,
        "/v1/businesses",
        Some(&user_token),
        Some(
            json!({"businessId": business, "name": name, "countryCode": "GB", "timezone": "Europe/London",
                    "currencyCode": "GBP", "currencyExponent": 2, "pricesIncludeTax": true}),
        ),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    let (code, creds) = call(
        s,
        Method::POST,
        "/v1/devices/register",
        Some(&user_token),
        Some(json!({"businessId": business, "registerId": register, "registerName": "Till 1", "registerCode": "R01",
                    "locationId": location, "locationName": "Main", "installationId": Uuid::now_v7(),
                    "appVersion": "0.1.0", "osInfo": null})),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    let creds: Value = serde_json::from_str(&creds).unwrap();
    Tenant {
        user_token,
        business,
        name,
        register,
        location,
        device: creds["deviceId"].as_str().unwrap().parse().unwrap(),
        device_token: creds["accessToken"].as_str().unwrap().to_owned(),
        refresh_token: creds["refreshToken"].as_str().unwrap().to_owned(),
    }
}

#[tokio::test]
async fn at12_sql_no_table_shows_another_business_rows() {
    let Some(s) = state().await else { return };
    let pool = s.db.clone().unwrap();
    let (a, b) = (tenant(&s, "A").await, tenant(&s, "B").await);
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'public' AND c.relkind = 'r'
            AND EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.oid AND a.attname = 'business_id' AND NOT a.attisdropped)
          ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(tables.len() >= 6, "{tables:?}");
    for table in &tables {
        // B has rows here (seen without RLS, as the migration owner)...
        let b_rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE business_id = $1"))
            .bind(b.business)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(b_rows > 0, "the fixture should give B rows in {table}");
        // ...and A's transaction sees none of them, by id or in total.
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL ROLE pos_app")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("SELECT set_config('app.business_id', $1, true)")
            .bind(a.business.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        let seen: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE business_id = $1"))
            .bind(b.business)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(seen, 0, "{table}: A sees B's rows");
        let foreign: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE business_id <> $1"))
                .bind(a.business)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(foreign, 0, "{table}: A sees another business's rows");
        tx.rollback().await.unwrap();
    }
}

/// Every operation in the contract that needs a token, as `METHOD path`.
fn secured_operations() -> Vec<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/openapi/pos-cloud.v1.yaml"
    );
    let spec: Value = serde_yaml_ng::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut out = Vec::new();
    for (p, ops) in spec["paths"].as_object().unwrap() {
        for (m, op) in ops.as_object().unwrap() {
            let open = op
                .get("security")
                .and_then(Value::as_array)
                .is_some_and(|s| s.is_empty());
            if !open {
                out.push(format!("{} {p}", m.to_uppercase()));
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn at12_http_every_secured_route_refuses_another_business() {
    let Some(s) = state().await else { return };
    let (a, b) = (tenant(&s, "A").await, tenant(&s, "B").await);
    // For each secured operation: A's call aimed at B. Adding a secured route to the
    // contract without a case here fails the test.
    let cases: BTreeMap<&str, (Method, String, Option<Value>)> = BTreeMap::from([
        (
            "POST /v1/businesses",
            (
                Method::POST,
                "/v1/businesses".into(),
                Some(
                    json!({"businessId": b.business, "name": "Takeover", "countryCode": "GB", "timezone": "Europe/London",
                            "currencyCode": "GBP", "currencyExponent": 2, "pricesIncludeTax": true}),
                ),
            ),
        ),
        ("GET /v1/businesses", (Method::GET, "/v1/businesses".into(), None)),
        (
            "POST /v1/devices/register",
            (
                Method::POST,
                "/v1/devices/register".into(),
                Some(
                    json!({"businessId": b.business, "registerId": b.register, "registerName": "Till 1",
                            "registerCode": "R01", "locationId": b.location, "locationName": "Main",
                            "installationId": Uuid::now_v7(), "appVersion": "0.1.0", "replaceExisting": true}),
                ),
            ),
        ),
        ("GET /v1/devices", (Method::GET, "/v1/devices".into(), None)),
        (
            "POST /v1/devices/{deviceId}/revoke",
            (Method::POST, format!("/v1/devices/{}/revoke", b.device), None),
        ),
    ]);
    let secured = secured_operations();
    let covered: Vec<String> = cases.keys().map(|k| (*k).to_owned()).collect();
    assert_eq!(secured, covered, "every secured route needs an AT-12 case");

    let b_secrets = [
        b.business.to_string(),
        b.name.clone(),
        b.register.to_string(),
        b.device.to_string(),
    ];
    for (op, (method, path, body)) in cases {
        let (code, text) = call(&s, method, &path, Some(&a.user_token), body).await;
        assert!(
            [
                StatusCode::NOT_FOUND,
                StatusCode::FORBIDDEN,
                StatusCode::CONFLICT,
                StatusCode::OK
            ]
            .contains(&code),
            "{op}: {code} {text}"
        );
        for secret in &b_secrets {
            assert!(
                !text.contains(secret.as_str()),
                "{op} returned B's {secret}: {text}"
            );
        }
        // A's own listings still work and show A.
        if code == StatusCode::OK {
            assert!(text.contains(&a.business.to_string()), "{op}: {text}");
        }
    }
    // B's device and tokens are untouched by all of that.
    let (code, devices) = call(&s, Method::GET, "/v1/devices", Some(&b.user_token), None).await;
    assert_eq!(code, StatusCode::OK);
    assert!(devices.contains("\"status\":\"active\""), "{devices}");
    let (code, _) = call(
        &s,
        Method::POST,
        "/v1/auth/device/refresh",
        None,
        Some(json!({"refreshToken": b.refresh_token})),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    // A device token is never accepted where an owner is needed.
    let (code, _) = call(&s, Method::GET, "/v1/businesses", Some(&a.device_token), None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
}

#[test]
fn a_body_naming_another_business_or_register_is_forbidden() {
    use cloud_api::devices::{DevicePrincipal, same_tenant};
    let me = DevicePrincipal {
        business_id: pos_domain::BusinessId::from_uuid(Uuid::now_v7()),
        register_id: Uuid::now_v7(),
        device_id: Uuid::now_v7(),
    };
    assert!(same_tenant(&me, Some(*me.business_id.as_uuid()), Some(me.register_id)).is_ok());
    assert!(same_tenant(&me, None, None).is_ok());
    let p = same_tenant(&me, Some(Uuid::now_v7()), Some(me.register_id)).unwrap_err();
    assert_eq!((p.status, p.code.as_str()), (403, "PERMISSION_DENIED"));
    assert!(same_tenant(&me, Some(*me.business_id.as_uuid()), Some(Uuid::now_v7())).is_err());
}
