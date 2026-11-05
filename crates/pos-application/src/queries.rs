//! Read ports implemented by `pos-local-db` (docs/backlog/06 §9).

use crate::records::{StaffRecord, ThrottleRecord};
use pos_domain::UserId;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct QueryError(pub String);

impl From<QueryError> for crate::context::AppError {
    fn from(e: QueryError) -> Self {
        crate::context::AppError::Query(e.0)
    }
}

#[async_trait::async_trait]
pub trait StaffQueries: Send + Sync {
    async fn staff(&self, user_id: UserId) -> Result<Option<StaffRecord>, QueryError>;
    /// Active staff, owners first, then by name.
    async fn active_staff(&self) -> Result<Vec<StaffRecord>, QueryError>;
    async fn throttle(&self, user_id: UserId) -> Result<Option<ThrottleRecord>, QueryError>;
}

#[async_trait::async_trait]
pub trait SetupQueries: Send + Sync {
    /// `None` until setup has completed on this installation.
    async fn device_identity(&self) -> Result<Option<crate::records::DeviceIdentity>, QueryError>;
}
