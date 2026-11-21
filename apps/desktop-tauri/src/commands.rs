//! IPC command adapters (docs/backlog/07-contracts.md §1.3).

use crate::{AppServices, IpcError};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub debug_build: bool,
    pub database: DatabaseStatus,
}

#[derive(Debug, Serialize, specta::Type)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DatabaseStatus {
    Ready { path: String, schema_version: u32 },
    Failed { error: IpcError },
}

/// Version, build flavour and local database status. No session required.
#[tauri::command]
#[specta::specta]
pub async fn app_info(services: State<'_, AppServices>) -> Result<AppInfo, IpcError> {
    let database = match &services.db {
        Ok(db) => {
            let v = db.schema_version().await.map_err(|e| IpcError::from_error(&e))?;
            DatabaseStatus::Ready {
                path: db.path().display().to_string(),
                schema_version: u32::try_from(v).map_err(IpcError::internal)?,
            }
        }
        Err(e) => DatabaseStatus::Failed { error: e.clone() },
    };
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        debug_build: cfg!(debug_assertions),
        database,
    })
}
