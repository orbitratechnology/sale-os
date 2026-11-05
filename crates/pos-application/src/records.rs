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
