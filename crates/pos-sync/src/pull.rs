//! Pulling from the cloud (E17-T07, SYN-002/008): the change feed (`GET /v1/sync/changes`)
//! and the bootstrap snapshot (`GET /v1/sync/bootstrap`).
//!
//! A cursor is the opaque string `"v1:<server_seq>"`. A bootstrap page token is opaque too;
//! the client stores it to resume an interrupted bootstrap, and the first page's
//! `snapshotCursor` is where the pull continues once the snapshot is complete.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Upsert,
    Archive,
    Fact,
}

/// One row of the change feed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    pub server_seq: i64,
    pub aggregate_type: String,
    pub aggregate_id: String,
    /// Master data: the aggregate's version after this change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    pub kind: ChangeKind,
    /// The register that made the change; absent when it was made in the cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_register_id: Option<Uuid>,
    /// The event type and version `snapshot` follows (parse it with `parse_payload`).
    pub event_type: String,
    pub schema_version: u32,
    pub snapshot: serde_json::Value,
}

/// `GET /v1/sync/changes?cursor=&limit=`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangesResponse {
    pub changes: Vec<Change>,
    /// Pass back as `cursor` next time (unchanged when there was nothing new).
    pub next_cursor: String,
    pub has_more: bool,
}

/// One master aggregate in a bootstrap snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapEntity {
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub schema_version: u32,
    pub version: u64,
    pub snapshot: serde_json::Value,
}

/// `GET /v1/sync/bootstrap?page=`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapPage {
    /// The feed position when the snapshot began: pull from here once every page is in.
    pub snapshot_cursor: String,
    pub entities: Vec<BootstrapEntity>,
    /// The next page's token; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page: Option<String>,
}

/// The cursor for a feed position.
pub fn cursor(server_seq: i64) -> String {
    format!("v1:{server_seq}")
}

/// The feed position of a cursor (`None` if it is not one).
pub fn parse_cursor(c: &str) -> Option<i64> {
    c.strip_prefix("v1:")?.parse().ok().filter(|n: &i64| *n >= 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_reject_anything_else() {
        assert_eq!(parse_cursor(&cursor(41)), Some(41));
        assert_eq!(parse_cursor("v1:0"), Some(0));
        for bad in ["41", "v2:41", "v1:-1", "v1:x", ""] {
            assert_eq!(parse_cursor(bad), None, "{bad}");
        }
    }
}

/// One of the calling register's own events, as the cloud received it (E19-T03).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayedEvent {
    pub event_id: Uuid,
    pub local_sequence: u64,
    pub event_type: String,
    pub schema_version: u32,
    /// RFC 3339.
    pub occurred_at: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_version: Option<u64>,
    pub payload: serde_json::Value,
    /// What the cloud did with it: `applied`, `conflict` or `rejected`.
    pub status: String,
}

/// `GET /v1/sync/replay?after=&limit=`: a restored register's own events after `after`, so it
/// can re-create what it did after its backup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayPage {
    pub events: Vec<ReplayedEvent>,
    /// The register's acknowledged sequence.
    pub ack_sequence: u64,
    pub has_more: bool,
}
