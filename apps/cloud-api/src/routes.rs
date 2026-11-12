use crate::{AppState, Problem};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use pos_domain::{Currency, Money, NumberFormat};
use serde::{Deserialize, Serialize};

pub(crate) fn router(state: AppState) -> Router {
    let mut v1 = Router::new()
        .route("/health", get(health))
        .route("/version-policy", get(version_policy));
    if state.config.debug_routes {
        v1 = v1.route("/_debug/money-add", post(debug_money_add));
    }
    let v1 = v1
        .merge(crate::auth::routes())
        .merge(crate::devices::routes())
        .merge(crate::sync::routes());
    Router::new().nest("/v1", v1).with_state(state)
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    /// `ok` | `unavailable` | `not_configured`
    pub database: String,
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let database = match &state.db {
        None => "not_configured",
        Some(pool) => match pos_cloud_db::ping(pool).await {
            Ok(()) => "ok",
            Err(e) => {
                tracing::warn!(error = %e, "database ping failed");
                "unavailable"
            }
        },
    };
    let (code, status) = if database == "unavailable" {
        (StatusCode::SERVICE_UNAVAILABLE, "degraded")
    } else {
        (StatusCode::OK, "ok")
    };
    (
        code,
        Json(HealthResponse {
            status: status.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            database: database.to_owned(),
        }),
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionPolicy {
    pub min_supported: String,
    pub latest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_url: Option<String>,
}

async fn version_policy(State(state): State<AppState>) -> Json<VersionPolicy> {
    Json(VersionPolicy {
        min_supported: state.config.min_app_version.clone(),
        latest: state.config.latest_app_version.clone(),
        update_url: state.config.update_url.clone(),
    })
}

/// Stage 0 proof that the cloud reuses the exact domain crate the desktop uses (E16-T01).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoneyAddRequest {
    a_minor: String,
    b_minor: String,
    currency: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoneyAddResponse {
    pub sum_minor: String,
    pub formatted: String,
}

async fn debug_money_add(Json(req): Json<MoneyAddRequest>) -> Result<Json<MoneyAddResponse>, Problem> {
    let currency = Currency::from_iso(&req.currency).map_err(|e| Problem::validation(e.to_string()))?;
    let parse = |s: &str| {
        s.parse::<i64>()
            .map(Money::from_minor)
            .map_err(|_| Problem::validation(format!("not minor units: {s:?}")))
    };
    let sum = parse(&req.a_minor)?
        .checked_add(parse(&req.b_minor)?)
        .map_err(|e| Problem::validation(e.to_string()))?;
    Ok(Json(MoneyAddResponse {
        sum_minor: sum.minor().to_string(),
        formatted: currency.format(sum, NumberFormat::EN_US),
    }))
}
