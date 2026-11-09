//! Owner authentication (E16-T03): `/auth/signup`, `/auth/login`, the user token and the
//! extractor that requires it.

pub mod identity;
pub mod jwt;
pub mod password;
pub mod rate_limit;
mod routes;

pub use routes::{UserPrincipal, routes};

use identity::IdentityProvider;
use jwt::TokenSigner;
use rate_limit::RateLimiter;
use std::sync::Arc;

/// Lifetime of a user token (seconds).
pub const USER_TOKEN_SECS: i64 = 15 * 60;

pub struct AuthState {
    pub signer: TokenSigner,
    pub limiter: RateLimiter,
    /// `None` without a database.
    pub identity: Option<Arc<dyn IdentityProvider>>,
    /// Trust `X-Forwarded-For` for the client address (behind the platform's proxy only).
    pub trust_proxy: bool,
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
