//! E19-T07: the diagnostics bundle, for support. Anyone signed in can make one (a cashier on
//! the phone with support); it holds no business records, only the till's state, settings,
//! audit identifiers and the redacted logs of the last 7 days. Rust asks where to save it.

use crate::{AppServices, IpcError};
use pos_local_db::diagnostics::{self, AppFacts, LogFile};
use serde::Serialize;
use specta::Type;
use std::path::Path;
use std::time::{Duration, SystemTime};
use tauri::State;

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsDto {
    /// Where it was saved (shown to the user).
    pub path: String,
    pub file_name: String,
    pub files: u32,
    pub integrity_ok: bool,
    /// Stock balances that differ from their movements.
    pub stock_drift: u32,
}

/// Log files in `dir` changed in the last 7 days, oldest first, masked once more for card
/// numbers (they were redacted when written).
pub(crate) fn recent_logs(dir: &Path, now: SystemTime) -> Vec<LogFile> {
    let week = Duration::from_secs(7 * 24 * 3600);
    let mut found: Vec<(SystemTime, LogFile)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            if now.duration_since(modified).unwrap_or_default() > week || !e.path().is_file() {
                return None;
            }
            let text = std::fs::read_to_string(e.path()).ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            Some((
                modified,
                LogFile {
                    name,
                    text: pos_observability::mask_card_numbers(&text).into_owned(),
                },
            ))
        })
        .collect();
    found.sort_by_key(|(t, _)| *t);
    found.into_iter().map(|(_, l)| l).collect()
}

/// Saves the bundle where the user chooses (null: cancelled; nothing written).
#[tauri::command]
#[specta::specta]
pub async fn diagnostics_export(
    services: State<'_, AppServices>,
) -> Result<Option<DiagnosticsDto>, IpcError> {
    let ctx = services.context().await?;
    // Anyone signed in, but someone must be (SESSION_LOCKED otherwise).
    ctx.actor().map_err(crate::app_commands::err)?;
    let db = services.db.as_ref().map_err(Clone::clone)?.clone();
    let now = ctx.clock.now();
    let stamp: String = now.to_rfc3339().chars().take(16).filter(|c| *c != ':').collect();
    let chosen = services
        .file_dialog
        .ask(&format!("sale-os-diagnostics-{stamp}.zip"), ("ZIP", &["zip"]))
        .await
        .ok()
        .flatten();
    let Some(path) = chosen else {
        return Ok(None);
    };
    let path = if path.extension().is_some() {
        path
    } else {
        path.with_extension("zip")
    };
    let app = AppFacts {
        app_version: env!("CARGO_PKG_VERSION").into(),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        webview_version: tauri::webview_version().ok(),
        encrypted: db.key().is_some(),
        created_at_ms: now.as_millis(),
    };
    let logs = recent_logs(&services.data_dir.join("logs"), SystemTime::now());
    let summary = diagnostics::write(&db, &path, &app, &logs)
        .await
        .map_err(|e| IpcError::from_error(&e))?;
    tracing::info!(
        files = summary.files,
        integrity_ok = summary.integrity_ok,
        "diagnostics bundle saved"
    );
    Ok(Some(DiagnosticsDto {
        path: path.display().to_string(),
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        files: u32::try_from(summary.files).unwrap_or(u32::MAX),
        integrity_ok: summary.integrity_ok,
        stock_drift: u32::try_from(summary.drift).unwrap_or(u32::MAX),
    }))
}
