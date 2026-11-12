//! The event envelope (PRD §11.3) and the push protocol (07 §3, "Push contract").

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Every event, whatever its type. `payload` is checked against the catalogue
/// (`events::parse_payload`) by its `eventType` and `schemaVersion`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventEnvelope {
    pub event_id: Uuid,
    pub business_id: Uuid,
    pub location_id: Uuid,
    pub register_id: Uuid,
    /// Gapless per register, from 1 (ADR-010).
    pub local_sequence: u64,
    pub event_type: String,
    pub schema_version: u32,
    pub aggregate_type: String,
    pub aggregate_id: String,
    /// Master data only (ADR-011): the version the edit started from (0 for a create).
    #[serde(default)]
    pub base_version: Option<u64>,
    /// RFC 3339, UTC.
    pub occurred_at: String,
    pub payload: serde_json::Value,
}

/// `POST /v1/sync/events`: at most 200 events or 2 MB.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PushRequest {
    /// Must equal the token's register (else 403).
    pub register_id: Uuid,
    pub events: Vec<EventEnvelope>,
}

pub const MAX_PUSH_EVENTS: usize = 200;
pub const MAX_PUSH_BYTES: usize = 2 * 1024 * 1024;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Applied,
    /// Already applied earlier: the original outcome is returned again.
    Duplicate,
    /// A master-data edit based on an old version: nothing changed on the server.
    Conflict,
    /// Permanently refused (`code`); the client keeps the event in its repair queue.
    Rejected,
    /// Not the next expected sequence (`expectedSequence`): the batch stopped here.
    OutOfOrder,
    /// Could not be applied now (`code`, e.g. ORIGINAL_SALE_PENDING); retry later.
    Transient,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventResult {
    pub event_id: Uuid,
    pub local_sequence: u64,
    pub status: ResultStatus,
    /// Position in the server's change log (applied and duplicate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_snapshot: Option<serde_json::Value>,
    /// conflict: why (e.g. `VERSION`, `BARCODE_TAKEN`); rejected/transient: the code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sequence: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PushResponse {
    /// The highest local sequence the server has handled for this register. The client
    /// never moves its own ack past this (E17-T03).
    pub ack_sequence: u64,
    /// RFC 3339, for the client's clock-skew check (ADR-006).
    pub server_time: String,
    pub results: Vec<EventResult>,
}
