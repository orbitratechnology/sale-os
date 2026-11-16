//! Linking a register to the cloud (E17-T08, SYN-001, PRD §7.1, D-04/D-10, ADR-013).
//!
//! The owner signs in with their cloud account, then either
//! - **creates** the cloud business from this register: the local ids are kept, the cloud
//!   gets the business and this register, and the outbox (everything done offline) drains; or
//! - **joins** an existing business: allowed only while this register has no live sales and
//!   no products. Joining replaces the local data with the business's, so the whole bootstrap
//!   is downloaded to a staging area first; only then, in one transaction, the local business
//!   is wiped and the snapshot applied.
//!
//! The refresh token goes to the OS credential store the moment the cloud issues it, and
//! progress is recorded (`link_state = 'linking'`), so a link that loses the network can be
//! resumed without registering again.

use crate::context::{AppError, CommandContext};
use crate::queries::QueryError;
use crate::sync::{InboundReport, SyncTransport};
use pos_domain::secret::Secret;
use pos_domain::{ErrorCode, Permission};
use pos_sync::pull::BootstrapPage;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// The owner's cloud sign-in (a 15-minute user token). Lives in memory only.
#[derive(Clone)]
pub struct OwnerSession {
    pub token: Secret<String>,
}

impl std::fmt::Debug for OwnerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerSession([REDACTED])")
    }
}

/// A cloud business the owner belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudBusiness {
    pub business_id: Uuid,
    pub name: String,
    pub currency_code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewCloudBusiness {
    pub business_id: Uuid,
    pub name: String,
    pub country_code: String,
    pub timezone: String,
    pub currency_code: String,
    pub currency_exponent: u8,
    pub prices_include_tax: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRegistration {
    pub business_id: Uuid,
    pub register_id: Uuid,
    pub register_name: String,
    /// `None` when joining: the cloud assigns the next free code.
    pub register_code: Option<String>,
    /// `None` when joining: the business's first location.
    pub location: Option<(Uuid, String)>,
    pub installation_id: Uuid,
    pub app_version: String,
    pub replace_existing: bool,
}

/// What the cloud issued for this register.
pub struct Registered {
    pub device_id: Uuid,
    pub access_token: Secret<String>,
    pub expires_in: i64,
    pub refresh_token: Secret<String>,
    pub register_code: String,
    pub location_id: Uuid,
}

/// Why linking failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("the email or the password is wrong")]
    CredentialsInvalid,
    #[error("too many sign-in attempts; try again later")]
    RateLimited,
    #[error("another cloud account owns a business with this id")]
    BusinessTaken,
    #[error("another device is linked to this register; confirm to replace it")]
    RegisterInUse,
    #[error("no such business for this account")]
    NotFound,
    #[error("the cloud could not be reached")]
    Offline,
    #[error("the cloud needs a newer app")]
    UpgradeRequired,
    #[error("this register has live sales or products, so it cannot join another business")]
    JoinNotAllowed,
    #[error("the credential store failed: {0}")]
    Credentials(String),
    #[error("{0}")]
    Other(String),
}

impl LinkError {
    pub fn code(&self) -> ErrorCode {
        match self {
            LinkError::CredentialsInvalid => ErrorCode::AuthCredentialsInvalid,
            LinkError::RateLimited => ErrorCode::AuthRateLimited,
            LinkError::BusinessTaken => ErrorCode::SyncBusinessIdTaken,
            LinkError::RegisterInUse => ErrorCode::SyncRegisterInUse,
            LinkError::NotFound => ErrorCode::NotFound,
            LinkError::Offline => ErrorCode::SyncOffline,
            LinkError::UpgradeRequired => ErrorCode::SyncUpgradeRequired,
            LinkError::JoinNotAllowed => ErrorCode::SyncJoinNotAllowed,
            LinkError::Credentials(_) | LinkError::Other(_) => ErrorCode::Internal,
        }
    }
}

impl From<LinkError> for AppError {
    fn from(e: LinkError) -> Self {
        let code = e.code();
        match code {
            ErrorCode::Internal => AppError::Internal(e.to_string()),
            _ => AppError::Validation(e.to_string(), code),
        }
    }
}

/// The owner-facing cloud API (`/v1/auth/login`, `/v1/businesses`, `/v1/devices/register`).
#[async_trait::async_trait]
pub trait CloudAccount: Send + Sync {
    async fn login(&self, email: &str, password: &Secret<String>) -> Result<OwnerSession, LinkError>;
    async fn businesses(&self, owner: &OwnerSession) -> Result<Vec<CloudBusiness>, LinkError>;
    /// Idempotent for the same owner and id.
    async fn create_business(&self, owner: &OwnerSession, b: &NewCloudBusiness) -> Result<(), LinkError>;
    async fn register_device(
        &self,
        owner: &OwnerSession,
        r: &DeviceRegistration,
    ) -> Result<Registered, LinkError>;
}

/// The OS keychain (ADR-013): the device's refresh token, and nothing else.
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get_refresh_token(&self) -> Result<Option<Secret<String>>, LinkError>;
    async fn set_refresh_token(&self, token: Secret<String>) -> Result<(), LinkError>;
    async fn clear(&self) -> Result<(), LinkError>;
}

/// Hands a freshly issued access token to the sync transport, so it need not refresh at once.
pub trait SessionSeed: Send + Sync {
    fn seed(&self, access_token: Secret<String>, expires_in_secs: i64);
}

/// What the link needs to know about this register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkFacts {
    /// `unlinked`, `linking`, `linked` or `revoked`.
    pub link_state: String,
    pub business: NewCloudBusiness,
    pub location_id: Uuid,
    pub location_name: String,
    pub register_id: Uuid,
    pub register_name: String,
    pub register_code: String,
    pub installation_id: Uuid,
    pub live_sales: i64,
    pub products: i64,
    pub progress: Option<LinkProgress>,
}

impl LinkFacts {
    /// D-10: joining replaces local data, so only a register with nothing of its own yet.
    pub fn may_join(&self) -> bool {
        self.live_sales == 0 && self.products == 0
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPath {
    Create,
    Join,
}

/// A link under way, kept in `sync_state` so it can resume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkProgress {
    pub path: LinkPath,
    pub business_id: Uuid,
    pub register_id: Uuid,
    pub register_name: String,
    /// Set once the cloud registered this device (its refresh token is then in the keychain).
    #[serde(default)]
    pub device_id: Option<Uuid>,
    #[serde(default)]
    pub register_code: Option<String>,
    #[serde(default)]
    pub location_id: Option<Uuid>,
    /// Join: the whole bootstrap is staged; this is where its pull continues.
    #[serde(default)]
    pub staged_cursor: Option<String>,
}

/// Local persistence of the link (implemented by `pos-local-db`).
#[async_trait::async_trait]
pub trait LinkStore: Send + Sync {
    async fn link_facts(&self) -> Result<LinkFacts, QueryError>;
    /// `linking`, with the progress so far.
    async fn save_progress(&self, progress: &LinkProgress) -> Result<(), QueryError>;
    /// Create path done: `linked`, with the device id.
    async fn finish_create(&self, device_id: Uuid) -> Result<(), QueryError>;
    /// Join: stage one bootstrap page (and where to resume).
    async fn stage_bootstrap(&self, page: &BootstrapPage) -> Result<(), QueryError>;
    /// Join: the bootstrap page to fetch next (`None` for the first).
    async fn staged_page(&self) -> Result<Option<String>, QueryError>;
    /// Join, once everything is staged: wipe this register's business, apply the snapshot,
    /// become `register_id` of the joined business, `linked`, in one transaction.
    async fn finish_join(
        &self,
        progress: &LinkProgress,
        at: pos_domain::Timestamp,
    ) -> Result<InboundReport, QueryError>;
    /// The owner disconnected this register: keep working locally, stop syncing.
    async fn mark_revoked(&self) -> Result<(), QueryError>;
}

/// What `start` tells the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkOptions {
    pub businesses: Vec<CloudBusiness>,
    /// Joining is offered only to a register with no live sales and no products.
    pub may_join: bool,
    /// This register's own business already exists in the cloud (a create resumes).
    pub own_business_in_cloud: bool,
}

/// The result of a completed link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub path: LinkPath,
    pub business_id: Uuid,
    pub register_code: String,
    /// Join: what the bootstrap brought.
    pub applied: InboundReport,
}

pub struct LinkService {
    pub account: Arc<dyn CloudAccount>,
    pub credentials: Arc<dyn CredentialStore>,
    pub store: Arc<dyn LinkStore>,
    pub local: Arc<dyn crate::LocalStore>,
    pub transport: Arc<dyn SyncTransport>,
    pub seed: Arc<dyn SessionSeed>,
    pub app_version: String,
}

impl LinkService {
    /// Signs the owner in and says what they can do.
    pub async fn start(
        &self,
        ctx: &CommandContext,
        email: &str,
        password: &Secret<String>,
    ) -> Result<(OwnerSession, LinkOptions), AppError> {
        ctx.authorize(Permission::DeviceLink)?;
        let facts = self.store.link_facts().await?;
        if facts.link_state == "linked" {
            return Err(AppError::Validation(
                "this register is already linked".into(),
                ErrorCode::ValidationFailed,
            ));
        }
        let owner = self.account.login(email, password).await?;
        let businesses = self.account.businesses(&owner).await?;
        let own = businesses
            .iter()
            .any(|b| b.business_id == facts.business.business_id);
        Ok((
            owner,
            LinkOptions {
                businesses,
                may_join: facts.may_join(),
                own_business_in_cloud: own,
            },
        ))
    }

    /// Creates the cloud business from this register (or resumes doing so).
    pub async fn create(
        &self,
        ctx: &CommandContext,
        owner: &OwnerSession,
        replace_existing: bool,
    ) -> Result<Linked, AppError> {
        ctx.authorize(Permission::DeviceLink)?;
        let facts = self.store.link_facts().await?;
        let mut progress = match facts.progress.clone() {
            Some(p) if p.path == LinkPath::Create => p,
            _ => LinkProgress {
                path: LinkPath::Create,
                business_id: facts.business.business_id,
                register_id: facts.register_id,
                register_name: facts.register_name.clone(),
                device_id: None,
                register_code: None,
                location_id: None,
                staged_cursor: None,
            },
        };
        self.store.save_progress(&progress).await?;
        if progress.device_id.is_none() {
            self.account.create_business(owner, &facts.business).await?;
            let reg = self
                .account
                .register_device(
                    owner,
                    &DeviceRegistration {
                        business_id: facts.business.business_id,
                        register_id: facts.register_id,
                        register_name: facts.register_name.clone(),
                        register_code: Some(facts.register_code.clone()),
                        location: Some((facts.location_id, facts.location_name.clone())),
                        installation_id: facts.installation_id,
                        app_version: self.app_version.clone(),
                        replace_existing,
                    },
                )
                .await?;
            self.keep(&reg).await?;
            progress.device_id = Some(reg.device_id);
            progress.register_code = Some(reg.register_code.clone());
            progress.location_id = Some(reg.location_id);
            self.store.save_progress(&progress).await?;
        }
        let device_id = progress.device_id.unwrap_or_default();
        self.store.finish_create(device_id).await?;
        self.audit_linked(ctx.clone(), &progress).await?;
        Ok(Linked {
            path: LinkPath::Create,
            business_id: progress.business_id,
            register_code: facts.register_code,
            applied: InboundReport::default(),
        })
    }

    /// Joins `business_id` (or resumes joining it): register, stage the whole bootstrap,
    /// then replace the local data in one transaction.
    pub async fn join(
        &self,
        ctx: &CommandContext,
        owner: &OwnerSession,
        business_id: Uuid,
    ) -> Result<Linked, AppError> {
        ctx.authorize(Permission::DeviceLink)?;
        let facts = self.store.link_facts().await?;
        let mut progress = match facts.progress.clone() {
            Some(p) if p.path == LinkPath::Join && p.business_id == business_id => p,
            _ => {
                if !facts.may_join() {
                    return Err(LinkError::JoinNotAllowed.into());
                }
                LinkProgress {
                    path: LinkPath::Join,
                    business_id,
                    // A new register of that business; the local one is replaced.
                    register_id: ctx.ids.new_uuid_v7(),
                    register_name: facts.register_name.clone(),
                    device_id: None,
                    register_code: None,
                    location_id: None,
                    staged_cursor: None,
                }
            }
        };
        self.store.save_progress(&progress).await?;
        if progress.device_id.is_none() {
            let reg = self
                .account
                .register_device(
                    owner,
                    &DeviceRegistration {
                        business_id,
                        register_id: progress.register_id,
                        register_name: progress.register_name.clone(),
                        register_code: None,
                        location: None,
                        installation_id: facts.installation_id,
                        app_version: self.app_version.clone(),
                        replace_existing: false,
                    },
                )
                .await?;
            self.keep(&reg).await?;
            progress.device_id = Some(reg.device_id);
            progress.register_code = Some(reg.register_code.clone());
            progress.location_id = Some(reg.location_id);
            self.store.save_progress(&progress).await?;
        }
        // Download everything to staging; an interruption resumes from the stored page.
        while progress.staged_cursor.is_none() {
            let next = self.store.staged_page().await?;
            let page = self
                .transport
                .bootstrap(next)
                .await
                .map_err(|e| AppError::from(crate::sync::link_error(&e)))?;
            self.store.stage_bootstrap(&page).await?;
            if page.next_page.is_none() {
                progress.staged_cursor = Some(page.snapshot_cursor.clone());
                self.store.save_progress(&progress).await?;
            }
        }
        let applied = self.store.finish_join(&progress, ctx.now()).await?;
        // The audit entry belongs to the joined business now: its first local event.
        let mut joined = ctx.clone();
        joined.register = crate::context::RegisterContext {
            business_id: pos_domain::BusinessId::from_uuid(business_id),
            location_id: pos_domain::LocationId::from_uuid(progress.location_id.unwrap_or_default()),
            register_id: pos_domain::RegisterId::from_uuid(progress.register_id),
        };
        joined.actor = None;
        self.audit_linked(joined, &progress).await?;
        Ok(Linked {
            path: LinkPath::Join,
            business_id,
            register_code: progress.register_code.clone().unwrap_or_default(),
            applied,
        })
    }

    /// Keeps the credentials the moment they exist: the refresh token in the keychain, the
    /// access token with the transport.
    async fn keep(&self, reg: &Registered) -> Result<(), AppError> {
        self.credentials
            .set_refresh_token(reg.refresh_token.clone())
            .await?;
        self.seed.seed(reg.access_token.clone(), reg.expires_in);
        Ok(())
    }

    async fn audit_linked(&self, ctx: CommandContext, p: &LinkProgress) -> Result<(), AppError> {
        use crate::audit::{AuditEntry, SYNC_LINKED};
        let ids = ctx.ids.as_ref();
        let entry = AuditEntry::new(ids, SYNC_LINKED, "register", p.register_id).with_metadata(serde_json::json!({
            "path": p.path, "businessId": p.business_id, "deviceId": p.device_id, "registerCode": p.register_code,
        }));
        let set = crate::WriteSet::builder(ctx.write_context())
            .outbox(crate::OutboxDraft::audit_recorded(ids, &entry)?)
            .audit(entry)
            .build()?;
        self.local.apply(set).await?;
        Ok(())
    }
}
