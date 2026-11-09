use super::identity::IdentityError;
use super::jwt::Claims;
use super::{AuthState, USER_TOKEN_SECS, now_secs};
use crate::{AppState, Problem};
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_domain::ErrorCode;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/signup", post(signup))
        .route("/auth/login", post(login))
}

#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
}

// Never derive Debug for anything holding a password or token: it could reach a log.

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SignupResponse {
    pub account_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    pub user_access_token: String,
    pub expires_in: i64,
}

/// The client's address: the connection's, or `X-Forwarded-For` behind a trusted proxy.
struct ClientIp(String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let forwarded = state
            .auth
            .trust_proxy
            .then(|| parts.headers.get("x-forwarded-for"))
            .flatten()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_owned());
        let direct = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_string());
        Ok(ClientIp(forwarded.or(direct).unwrap_or_else(|| "unknown".into())))
    }
}

fn limited(wait: Duration) -> Response {
    let mut res = Problem::new(
        StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::AuthRateLimited,
        "Too many attempts",
        Some(format!("try again in {} seconds", wait.as_secs().max(1))),
    )
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
        res.headers_mut().insert(header::RETRY_AFTER, v);
    }
    res
}

fn unavailable() -> Response {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::Internal,
        "Service unavailable",
        None,
    )
    .into_response()
}

fn identity_error(e: IdentityError) -> Response {
    match e {
        IdentityError::InvalidCredentials => Problem::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::AuthCredentialsInvalid,
            "Sign-in failed",
            Some("the email or the password is wrong".into()),
        )
        .into_response(),
        IdentityError::EmailTaken => Problem::new(
            StatusCode::CONFLICT,
            ErrorCode::AuthEmailTaken,
            "Email already registered",
            None,
        )
        .into_response(),
        IdentityError::Invalid(why) => Problem::validation(why).into_response(),
        IdentityError::Store(e) => {
            tracing::error!(error = %e, "identity store failed");
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "Internal error",
                None,
            )
            .into_response()
        }
    }
}

async fn signup(State(state): State<AppState>, ip: ClientIp, Json(c): Json<Credentials>) -> Response {
    let auth: &AuthState = &state.auth;
    if let Err(wait) = auth
        .limiter
        .attempt(&format!("signup-ip:{}", ip.0), Instant::now())
    {
        return limited(wait);
    }
    let Some(identity) = &auth.identity else {
        return unavailable();
    };
    match identity.signup(&c.email, &c.password).await {
        Ok(id) => {
            tracing::info!(account_id = %id, "account created");
            (
                StatusCode::CREATED,
                Json(SignupResponse {
                    account_id: id.to_string(),
                }),
            )
                .into_response()
        }
        Err(e) => identity_error(e),
    }
}

async fn login(State(state): State<AppState>, ip: ClientIp, Json(c): Json<Credentials>) -> Response {
    let auth: &AuthState = &state.auth;
    let now = Instant::now();
    let by_ip = auth.limiter.attempt(&format!("login-ip:{}", ip.0), now);
    let by_email = auth
        .limiter
        .attempt(&format!("login-email:{}", c.email.trim().to_lowercase()), now);
    if let Err(wait) = by_ip.and(by_email) {
        return limited(wait);
    }
    let Some(identity) = &auth.identity else {
        return unavailable();
    };
    match identity.login(&c.email, &c.password).await {
        Ok(id) => {
            let now = now_secs();
            let token = auth.signer.sign(&Claims {
                sub: id.to_string(),
                kind: "user".into(),
                iss: auth.signer.issuer().to_owned(),
                iat: now,
                exp: now + USER_TOKEN_SECS,
                business_id: None,
                register_id: None,
                device_id: None,
            });
            tracing::info!(account_id = %id, "owner signed in");
            Json(LoginResponse {
                user_access_token: token,
                expires_in: USER_TOKEN_SECS,
            })
            .into_response()
        }
        Err(e) => identity_error(e),
    }
}

/// An owner signed in to the cloud (`Authorization: Bearer <user token>`).
#[derive(Clone, Debug)]
pub struct UserPrincipal {
    pub account_id: uuid::Uuid,
}

impl FromRequestParts<AppState> for UserPrincipal {
    type Rejection = Problem;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let required = || {
            Problem::new(
                StatusCode::UNAUTHORIZED,
                ErrorCode::AuthRequired,
                "Sign in required",
                None,
            )
        };
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(required)?;
        let claims = state
            .auth
            .signer
            .verify(token, "user", now_secs())
            .map_err(|_| required())?;
        let account_id = claims.sub.parse().map_err(|_| required())?;
        Ok(UserPrincipal { account_id })
    }
}
