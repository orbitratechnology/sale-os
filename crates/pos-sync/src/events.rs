//! Every event's payload, by type and schema version (07 §4). Field names are exactly what
//! the desktop writes: camelCase, except a few nested snake_case names that come from domain
//! types serialized as they are (`tax_rate_id`, `external_reference`, `authorized_by`, the
//! movement `source` fields). They are part of the v1 contract; renaming them is a new
//! schema version.
//!
//! Units: amounts are integer minor units, quantities milli-units, rates ppm, timestamps
//! epoch milliseconds, business dates `YYYY-MM-DD`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Every (event type, schema version) the contract defines.
pub const CATALOGUE: &[(&str, u32)] = &[
    ("business.updated", 1),
    ("location.upserted", 1),
    ("register.upserted", 1),
    ("setting.updated", 1),
    ("staff.upserted", 1),
    ("tax_rate.upserted", 1),
    ("tax_category.upserted", 1),
    ("category.upserted", 1),
    ("product.upserted", 1),
    ("sale.completed", 1),
    ("return.completed", 1),
    ("inventory.document_posted", 1),
    ("cash_session.opened", 1),
    ("cash_session.closed", 1),
    ("audit.recorded", 1),
];

/// Master-data events: versioned aggregates whose edits can conflict (ADR-011).
pub fn is_master(event_type: &str) -> bool {
    matches!(
        event_type,
        "business.updated"
            | "location.upserted"
            | "register.upserted"
            | "setting.updated"
            | "staff.upserted"
            | "tax_rate.upserted"
            | "tax_category.upserted"
            | "category.upserted"
            | "product.upserted"
    )
}

// ---------- shared pieces ----------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Currency {
    pub code: String,
    pub exponent: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Ea,
    Kg,
    G,
    L,
    Ml,
    M,
    Cm,
    Box,
    Pack,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProductType {
    Stock,
    NonStock,
    Service,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Manager,
    Cashier,
}

// ---------- master data ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessUpdatedV1 {
    pub business_id: Uuid,
    pub name: String,
    pub legal_name: Option<String>,
    pub tax_registration_no: Option<String>,
    /// `{ "lines": [..] }`, plus anything later versions add.
    pub address: serde_json::Value,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub logo_path: Option<String>,
    pub country_code: String,
    pub timezone: String,
    pub currency: Currency,
    pub prices_include_tax: bool,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocationUpsertedV1 {
    pub location_id: Uuid,
    pub business_id: Uuid,
    pub name: String,
    pub address: serde_json::Value,
    pub archived: bool,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterUpsertedV1 {
    pub register_id: Uuid,
    pub business_id: Uuid,
    pub location_id: Uuid,
    pub name: String,
    pub code: String,
    pub receipt_prefix: String,
    pub retired: bool,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingUpdatedV1 {
    pub business_id: Uuid,
    pub key: String,
    pub value: serde_json::Value,
    pub version: u64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StaffUpsertedV1 {
    pub user_id: Uuid,
    pub business_id: Uuid,
    pub display_name: String,
    pub role: Role,
    /// Argon2id PHC string; the PIN itself never leaves the device.
    pub pin_hash: String,
    pub pin_updated_at: i64,
    pub must_change_pin: bool,
    pub active: bool,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaxRateUpsertedV1 {
    pub tax_rate_id: Uuid,
    pub business_id: Uuid,
    pub name: String,
    /// Parts per million (200000 = 20%).
    pub rate: u32,
    pub effective_from: Option<String>,
    pub effective_to: Option<String>,
    pub archived: bool,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaxCategoryUpsertedV1 {
    pub tax_category_id: Uuid,
    pub business_id: Uuid,
    pub name: String,
    pub is_default: bool,
    pub rate_ids: Vec<Uuid>,
    pub archived: bool,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CategoryUpsertedV1 {
    pub category_id: Uuid,
    pub business_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub sort_order: i64,
    pub archived: bool,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Symbology {
    Ean13,
    Ean8,
    UpcA,
    UpcE,
    Code128,
    Code39,
    Itf14,
    Qr,
    DataMatrix,
    Internal,
    Unknown,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BarcodeQualifier {
    Primary,
    Alias,
    Pack,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BarcodeV1 {
    pub id: Uuid,
    /// Normalized: the lookup key, unique among active products.
    pub value: String,
    pub raw: String,
    pub symbology: Symbology,
    pub qualifier: BarcodeQualifier,
    pub pack_qty: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VariantV1 {
    pub id: Uuid,
    pub is_default: bool,
    pub sku: Option<String>,
    pub options: BTreeMap<String, String>,
    pub price: i64,
    pub cost: Option<i64>,
    pub track_stock: bool,
    pub low_stock_threshold: Option<i64>,
    pub scan_qty: i64,
    pub barcodes: Vec<BarcodeV1>,
    pub archived: bool,
}

/// The whole product aggregate. An archive is an upsert with `archived: true`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProductUpsertedV1 {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub category_id: Option<Uuid>,
    pub product_type: ProductType,
    pub unit: Unit,
    pub tax_category_id: Uuid,
    pub quick_select: bool,
    pub archived: bool,
    pub variants: Vec<VariantV1>,
    pub version: u64,
}

// ---------- facts ----------

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StockBucket {
    Sellable,
    Damaged,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MovementType {
    Opening,
    Receipt,
    Sale,
    ReturnRestock,
    ReturnDamaged,
    Damage,
    Loss,
    Adjustment,
    WriteOff,
    Correction,
}

/// What caused a movement (field names are snake_case on the wire).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MovementSource {
    Sale { sale_id: Uuid, line_id: Uuid },
    Return { return_id: Uuid, line_id: Uuid },
    InventoryDocument { document_id: Uuid },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StockMovementV1 {
    pub id: Uuid,
    pub variant_id: Uuid,
    pub location_id: Uuid,
    pub bucket: StockBucket,
    pub movement_type: MovementType,
    /// Signed milli-units.
    pub delta: i64,
    pub unit_cost: Option<i64>,
    pub source: MovementSource,
    pub actor: Uuid,
    pub occurred_at: i64,
    pub business_date: String,
}

/// Field names are snake_case on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaxLineV1 {
    pub tax_rate_id: Uuid,
    pub name: String,
    pub rate: u32,
    pub taxable: i64,
    pub tax: i64,
}

/// Externally tagged: `{"Percent": ppm}` or `{"Fixed": minor}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DiscountValue {
    Percent(u32),
    Fixed(i64),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscountV1 {
    pub value: DiscountValue,
    pub reason: Option<String>,
    pub authorized_by: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaleLineV1 {
    pub id: Uuid,
    pub line_no: u32,
    pub product_id: Uuid,
    pub variant_id: Uuid,
    pub name: String,
    pub variant_label: String,
    pub sku: Option<String>,
    pub product_type: ProductType,
    pub unit: Unit,
    pub track_stock: bool,
    pub quantity: i64,
    pub unit_price: i64,
    pub unit_cost: Option<i64>,
    pub price_overridden: bool,
    pub gross: i64,
    pub discount: Option<DiscountV1>,
    pub line_discount: i64,
    pub order_discount_alloc: i64,
    pub tax: i64,
    pub tax_lines: Vec<TaxLineV1>,
    pub total: i64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TenderKind {
    Cash,
    CardExternal,
    BankTransferExternal,
    QrExternal,
    OtherExternal,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TenderStatus {
    Pending,
    Confirmed,
    Declined,
    Reversed,
    Refunded,
    ReconciliationRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderApprovalV1 {
    pub reference: String,
}

/// `"operator"` (recorded by the cashier, PAY-003) or `{"provider": {...}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    Operator,
    Provider(ProviderApprovalV1),
}

/// A sale tender or a return refund. Field names are snake_case on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TenderV1 {
    pub id: Uuid,
    pub seq: u32,
    pub kind: TenderKind,
    /// Applied to the sale (cash: handed over less change).
    pub amount: i64,
    pub received: Option<i64>,
    pub change: i64,
    pub external_reference: Option<String>,
    pub status: TenderStatus,
    pub confirmation: Confirmation,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SaleMode {
    Sandbox,
    Live,
}

/// The whole sale, self-contained: the server never looks up current prices (PRD §8.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaleCompletedV1 {
    pub id: Uuid,
    pub cart_id: Uuid,
    pub register_id: Uuid,
    pub location_id: Uuid,
    pub cashier: Uuid,
    pub approver: Option<Uuid>,
    pub mode: SaleMode,
    pub receipt_seq: u64,
    pub receipt_number: String,
    pub occurred_at: i64,
    pub business_date: String,
    pub timezone: String,
    pub currency: Currency,
    pub prices_include_tax: bool,
    pub lines: Vec<SaleLineV1>,
    pub order_discount: Option<DiscountV1>,
    pub subtotal: i64,
    pub line_discount_total: i64,
    pub order_discount_total: i64,
    pub tax_total: i64,
    pub tax_breakdown: Vec<TaxLineV1>,
    pub rounding: i64,
    pub total: i64,
    pub tendered_total: i64,
    pub change: i64,
    pub tenders: Vec<TenderV1>,
    pub stock_movements: Vec<StockMovementV1>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReturnReason {
    ChangedMind,
    Defective,
    WrongItem,
    DamagedInTransit,
    Other,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Restock,
    Damaged,
    NoStockChange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReturnLineV1 {
    pub id: Uuid,
    pub sale_line_id: Uuid,
    pub product_id: Uuid,
    pub variant_id: Uuid,
    pub name: String,
    pub variant_label: String,
    pub unit: Unit,
    pub track_stock: bool,
    pub quantity: i64,
    pub disposition: Disposition,
    pub refund: i64,
    pub tax: i64,
    pub unit_cost: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReturnCompletedV1 {
    pub id: Uuid,
    pub original_sale: Uuid,
    pub original_receipt_number: String,
    pub register_id: Uuid,
    pub location_id: Uuid,
    pub actor: Uuid,
    pub approver: Option<Uuid>,
    pub receipt_seq: u64,
    pub receipt_number: String,
    pub reason: ReturnReason,
    pub note: Option<String>,
    pub occurred_at: i64,
    pub business_date: String,
    pub timezone: String,
    pub currency: Currency,
    pub lines: Vec<ReturnLineV1>,
    pub subtotal: i64,
    pub tax: i64,
    pub total: i64,
    pub refunds: Vec<TenderV1>,
    pub stock_movements: Vec<StockMovementV1>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    Opening,
    Receipt,
    Adjustment,
    WriteOff,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdjustReason {
    Damage,
    Loss,
    Theft,
    CountCorrection,
    Expired,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentLineV1 {
    pub variant_id: Uuid,
    pub bucket: StockBucket,
    pub movement_type: MovementType,
    pub delta: i64,
    pub unit_cost: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentPostedV1 {
    pub id: Uuid,
    pub doc_type: DocumentType,
    pub document_seq: u64,
    pub document_no: String,
    pub register_id: Uuid,
    pub location_id: Uuid,
    pub actor: Uuid,
    pub reason: Option<AdjustReason>,
    pub supplier_name: Option<String>,
    pub reference: Option<String>,
    pub note: Option<String>,
    pub occurred_at: i64,
    pub business_date: String,
    pub movements: Vec<DocumentLineV1>,
    pub stock_movements: Vec<StockMovementV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenedSessionV1 {
    pub id: Uuid,
    pub opened_by: Uuid,
    pub opened_at: i64,
    pub business_date: String,
    pub opening_float: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CashSessionOpenedV1 {
    pub register_id: Uuid,
    pub location_id: Uuid,
    pub session: OpenedSessionV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KindTotalV1 {
    pub kind: TenderKind,
    pub count: u32,
    pub amount: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTotalsV1 {
    pub sales_count: u32,
    pub sales_total: i64,
    pub tax_total: i64,
    pub discount_total: i64,
    pub rounding_total: i64,
    pub returns_count: u32,
    pub refunds_total: i64,
    pub tenders: Vec<KindTotalV1>,
    pub refunds: Vec<KindTotalV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClosedSessionV1 {
    pub id: Uuid,
    pub closed_by: Uuid,
    pub closed_at: i64,
    pub opening_float: i64,
    pub totals: SessionTotalsV1,
    pub expected: i64,
    pub counted: i64,
    pub variance: i64,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CashSessionClosedV1 {
    pub register_id: Uuid,
    pub session_number: String,
    pub session: ClosedSessionV1,
}

/// A standalone audit entry (login failures, lockouts, approvals, reprints, exports...).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditRecordedV1 {
    pub audit_id: Uuid,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub metadata: serde_json::Value,
}

// ---------- the catalogue ----------

/// A parsed payload.
#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    BusinessUpdatedV1(BusinessUpdatedV1),
    LocationUpsertedV1(LocationUpsertedV1),
    RegisterUpsertedV1(RegisterUpsertedV1),
    SettingUpdatedV1(SettingUpdatedV1),
    StaffUpsertedV1(StaffUpsertedV1),
    TaxRateUpsertedV1(TaxRateUpsertedV1),
    TaxCategoryUpsertedV1(TaxCategoryUpsertedV1),
    CategoryUpsertedV1(CategoryUpsertedV1),
    ProductUpsertedV1(ProductUpsertedV1),
    SaleCompletedV1(Box<SaleCompletedV1>),
    ReturnCompletedV1(Box<ReturnCompletedV1>),
    DocumentPostedV1(Box<DocumentPostedV1>),
    CashSessionOpenedV1(CashSessionOpenedV1),
    CashSessionClosedV1(CashSessionClosedV1),
    AuditRecordedV1(AuditRecordedV1),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    #[error("unknown event type {0:?}")]
    UnknownType(String),
    #[error("{event_type} v{version} is newer than this build supports (v{supported})")]
    UnsupportedVersion {
        event_type: String,
        version: u32,
        supported: u32,
    },
    #[error("payload does not match {event_type} v{version}: {detail}")]
    Invalid {
        event_type: String,
        version: u32,
        detail: String,
    },
}

/// The newest version of `event_type` this build knows, if any.
pub fn current_version(event_type: &str) -> Option<u32> {
    CATALOGUE
        .iter()
        .filter(|(t, _)| *t == event_type)
        .map(|(_, v)| *v)
        .max()
}

/// Parses a payload by type and version, after upcasting older versions (`upcast`).
pub fn parse_payload(
    event_type: &str,
    version: u32,
    payload: &serde_json::Value,
) -> Result<Payload, PayloadError> {
    let supported =
        current_version(event_type).ok_or_else(|| PayloadError::UnknownType(event_type.to_owned()))?;
    if version > supported {
        return Err(PayloadError::UnsupportedVersion {
            event_type: event_type.to_owned(),
            version,
            supported,
        });
    }
    let (version, value) = crate::upcast::to_current(event_type, version, payload.clone());
    let invalid = |e: serde_json::Error| PayloadError::Invalid {
        event_type: event_type.to_owned(),
        version,
        detail: e.to_string(),
    };
    fn p<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, serde_json::Error> {
        serde_json::from_value(v)
    }
    Ok(match (event_type, version) {
        ("business.updated", 1) => Payload::BusinessUpdatedV1(p(value).map_err(invalid)?),
        ("location.upserted", 1) => Payload::LocationUpsertedV1(p(value).map_err(invalid)?),
        ("register.upserted", 1) => Payload::RegisterUpsertedV1(p(value).map_err(invalid)?),
        ("setting.updated", 1) => Payload::SettingUpdatedV1(p(value).map_err(invalid)?),
        ("staff.upserted", 1) => Payload::StaffUpsertedV1(p(value).map_err(invalid)?),
        ("tax_rate.upserted", 1) => Payload::TaxRateUpsertedV1(p(value).map_err(invalid)?),
        ("tax_category.upserted", 1) => Payload::TaxCategoryUpsertedV1(p(value).map_err(invalid)?),
        ("category.upserted", 1) => Payload::CategoryUpsertedV1(p(value).map_err(invalid)?),
        ("product.upserted", 1) => Payload::ProductUpsertedV1(p(value).map_err(invalid)?),
        ("sale.completed", 1) => Payload::SaleCompletedV1(Box::new(p(value).map_err(invalid)?)),
        ("return.completed", 1) => Payload::ReturnCompletedV1(Box::new(p(value).map_err(invalid)?)),
        ("inventory.document_posted", 1) => Payload::DocumentPostedV1(Box::new(p(value).map_err(invalid)?)),
        ("cash_session.opened", 1) => Payload::CashSessionOpenedV1(p(value).map_err(invalid)?),
        ("cash_session.closed", 1) => Payload::CashSessionClosedV1(p(value).map_err(invalid)?),
        ("audit.recorded", 1) => Payload::AuditRecordedV1(p(value).map_err(invalid)?),
        _ => {
            return Err(PayloadError::UnsupportedVersion {
                event_type: event_type.to_owned(),
                version,
                supported,
            });
        }
    })
}

/// The JSON Schema of one catalogue entry.
pub fn json_schema(event_type: &str, version: u32) -> Option<serde_json::Value> {
    use schemars::schema_for;
    let s = match (event_type, version) {
        ("business.updated", 1) => schema_for!(BusinessUpdatedV1),
        ("location.upserted", 1) => schema_for!(LocationUpsertedV1),
        ("register.upserted", 1) => schema_for!(RegisterUpsertedV1),
        ("setting.updated", 1) => schema_for!(SettingUpdatedV1),
        ("staff.upserted", 1) => schema_for!(StaffUpsertedV1),
        ("tax_rate.upserted", 1) => schema_for!(TaxRateUpsertedV1),
        ("tax_category.upserted", 1) => schema_for!(TaxCategoryUpsertedV1),
        ("category.upserted", 1) => schema_for!(CategoryUpsertedV1),
        ("product.upserted", 1) => schema_for!(ProductUpsertedV1),
        ("sale.completed", 1) => schema_for!(SaleCompletedV1),
        ("return.completed", 1) => schema_for!(ReturnCompletedV1),
        ("inventory.document_posted", 1) => schema_for!(DocumentPostedV1),
        ("cash_session.opened", 1) => schema_for!(CashSessionOpenedV1),
        ("cash_session.closed", 1) => schema_for!(CashSessionClosedV1),
        ("audit.recorded", 1) => schema_for!(AuditRecordedV1),
        _ => return None,
    };
    let mut v = s.to_value();
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "title".into(),
            serde_json::Value::String(format!("{event_type} v{version}")),
        );
        o.insert("x-schema-version".into(), serde_json::json!(version));
    }
    Some(v)
}

/// Schemas of the envelope and the push protocol.
pub fn protocol_schemas() -> Vec<(&'static str, serde_json::Value)> {
    use schemars::schema_for;
    vec![
        ("envelope", schema_for!(crate::EventEnvelope).to_value()),
        ("push-request", schema_for!(crate::PushRequest).to_value()),
        ("push-response", schema_for!(crate::PushResponse).to_value()),
        ("ack-response", schema_for!(crate::AckResponse).to_value()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogue_entry_has_a_schema_and_a_parser() {
        for (t, v) in CATALOGUE {
            assert!(json_schema(t, *v).is_some(), "{t} v{v}");
            // An empty object is rejected for a reason other than "unknown".
            match parse_payload(t, *v, &serde_json::json!({})) {
                Err(PayloadError::Invalid { .. }) => {}
                other => panic!("{t} v{v}: {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_types_and_newer_versions_are_refused_not_dropped() {
        assert_eq!(
            parse_payload("sale.teleported", 1, &serde_json::json!({})),
            Err(PayloadError::UnknownType("sale.teleported".into()))
        );
        assert!(matches!(
            parse_payload("sale.completed", 2, &serde_json::json!({})),
            Err(PayloadError::UnsupportedVersion {
                version: 2,
                supported: 1,
                ..
            })
        ));
    }

    #[test]
    fn unknown_fields_are_refused() {
        let ok = serde_json::json!({"auditId": "0190a5b0-0000-7000-8000-000000000001", "action": "x.y",
            "entityType": "e", "entityId": "1", "metadata": {}});
        assert!(parse_payload("audit.recorded", 1, &ok).is_ok());
        let mut extra = ok.clone();
        extra["surprise"] = serde_json::json!(1);
        assert!(matches!(
            parse_payload("audit.recorded", 1, &extra),
            Err(PayloadError::Invalid { .. })
        ));
    }
}
