//! Persistence-facing shapes of the records a `WriteSet` can write. They mirror the local
//! schema (docs/backlog/04-local-database.md) without knowing any SQL.

use pos_domain::{BusinessId, Currency, LocationId, RegisterId, Timestamp};
use serde::Serialize;

/// Aggregate identity used to pair a record with its outbox event (ADR-008 validation).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AggregateRef {
    pub aggregate_type: &'static str,
    pub aggregate_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessRecord {
    pub business_id: BusinessId,
    pub name: String,
    pub legal_name: Option<String>,
    pub tax_registration_no: Option<String>,
    pub address: serde_json::Value,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub logo_path: Option<String>,
    pub country_code: String,
    pub timezone: String,
    pub currency: Currency,
    pub prices_include_tax: bool,
    /// The version this write produces (ADR-011: local edits bump by one).
    pub version: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationRecord {
    pub location_id: LocationId,
    pub business_id: BusinessId,
    pub name: String,
    pub address: serde_json::Value,
    pub archived: bool,
    pub version: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRecord {
    pub register_id: RegisterId,
    pub business_id: BusinessId,
    pub location_id: LocationId,
    pub name: String,
    /// Short code used in receipt numbers, `[A-Z0-9]{1,6}`.
    pub code: String,
    pub receipt_prefix: String,
    pub retired: bool,
    pub version: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingRecord {
    pub business_id: BusinessId,
    pub key: String,
    pub value: serde_json::Value,
    pub version: u64,
    pub updated_at: Timestamp,
}

impl BusinessRecord {
    pub fn aggregate(&self) -> AggregateRef {
        AggregateRef {
            aggregate_type: "business",
            aggregate_id: self.business_id.to_string(),
        }
    }
}
impl LocationRecord {
    pub fn aggregate(&self) -> AggregateRef {
        AggregateRef {
            aggregate_type: "location",
            aggregate_id: self.location_id.to_string(),
        }
    }
}
impl RegisterRecord {
    pub fn aggregate(&self) -> AggregateRef {
        AggregateRef {
            aggregate_type: "register",
            aggregate_id: self.register_id.to_string(),
        }
    }
}
impl SettingRecord {
    pub fn aggregate(&self) -> AggregateRef {
        AggregateRef {
            aggregate_type: "setting",
            aggregate_id: self.key.clone(),
        }
    }
}

/// A staff member as stored and synced (ADR-013: the PIN hash syncs so a cashier can sign
/// in on any register offline; it is an Argon2id PHC string, never the PIN).
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaffRecord {
    pub user_id: pos_domain::UserId,
    pub business_id: BusinessId,
    pub display_name: String,
    pub role: pos_domain::Role,
    pub pin_hash: String,
    pub pin_updated_at: Timestamp,
    pub must_change_pin: bool,
    pub active: bool,
    pub version: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl std::fmt::Debug for StaffRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaffRecord")
            .field("user_id", &self.user_id)
            .field("display_name", &self.display_name)
            .field("role", &self.role)
            .field("pin_hash", &"[REDACTED]")
            .field("active", &self.active)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl StaffRecord {
    pub fn aggregate(&self) -> AggregateRef {
        AggregateRef {
            aggregate_type: "staff_user",
            aggregate_id: self.user_id.to_string(),
        }
    }
}

/// Device-local wrong-PIN counter (never synced).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThrottleRecord {
    pub user_id: pos_domain::UserId,
    pub failed_attempts: u32,
    pub locked_until: Option<Timestamp>,
    pub last_failed_at: Option<Timestamp>,
}

/// Why a staff session ended (`staff_sessions.end_reason`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SessionEnd {
    Logout,
    InactivityLock,
    AppExit,
    SwitchUser,
}

impl SessionEnd {
    pub const fn as_str(self) -> &'static str {
        match self {
            SessionEnd::Logout => "logout",
            SessionEnd::InactivityLock => "inactivity_lock",
            SessionEnd::AppExit => "app_exit",
            SessionEnd::SwitchUser => "switch_user",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxRateRecord {
    pub tax_rate_id: pos_domain::TaxRateId,
    pub business_id: BusinessId,
    pub name: String,
    pub rate: pos_domain::RatePpm,
    pub effective_from: Option<pos_domain::BusinessDate>,
    pub effective_to: Option<pos_domain::BusinessDate>,
    pub archived: bool,
    pub version: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A product category (E06-T05): a tree at most 3 levels deep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRecord {
    pub category_id: pos_domain::CategoryId,
    pub business_id: BusinessId,
    pub parent_id: Option<pos_domain::CategoryId>,
    pub name: String,
    pub sort_order: i64,
    pub archived: bool,
    pub version: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl CategoryRecord {
    pub fn aggregate(&self) -> AggregateRef {
        AggregateRef {
            aggregate_type: "category",
            aggregate_id: self.category_id.to_string(),
        }
    }
}

impl TaxRateRecord {
    pub fn aggregate(&self) -> AggregateRef {
        AggregateRef {
            aggregate_type: "tax_rate",
            aggregate_id: self.tax_rate_id.to_string(),
        }
    }
}

/// A tax category and the rates it applies (one aggregate: rate membership changes bump it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxCategoryRecord {
    pub tax_category_id: pos_domain::TaxCategoryId,
    pub business_id: BusinessId,
    pub name: String,
    pub is_default: bool,
    pub rate_ids: Vec<pos_domain::TaxRateId>,
    pub archived: bool,
    pub version: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl TaxCategoryRecord {
    pub fn aggregate(&self) -> AggregateRef {
        AggregateRef {
            aggregate_type: "tax_category",
            aggregate_id: self.tax_category_id.to_string(),
        }
    }
}

/// Sandbox sales are excluded from reports; live is switched on by the owner (PRD §7.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisterMode {
    Sandbox,
    Live,
}

impl RegisterMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            RegisterMode::Sandbox => "sandbox",
            RegisterMode::Live => "live",
        }
    }
}

/// Which register this installation is (`device_identity`, local only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub installation_id: String,
    pub business_id: BusinessId,
    pub register_id: RegisterId,
    pub mode: RegisterMode,
}

/// Print job states (E13-T06). `Submitted` is never retried automatically: if the printer
/// does not answer, the job becomes `Unknown` and the operator decides.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrintStatus {
    Queued,
    Submitted,
    Completed,
    Failed,
    Unknown,
    Cancelled,
}

impl PrintStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            PrintStatus::Queued => "queued",
            PrintStatus::Submitted => "submitted",
            PrintStatus::Completed => "completed",
            PrintStatus::Failed => "failed",
            PrintStatus::Unknown => "unknown",
            PrintStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => PrintStatus::Queued,
            "submitted" => PrintStatus::Submitted,
            "completed" => PrintStatus::Completed,
            "failed" => PrintStatus::Failed,
            "unknown" => PrintStatus::Unknown,
            "cancelled" => PrintStatus::Cancelled,
            _ => return None,
        })
    }
}

/// A print job (local only, ADR-009).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintJobRecord {
    pub job_id: pos_domain::PrintJobId,
    pub receipt_id: pos_domain::ReceiptId,
    pub adapter: &'static str,
    /// `true`: printed with the REPRINT marker (RCP-007).
    pub reprint: bool,
    pub status: PrintStatus,
    pub attempt_count: u32,
    pub last_error_code: Option<String>,
    pub requested_by: pos_domain::UserId,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---------- sales history (E12-T03) ----------

/// One row of the sales list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaleRow {
    pub sale_id: pos_domain::SaleId,
    pub receipt_number: String,
    pub occurred_at: Timestamp,
    /// "2026-11-07 09:31" in the business time zone at the time of the sale.
    pub local_time: String,
    pub business_date: String,
    pub cashier_name: String,
    pub mode: pos_domain::sale::SaleMode,
    pub currency_exponent: u8,
    pub total: pos_domain::Money,
    pub item_count: pos_domain::Quantity,
    /// Tender kinds used, in tender order, without repeats.
    pub tenders: Vec<pos_domain::settlement::TenderKind>,
    /// Refunded so far by returns against this sale.
    pub refunded: pos_domain::Money,
}

/// Where the next page starts: the last row's sort key (newest first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SalesCursor {
    pub business_date: String,
    pub occurred_at: i64,
    pub sale_id: pos_domain::SaleId,
}

impl SalesCursor {
    /// An opaque token for the UI.
    pub fn encode(&self) -> String {
        format!("{}|{}|{}", self.business_date, self.occurred_at, self.sale_id)
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut it = s.splitn(3, '|');
        let business_date = it.next()?.to_owned();
        let occurred_at = it.next()?.parse().ok()?;
        let sale_id = it.next()?.parse().ok()?;
        business_date.parse::<pos_domain::time::BusinessDate>().ok()?;
        Some(Self {
            business_date,
            occurred_at,
            sale_id,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SalesPage {
    pub rows: Vec<SaleRow>,
    /// `None` when this is the last page.
    pub next: Option<SalesCursor>,
}

/// A completed sale as recorded, with what has been returned since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaleDetail {
    pub sale_id: pos_domain::SaleId,
    pub receipt_number: String,
    pub local_time: String,
    pub business_date: String,
    pub cashier_name: String,
    pub approver_name: Option<String>,
    pub mode: pos_domain::sale::SaleMode,
    pub currency: Currency,
    pub prices_include_tax: bool,
    pub subtotal: pos_domain::Money,
    pub line_discounts: pos_domain::Money,
    pub order_discount: pos_domain::Money,
    pub tax_total: pos_domain::Money,
    pub rounding: pos_domain::Money,
    pub total: pos_domain::Money,
    pub tendered: pos_domain::Money,
    pub change: pos_domain::Money,
    pub tax_breakdown: Vec<SaleTaxRow>,
    pub lines: Vec<SaleDetailLine>,
    pub tenders: Vec<SaleTenderRow>,
    pub returns: Vec<SaleReturnRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaleTaxRow {
    pub name: String,
    pub rate_ppm: u32,
    pub taxable: pos_domain::Money,
    pub tax: pos_domain::Money,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaleDetailLine {
    pub line_no: u32,
    pub name: String,
    pub variant_label: String,
    pub sku: Option<String>,
    pub unit: pos_domain::catalog::Unit,
    pub quantity: pos_domain::Quantity,
    pub unit_price: pos_domain::Money,
    pub price_overridden: bool,
    pub gross: pos_domain::Money,
    /// The line's own discount plus its share of the order discount.
    pub discount: pos_domain::Money,
    pub tax: pos_domain::Money,
    pub total: pos_domain::Money,
    pub returned: pos_domain::Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaleTenderRow {
    pub kind: pos_domain::settlement::TenderKind,
    pub amount: pos_domain::Money,
    pub received: Option<pos_domain::Money>,
    pub change: pos_domain::Money,
    /// `confirmed`, `pending`, ...
    pub status: String,
    /// `operator` (recorded by the cashier) or `provider` (confirmed by a payment integration).
    pub confirmation_source: String,
    pub reference: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaleReturnRow {
    pub return_id: pos_domain::ReturnId,
    pub return_number: String,
    pub local_time: String,
    pub reason: String,
    pub total: pos_domain::Money,
    pub refunds: Vec<pos_domain::settlement::TenderKind>,
}

// ---------- stock projection (E11-T06) ----------

/// A balance that does not equal the sum of its movements (INV-004).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StockDrift {
    pub location_id: pos_domain::LocationId,
    pub variant_id: pos_domain::VariantId,
    /// `sellable` or `damaged`.
    pub bucket: String,
    /// What `stock_balances` says (0 when the row is missing).
    pub recorded: pos_domain::Quantity,
    /// The sum of the movements (0 when there are none).
    pub from_movements: pos_domain::Quantity,
}

/// One stock movement as the history shows it (E11-T05).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StockHistoryRow {
    pub movement_id: pos_domain::MovementId,
    pub occurred_at: Timestamp,
    pub local_time: String,
    /// `sale`, `receipt`, `damage`, `return_restock`...
    pub movement_type: String,
    pub bucket: String,
    pub delta: pos_domain::Quantity,
    /// `sale`, `return` or `inventory_document`.
    pub source_type: String,
    pub source_id: String,
    /// The receipt, return or document number.
    pub source_number: String,
    pub actor_name: String,
}

// ---------- cash sessions (E15-T02) ----------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CashSessionRecord {
    pub id: pos_domain::CashSessionId,
    /// `R01-Z-0001`.
    pub number: String,
    pub open: bool,
    pub opened_by_name: String,
    pub opened_at: Timestamp,
    pub opened_local: String,
    pub opening_float: pos_domain::Money,
    pub business_date: String,
    pub closed_by_name: Option<String>,
    pub closed_local: Option<String>,
    pub expected: Option<pos_domain::Money>,
    pub counted: Option<pos_domain::Money>,
    pub variance: Option<pos_domain::Money>,
    /// Recorded at close.
    pub totals: Option<pos_domain::cash::SessionTotals>,
    pub note: Option<String>,
    pub currency_exponent: u8,
}

/// Where an import job is (E06-T07).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    Parsed,
    Validated,
    Committing,
    Committed,
    Failed,
    Cancelled,
}

impl ImportStatus {
    pub fn code(self) -> &'static str {
        match self {
            ImportStatus::Parsed => "parsed",
            ImportStatus::Validated => "validated",
            ImportStatus::Committing => "committing",
            ImportStatus::Committed => "committed",
            ImportStatus::Failed => "failed",
            ImportStatus::Cancelled => "cancelled",
        }
    }
    pub fn from_code(c: &str) -> Option<Self> {
        [
            ImportStatus::Parsed,
            ImportStatus::Validated,
            ImportStatus::Committing,
            ImportStatus::Committed,
            ImportStatus::Failed,
            ImportStatus::Cancelled,
        ]
        .into_iter()
        .find(|s| s.code() == c)
    }
}

/// What became of one row of an import file.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportRowStatus {
    Pending,
    Valid,
    Error,
    DuplicateInFile,
    DuplicateExisting,
    Skipped,
    Committed,
}

impl ImportRowStatus {
    pub fn code(self) -> &'static str {
        match self {
            ImportRowStatus::Pending => "pending",
            ImportRowStatus::Valid => "valid",
            ImportRowStatus::Error => "error",
            ImportRowStatus::DuplicateInFile => "duplicate_in_file",
            ImportRowStatus::DuplicateExisting => "duplicate_existing",
            ImportRowStatus::Skipped => "skipped",
            ImportRowStatus::Committed => "committed",
        }
    }
    pub fn from_code(c: &str) -> Option<Self> {
        [
            ImportRowStatus::Pending,
            ImportRowStatus::Valid,
            ImportRowStatus::Error,
            ImportRowStatus::DuplicateInFile,
            ImportRowStatus::DuplicateExisting,
            ImportRowStatus::Skipped,
            ImportRowStatus::Committed,
        ]
        .into_iter()
        .find(|s| s.code() == c)
    }
    /// Not imported because of a problem the report should list.
    pub fn is_problem(self) -> bool {
        matches!(
            self,
            ImportRowStatus::Error | ImportRowStatus::DuplicateInFile | ImportRowStatus::DuplicateExisting
        )
    }
}

/// An import job (`import_jobs`).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportJobRecord {
    pub job_id: pos_domain::ImportJobId,
    pub file_name: String,
    pub file_sha256: String,
    /// Column → file column index, once chosen.
    pub mapping: Option<serde_json::Value>,
    /// The header, encoding, delimiter and duplicate policy.
    pub options: serde_json::Value,
    pub status: ImportStatus,
    pub total_rows: u32,
    pub valid_rows: u32,
    pub error_rows: u32,
    pub created_by: pos_domain::UserId,
    pub created_at: Timestamp,
    pub committed_at: Option<Timestamp>,
}

/// One row of an import file (`import_rows`). `line` is where it starts in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportRowRecord {
    pub line: u32,
    pub raw: Vec<String>,
    pub parsed: Option<serde_json::Value>,
    pub status: ImportRowStatus,
    pub errors: Vec<pos_domain::catalog::import::RowError>,
}
