//! Backups and restore (E19-T03), owner only: the backups there are, "Back up now", the second
//! folder (chosen in the system dialog, opened by Rust), a full check of a backup, and restore.

use crate::app_commands::err;
use crate::{AppServices, IpcError};
use pos_domain::{ErrorCode, Permission, Timestamp};
use pos_local_db::backup::{self, BackupInfo, BackupKind};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use tauri::State;

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BackupDto {
    /// The file name: how the UI names a backup (never a path).
    pub id: String,
    /// `hourly`, `daily`, `shutdown`, `pre_migration`, `manual` or `pre_restore`.
    pub kind: String,
    /// RFC 3339.
    pub created_at: String,
    pub size_kb: u32,
    pub sales: u32,
    /// Only in the second folder (the copy on this computer was pruned or lost).
    pub second_folder_only: bool,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BackupStatusDto {
    pub backups: Vec<BackupDto>,
    /// Where the second copies go, as the system names the folder (shown to the owner).
    pub second_folder: Option<String>,
    /// The last copy to it failed with this.
    pub second_folder_problem: Option<String>,
    /// Restoring on a linked register is not available yet (E19-T03 part 2).
    pub linked: bool,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BackupCheckDto {
    pub restorable: bool,
    pub problem: Option<String>,
    pub too_new: bool,
}

fn dto((b, second_only): (BackupInfo, bool)) -> BackupDto {
    BackupDto {
        id: b.id,
        kind: b.meta.kind.as_str().into(),
        created_at: Timestamp::from_millis(b.meta.created_at).to_rfc3339(),
        size_kb: u32::try_from(b.size_bytes / 1024).unwrap_or(u32::MAX),
        sales: u32::try_from(b.meta.sales).unwrap_or(u32::MAX),
        second_folder_only: second_only,
    }
}

fn backups(services: &AppServices) -> Result<&Arc<crate::backups::Backups>, IpcError> {
    services
        .backups
        .as_ref()
        .ok_or_else(|| IpcError::coded(ErrorCode::StoreIo, "the database is not open"))
}

async fn owner(services: &AppServices) -> Result<pos_application::context::CommandContext, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(Permission::SettingsEdit).map_err(err)?;
    Ok(ctx)
}

async fn status(services: &AppServices) -> Result<BackupStatusDto, IpcError> {
    let b = backups(services)?;
    let link = services
        .app()?
        .queries
        .sync_status()
        .await
        .map_err(|e| err(e.into()))?
        .link_state;
    Ok(BackupStatusDto {
        backups: b.all().into_iter().map(dto).collect(),
        second_folder: b.secondary_folder().map(|p| p.display().to_string()),
        second_folder_problem: b.secondary_state().problem,
        linked: link == "linked",
    })
}

/// The backups on this computer and in the second folder, newest first.
#[tauri::command]
#[specta::specta]
pub async fn backup_status(services: State<'_, AppServices>) -> Result<BackupStatusDto, IpcError> {
    owner(&services).await?;
    status(&services).await
}

/// Makes a backup now (and its second copy), audited as `backup.created`.
#[tauri::command]
#[specta::specta]
pub async fn backup_now(services: State<'_, AppServices>) -> Result<BackupDto, IpcError> {
    let ctx = owner(&services).await?;
    let made = backups(&services)?
        .make(BackupKind::Manual)
        .await
        .map_err(|e| IpcError::from_error(&e))?;
    services
        .app()?
        .backup_audit
        .created(&ctx, &made.id, made.meta.sales)
        .await
        .map_err(err)?;
    Ok(dto((made, false)))
}

/// Asks for the second folder in the system dialog; cancelling changes nothing.
#[tauri::command]
#[specta::specta]
pub async fn backup_choose_folder(services: State<'_, AppServices>) -> Result<BackupStatusDto, IpcError> {
    owner(&services).await?;
    let b = backups(&services)?;
    if let Ok(Some(folder)) = services.file_dialog.folder().await {
        b.set_secondary_folder(Some(folder))
            .map_err(|e| IpcError::coded(ErrorCode::StoreIo, e.to_string()))?;
        // A first copy straight away, so a wrong folder shows at once.
        if let Some(newest) = b.db().backups().into_iter().next()
            && let Some(folder) = b.secondary_folder()
            && let Err(e) = backup::copy_to(&newest, &folder)
        {
            return Err(IpcError::coded(
                ErrorCode::StoreIo,
                format!("Backups cannot be written to that folder: {e}"),
            ));
        }
    }
    status(&services).await
}

/// Stops copying backups to a second folder.
#[tauri::command]
#[specta::specta]
pub async fn backup_clear_folder(services: State<'_, AppServices>) -> Result<BackupStatusDto, IpcError> {
    owner(&services).await?;
    backups(&services)?
        .set_secondary_folder(None)
        .map_err(|e| IpcError::coded(ErrorCode::StoreIo, e.to_string()))?;
    status(&services).await
}

/// A full `integrity_check` of one backup.
#[tauri::command]
#[specta::specta]
pub async fn backup_verify(
    services: State<'_, AppServices>,
    backup_id: String,
) -> Result<BackupCheckDto, IpcError> {
    owner(&services).await?;
    let found = backups(&services)?
        .find(&backup_id)
        .ok_or_else(|| IpcError::coded(ErrorCode::NotFound, "no such backup"))?;
    let v = backup::verify(&found.path)
        .await
        .map_err(|e| IpcError::from_error(&e))?;
    Ok(BackupCheckDto {
        restorable: v.restorable(),
        problem: v.problem,
        too_new: v.too_new,
    })
}

/// Restores a backup, then restarts the app. The database it replaces is kept as a
/// `pre_restore` backup. On a register that is not linked, sales made after the backup are
/// lost: `confirm_loss` must say the owner saw that. A linked register cannot restore yet
/// (its sales after the backup would have to come back from the cloud first).
#[tauri::command]
#[specta::specta]
pub async fn backup_restore(
    services: State<'_, AppServices>,
    backup_id: String,
    confirm_loss: bool,
) -> Result<(), IpcError> {
    let ctx = owner(&services).await?;
    let st = status(&services).await?;
    if st.linked {
        return Err(IpcError::coded(
            ErrorCode::ValidationFailed,
            "This till is linked to the cloud. Restoring a linked till is not available yet: its sales after the backup must first come back from the cloud.",
        ));
    }
    if !confirm_loss {
        return Err(IpcError::coded(
            ErrorCode::ValidationFailed,
            "Confirm that sales made after the backup will be lost.",
        ));
    }
    let b = backups(&services)?;
    let found = b
        .find(&backup_id)
        .ok_or_else(|| IpcError::coded(ErrorCode::NotFound, "no such backup"))?;
    let v = backup::verify(&found.path)
        .await
        .map_err(|e| IpcError::from_error(&e))?;
    if !v.restorable() {
        return Err(IpcError::coded(
            if v.too_new {
                ErrorCode::StoreSchemaTooNew
            } else {
                ErrorCode::StoreCorrupt
            },
            "That backup cannot be restored: it failed its check.",
        ));
    }
    let db = b.db().clone();
    let opts = DbOptions {
        path: db.path().to_path_buf(),
        backups_dir: db.backups_dir().to_path_buf(),
        reader_connections: 1,
    };
    db.close().await;
    let report = backup::restore(&opts, &found.path)
        .await
        .map_err(|e| IpcError::from_error(&e))?;
    // The restored database records who restored what, then the app starts again on it.
    let reopened = LocalDb::open(&opts).await.map_err(|e| IpcError::from_error(&e))?;
    let audit = pos_application::backups::BackupAudit {
        store: Arc::new(SqliteStore::new(reopened.clone())),
    };
    let audited = audit
        .restored(
            &ctx,
            &found.id,
            found.meta.created_at,
            report.kept.as_ref().map(|k| k.id.as_str()),
        )
        .await;
    reopened.close().await;
    if let Err(e) = audited {
        tracing::warn!(error = %e, "restored, but the audit row could not be written");
    }
    (services.restart)();
    Ok(())
}
