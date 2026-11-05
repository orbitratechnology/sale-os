use std::net::SocketAddr;

/// Runtime configuration from environment variables. Secrets never come from files in the
/// repository (docs/backlog/tasks/stage-3-cloud-sync.md E16-T07).
#[derive(Debug, Clone)]
pub struct Config {
    /// `POS_BIND_ADDR`, default `0.0.0.0:8080`.
    pub bind_addr: SocketAddr,
    /// `DATABASE_URL` (optional for smoke runs).
    pub database_url: Option<String>,
    /// `POS_DEBUG_ROUTES=1` mounts `/v1/_debug/*`. Never enabled in production.
    pub debug_routes: bool,
    /// `POS_MIN_APP_VERSION`, default `0.1.0`.
    pub min_app_version: String,
    /// `POS_LATEST_APP_VERSION`, default this build's version.
    pub latest_app_version: String,
    /// `POS_UPDATE_URL`, optional.
    pub update_url: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let bind_addr = get("POS_BIND_ADDR")
            .unwrap_or_else(|| "0.0.0.0:8080".to_owned())
            .parse()
            .map_err(|e| anyhow::anyhow!("POS_BIND_ADDR: {e}"))?;
        let flag = |k: &str| matches!(get(k).as_deref(), Some("1" | "true" | "yes"));
        Ok(Self {
            bind_addr,
            database_url: get("DATABASE_URL").filter(|s| !s.is_empty()),
            debug_routes: flag("POS_DEBUG_ROUTES"),
            min_app_version: get("POS_MIN_APP_VERSION").unwrap_or_else(|| "0.1.0".to_owned()),
            latest_app_version: get("POS_LATEST_APP_VERSION")
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned()),
            update_url: get("POS_UPDATE_URL").filter(|s| !s.is_empty()),
        })
    }

    /// Configuration for tests: no database, debug routes on.
    pub fn for_tests() -> Self {
        Self::from_lookup(|k| (k == "POS_DEBUG_ROUTES").then(|| "1".to_owned())).expect("valid test config")
    }
}
