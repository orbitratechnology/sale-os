//! Sync commands (E17).

use crate::{AppServices, IpcError};
use tauri::State;

/// Pushes pending events now, with the retry delay reset (E17-T03). Any signed-in user.
#[tauri::command]
#[specta::specta]
pub async fn sync_retry_now(services: State<'_, AppServices>) -> Result<(), IpcError> {
    services.context().await?;
    if let Some(sync) = &services.sync {
        sync.retry_now();
    }
    Ok(())
}
