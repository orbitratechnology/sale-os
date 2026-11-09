//! Businesses, device registration, refresh-token rotation and revocation (E16-T04,
//! SYN-001, ADR-013). The tenant always comes from a verified token; a business or device id
//! in a request is only ever looked up inside what that token may see.

use crate::auth::jwt::Claims;
use crate::auth::{UserPrincipal, now_secs};
use crate::{AppState, Problem};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pos_cloud_db::devices::{self as db, Rotation};
use pos_cloud_db::{AccountTx, Principal, TenantTx};
use pos_domain::{BusinessId, ErrorCode};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Device access tokens live 15 minutes; refresh tokens 90 days (rotated on every use).
pub const DEVICE_TOKEN_SECS: i64 = 15 * 60;
pub const REFRESH_DAYS: i32 = 90;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/businesses", post(create_business).get(list_businesses))
        .route("/devices/register", post(register_device))
        .route("/devices", get(list_devices))
        .route("/devices/{device_id}/revoke", post(revoke))
        .route("/auth/device/refresh", post(refresh))
}

fn problem(status: StatusCode, code: ErrorCode, title: &str) -> Problem {
    Problem::new(status, code, title, None)
}

fn not_found() -> Problem {
    problem(StatusCode::NOT_FOUND, ErrorCode::NotFound, "Not found")
}

fn internal(e: impl std::fmt::Display) -> Problem {
    tracing::error!(error = %e, "database error");
    problem(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
        "Internal error",
    )
}

fn pool(state: &AppState) -> Result<&PgPool, Problem> {
    state.db.as_ref().ok_or_else(|| {
        problem(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Internal,
            "Service unavailable",
        )
    })
}

// ---------- businesses ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewBusinessRequest {
    business_id: Uuid,
    name: String,
    country_code: String,
    timezone: String,
    currency_code: String,
    currency_exponent: i16,
    prices_include_tax: bool,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BusinessDto {
    pub business_id: Uuid,
    pub name: String,
    pub country_code: String,
    pub timezone: String,
    pub currency_code: String,
    pub currency_exponent: i16,
    pub prices_include_tax: bool,
    pub version: i64,
    pub status: String,
}

impl From<db::BusinessFull> for BusinessDto {
    fn from(b: db::BusinessFull) -> Self {
        Self {
            business_id: b.business_id,
            name: b.name,
            country_code: b.country_code,
            timezone: b.timezone,
            currency_code: b.currency_code,
            currency_exponent: b.currency_exponent,
            prices_include_tax: b.prices_include_tax,
            version: b.version,
            status: b.status,
        }
    }
}

/// Creates the cloud business for a till's workspace (its id is the till's own, ADR-005).
/// The same owner sending the same id again gets it back (200); another account's id: 409.
async fn create_business(
    State(state): State<AppState>,
    user: UserPrincipal,
    Json(req): Json<NewBusinessRequest>,
) -> Result<Response, Problem> {
    let pool = pool(&state)?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(Problem::validation("name: required, at most 80 characters"));
    }
    if req.country_code.len() != 2 || !req.country_code.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(Problem::validation("countryCode: two capital letters (ISO 3166)"));
    }
    if req.timezone.trim().is_empty() || req.timezone.len() > 64 {
        return Err(Problem::validation("timezone: an IANA time zone name"));
    }
    let currency =
        pos_domain::Currency::from_iso(&req.currency_code).map_err(|e| Problem::validation(e.to_string()))?;
    if i16::from(currency.exponent) != req.currency_exponent {
        return Err(Problem::validation(
            "currencyExponent: does not match the currency",
        ));
    }
    let principal = Principal {
        business_id: BusinessId::from_uuid(req.business_id),
    };
    let mut tx = TenantTx::begin(pool, &principal).await.map_err(internal)?;
    if let Some(existing) = db::business_full(&mut tx).await.map_err(internal)? {
        if !db::is_member(&mut tx, user.account_id).await.map_err(internal)? {
            return Err(problem(
                StatusCode::CONFLICT,
                ErrorCode::SyncBusinessIdTaken,
                "Business ID taken",
            ));
        }
        return Ok(Json(BusinessDto::from(existing)).into_response());
    }
    pos_cloud_db::repo::insert_business(
        &mut tx,
        &pos_cloud_db::repo::NewBusiness {
            business_id: principal.business_id,
            name: name.to_owned(),
            country_code: req.country_code.clone(),
            timezone: req.timezone.trim().to_owned(),
            currency_code: req.currency_code.clone(),
            currency_exponent: req.currency_exponent,
            prices_include_tax: req.prices_include_tax,
        },
    )
    .await
    .map_err(internal)?;
    db::insert_owner(&mut tx, user.account_id)
        .await
        .map_err(internal)?;
    let created = db::business_full(&mut tx)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    tx.commit().await.map_err(internal)?;
    tracing::info!(business_id = %req.business_id, account_id = %user.account_id, "business created");
    Ok((StatusCode::CREATED, Json(BusinessDto::from(created))).into_response())
}

/// The businesses the owner belongs to (the "join existing" path, D-10).
async fn list_businesses(
    State(state): State<AppState>,
    user: UserPrincipal,
) -> Result<Json<Vec<BusinessDto>>, Problem> {
    let pool = pool(&state)?;
    let mut tx = AccountTx::begin(pool, user.account_id).await.map_err(internal)?;
    let all = db::member_businesses(&mut tx).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(all.into_iter().map(BusinessDto::from).collect()))
}

// ---------- devices ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterRequest {
    business_id: Uuid,
    register_id: Uuid,
    register_name: String,
    register_code: String,
    location_id: Uuid,
    location_name: String,
    installation_id: Uuid,
    app_version: String,
    os_info: Option<String>,
    /// The owner confirmed replacing the device already linked to this register.
    #[serde(default)]
    replace_existing: bool,
}

/// A device's credentials. Never logged; never derive Debug.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCredentials {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

fn new_refresh_token() -> (String, Vec<u8>) {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    use base64::Engine;
    use sha2::Digest;
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let hash = sha2::Sha256::digest(token.as_bytes()).to_vec();
    (token, hash)
}

fn hash_token(token: &str) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(token.as_bytes()).to_vec()
}

fn device_token(state: &AppState, business: Uuid, register: Uuid, device: Uuid) -> String {
    let now = now_secs();
    state.auth.signer.sign(&Claims {
        sub: device.to_string(),
        kind: "device".into(),
        iss: state.auth.signer.issuer().to_owned(),
        iat: now,
        exp: now + DEVICE_TOKEN_SECS,
        business_id: Some(business.to_string()),
        register_id: Some(register.to_string()),
        device_id: Some(device.to_string()),
    })
}

/// Links this installation to a register of a business the owner belongs to. A device
/// already active on the register is revoked only when the owner confirmed it.
async fn register_device(
    State(state): State<AppState>,
    user: UserPrincipal,
    Json(req): Json<RegisterRequest>,
) -> Result<Response, Problem> {
    let pool = pool(&state)?;
    let code_ok = (1..=6).contains(&req.register_code.len())
        && req
            .register_code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if !code_ok || req.register_name.trim().is_empty() || req.app_version.trim().is_empty() {
        return Err(Problem::validation(
            "registerCode (A-Z, 0-9, up to 6), registerName and appVersion are required",
        ));
    }
    let principal = Principal {
        business_id: BusinessId::from_uuid(req.business_id),
    };
    let mut tx = TenantTx::begin(pool, &principal).await.map_err(internal)?;
    // Not a member, or no such business: the same answer.
    if !db::is_member(&mut tx, user.account_id).await.map_err(internal)? {
        return Err(not_found());
    }
    db::ensure_register(
        &mut tx,
        req.location_id,
        req.location_name.trim(),
        req.register_id,
        req.register_name.trim(),
        &req.register_code,
    )
    .await
    .map_err(internal)?;
    if let Some(previous) = db::active_device(&mut tx, req.register_id)
        .await
        .map_err(internal)?
    {
        if !req.replace_existing {
            return Err(problem(
                StatusCode::CONFLICT,
                ErrorCode::SyncRegisterInUse,
                "Register already linked",
            ));
        }
        db::revoke_device(&mut tx, previous).await.map_err(internal)?;
        tracing::info!(device_id = %previous, "device replaced and revoked");
    }
    let device_id = Uuid::now_v7();
    db::insert_device(
        &mut tx,
        &db::NewDevice {
            device_id,
            register_id: req.register_id,
            installation_id: req.installation_id,
            app_version: req.app_version.trim(),
            os_info: req.os_info.as_deref(),
            registered_by: user.account_id,
        },
    )
    .await
    .map_err(internal)?;
    let (refresh, hash) = new_refresh_token();
    db::insert_refresh_token(&mut tx, &hash, device_id, Uuid::now_v7(), REFRESH_DAYS)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    tracing::info!(%device_id, register_id = %req.register_id, "device registered");
    Ok((
        StatusCode::CREATED,
        Json(DeviceCredentials {
            device_id: Some(device_id),
            access_token: device_token(&state, req.business_id, req.register_id, device_id),
            refresh_token: refresh,
            expires_in: DEVICE_TOKEN_SECS,
        }),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshRequest {
    refresh_token: String,
}

/// Rotation: the token is spent and a new one issued. A spent token used again revokes
/// the device (`401 SYNC_DEVICE_REVOKED`), as does a revoked device.
async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> Result<Json<DeviceCredentials>, Problem> {
    let pool = pool(&state)?;
    let (next, next_hash) = new_refresh_token();
    match db::rotate(pool, &hash_token(&req.refresh_token), &next_hash, REFRESH_DAYS)
        .await
        .map_err(internal)?
    {
        Rotation::Rotated {
            business_id,
            register_id,
            device_id,
        } => Ok(Json(DeviceCredentials {
            device_id: None,
            access_token: device_token(&state, business_id, register_id, device_id),
            refresh_token: next,
            expires_in: DEVICE_TOKEN_SECS,
        })),
        Rotation::Reused => {
            tracing::warn!("a spent refresh token was reused; the device is revoked");
            Err(revoked())
        }
        Rotation::Revoked => Err(revoked()),
        Rotation::Expired | Rotation::Unknown => Err(problem(
            StatusCode::UNAUTHORIZED,
            ErrorCode::AuthRequired,
            "Link again",
        )),
    }
}

fn revoked() -> Problem {
    Problem::new(
        StatusCode::UNAUTHORIZED,
        ErrorCode::SyncDeviceRevoked,
        "Device disconnected",
        Some("This register was disconnected by the owner".into()),
    )
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    pub device_id: Uuid,
    pub business_id: Uuid,
    pub register_id: Uuid,
    pub app_version: String,
    pub os_info: Option<String>,
    pub status: String,
    pub registered_at: String,
    pub last_seen_at: Option<String>,
    pub revoked_at: Option<String>,
}

async fn list_devices(
    State(state): State<AppState>,
    user: UserPrincipal,
) -> Result<Json<Vec<DeviceDto>>, Problem> {
    let pool = pool(&state)?;
    let mut tx = AccountTx::begin(pool, user.account_id).await.map_err(internal)?;
    let rows = db::member_devices(&mut tx).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|d| DeviceDto {
                device_id: d.device_id,
                business_id: d.business_id,
                register_id: d.register_id,
                app_version: d.app_version,
                os_info: d.os_info,
                status: d.status,
                registered_at: d.registered_at.to_rfc3339(),
                last_seen_at: d.last_seen_at.map(|t| t.to_rfc3339()),
                revoked_at: d.revoked_at.map(|t| t.to_rfc3339()),
            })
            .collect(),
    ))
}

/// The owner disconnects a device. Another owner's device: 404, as if it did not exist.
async fn revoke(
    State(state): State<AppState>,
    user: UserPrincipal,
    Path(device_id): Path<Uuid>,
) -> Result<StatusCode, Problem> {
    let pool = pool(&state)?;
    let mut tx = AccountTx::begin(pool, user.account_id).await.map_err(internal)?;
    let business = db::device_business(&mut tx, device_id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    tx.commit().await.map_err(internal)?;
    let principal = Principal {
        business_id: BusinessId::from_uuid(business),
    };
    let mut tx = TenantTx::begin(pool, &principal).await.map_err(internal)?;
    if !db::is_member(&mut tx, user.account_id).await.map_err(internal)? {
        return Err(not_found());
    }
    db::revoke_device(&mut tx, device_id).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    tracing::info!(%device_id, "device revoked by the owner");
    Ok(StatusCode::NO_CONTENT)
}

/// A linked register (`Authorization: Bearer <device token>`). The token is checked, and so
/// is the device's status in the database on every request, so a revoked device is cut off
/// at once rather than when its token expires.
#[derive(Clone, Copy, Debug)]
pub struct DevicePrincipal {
    pub business_id: BusinessId,
    pub register_id: Uuid,
    pub device_id: Uuid,
}

impl FromRequestParts<AppState> for DevicePrincipal {
    type Rejection = Problem;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let required = || {
            problem(
                StatusCode::UNAUTHORIZED,
                ErrorCode::AuthRequired,
                "Device token required",
            )
        };
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(required)?;
        let c = state
            .auth
            .signer
            .verify(token, "device", now_secs())
            .map_err(|_| required())?;
        let parse = |v: Option<String>| v.and_then(|s| s.parse::<Uuid>().ok()).ok_or_else(required);
        let me = DevicePrincipal {
            business_id: BusinessId::from_uuid(parse(c.business_id)?),
            register_id: parse(c.register_id)?,
            device_id: parse(c.device_id)?,
        };
        let pool = pool(state)?;
        let mut tx = TenantTx::begin(
            pool,
            &Principal {
                business_id: me.business_id,
            },
        )
        .await
        .map_err(internal)?;
        let status = db::device_seen(&mut tx, me.device_id).await.map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        match status.as_deref() {
            Some("active") => Ok(me),
            Some(_) => Err(revoked()),
            None => Err(required()),
        }
    }
}
