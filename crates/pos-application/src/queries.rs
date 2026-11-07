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
    /// Everyone, including deactivated staff, active first, for staff management.
    async fn all_staff(&self) -> Result<Vec<StaffRecord>, QueryError>;
    async fn throttle(&self, user_id: UserId) -> Result<Option<ThrottleRecord>, QueryError>;
}

#[async_trait::async_trait]
pub trait SetupQueries: Send + Sync {
    /// `None` until setup has completed on this installation.
    async fn device_identity(&self) -> Result<Option<crate::records::DeviceIdentity>, QueryError>;
    /// The register this installation is bound to, `None` before setup.
    async fn register_context(&self) -> Result<Option<crate::context::RegisterContext>, QueryError>;
    /// What the shell shows before sign-in (`app_bootstrap_state`), `None` before setup.
    async fn workspace_summary(&self) -> Result<Option<WorkspaceSummary>, QueryError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub business_name: String,
    pub register_name: String,
    pub register_code: String,
    pub currency_code: String,
    /// Minor-unit digits of the currency (ISO 4217), for parsing typed prices.
    pub currency_exponent: u8,
    pub mode: crate::records::RegisterMode,
}

/// A tax category with its active rates, for the product editor (`tax_list`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxCategorySummary {
    pub tax_category_id: pos_domain::TaxCategoryId,
    pub name: String,
    pub is_default: bool,
    /// `(name, rate)` of the linked active rates, by name.
    pub rates: Vec<(String, pos_domain::RatePpm)>,
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
    /// Active tax categories, default first, then by name.
    async fn tax_categories(&self) -> Result<Vec<TaxCategorySummary>, QueryError>;
    /// One page of the product list (E06-T06a), by name.
    async fn list_products(
        &self,
        filter: &ProductFilter,
        offset: u32,
        limit: u32,
    ) -> Result<ProductPage, QueryError>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProductStatusFilter {
    #[default]
    Active,
    Archived,
    All,
}

/// Product list filters. Empty `query` matches everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProductFilter {
    /// Name, SKU or barcode (the search index).
    pub query: String,
    pub status: ProductStatusFilter,
    /// `Some(true)`: only products with a stock-tracked variant.
    pub track_stock: Option<bool>,
    pub category_id: Option<pos_domain::CategoryId>,
}

/// A product list row: one per product, summarising its active variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductRow {
    pub product_id: pos_domain::ProductId,
    pub name: String,
    pub archived: bool,
    pub version: u64,
    pub variant_count: u32,
    /// The default variant's SKU.
    pub sku: Option<String>,
    pub price_min: pos_domain::Money,
    pub price_max: pos_domain::Money,
    pub barcode_count: u32,
    pub track_stock: bool,
    pub quick_select: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductPage {
    pub rows: Vec<ProductRow>,
    /// Rows matching the filter, across all pages.
    pub total: u32,
}

/// A cart line as stored: the variant is re-read from the catalogue on load, so a recovered
/// cart is priced against the current catalogue (POS-011).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCartLine {
    pub line_id: pos_domain::CartLineId,
    pub variant_id: pos_domain::VariantId,
    pub quantity: pos_domain::Quantity,
    pub price_override: Option<pos_domain::Money>,
    pub line_discount: Option<pos_domain::pricing::Discount>,
    pub added_via: pos_domain::cart::AddVia,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCart {
    pub cart_id: pos_domain::CartId,
    pub revision: u64,
    pub opened_by: pos_domain::UserId,
    pub order_discount: Option<pos_domain::pricing::Discount>,
    pub lines: Vec<StoredCartLine>,
}

/// How this business prices (from `businesses`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricingSettings {
    pub tax_mode: pos_domain::pricing::TaxMode,
    pub currency: pos_domain::Currency,
    pub timezone: String,
    /// `discount.cashier_max_ppm`: above this, a discount needs `DiscountAboveThreshold`
    /// (E08-T02). Default 10%.
    pub discount_limit_ppm: u32,
}

#[async_trait::async_trait]
pub trait CartQueries: Send + Sync {
    /// The register's open cart, if any (at most one, enforced by a unique index).
    async fn open_cart(&self, register: pos_domain::RegisterId) -> Result<Option<StoredCart>, QueryError>;
    /// A variant ready for the cart, with its tax rates resolved for `date`. Returned even
    /// if since archived, so a recovered cart can still show (and remove) it.
    async fn sellable(
        &self,
        variant: pos_domain::VariantId,
        date: pos_domain::BusinessDate,
    ) -> Result<Option<pos_domain::cart::SellableVariant>, QueryError>;
    async fn pricing_settings(&self) -> Result<PricingSettings, QueryError>;
}

/// Stock reads for selling (E11-T04).
#[async_trait::async_trait]
pub trait StockQueries: Send + Sync {
    /// `inventory.allow_negative`: true (the default) sells anyway and flags it.
    async fn negative_stock_policy(&self) -> Result<pos_domain::inventory::NegativeStockPolicy, QueryError>;
    /// Sellable on-hand quantity per variant at `location` (missing = never moved = 0).
    async fn on_hand(
        &self,
        location: pos_domain::LocationId,
        variants: &[pos_domain::VariantId],
    ) -> Result<std::collections::BTreeMap<pos_domain::VariantId, pos_domain::Quantity>, QueryError>;
}

/// What a receipt says about the shop, the till and who served (E13-T02).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptInfo {
    pub business_name: String,
    pub address_lines: Vec<String>,
    pub phone: Option<String>,
    pub tax_registration_no: Option<String>,
    /// `receipt.header_lines` / `receipt.footer_lines` settings.
    pub header_lines: Vec<String>,
    pub footer_lines: Vec<String>,
    pub register_name: String,
    pub register_code: String,
    pub cashier_name: String,
    /// `receipt.paper_width_mm`: 58 or 80.
    pub paper_width_mm: u32,
    /// `receipt.auto_print`: print as soon as a sale completes.
    pub auto_print: bool,
}

#[async_trait::async_trait]
pub trait ReceiptQueries: Send + Sync {
    async fn receipt_info(
        &self,
        register: pos_domain::RegisterId,
        cashier: pos_domain::UserId,
    ) -> Result<ReceiptInfo, QueryError>;
    /// The stored document of a sale's receipt, as JSON text.
    async fn sale_receipt(
        &self,
        sale: pos_domain::SaleId,
    ) -> Result<Option<(pos_domain::ReceiptId, String)>, QueryError>;
    /// The stored document of a return's refund receipt, as JSON text.
    async fn return_receipt(
        &self,
        ret: pos_domain::ReturnId,
    ) -> Result<Option<(pos_domain::ReceiptId, String)>, QueryError>;
    /// The most recent sale on this register, for "last receipt".
    async fn last_sale(
        &self,
        register: pos_domain::RegisterId,
    ) -> Result<Option<pos_domain::SaleId>, QueryError>;
}

/// Print queue reads (E13-T06).
#[async_trait::async_trait]
pub trait PrintQueries: Send + Sync {
    async fn print_job(
        &self,
        job: pos_domain::PrintJobId,
    ) -> Result<Option<crate::records::PrintJobRecord>, QueryError>;
    /// Jobs for a receipt, oldest first.
    async fn receipt_jobs(
        &self,
        receipt: pos_domain::ReceiptId,
    ) -> Result<Vec<crate::records::PrintJobRecord>, QueryError>;
    /// A receipt's stored document (JSON text).
    async fn receipt_document(&self, receipt: pos_domain::ReceiptId) -> Result<Option<String>, QueryError>;
}

/// Reads for returns (E12).
#[async_trait::async_trait]
pub trait ReturnQueries: Send + Sync {
    /// A sale with, per line, what was sold and what has already been returned.
    async fn returnable_sale(
        &self,
        sale: pos_domain::SaleId,
    ) -> Result<Option<pos_domain::returns::ReturnableSale>, QueryError>;
    /// A sale by its receipt number (typed or scanned from the receipt's barcode).
    async fn sale_by_receipt(&self, receipt_number: &str) -> Result<Option<pos_domain::SaleId>, QueryError>;
    /// `returns.cashier_requires_approval` (default true).
    async fn cashier_returns_need_approval(&self) -> Result<bool, QueryError>;
}
