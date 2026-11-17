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
    /// `POS_JWT_SIGNING_KEY`: the Ed25519 private seed (32 bytes, base64). Required to serve.
    pub jwt_signing_key: Option<[u8; 32]>,
    /// `POS_JWT_ISSUER`, default `sale-os`.
    pub jwt_issuer: String,
    /// `POS_ARGON2_MEMORY_KIB` and `POS_ARGON2_PASSES` (ADR-013 defaults: 65536, 3).
    pub argon2_memory_kib: u32,
    pub argon2_passes: u32,
    /// `POS_TRUST_PROXY=1`: take the client address from `X-Forwarded-For` (only behind the
    /// platform's own proxy).
    pub trust_proxy: bool,
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
            jwt_signing_key: get("POS_JWT_SIGNING_KEY")
                .filter(|s| !s.is_empty())
                .map(|k| {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD
                        .decode(k.trim())
                        .ok()
                        .and_then(|b| <[u8; 32]>::try_from(b).ok())
                        .ok_or_else(|| anyhow::anyhow!("POS_JWT_SIGNING_KEY: expected 32 bytes, base64"))
                })
                .transpose()?,
            jwt_issuer: get("POS_JWT_ISSUER").unwrap_or_else(|| "sale-os".to_owned()),
            argon2_memory_kib: get("POS_ARGON2_MEMORY_KIB")
                .map(|v| {
                    v.parse()
                        .map_err(|e| anyhow::anyhow!("POS_ARGON2_MEMORY_KIB: {e}"))
                })
                .transpose()?
                .unwrap_or(65_536),
            argon2_passes: get("POS_ARGON2_PASSES")
                .map(|v| v.parse().map_err(|e| anyhow::anyhow!("POS_ARGON2_PASSES: {e}")))
                .transpose()?
                .unwrap_or(3),
            trust_proxy: flag("POS_TRUST_PROXY"),
        })
    }

    /// Configuration for tests: no database, debug routes on, a fixed signing key and a
    /// cheap Argon2 cost (the production cost is ADR-013's).
    pub fn for_tests() -> Self {
        Self::from_lookup(|k| match k {
            "POS_DEBUG_ROUTES" => Some("1".to_owned()),
            // 32 bytes of 0x2a.
            "POS_JWT_SIGNING_KEY" => Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_owned()),
            "POS_ARGON2_MEMORY_KIB" => Some("1024".to_owned()),
            "POS_ARGON2_PASSES" => Some("1".to_owned()),
            _ => None,
        })
        .expect("valid test config")
    }
}
