//! E00-T05 / E16-T06: real responses must match `contracts/openapi/pos-cloud.v1.yaml`.
//! OpenAPI 3.1 component schemas are JSON Schema 2020-12, so they are validated directly.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cloud_api::{AppState, Config, router};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const SPEC: &str = include_str!("../../../contracts/openapi/pos-cloud.v1.yaml");

fn spec() -> Value {
    serde_yaml_ng::from_str(SPEC).expect("spec parses as YAML")
}

/// Validates `instance` against `#/components/schemas/<name>`, resolving refs in the spec.
fn assert_matches(name: &str, instance: &Value) {
    let mut root = spec();
    root["$schema"] = Value::from("https://json-schema.org/draft/2020-12/schema");
    root["$ref"] = Value::from(format!("#/components/schemas/{name}"));
    let validator = jsonschema::validator_for(&root).expect("schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "response does not match {name}: {errors:?}\n{instance}"
    );
}

async fn get(state: AppState, uri: &str) -> (StatusCode, Value) {
    let res = router(state)
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn state(config: Config) -> AppState {
    AppState {
        config: Arc::new(config),
        db: None,
    }
}

#[tokio::test]
async fn health_matches_schema_when_up_and_degraded() {
    let (status, body) = get(state(Config::for_tests()), "/v1/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_matches("HealthResponse", &body);

    let mut cfg = Config::for_tests();
    cfg.database_url = Some("postgres://nobody@127.0.0.1:1/none".into());
    let db = Some(pos_cloud_db::connect_lazy(cfg.database_url.as_deref().unwrap()).unwrap());
    let (status, body) = get(
        AppState {
            config: Arc::new(cfg),
            db,
        },
        "/v1/health",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_matches("HealthResponse", &body);
}

#[tokio::test]
async fn version_policy_matches_schema_with_and_without_update_url() {
    let (_, body) = get(state(Config::for_tests()), "/v1/version-policy").await;
    assert_matches("VersionPolicy", &body);
    let mut cfg = Config::for_tests();
    cfg.update_url = Some("https://updates.sale-os.example/stable".into());
    let (_, body) = get(state(cfg), "/v1/version-policy").await;
    assert_matches("VersionPolicy", &body);
    assert_eq!(body["updateUrl"], "https://updates.sale-os.example/stable");
}

#[tokio::test]
async fn error_bodies_match_the_problem_schema() {
    let req = Request::post("/v1/_debug/money-add")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"aMinor":"1","bMinor":"1","currency":"usd"}"#))
        .unwrap();
    let res = router(state(Config::for_tests())).oneshot(req).await.unwrap();
    let body: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_matches("Problem", &body);
}

#[tokio::test]
async fn every_documented_get_path_is_served() {
    let spec = spec();
    let paths = spec["paths"].as_object().expect("paths");
    assert!(!paths.is_empty());
    for (path, ops) in paths {
        if ops.get("get").is_some() {
            let (status, _) = get(state(Config::for_tests()), path).await;
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "{path} is documented but not served"
            );
        }
    }
}

#[test]
fn error_code_enum_equals_the_domain_catalogue() {
    let spec = spec();
    let documented: Vec<String> = spec["components"]["schemas"]["ErrorCode"]["enum"]
        .as_array()
        .expect("enum")
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let domain: Vec<String> = pos_domain::ErrorCode::all()
        .map(|c| c.as_str().to_owned())
        .collect();
    assert_eq!(
        documented, domain,
        "update contracts/openapi ErrorCode enum to match pos_domain::ErrorCode"
    );
}
