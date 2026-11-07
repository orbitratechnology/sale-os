//! Who is acting, where, and the ports every use case needs (docs/backlog/06 §9).

use crate::WriteContext;
use crate::write::{WriteError, WriteSetError};
use pos_domain::staff::StaffError;
use pos_domain::{
    ApprovalId, ApprovalScope, BusinessId, Clock, ErrorCode, HasErrorCode, IdGenerator, LocationId,
    Permission, RegisterId, Role, SessionId, Timestamp, UserId, approvable, role_has,
};
use std::sync::Arc;

/// The register this installation is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterContext {
    pub business_id: BusinessId,
    pub location_id: LocationId,
    pub register_id: RegisterId,
}

/// The signed-in staff member. Comes from the unlocked session held in Rust, never from
/// IPC arguments (docs/backlog/07 §1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    pub user_id: UserId,
    pub role: Role,
    pub session_id: SessionId,
}

/// A manager's approval (E05-T04): their verified PIN, for one scope, until `expires_at`.
/// Single use: the shell takes it from the registry before the command runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalToken {
    pub id: ApprovalId,
    pub approver: UserId,
    pub scope: ApprovalScope,
    pub expires_at: Timestamp,
}

#[derive(Clone)]
pub struct CommandContext {
    pub register: RegisterContext,
    pub actor: Option<Actor>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    /// An approval the caller presented with this command, if any.
    pub approval: Option<ApprovalToken>,
}

impl CommandContext {
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// The signed-in actor, or `SESSION_LOCKED`.
    pub fn actor(&self) -> Result<&Actor, AppError> {
        self.actor.as_ref().ok_or(AppError::SessionLocked)
    }

    /// The actor if they hold `permission` (IAM-003), else `PERMISSION_DENIED`.
    pub fn authorize(&self, permission: Permission) -> Result<&Actor, AppError> {
        let actor = self.actor()?;
        if role_has(actor.role, permission) {
            Ok(actor)
        } else {
            Err(AppError::PermissionDenied(permission))
        }
    }

    /// For actions a role may do only with approval (07 §1.2 "approval" cells). Returns the
    /// approver when an approval was needed and is valid, `None` when the actor's own role
    /// suffices. `APPROVAL_REQUIRED` carries the scope the UI must get approved;
    /// `APPROVAL_INVALID` means the presented token is expired or covers something else.
    pub fn authorize_with_approval(
        &self,
        permission: Permission,
        entity_id: impl ToString,
        max_ppm: Option<u32>,
    ) -> Result<Option<UserId>, AppError> {
        let actor = self.actor()?;
        if role_has(actor.role, permission) {
            return Ok(None);
        }
        if !approvable(actor.role, permission) {
            return Err(AppError::PermissionDenied(permission));
        }
        let needed = ApprovalScope {
            permission,
            entity_id: entity_id.to_string(),
            max_ppm,
        };
        match &self.approval {
            None => Err(AppError::ApprovalRequired(needed)),
            Some(t) if t.expires_at <= self.now() => {
                Err(AppError::ApprovalInvalid("the approval has expired"))
            }
            Some(t) if !t.scope.covers(&needed) => Err(AppError::ApprovalInvalid(
                "the approval does not cover this action",
            )),
            Some(t) => Ok(Some(t.approver)),
        }
    }

    /// A write context stamped with the actor and the current time.
    pub fn write_context(&self) -> WriteContext {
        WriteContext {
            business_id: self.register.business_id,
            location_id: self.register.location_id,
            register_id: self.register.register_id,
            actor: self.actor.as_ref().map(|a| a.user_id),
            occurred_at: self.now(),
        }
    }
}

/// Application-level errors. Each maps to one catalogued code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    #[error("no staff member is signed in")]
    SessionLocked,
    #[error("your role does not allow {0:?}")]
    PermissionDenied(Permission),
    #[error("{:?} needs a manager's approval", .0.permission)]
    ApprovalRequired(ApprovalScope),
    #[error("approval not accepted: {0}")]
    ApprovalInvalid(&'static str),
    #[error("wrong PIN; {remaining_attempts} attempt(s) left before a temporary lock")]
    PinInvalid { remaining_attempts: u32 },
    #[error("too many wrong PINs; try again after {retry_at_ms} (epoch ms)")]
    LockedOut { retry_at_ms: i64 },
    #[error(transparent)]
    Staff(#[from] StaffError),
    #[error(transparent)]
    Setup(#[from] crate::setup::SetupError),
    #[error(transparent)]
    Catalog(#[from] pos_domain::catalog::CatalogError),
    #[error(transparent)]
    Settlement(#[from] pos_domain::settlement::SettlementError),
    #[error(transparent)]
    Inventory(#[from] pos_domain::inventory::InventoryError),
    #[error(transparent)]
    Sale(#[from] pos_domain::sale::SaleError),
    #[error(transparent)]
    Return(#[from] pos_domain::returns::ReturnError),
    #[error(transparent)]
    Document(#[from] pos_domain::stock_docs::DocumentError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error(transparent)]
    WriteSet(#[from] WriteSetError),
    #[error("could not read local data: {0}")]
    Query(String),
    /// A domain rule failed; carries its catalogued code (cart, pricing...).
    #[error("{0}")]
    Validation(String, ErrorCode),
    #[error("internal error: {0}")]
    Internal(String),
}

impl HasErrorCode for AppError {
    fn error_code(&self) -> ErrorCode {
        match self {
            AppError::SessionLocked => ErrorCode::SessionLocked,
            AppError::PermissionDenied(_) => ErrorCode::PermissionDenied,
            AppError::ApprovalRequired(_) => ErrorCode::ApprovalRequired,
            AppError::ApprovalInvalid(_) => ErrorCode::ApprovalInvalid,
            AppError::PinInvalid { .. } => ErrorCode::AuthPinInvalid,
            AppError::LockedOut { .. } => ErrorCode::AuthLockedOut,
            AppError::Staff(e) => e.error_code(),
            AppError::Setup(e) => e.error_code(),
            AppError::Catalog(e) => e.error_code(),
            AppError::Settlement(e) => e.error_code(),
            AppError::Inventory(e) => e.error_code(),
            AppError::Sale(e) => e.error_code(),
            AppError::Return(e) => e.error_code(),
            AppError::Document(e) => e.error_code(),
            AppError::Write(e) => e.error_code(),
            AppError::WriteSet(e) => e.error_code(),
            AppError::Query(_) => ErrorCode::StoreIo,
            AppError::Validation(_, code) => *code,
            AppError::Internal(_) => ErrorCode::Internal,
        }
    }
}
