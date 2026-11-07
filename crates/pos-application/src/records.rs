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
