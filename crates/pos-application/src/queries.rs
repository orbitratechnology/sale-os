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

/// A sellable variant found by barcode, SKU or search (BAR-004, POS-002).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogHit {
    pub product_id: pos_domain::ProductId,
    pub variant_id: pos_domain::VariantId,
    pub name: String,
    pub label: String,
    pub sku: Option<String>,
    pub price: pos_domain::Money,
    pub unit: pos_domain::catalog::Unit,
    pub track_stock: bool,
    /// Quantity one scan adds: the pack quantity for a pack barcode, else the scan unit.
    pub scan_qty: pos_domain::Quantity,
    pub matched_by: MatchedBy,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MatchedBy {
    Barcode,
    Sku,
    Search,
}

#[async_trait::async_trait]
pub trait CatalogQueries: Send + Sync {
    async fn product(
        &self,
        id: pos_domain::ProductId,
    ) -> Result<Option<pos_domain::catalog::Product>, QueryError>;
    /// Owner of an active barcode (canonical value), if any.
    async fn barcode_owner(&self, value: &str) -> Result<Option<pos_domain::ProductId>, QueryError>;
    /// Owner of an active SKU (case-insensitive), if any.
    async fn sku_owner(&self, sku: &str) -> Result<Option<pos_domain::ProductId>, QueryError>;
    /// Barcode first, then SKU (`catalog_lookup_code`). Active products only.
    async fn lookup_code(&self, raw: &str) -> Result<Option<CatalogHit>, QueryError>;
    /// Name / SKU / barcode search with prefix matching, best matches first.
    async fn search(&self, query: &str, limit: u32) -> Result<Vec<CatalogHit>, QueryError>;
}
