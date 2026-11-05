//! Structured, privacy-safe logging (E01-T04, PRD §13 "logs must redact access tokens, PINs
//! and personal data not required for diagnostics").
//!
//! - One JSON object per line: `ts`, `level`, `target`, `message`, event fields, the fields
//!   of every enclosing span, and process-wide context set with [`set_context`]
//!   (e.g. `register_id`).
//! - Fields whose *name* looks secret (`pin`, `password`, `token`, …) are written as
//!   `"[REDACTED]"`, whatever their value.
//! - Any string value is scanned for card-number-like digit runs (13–19 digits passing the
//!   Luhn check), which are masked: card data must never reach a log (PAY-005).
//! - Files roll daily and only the newest `retention_days` files are kept.

mod layer;
mod redact;

pub use layer::{JsonLayer, capture_json_lines};
pub use redact::{REDACTED, is_secret_field, mask_card_numbers};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

static CONTEXT: OnceLock<RwLock<BTreeMap<&'static str, String>>> = OnceLock::new();

fn context() -> &'static RwLock<BTreeMap<&'static str, String>> {
    CONTEXT.get_or_init(Default::default)
}

/// Adds a field to every subsequent log line, e.g. `set_context("register_id", id)`.
pub fn set_context(key: &'static str, value: impl ToString) {
    if let Ok(mut map) = context().write() {
        map.insert(key, value.to_string());
    }
}

pub(crate) fn context_snapshot() -> BTreeMap<&'static str, String> {
    context().read().map(|m| m.clone()).unwrap_or_default()
}

/// Where and how to log.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Directory for rolling log files; `None` logs to stderr only.
    pub dir: Option<PathBuf>,
    pub file_prefix: String,
    pub retention_days: usize,
    /// Also write JSON lines to stderr (development, containers).
    pub stderr: bool,
    /// `RUST_LOG`-style filter used when `RUST_LOG` is not set.
    pub default_filter: String,
}

impl LogConfig {
    pub fn stderr_only(default_filter: &str) -> Self {
        Self {
            dir: None,
            file_prefix: String::new(),
            retention_days: 7,
            stderr: true,
            default_filter: default_filter.to_owned(),
        }
    }
}

/// Keeps the background file writer alive; drop it on shutdown to flush.
pub struct LogGuard {
    _file: Option<tracing_appender::non_blocking::WorkerGuard>,
}

#[derive(Debug, thiserror::Error)]
pub enum LogInitError {
    #[error("cannot create log directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot create rolling log file: {0}")]
    Appender(String),
    #[error("a global logger is already installed")]
    AlreadyInstalled,
}

/// Installs the global subscriber.
pub fn init(config: &LogConfig) -> Result<LogGuard, LogInitError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.default_filter));
    let (file_layer, guard) = match &config.dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            let appender = tracing_appender::rolling::Builder::new()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix(&config.file_prefix)
                .filename_suffix("log")
                .max_log_files(config.retention_days.max(1))
                .build(dir)
                .map_err(|e| LogInitError::Appender(e.to_string()))?;
            let (writer, guard) = tracing_appender::non_blocking(appender);
            (Some(JsonLayer::new(writer)), Some(guard))
        }
        None => (None, None),
    };
    let stderr_layer = config.stderr.then(|| JsonLayer::new(std::io::stderr));
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|_| LogInitError::AlreadyInstalled)?;
    Ok(LogGuard { _file: guard })
}
