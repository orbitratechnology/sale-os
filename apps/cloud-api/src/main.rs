//! `cloud-api [serve|migrate]` — see `docs/backlog/tasks/stage-0-foundations.md` (E16-T01).

use anyhow::Context;
use cloud_api::{AppState, Config, router};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // JSON lines on stderr with secret redaction (E16-T08 adds metrics and error reporting).
    let _log = pos_observability::init(&pos_observability::LogConfig::stderr_only("info"))?;

    let config = Config::from_env()?;
    let command = std::env::args().nth(1).unwrap_or_else(|| "serve".to_owned());
    match command.as_str() {
        "serve" => serve(config).await,
        "migrate" => {
            let url = config
                .database_url
                .as_deref()
                .context("DATABASE_URL is required for migrate")?;
            let pool = pos_cloud_db::connect_lazy(url)?;
            pos_cloud_db::migrate(&pool)
                .await
                .context("applying migrations")?;
            tracing::info!("migrations applied");
            Ok(())
        }
        "--help" | "-h" | "help" => {
            println!(
                "usage: cloud-api [serve|migrate]\n\nEnvironment: POS_BIND_ADDR, DATABASE_URL, POS_DEBUG_ROUTES, POS_MIN_APP_VERSION, POS_LATEST_APP_VERSION, POS_UPDATE_URL"
            );
            Ok(())
        }
        other => anyhow::bail!("unknown command {other:?}; expected serve or migrate"),
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let db = config
        .database_url
        .as_deref()
        .map(pos_cloud_db::connect_lazy)
        .transpose()?;
    if config.debug_routes {
        tracing::warn!("debug routes are enabled; never enable POS_DEBUG_ROUTES in production");
    }
    let addr = config.bind_addr;
    let app = router(AppState {
        config: Arc::new(config),
        db,
    });
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, version = env!("CARGO_PKG_VERSION"), "cloud-api listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}
