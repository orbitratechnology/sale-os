//! `AppServices`: the process-wide container of ports and services (E01-T04). Commands get
//! it as Tauri managed state; nothing else holds database handles or clocks.

use crate::IpcError;
use pos_domain::{Clock, IdGenerator, SystemClock, UuidV7Generator};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_observability::LogGuard;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct AppServices {
    /// The local database, or why it could not be opened (shown by `app_info`).
    pub db: Result<LocalDb, IpcError>,
    /// The single write path (ADR-008). `None` when the database failed to open.
    pub store: Option<SqliteStore>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    /// The app data directory (database, logs, virtual-printer captures).
    pub data_dir: PathBuf,
    _log: Option<LogGuard>,
}

impl AppServices {
    /// Opens the database under `data_dir` and wires the production ports.
    pub async fn start(data_dir: &Path, log: Option<LogGuard>) -> Self {
        let db = match LocalDb::open(&DbOptions::in_dir(data_dir)).await {
            Ok(db) => {
                if let Ok(Some(register)) = db.device_register_id().await {
                    pos_observability::set_context("register_id", register);
                }
                Ok(db)
            }
            Err(e) => {
                // A database failure must not crash the app silently: `app_info` reports it.
                tracing::error!(error = %e, "local database unavailable");
                Err(IpcError::from_error(&e))
            }
        };
        let store = db.as_ref().ok().map(|db| SqliteStore::new(db.clone()));
        Self {
            db,
            store,
            clock: Arc::new(SystemClock),
            ids: Arc::new(UuidV7Generator),
            data_dir: data_dir.to_path_buf(),
            _log: log,
        }
    }
}
