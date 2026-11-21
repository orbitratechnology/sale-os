use crate::extract::ApiJson;
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
    // E19-T09: every route but health and the version policy itself refuses a desktop build
    // below the minimum.
    let gated = Router::new()
        .merge(crate::auth::routes())
        .merge(crate::devices::routes())
        .merge(crate::sync::routes())
        .merge(crate::catalog::routes())
        .merge(crate::reports::routes())
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), version_gate));
    let v1 = v1.merge(gated);
    Router::new().nest("/v1", v1).with_state(state)
}

/// `major.minor.patch` (a pre-release or build suffix is ignored), or `None`.
fn version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// PRD §10.1: a desktop that sends `x-app-version` below `POS_MIN_APP_VERSION` (or one that
/// cannot be read) gets `426 SYNC_UPGRADE_REQUIRED` with `details.minVersion`. It keeps
/// selling locally and pushes after its update. Requests without the header (the owner's
/// tools) are not desktop builds and pass.
async fn version_gate(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(sent) = req.headers().get("x-app-version") else {
        return next.run(req).await;
    };
    let min = &state.config.min_app_version;
    let ok = match (sent.to_str().ok().and_then(version), version(min)) {
        (Some(have), Some(need)) => have >= need,
        // A minimum that cannot be read is a deployment error: do not lock every till out.
        (_, None) => true,
        (None, Some(_)) => false,
    };
    if ok {
        return next.run(req).await;
    }
    let mut p = crate::problem::Problem::new(
        axum::http::StatusCode::UPGRADE_REQUIRED,
        pos_domain::ErrorCode::SyncUpgradeRequired,
        "Update required",
        Some(format!(
            "this app is older than the oldest the cloud accepts ({min})"
        )),
    );
    p.details = Some(serde_json::json!({ "minVersion": min }));
    p.into_response()
}

#[cfg(test)]
mod version_tests {
    use super::version;

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(version("0.10.2"), Some((0, 10, 2)));
        assert_eq!(version("1.2.3-pilot.4"), Some((1, 2, 3)));
        assert!(version("0.9.9") < version("0.10.0"));
        assert_eq!(version("1.2"), None);
        assert_eq!(version("1.2.x"), None);
        assert_eq!(version("1.2.3.4"), None);
    }
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

async fn debug_money_add(ApiJson(req): ApiJson<MoneyAddRequest>) -> Result<Json<MoneyAddResponse>, Problem> {
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
