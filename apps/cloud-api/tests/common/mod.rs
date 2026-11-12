//! Shared fixtures for the cloud API tests that need PostgreSQL (`TEST_DATABASE_URL`): a
//! business with an owner and a linked register, HTTP calls through the router, and test
//! appliers for the sync ingestion framework (E17-T04).
#![allow(dead_code)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use cloud_api::devices::DevicePrincipal;
use cloud_api::sync::{Appliers, ApplyError, ApplyOutcome, EventApplier};
use cloud_api::{AppState, Config, router};
use http_body_util::BodyExt;
use pos_cloud_db::TenantTx;
use pos_cloud_db::sync::{NewChange, append_change};
use pos_sync::{EventEnvelope, Payload, codes};
use serde_json::{Value, json};
use std::ops::RangeInclusive;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

/// The state against the test database, with `appliers`; `None` (skip) without one.
pub async fn state_with(appliers: Appliers) -> Option<AppState> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return None;
    };
    let pool = pos_cloud_db::connect_lazy(&url).unwrap();
    pos_cloud_db::migrate(&pool).await.unwrap();
    let mut cfg = Config::for_tests();
    cfg.trust_proxy = true;
    Some(AppState::new(cfg, Some(pool)).with_appliers(appliers))
}

pub async fn call(
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
#[derive(Clone)]
pub struct Tenant {
    pub user_token: String,
    pub business: Uuid,
    pub name: String,
    pub register: Uuid,
    pub location: Uuid,
    pub device: Uuid,
    pub device_token: String,
    pub refresh_token: String,
}

pub async fn tenant(s: &AppState, name: &str) -> Tenant {
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

// ---------- sync ----------

/// An `audit.recorded` event at `seq`. `action` steers [`TestFacts`]: `ok`, `transient`
/// (writes, then fails transiently) or `reject` (writes, then rejects).
pub fn audit_event(t: &Tenant, seq: u64, action: &str) -> EventEnvelope {
    let audit_id = Uuid::now_v7();
    EventEnvelope {
        event_id: Uuid::now_v7(),
        business_id: t.business,
        location_id: t.location,
        register_id: t.register,
        local_sequence: seq,
        event_type: "audit.recorded".into(),
        schema_version: 1,
        aggregate_type: "audit".into(),
        aggregate_id: audit_id.to_string(),
        base_version: None,
        occurred_at: "2026-11-05T09:14:03.120Z".into(),
        payload: json!({"auditId": audit_id, "action": action, "entityType": "test", "entityId": "x",
                        "metadata": {"n": seq}}),
    }
}

/// A `setting.updated` event based on `base`; [`TestSettings`] accepts only base 0.
pub fn setting_event(t: &Tenant, seq: u64, base: u64) -> EventEnvelope {
    EventEnvelope {
        event_id: Uuid::now_v7(),
        business_id: t.business,
        location_id: t.location,
        register_id: t.register,
        local_sequence: seq,
        event_type: "setting.updated".into(),
        schema_version: 1,
        aggregate_type: "setting".into(),
        aggregate_id: "receipt.footer".into(),
        base_version: Some(base),
        occurred_at: "2026-11-05T09:14:03.120Z".into(),
        payload: json!({"businessId": t.business, "key": "receipt.footer", "value": "Thanks",
                        "version": base + 1, "updatedAt": 1_793_000_000_000_i64}),
    }
}

/// Pushes `events` as `t`'s device; returns the status and the parsed body.
pub async fn push(s: &AppState, t: &Tenant, events: &[EventEnvelope]) -> (StatusCode, Value) {
    let body = json!({"registerId": t.register, "events": events});
    let (code, text) = call(
        s,
        Method::POST,
        "/v1/sync/events",
        Some(&t.device_token),
        Some(body),
    )
    .await;
    (code, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Facts for tests: one change-feed row per event, steered by the payload's `action`.
pub struct TestFacts;

#[async_trait]
impl EventApplier for TestFacts {
    fn event_type(&self) -> &'static str {
        "audit.recorded"
    }
    fn supported_versions(&self) -> RangeInclusive<u32> {
        1..=1
    }
    async fn apply(
        &self,
        tx: &mut TenantTx<'_>,
        me: &DevicePrincipal,
        env: &EventEnvelope,
        payload: &Payload,
    ) -> Result<ApplyOutcome, ApplyError> {
        let Payload::AuditRecordedV1(a) = payload else {
            unreachable!("registered for audit.recorded")
        };
        let seq = append_change(
            tx,
            &NewChange {
                aggregate_type: "audit",
                aggregate_id: &env.aggregate_id,
                version: None,
                kind: "fact",
                origin_register: Some(me.register_id),
                origin_event_id: Some(env.event_id),
                snapshot: &env.payload,
            },
        )
        .await?;
        match a.action.as_str() {
            "transient" => Err(ApplyError::Transient {
                code: codes::SERVER_BUSY,
                detail: "simulated failure after writing".into(),
            }),
            "reject" => Err(ApplyError::Rejected {
                code: codes::EVENT_INVALID,
                detail: "simulated rejection after writing".into(),
            }),
            _ => Ok(ApplyOutcome::Applied {
                server_seq: Some(seq),
                new_version: None,
            }),
        }
    }
}

/// Master data for tests: a setting the server holds at version 0.
pub struct TestSettings;

#[async_trait]
impl EventApplier for TestSettings {
    fn event_type(&self) -> &'static str {
        "setting.updated"
    }
    fn supported_versions(&self) -> RangeInclusive<u32> {
        1..=1
    }
    async fn apply(
        &self,
        _tx: &mut TenantTx<'_>,
        _me: &DevicePrincipal,
        env: &EventEnvelope,
        _payload: &Payload,
    ) -> Result<ApplyOutcome, ApplyError> {
        if env.base_version == Some(0) {
            Ok(ApplyOutcome::Applied {
                server_seq: None,
                new_version: Some(1),
            })
        } else {
            Ok(ApplyOutcome::Conflict {
                server_version: 0,
                server_snapshot: json!({"key": "receipt.footer", "value": "Server text", "version": 0}),
                reason: codes::VERSION,
            })
        }
    }
}

pub fn test_appliers() -> Appliers {
    Appliers::default()
        .with(Arc::new(TestFacts))
        .with(Arc::new(TestSettings))
}

/// Raw counts as the migration owner (bypasses RLS), for exactly-once checks.
pub async fn count(s: &AppState, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(s.db.as_ref().unwrap())
        .await
        .unwrap()
}

/// Asserts `instance` matches `#/components/schemas/<name>` of the OpenAPI contract.
pub fn assert_matches_spec(name: &str, instance: &Value) {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/openapi/pos-cloud.v1.yaml"
    );
    let mut root: Value = serde_yaml_ng::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    root["$schema"] = Value::from("https://json-schema.org/draft/2020-12/schema");
    root["$ref"] = Value::from(format!("#/components/schemas/{name}"));
    let validator = jsonschema::validator_for(&root).expect("schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "does not match {name}: {errors:?}\n{instance}");
}
