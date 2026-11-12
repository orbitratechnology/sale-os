//! Cloud API library: configuration, state and router. `main.rs` is a thin CLI around it.

pub mod auth;
pub mod config;
pub mod devices;
mod problem;
mod routes;
pub mod sync;

pub use config::Config;
pub use problem::Problem;

use axum::Router;
use sqlx::PgPool;
use std::sync::Arc;

/// Shared request state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// `None` when no `DATABASE_URL` is configured (local smoke runs).
    pub db: Option<PgPool>,
    pub auth: Arc<auth::AuthState>,
    /// Sync event appliers by type (E17-T04..T06).
    pub appliers: Arc<sync::Appliers>,
}

impl AppState {
    /// Builds the state. Without a signing key a random one is used, so tokens do not survive
    /// a restart (`serve` refuses to start that way outside debug runs).
    pub fn new(config: Config, db: Option<PgPool>) -> Self {
        let seed = config.jwt_signing_key.unwrap_or_else(|| {
            use argon2::password_hash::rand_core::{OsRng, RngCore};
            let mut s = [0u8; 32];
            OsRng.fill_bytes(&mut s);
            s
        });
        let hasher = Arc::new(auth::password::Hasher::new(
            config.argon2_memory_kib,
            config.argon2_passes,
        ));
        let identity = db.clone().map(|pool| {
            Arc::new(auth::identity::PasswordIdentity::new(pool, hasher))
                as Arc<dyn auth::identity::IdentityProvider>
        });
        let auth = Arc::new(auth::AuthState {
            signer: auth::jwt::TokenSigner::from_seed(&seed, &config.jwt_issuer),
            limiter: auth::rate_limit::RateLimiter::default(),
            identity,
            trust_proxy: config.trust_proxy,
        });
        Self {
            config: Arc::new(config),
            db,
            auth,
            appliers: Arc::new(sync::Appliers::standard()),
        }
    }

    /// The same state with another applier set (tests).
    pub fn with_appliers(mut self, appliers: sync::Appliers) -> Self {
        self.appliers = Arc::new(appliers);
        self
    }
}

/// Builds the HTTP router. Debug-only routes are mounted only when enabled in config.
pub fn router(state: AppState) -> Router {
    routes::router(state)
}
