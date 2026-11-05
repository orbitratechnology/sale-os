//! Cloud API library: configuration, state and router. `main.rs` is a thin CLI around it.

pub mod config;
mod problem;
mod routes;

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
}

/// Builds the HTTP router. Debug-only routes are mounted only when enabled in config.
pub fn router(state: AppState) -> Router {
    routes::router(state)
}
