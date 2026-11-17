//! Sync commands (E17): status, retry, and the link wizard (E17-T08).

use crate::app_commands::err;
use crate::{AppServices, IpcError};
use pos_application::link::LinkPath;
use pos_domain::{ErrorCode, Secret, Timestamp};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

/// Pushes pending events now, with the retry delay reset (E17-T03). Any signed-in user.
#[tauri::command]
#[specta::specta]
pub async fn sync_retry_now(services: State<'_, AppServices>) -> Result<(), IpcError> {
    services.context().await?.actor().map_err(err)?;
    if let Some(sync) = &services.sync {
        sync.retry_now();
    }
    Ok(())
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatusDto {
    /// `unlinked`, `linking`, `linked` or `revoked` (the owner disconnected this register).
    pub link_state: String,
    /// Whether this app has a cloud to link to.
    pub cloud_configured: bool,
    /// Events not yet acknowledged by the cloud.
    pub pending: u32,
    /// RFC 3339.
    pub last_push_ok_at: Option<String>,
    pub last_pull_ok_at: Option<String>,
    pub last_error_code: Option<String>,
    /// The cloud's clock and this computer's differ by more than 10 minutes (ADR-006).
    pub clock_skew_warning: bool,
    pub register_code: Option<String>,
    /// The status bar's state: `not_linked`, `synced`, `pending`, `offline`, `attention`,
    /// `revoked` or `update_required` (E17-T09).
    pub indicator: String,
    /// Events the cloud refused (the repair queue), including ones holding up the rest.
    pub rejected: u32,
    /// Open conflicts.
    pub conflicts: u32,
}

/// Where sync stands. Any signed-in user.
#[tauri::command]
#[specta::specta]
pub async fn sync_status(services: State<'_, AppServices>) -> Result<SyncStatusDto, IpcError> {
    services.context().await?.actor().map_err(err)?;
    let s = services
        .app()?
        .queries
        .sync_status()
        .await
        .map_err(|e| err(e.into()))?;
    let at = |ms: Option<i64>| ms.map(|m| Timestamp::from_millis(m).to_rfc3339());
    let indicator = pos_application::sync::indicator(
        &s.link_state,
        s.pending,
        s.rejected + s.blocked + s.conflicts,
        s.last_error_code.as_deref(),
    );
    let small = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    Ok(SyncStatusDto {
        indicator: indicator.as_str().into(),
        rejected: small(s.rejected + s.blocked),
        conflicts: small(s.conflicts),
        link_state: s.link_state,
        cloud_configured: services.cloud.is_some(),
        pending: u32::try_from(s.pending).unwrap_or(u32::MAX),
        last_push_ok_at: at(s.last_push_ok_at),
        last_pull_ok_at: at(s.last_pull_ok_at),
        last_error_code: s.last_error_code,
        clock_skew_warning: pos_application::sync::clock_skew_warning(s.clock_skew_ms),
        register_code: s.register_code,
    })
}

fn cloud(services: &AppServices) -> Result<&crate::services::Cloud, IpcError> {
    services
        .cloud
        .as_ref()
        .ok_or_else(|| IpcError::coded(ErrorCode::SyncOffline, "no cloud is configured for this app"))
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CloudBusinessDto {
    pub business_id: String,
    pub name: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LinkOptionsDto {
    pub businesses: Vec<CloudBusinessDto>,
    /// Joining is only for a register with no live sales and no products (it replaces them).
    pub may_join: bool,
    /// This register's business is already in the cloud (an earlier link to finish).
    pub own_business_in_cloud: bool,
}

/// The owner signs in to the cloud (owner only). The password is passed through once and
/// never stored or logged; the sign-in is kept in memory for `sync_link_complete`.
#[tauri::command]
#[specta::specta]
pub async fn sync_link_start(
    services: State<'_, AppServices>,
    email: String,
    password: String,
) -> Result<LinkOptionsDto, IpcError> {
    let ctx = services.context().await?;
    // The role first, as every command: then whether there is a cloud at all.
    ctx.authorize(pos_domain::Permission::DeviceLink).map_err(err)?;
    let cloud = cloud(&services)?;
    let (owner, options) = cloud
        .link
        .start(&ctx, email.trim(), &Secret::new(password))
        .await
        .map_err(err)?;
    *cloud.owner.lock().await = Some(owner);
    Ok(LinkOptionsDto {
        businesses: options
            .businesses
            .into_iter()
            .map(|b| CloudBusinessDto {
                business_id: b.business_id.to_string(),
                name: b.name,
            })
            .collect(),
        may_join: options.may_join,
        own_business_in_cloud: options.own_business_in_cloud,
    })
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LinkMode {
    /// Create the cloud business from this register.
    Create,
    /// Join an existing business (replaces this register's data).
    Join,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LinkResultDto {
    /// `create` or `join`.
    pub path: String,
    pub business_id: String,
    pub register_code: String,
    /// Joined: this register's data was replaced, so everyone signs in again.
    pub sign_in_again: bool,
}

/// Links this register (owner only), after `sync_link_start`. A link interrupted by a lost
/// connection resumes when this is called again.
#[tauri::command]
#[specta::specta]
pub async fn sync_link_complete(
    services: State<'_, AppServices>,
    mode: LinkMode,
    business_id: Option<String>,
    replace_existing: bool,
) -> Result<LinkResultDto, IpcError> {
    let ctx = services.context().await?;
    // The role first, as every command: then whether there is a cloud at all.
    ctx.authorize(pos_domain::Permission::DeviceLink).map_err(err)?;
    let cloud = cloud(&services)?;
    let owner = cloud
        .owner
        .lock()
        .await
        .clone()
        .ok_or_else(|| IpcError::coded(ErrorCode::AuthRequired, "sign in to the cloud first"))?;
    let linked = match mode {
        LinkMode::Create => cloud.link.create(&ctx, &owner, replace_existing).await,
        LinkMode::Join => {
            let id = business_id
                .as_deref()
                .and_then(|b| b.parse().ok())
                .ok_or_else(|| IpcError::coded(ErrorCode::ValidationFailed, "choose a business to join"))?;
            cloud.link.join(&ctx, &owner, id).await
        }
    }
    .map_err(err)?;
    *cloud.owner.lock().await = None;
    let joined = linked.path == LinkPath::Join;
    if joined {
        // The staff and the business are the joined one's now.
        services.set_session(None).await;
    }
    if let Some(sync) = &services.sync {
        sync.retry_now();
    }
    Ok(LinkResultDto {
        path: if joined { "join".into() } else { "create".into() },
        business_id: linked.business_id.to_string(),
        register_code: linked.register_code,
        sign_in_again: joined,
    })
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RepairEventDto {
    pub event_id: String,
    pub local_sequence: u32,
    pub event_type: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    /// The reason code (`EVENT_INVALID`, `SNAPSHOT_MISMATCH`, `EVENT_VERSION_UNSUPPORTED`...).
    pub code: Option<String>,
    /// The cloud's own words, for support.
    pub detail: Option<String>,
    /// `rejected` (set aside; later events went on) or `blocked` (holds up the rest until the
    /// cloud accepts it, e.g. after an update).
    pub kind: String,
    pub occurred_at: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConflictSummaryDto {
    pub conflict_id: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub local_base_version: u32,
    pub server_version: u32,
    pub created_at: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RepairQueueDto {
    pub events: Vec<RepairEventDto>,
    pub conflicts: Vec<ConflictSummaryDto>,
}

fn small(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(0)
}

/// Events the cloud refused and open conflicts (E17-T09, owner).
#[tauri::command]
#[specta::specta]
pub async fn sync_repair_queue(services: State<'_, AppServices>) -> Result<RepairQueueDto, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::SyncManage).map_err(err)?;
    let (events, conflicts) = services
        .app()?
        .queries
        .repair_queue()
        .await
        .map_err(|e| err(e.into()))?;
    let at = |ms: i64| Timestamp::from_millis(ms).to_rfc3339();
    Ok(RepairQueueDto {
        events: events
            .into_iter()
            .map(|e| RepairEventDto {
                event_id: e.event_id,
                local_sequence: small(e.local_sequence),
                event_type: e.event_type,
                aggregate_type: e.aggregate_type,
                aggregate_id: e.aggregate_id,
                code: e.code,
                detail: e.detail,
                kind: e.kind,
                occurred_at: at(e.occurred_at),
            })
            .collect(),
        conflicts: conflicts
            .into_iter()
            .map(|c| ConflictSummaryDto {
                conflict_id: c.conflict_id,
                aggregate_type: c.aggregate_type,
                aggregate_id: c.aggregate_id,
                local_base_version: small(c.local_base_version),
                server_version: small(c.server_version),
                created_at: at(c.created_at),
            })
            .collect(),
    })
}

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RepairAction {
    /// The event as it was sent (PIN hashes removed).
    ViewPayload,
    /// A file for support: the status, the queue and the refused events.
    ExportSupport,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RepairActionResultDto {
    /// `view_payload`: the event, pretty-printed JSON.
    pub payload_json: Option<String>,
    /// `export_support`: the saved file's name, or `None` if the save was cancelled.
    pub saved_file_name: Option<String>,
}

/// Repair-queue actions (E17-T09, owner): view an event's payload, or export a support file.
#[tauri::command]
#[specta::specta]
pub async fn sync_repair_action(
    services: State<'_, AppServices>,
    action: RepairAction,
    event_id: Option<String>,
) -> Result<RepairActionResultDto, IpcError> {
    let ctx = services.context().await?;
    ctx.authorize(pos_domain::Permission::SyncManage).map_err(err)?;
    let store = &services.app()?.queries;
    let pretty = |v: &serde_json::Value| serde_json::to_string_pretty(v).unwrap_or_default();
    match action {
        RepairAction::ViewPayload => {
            let id = event_id
                .ok_or_else(|| IpcError::coded(ErrorCode::ValidationFailed, "eventId is required"))?;
            let event = store
                .outbox_event(&id)
                .await
                .map_err(|e| err(e.into()))?
                .ok_or_else(|| IpcError::coded(ErrorCode::NotFound, "no such event"))?;
            Ok(RepairActionResultDto {
                payload_json: Some(pretty(&pos_application::sync::redact_payload(event))),
                saved_file_name: None,
            })
        }
        RepairAction::ExportSupport => {
            let status = store.sync_status().await.map_err(|e| err(e.into()))?;
            let (events, conflicts) = store.repair_queue().await.map_err(|e| err(e.into()))?;
            let mut refused = Vec::new();
            for e in &events {
                if let Some(v) = store.outbox_event(&e.event_id).await.map_err(|e| err(e.into()))? {
                    refused.push(pos_application::sync::redact_payload(v));
                }
            }
            let bundle = serde_json::json!({
                "generatedAt": ctx.now().to_rfc3339(),
                "appVersion": env!("CARGO_PKG_VERSION"),
                "status": {
                    "linkState": status.link_state, "pending": status.pending, "rejected": status.rejected,
                    "blocked": status.blocked, "conflicts": status.conflicts,
                    "lastPushOkAt": status.last_push_ok_at.map(|m| Timestamp::from_millis(m).to_rfc3339()),
                    "lastPullOkAt": status.last_pull_ok_at.map(|m| Timestamp::from_millis(m).to_rfc3339()),
                    "lastErrorCode": status.last_error_code, "clockSkewMs": status.clock_skew_ms,
                    "registerCode": status.register_code,
                },
                "refusedEvents": refused,
                "conflicts": conflicts.iter().map(|c| serde_json::json!({
                    "conflictId": c.conflict_id, "aggregateType": c.aggregate_type, "aggregateId": c.aggregate_id,
                    "localBaseVersion": c.local_base_version, "serverVersion": c.server_version,
                })).collect::<Vec<_>>(),
            });
            let chosen = services
                .file_dialog
                .ask("sale-os-sync-support.json", ("JSON", &["json"]))
                .await
                .ok()
                .flatten();
            let Some(path) = chosen else {
                return Ok(RepairActionResultDto {
                    payload_json: None,
                    saved_file_name: None,
                });
            };
            let path = if path.extension().is_some() {
                path
            } else {
                path.with_extension("json")
            };
            crate::sale_commands::write_replacing(&path, pretty(&bundle).as_bytes())
                .map_err(IpcError::internal)?;
            tracing::info!(events = refused.len(), "sync support file exported");
            Ok(RepairActionResultDto {
                payload_json: None,
                saved_file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
            })
        }
    }
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaleSyncDto {
    /// `local` (no cloud link), `pending` (saved on this device, waiting), `synced`, or
    /// `attention` (the cloud refused it; see the repair queue).
    pub state: String,
}

/// Whether a sale has reached the cloud (AT-13). Any signed-in user: the till shows it after
/// a sale.
#[tauri::command]
#[specta::specta]
pub async fn sale_sync_state(
    services: State<'_, AppServices>,
    sale_id: pos_domain::SaleId,
) -> Result<SaleSyncDto, IpcError> {
    services.context().await?.actor().map_err(err)?;
    let (link, status) = services
        .app()?
        .queries
        .sale_sync(&sale_id.to_string())
        .await
        .map_err(|e| err(e.into()))?;
    Ok(SaleSyncDto {
        state: pos_application::sync::sale_sync(&link, status.as_deref()).into(),
    })
}
