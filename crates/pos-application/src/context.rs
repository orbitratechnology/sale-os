//! Who is acting, where, and the ports every use case needs (docs/backlog/06 §9).

use crate::WriteContext;
use crate::write::{WriteError, WriteSetError};
use pos_domain::staff::StaffError;
use pos_domain::{
    BusinessId, Clock, ErrorCode, HasErrorCode, IdGenerator, LocationId, Permission, RegisterId, Role,
    SessionId, Timestamp, UserId, role_has,
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

#[derive(Clone)]
pub struct CommandContext {
    pub register: RegisterContext,
    pub actor: Option<Actor>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
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
    Write(#[from] WriteError),
    #[error(transparent)]
    WriteSet(#[from] WriteSetError),
    #[error("could not read local data: {0}")]
    Query(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl HasErrorCode for AppError {
    fn error_code(&self) -> ErrorCode {
        match self {
            AppError::SessionLocked => ErrorCode::SessionLocked,
            AppError::PermissionDenied(_) => ErrorCode::PermissionDenied,
            AppError::PinInvalid { .. } => ErrorCode::AuthPinInvalid,
            AppError::LockedOut { .. } => ErrorCode::AuthLockedOut,
            AppError::Staff(e) => e.error_code(),
            AppError::Setup(e) => e.error_code(),
            AppError::Catalog(e) => e.error_code(),
            AppError::Write(e) => e.error_code(),
            AppError::WriteSet(e) => e.error_code(),
            AppError::Query(_) => ErrorCode::StoreIo,
            AppError::Internal(_) => ErrorCode::Internal,
        }
    }
}
