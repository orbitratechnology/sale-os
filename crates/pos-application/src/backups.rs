//! Audit rows for backups and restores (E19-T03) and the full data export (E19-T04). The backup files themselves are written by
//! the local database (`pos_local_db::backup`); this records who made or restored one.

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::Permission;
use std::sync::Arc;

pub struct BackupAudit {
    pub store: Arc<dyn LocalStore>,
}

impl BackupAudit {
    async fn record(
        &self,
        ctx: &CommandContext,
        action: audit::AuditAction,
        file: &str,
        metadata: serde_json::Value,
    ) -> Result<(), AppError> {
        let ids = ctx.ids.as_ref();
        let entry = AuditEntry::new(ids, action, "backup", file).with_metadata(metadata);
        let set = WriteSet::builder(ctx.write_context())
            .outbox(OutboxDraft::audit_recorded(ids, &entry)?)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }

    /// The owner made a backup (`backup.created`). Automatic ones are not audited: they are
    /// listed with their reason on the backups page.
    pub async fn created(&self, ctx: &CommandContext, file: &str, sales: i64) -> Result<(), AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        self.record(
            ctx,
            audit::BACKUP_CREATED,
            file,
            serde_json::json!({ "file": file, "sales": sales }),
        )
        .await
    }

    /// Written into the restored database (`backup.restored`): which backup, when it was
    /// made, and the copy kept of the database it replaced.
    pub async fn restored(
        &self,
        ctx: &CommandContext,
        file: &str,
        backup_created_at: i64,
        kept: Option<&str>,
    ) -> Result<(), AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        self.record(
            ctx,
            audit::BACKUP_RESTORED,
            file,
            serde_json::json!({ "file": file, "backupCreatedAt": backup_created_at, "kept": kept }),
        )
        .await
    }

    /// The owner exported all the business's data (`data.exported`): the file's name (never
    /// its folder), the tables and the rows.
    pub async fn data_exported(
        &self,
        ctx: &CommandContext,
        file: &str,
        tables: usize,
        rows: u64,
    ) -> Result<(), AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        let ids = ctx.ids.as_ref();
        let entry = AuditEntry::new(ids, audit::DATA_EXPORTED, "data_export", file)
            .with_metadata(serde_json::json!({ "file": file, "tables": tables, "rows": rows }));
        let set = WriteSet::builder(ctx.write_context())
            .outbox(OutboxDraft::audit_recorded(ids, &entry)?)
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(())
    }

    /// The owner looked at the database recovery key (ADR-023). The key itself is never
    /// recorded.
    pub async fn recovery_key_viewed(&self, ctx: &CommandContext) -> Result<(), AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        self.record(
            ctx,
            audit::SECURITY_RECOVERY_KEY_VIEWED,
            "database",
            serde_json::json!({}),
        )
        .await
    }
}
