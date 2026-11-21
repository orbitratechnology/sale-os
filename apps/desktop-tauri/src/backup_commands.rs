//! Backups and restore (E19-T03), owner only: the backups there are, "Back up now", the second
//! folder (chosen in the system dialog, opened by Rust), a full check of a backup, and restore.

use crate::app_commands::err;
use crate::{AppServices, IpcError};
use pos_domain::{ErrorCode, Permission, Timestamp};
use pos_local_db::backup::{self, BackupInfo, BackupKind};
use pos_local_db::{LocalDb, SqliteStore};
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
    /// A linked register gets its sales after the backup back from the cloud; an unlinked one
    /// loses them.
    pub linked: bool,
    /// The database and backups are encrypted (ADR-023).
    pub encrypted: bool,
    /// Why they are not, when they are not.
    pub encryption_problem: Option<String>,
    /// The owner confirmed writing the recovery key down.
    pub recovery_key_confirmed: bool,
}

/// A finished full data export (E19-T04).
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DataExportDto {
    /// Where the owner chose to save it (shown to the owner).
    pub path: String,
    pub file_name: String,
    pub tables: u32,
    pub rows: u32,
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
        encrypted: b.db().key().is_some(),
        encryption_problem: match &services.encryption {
            crate::db_key::Encryption::Off(why) => Some(why.clone()),
            crate::db_key::Encryption::On => None,
        },
        recovery_key_confirmed: b.recovery_key_confirmed(),
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
    let b = backups(&services)?;
    let found = b
        .find(&backup_id)
        .ok_or_else(|| IpcError::coded(ErrorCode::NotFound, "no such backup"))?;
    let v = backup::verify(&found.path, b.db().key())
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
/// lost: `confirm_loss` must say the owner saw that. A linked register gets them back from the
/// cloud after the restart, so the cloud must be reachable now and the backup must be from this
/// same link.
#[tauri::command]
#[specta::specta]
pub async fn backup_restore(
    services: State<'_, AppServices>,
    backup_id: String,
    confirm_loss: bool,
) -> Result<(), IpcError> {
    let ctx = owner(&services).await?;
    let st = status(&services).await?;
    let b = backups(&services)?;
    let found = b
        .find(&backup_id)
        .ok_or_else(|| IpcError::coded(ErrorCode::NotFound, "no such backup"))?;
    let v = backup::verify(&found.path, b.db().key())
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
    if st.linked {
        linked_restore_allowed(&services, b.db(), &found.path).await?;
    } else if !confirm_loss {
        return Err(IpcError::coded(
            ErrorCode::ValidationFailed,
            "Confirm that sales made after the backup will be lost.",
        ));
    }
    let db = b.db().clone();
    let opts = db.options();
    db.close().await;
    let report = backup::restore(&opts, &found.path)
        .await
        .map_err(|e| IpcError::from_error(&e))?;
    // The restored database records who restored what, then the app starts again on it.
    let reopened = LocalDb::open(&opts).await.map_err(|e| IpcError::from_error(&e))?;
    if st.linked {
        // Before anything else can write: the sync loop brings this till's later events back
        // first, and new syncing writes wait for them (SYNC_REPLAY_PENDING).
        if let Err(e) = backup::begin_replay(&reopened).await {
            tracing::error!(error = %e, "restored, but the replay from the cloud could not be scheduled");
        }
    }
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

/// A linked register may restore a backup made while it had this same link, and only while the
/// cloud answers (its later sales must come back from there).
async fn linked_restore_allowed(
    services: &AppServices,
    db: &LocalDb,
    backup_path: &std::path::Path,
) -> Result<(), IpcError> {
    let (state, device) = backup::link_of(backup_path, db.key())
        .await
        .map_err(|e| IpcError::from_error(&e))?;
    let current = backup::device_of(db)
        .await
        .map_err(|e| IpcError::from_error(&e))?;
    if state != "linked" || device != current {
        return Err(IpcError::coded(
            ErrorCode::ValidationFailed,
            "That backup was made before this till was linked to the cloud as it is now. Choose a backup made since.",
        ));
    }
    let cloud = services.cloud.as_ref().ok_or_else(|| {
        IpcError::coded(
            ErrorCode::SyncOffline,
            "The cloud cannot be reached from this till.",
        )
    })?;
    cloud.reports.transport.replay(0, 1).await.map_err(|_| {
        IpcError::coded(
            ErrorCode::SyncOffline,
            "The cloud cannot be reached. A linked till restores only while online, so its sales after the backup can come back.",
        )
    })?;
    Ok(())
}

/// E19-T04: every table of this till's database as CSV in one ZIP, with a manifest, saved where
/// the owner chooses in the system dialog (null: cancelled; nothing written or audited). PIN
/// hashes are left out. Audited `data.exported` with the file's name.
#[tauri::command]
#[specta::specta]
pub async fn data_export_full(services: State<'_, AppServices>) -> Result<Option<DataExportDto>, IpcError> {
    let ctx = owner(&services).await?;
    let b = backups(&services)?;
    let now = ctx.clock.now();
    let date: String = now.to_rfc3339().chars().take(10).collect();
    let chosen = services
        .file_dialog
        .ask(&format!("sale-os-export-{date}.zip"), ("ZIP", &["zip"]))
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
    let manifest =
        pos_local_db::full_export::write(b.db(), &path, env!("CARGO_PKG_VERSION"), now.as_millis())
            .await
            .map_err(|e| IpcError::from_error(&e))?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    services
        .app()?
        .backup_audit
        .data_exported(&ctx, &file_name, manifest.tables.len(), manifest.total_rows())
        .await
        .map_err(err)?;
    Ok(Some(DataExportDto {
        path: path.display().to_string(),
        file_name,
        tables: u32::try_from(manifest.tables.len()).unwrap_or(u32::MAX),
        rows: u32::try_from(manifest.total_rows()).unwrap_or(u32::MAX),
    }))
}

/// ADR-023: the database recovery key, for the owner to write down (audited
/// `security.recovery_key_viewed`; the key itself is never recorded). Without it, a backup
/// cannot be read on another computer.
#[tauri::command]
#[specta::specta]
pub async fn security_recovery_key(services: State<'_, AppServices>) -> Result<String, IpcError> {
    let ctx = owner(&services).await?;
    let key =
        backups(&services)?.db().key().cloned().ok_or_else(|| {
            IpcError::coded(ErrorCode::ValidationFailed, "This till's data is not encrypted.")
        })?;
    services
        .app()?
        .backup_audit
        .recovery_key_viewed(&ctx)
        .await
        .map_err(err)?;
    Ok(key.recovery_key())
}

/// The owner wrote the recovery key down: the Backups page stops asking.
#[tauri::command]
#[specta::specta]
pub async fn security_recovery_key_confirmed(
    services: State<'_, AppServices>,
) -> Result<BackupStatusDto, IpcError> {
    owner(&services).await?;
    backups(&services)?
        .confirm_recovery_key()
        .map_err(|e| IpcError::coded(ErrorCode::StoreIo, e.to_string()))?;
    status(&services).await
}

/// ADR-023: when the database is encrypted and this computer has no key for it (a new Windows
/// account, a reinstalled PC, a backup copied from another till), the owner types the recovery
/// key. It is checked against the database, kept in the keychain, and the app restarts. Needs
/// no session (nobody can sign in to a locked database); refused when the database is open.
#[tauri::command]
#[specta::specta]
pub async fn db_unlock(services: State<'_, AppServices>, recovery_key: String) -> Result<(), IpcError> {
    match services.app() {
        Err(e) if e.code == ErrorCode::StoreLocked => {}
        _ => {
            return Err(IpcError::coded(
                ErrorCode::ValidationFailed,
                "The database is not locked.",
            ));
        }
    }
    let key = pos_local_db::crypto::DbKey::from_recovery_key(&recovery_key)
        .map_err(|e| IpcError::coded(ErrorCode::ValidationFailed, e.to_string()))?;
    let path = pos_local_db::DbOptions::in_dir(&services.data_dir).path;
    if !pos_local_db::crypto::key_opens(&path, &key).await {
        return Err(IpcError::coded(
            ErrorCode::StoreLocked,
            "That recovery key does not open this till's data. Check it and try again.",
        ));
    }
    services.db_keys.set(&key).await.map_err(|e| {
        IpcError::coded(
            ErrorCode::StoreIo,
            format!("The key is right, but the keychain could not keep it: {e}"),
        )
    })?;
    tracing::warn!("database unlocked with the recovery key; restarting");
    (services.restart)();
    Ok(())
}
