//! E16-T06: every route against the contract. For each operation in
//! `contracts/openapi/pos-cloud.v1.yaml`, real requests (a success and typical errors) go
//! through the router, and each response must have a status the operation declares (or its
//! `default`), the declared content type, and a body matching that response's schema. A
//! route added to the contract without a case here fails the test. Every response example
//! must match its own schema too. PostgreSQL required (`TEST_DATABASE_URL`); the checks that
//! need no database are in `contract.rs`.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use cloud_api::{AppState, router};
use common::{end_of_day, fact_appliers, product_payload, state_with, tenant};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tower::ServiceExt;
use uuid::Uuid;

fn spec() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/openapi/pos-cloud.v1.yaml"
    );
    serde_yaml_ng::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A JSON pointer segment (`/` and `~` escaped).
fn seg(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

struct Response {
    status: StatusCode,
    content_type: Option<String>,
    body: String,
}

async fn send(s: &AppState, method: Method, uri: &str, token: Option<&str>, body: Option<Value>) -> Response {
    let mut r = Request::builder().method(method).uri(uri).header(
        "x-forwarded-for",
        format!("10.99.{}.{}", rand_octet(), rand_octet()),
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
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_owned());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Response {
        status,
        content_type,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// Each case gets its own client address, so the auth rate limits never interfere.
fn rand_octet() -> u8 {
    Uuid::now_v7().as_bytes()[15]
}

/// Checks `res` against the operation `op` ("METHOD /path" as in the spec). Returns the
/// errors, empty when it conforms.
fn conformance(spec: &Value, op: &str, res: &Response) -> Vec<String> {
    let (method, path) = op.split_once(' ').unwrap();
    let base = format!("/paths/{}/{}/responses", seg(path), method.to_lowercase());
    let responses = spec
        .pointer(&base)
        .unwrap_or_else(|| panic!("{op} is not in the spec"));
    let code = res.status.as_u16().to_string();
    let (key, declared) = match responses.get(&code) {
        Some(d) => (code.clone(), d),
        None => match responses.get("default") {
            // `default` covers errors only: an undeclared success is a contract gap.
            Some(d) if !res.status.is_success() => ("default".to_owned(), d),
            _ => return vec![format!("status {code} is not declared")],
        },
    };
    let content = declared.get("content").and_then(Value::as_object);
    match (content, res.content_type.as_deref()) {
        (None, _) if res.body.is_empty() => vec![],
        (None, _) => vec![format!("{key}: declares no body but got {:?}", res.body)],
        (Some(_), None) => vec![format!("{key}: no content type")],
        (Some(c), Some(ct)) => {
            if !c.contains_key(ct) {
                return vec![format!(
                    "{key}: content type {ct} is not declared ({:?})",
                    c.keys()
                )];
            }
            let instance: Value = match serde_json::from_str(&res.body) {
                Ok(v) => v,
                Err(e) => return vec![format!("{key}: body is not JSON ({e}): {}", res.body)],
            };
            // The response's schema, copied to a plain key (path templates and media types
            // would need escaping in a `$ref`); its own `$ref`s point into `components`.
            let target = spec
                .pointer(&format!("{base}/{}/content/{}/schema", seg(&key), seg(ct)))
                .cloned()
                .unwrap_or(Value::Bool(true));
            let mut root = spec.clone();
            root["$schema"] = Value::from("https://json-schema.org/draft/2020-12/schema");
            root["$defs"] = json!({ "response": target });
            root["$ref"] = Value::from("#/$defs/response");
            let validator = jsonschema::validator_for(&root).expect("schema compiles");
            validator
                .iter_errors(&instance)
                .map(|e| format!("{key}: {e} at {}", e.instance_path()))
                .collect()
        }
    }
}

/// Every operation in the spec, as "METHOD /path".
fn operations(spec: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (p, ops) in spec["paths"].as_object().unwrap() {
        for m in ops.as_object().unwrap().keys() {
            if ["get", "post", "put", "patch", "delete"].contains(&m.as_str()) {
                out.insert(format!("{} {p}", m.to_uppercase()));
            }
        }
    }
    out
}

fn secured(spec: &Value, op: &str) -> bool {
    let (method, path) = op.split_once(' ').unwrap();
    !spec["paths"][path][method.to_lowercase()]
        .get("security")
        .and_then(Value::as_array)
        .is_some_and(|s| s.is_empty())
}

#[tokio::test]
async fn every_route_answers_as_the_contract_says() {
    let Some(s) = state_with(fact_appliers()).await else {
        return;
    };
    let spec = spec();
    let t = tenant(&s, "Contract").await;
    let other = tenant(&s, "Other").await;
    let day = end_of_day(&t);
    let product = Uuid::now_v7();
    let email = format!("contract-{}@example.com", Uuid::now_v7().simple());
    let creds = json!({"email": email, "password": "correct horse battery staple"});
    let biz = t.business;
    // E19-T10: a business of its own (the others are renamed by the cases' pushes).
    let gone = tenant(&s, "Gone").await;
    let gone_user = Some(gone.user_token.clone());
    let new_business = Uuid::now_v7();

    // (operation, method, uri, token, body, expected status). Order matters where one call
    // sets up the next; revocation and refresh rotation come last.
    type Case<'a> = (&'a str, Method, String, Option<String>, Option<Value>, u16);
    let user = Some(t.user_token.clone());
    let device = Some(t.device_token.clone());
    let cases: Vec<Case> = vec![
        (
            "GET /v1/health",
            Method::GET,
            "/v1/health".into(),
            None,
            None,
            200,
        ),
        (
            "GET /v1/version-policy",
            Method::GET,
            "/v1/version-policy".into(),
            None,
            None,
            200,
        ),
        (
            "POST /v1/auth/signup",
            Method::POST,
            "/v1/auth/signup".into(),
            None,
            Some(creds.clone()),
            201,
        ),
        (
            "POST /v1/auth/signup",
            Method::POST,
            "/v1/auth/signup".into(),
            None,
            Some(json!({"email": "not-an-email", "password": "correct horse battery staple"})),
            422,
        ),
        (
            "POST /v1/auth/login",
            Method::POST,
            "/v1/auth/login".into(),
            None,
            Some(creds.clone()),
            200,
        ),
        (
            "POST /v1/auth/login",
            Method::POST,
            "/v1/auth/login".into(),
            None,
            Some(json!({"email": email, "password": "wrong horse battery staple"})),
            401,
        ),
        (
            "POST /v1/businesses",
            Method::POST,
            "/v1/businesses".into(),
            user.clone(),
            Some(
                json!({"businessId": new_business, "name": "Contract Two", "countryCode": "GB",
                     "timezone": "Europe/London", "currencyCode": "GBP", "currencyExponent": 2,
                     "pricesIncludeTax": true}),
            ),
            201,
        ),
        (
            "POST /v1/businesses",
            Method::POST,
            "/v1/businesses".into(),
            user.clone(),
            Some(
                json!({"businessId": other.business, "name": "Takeover", "countryCode": "GB",
                     "timezone": "Europe/London", "currencyCode": "GBP", "currencyExponent": 2,
                     "pricesIncludeTax": true}),
            ),
            409,
        ),
        (
            "GET /v1/businesses",
            Method::GET,
            "/v1/businesses".into(),
            user.clone(),
            None,
            200,
        ),
        (
            "POST /v1/devices/register",
            Method::POST,
            "/v1/devices/register".into(),
            user.clone(),
            Some(
                json!({"businessId": biz, "registerId": Uuid::now_v7(), "registerName": "Till 9",
                     "registerCode": "R09", "locationId": t.location, "locationName": "Main",
                     "installationId": Uuid::now_v7(), "appVersion": "0.1.0", "osInfo": null}),
            ),
            201,
        ),
        (
            "POST /v1/devices/register",
            Method::POST,
            "/v1/devices/register".into(),
            user.clone(),
            Some(
                json!({"businessId": biz, "registerId": t.register, "registerName": "Till 1",
                     "registerCode": "R01", "locationId": t.location, "locationName": "Main",
                     "installationId": Uuid::now_v7(), "appVersion": "0.1.0", "osInfo": null}),
            ),
            409,
        ),
        (
            "GET /v1/devices",
            Method::GET,
            "/v1/devices".into(),
            user.clone(),
            None,
            200,
        ),
        (
            "POST /v1/sync/events",
            Method::POST,
            "/v1/sync/events".into(),
            device.clone(),
            Some(json!({"registerId": t.register, "events": day})),
            200,
        ),
        (
            "POST /v1/sync/events",
            Method::POST,
            "/v1/sync/events".into(),
            device.clone(),
            Some(json!({"registerId": other.register, "events": []})),
            403,
        ),
        (
            "GET /v1/sync/ack",
            Method::GET,
            "/v1/sync/ack".into(),
            device.clone(),
            None,
            200,
        ),
        (
            "GET /v1/sync/changes",
            Method::GET,
            "/v1/sync/changes?limit=5&unsent=0".into(),
            device.clone(),
            None,
            200,
        ),
        (
            "GET /v1/sync/changes",
            Method::GET,
            "/v1/sync/changes?cursor=nonsense".into(),
            device.clone(),
            None,
            422,
        ),
        (
            "GET /v1/sync/changes",
            Method::GET,
            "/v1/sync/changes?limit=5000".into(),
            device.clone(),
            None,
            422,
        ),
        (
            "GET /v1/sync/changes",
            Method::GET,
            "/v1/sync/changes?limit=lots".into(),
            device.clone(),
            None,
            422,
        ),
        (
            "GET /v1/sync/changes",
            Method::GET,
            "/v1/sync/changes?cursor=".into(),
            device.clone(),
            None,
            422,
        ),
        (
            "POST /v1/businesses",
            Method::POST,
            "/v1/businesses".into(),
            user.clone(),
            Some(
                json!({"businessId": Uuid::now_v7(), "name": "Nowhere", "countryCode": "GB",
                        "timezone": "Not/A_Zone", "currencyCode": "GBP", "currencyExponent": 2,
                        "pricesIncludeTax": true}),
            ),
            422,
        ),
        // No body, so no content type: refused as a problem, not plain text.
        (
            "POST /v1/auth/login",
            Method::POST,
            "/v1/auth/login".into(),
            None,
            None,
            415,
        ),
        (
            "GET /v1/sync/replay",
            Method::GET,
            "/v1/sync/replay?after=0&limit=5".into(),
            device.clone(),
            None,
            200,
        ),
        (
            "GET /v1/sync/replay",
            Method::GET,
            "/v1/sync/replay?after=-1".into(),
            device.clone(),
            None,
            422,
        ),
        (
            "GET /v1/sync/bootstrap",
            Method::GET,
            "/v1/sync/bootstrap?size=50".into(),
            device.clone(),
            None,
            200,
        ),
        (
            "GET /v1/sync/bootstrap",
            Method::GET,
            "/v1/sync/bootstrap?page=nonsense".into(),
            device.clone(),
            None,
            422,
        ),
        (
            "POST /v1/catalog/products",
            Method::POST,
            "/v1/catalog/products".into(),
            user.clone(),
            Some(json!({"businessId": biz, "expectedVersion": 0,
                     "product": product_payload(product, Uuid::now_v7(), 0, "Contract cola", Some("CC-1"), None, false)})),
            200,
        ),
        (
            "POST /v1/catalog/products",
            Method::POST,
            "/v1/catalog/products".into(),
            user.clone(),
            Some(json!({"businessId": biz, "expectedVersion": 0,
                     "product": product_payload(product, Uuid::now_v7(), 0, "Contract cola again", None, None, false)})),
            409,
        ),
        (
            "GET /v1/catalog/products",
            Method::GET,
            format!("/v1/catalog/products?businessId={biz}&query=cola&pageSize=10"),
            user.clone(),
            None,
            200,
        ),
        (
            "GET /v1/catalog/products",
            Method::GET,
            format!("/v1/catalog/products?businessId={biz}&page=0"),
            user.clone(),
            None,
            422,
        ),
        (
            "GET /v1/catalog/products",
            Method::GET,
            format!("/v1/catalog/products?businessId={}", other.business),
            user.clone(),
            None,
            404,
        ),
        (
            "GET /v1/reports/sales",
            Method::GET,
            "/v1/reports/sales?from=2026-01-01&to=2026-01-31&groupBy=tender".into(),
            device.clone(),
            None,
            200,
        ),
        (
            "GET /v1/reports/sales",
            Method::GET,
            format!("/v1/reports/sales?from=2026-01-01&to=2026-01-31&groupBy=day&businessId={biz}"),
            user.clone(),
            None,
            200,
        ),
        (
            "GET /v1/reports/sales",
            Method::GET,
            "/v1/reports/sales?from=2026-02-01&to=2026-01-31&groupBy=day".into(),
            device.clone(),
            None,
            422,
        ),
        (
            "POST /v1/auth/device/refresh",
            Method::POST,
            "/v1/auth/device/refresh".into(),
            None,
            Some(json!({"refreshToken": t.refresh_token})),
            200,
        ),
        (
            "POST /v1/auth/device/refresh",
            Method::POST,
            "/v1/auth/device/refresh".into(),
            None,
            Some(json!({"refreshToken": "not-a-token"})),
            401,
        ),
        (
            "POST /v1/devices/{deviceId}/revoke",
            Method::POST,
            format!("/v1/devices/{}/revoke", t.device),
            user.clone(),
            None,
            204,
        ),
        (
            "POST /v1/devices/{deviceId}/revoke",
            Method::POST,
            format!("/v1/devices/{}/revoke", other.device),
            user.clone(),
            None,
            404,
        ),
        // Revoked: refused on every device route.
        (
            "GET /v1/sync/ack",
            Method::GET,
            "/v1/sync/ack".into(),
            device.clone(),
            None,
            401,
        ),
        // E19-T10, last: a deletion request revokes the business's tills.
        (
            "POST /v1/businesses/{businessId}/deletion",
            Method::POST,
            format!("/v1/businesses/{}/deletion", gone.business),
            gone_user.clone(),
            Some(json!({"confirmName": "not the name"})),
            422,
        ),
        (
            "POST /v1/businesses/{businessId}/deletion",
            Method::POST,
            format!("/v1/businesses/{}/deletion", gone.business),
            gone_user.clone(),
            Some(json!({"confirmName": gone.name})),
            202,
        ),
        (
            "POST /v1/businesses/{businessId}/deletion",
            Method::POST,
            format!("/v1/businesses/{}/deletion", gone.business),
            gone_user.clone(),
            Some(json!({"confirmName": gone.name})),
            404,
        ),
        (
            "DELETE /v1/businesses/{businessId}/deletion",
            Method::DELETE,
            format!("/v1/businesses/{}/deletion", gone.business),
            gone_user.clone(),
            None,
            204,
        ),
        (
            "DELETE /v1/businesses/{businessId}/deletion",
            Method::DELETE,
            format!("/v1/businesses/{}/deletion", gone.business),
            gone_user.clone(),
            None,
            404,
        ),
    ];

    let mut failures = Vec::new();
    let mut succeeded = BTreeSet::new();
    for (op, method, uri, token, body, expected) in cases {
        let res = send(&s, method, &uri, token.as_deref(), body).await;
        if res.status.as_u16() != expected {
            failures.push(format!(
                "{op} {uri}: expected {expected}, got {} {}",
                res.status, res.body
            ));
            continue;
        }
        if res.status.is_success() {
            succeeded.insert(op.to_owned());
        }
        for e in conformance(&spec, op, &res) {
            failures.push(format!("{op} {uri}: {e}"));
        }
    }
    // And every secured route without a token: 401, as a declared problem.
    for op in operations(&spec).iter().filter(|op| secured(&spec, op)) {
        let (method, path) = op.split_once(' ').unwrap();
        let uri = path
            .replace("{deviceId}", &Uuid::now_v7().to_string())
            .replace("{businessId}", &Uuid::now_v7().to_string());
        let res = send(&s, method.parse().unwrap(), &uri, None, None).await;
        if res.status != StatusCode::UNAUTHORIZED {
            failures.push(format!("{op} without a token: {} {}", res.status, res.body));
        }
        for e in conformance(&spec, op, &res) {
            failures.push(format!("{op} without a token: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} contract failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let missing: Vec<_> = operations(&spec).difference(&succeeded).cloned().collect();
    assert!(
        missing.is_empty(),
        "operations with no successful case: {missing:?}"
    );
}

/// Every response example in the contract matches its own schema, so the examples the
/// register's client is tested against (`pos-sync-http`) are ones the server may send.
#[test]
fn every_example_matches_its_schema() {
    let spec = spec();
    let mut checked = 0;
    let mut failures = Vec::new();
    for (path, ops) in spec["paths"].as_object().unwrap() {
        for (method, op) in ops.as_object().unwrap() {
            let Some(responses) = op.get("responses").and_then(Value::as_object) else {
                continue;
            };
            for (status, r) in responses {
                for (ct, media) in r.get("content").and_then(Value::as_object).into_iter().flatten() {
                    let Some(example) = media.get("example") else {
                        continue;
                    };
                    let mut root = spec.clone();
                    root["$schema"] = Value::from("https://json-schema.org/draft/2020-12/schema");
                    root["$defs"] = json!({ "response": media["schema"].clone() });
                    root["$ref"] = Value::from("#/$defs/response");
                    let validator = jsonschema::validator_for(&root).expect("schema compiles");
                    for e in validator.iter_errors(example) {
                        failures.push(format!(
                            "{method} {path} {status} {ct}: {e} at {}",
                            e.instance_path()
                        ));
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 9, "only {checked} examples found");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
