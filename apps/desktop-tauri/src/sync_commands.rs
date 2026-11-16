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
    services.context().await?;
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
}

/// Where sync stands. Any signed-in user.
#[tauri::command]
#[specta::specta]
pub async fn sync_status(services: State<'_, AppServices>) -> Result<SyncStatusDto, IpcError> {
    services.context().await?;
    let s = services
        .app()?
        .queries
        .sync_status()
        .await
        .map_err(|e| err(e.into()))?;
    let at = |ms: Option<i64>| ms.map(|m| Timestamp::from_millis(m).to_rfc3339());
    Ok(SyncStatusDto {
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
