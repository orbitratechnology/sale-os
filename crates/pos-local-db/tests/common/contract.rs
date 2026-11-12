//! E17-T01: every outbox event matches the sync contract (`pos-sync`). Each row becomes an
//! envelope, which must validate against the envelope schema; its payload must validate
//! against its type's generated JSON Schema and parse into the typed payload with unknown
//! fields refused.

use pos_local_db::SqliteStore;
use std::collections::BTreeSet;

fn schema_file(name: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/events/current")
        .join(name);
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap()
}

/// Checks every outbox event; returns the event types seen.
pub async fn check_outbox(store: &SqliteStore) -> BTreeSet<String> {
    type Row = (
        String,
        String,
        String,
        String,
        i64,
        String,
        i64,
        String,
        String,
        Option<i64>,
        i64,
        String,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT event_id, business_id, location_id, register_id, local_sequence, event_type, schema_version,
                aggregate_type, aggregate_id, base_version, occurred_at, payload_json
           FROM outbox_events ORDER BY local_sequence",
    )
    .fetch_all(store.db().readers())
    .await
    .unwrap();
    assert!(!rows.is_empty());
    let envelope_schema = jsonschema::validator_for(&schema_file("envelope.json")).unwrap();
    let mut seen = BTreeSet::new();
    let mut previous = 0;
    for (id, business, location, register, seq, event_type, version, agg_type, agg_id, base, at, payload) in
        rows
    {
        assert_eq!(seq, previous + 1, "local sequences are gapless");
        previous = seq;
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let envelope = serde_json::json!({
            "eventId": id, "businessId": business, "locationId": location, "registerId": register,
            "localSequence": seq, "eventType": event_type, "schemaVersion": version,
            "aggregateType": agg_type, "aggregateId": agg_id, "baseVersion": base,
            "occurredAt": pos_domain::Timestamp::from_millis(at).to_rfc3339(), "payload": payload,
        });
        let errors: Vec<String> = envelope_schema
            .iter_errors(&envelope)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "envelope of {event_type} #{seq}: {errors:?}");
        let typed: pos_sync::EventEnvelope = serde_json::from_value(envelope).unwrap();
        let version = u32::try_from(version).unwrap();
        let schema = schema_file(&format!("{event_type}.v{version}.json"));
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&typed.payload)
            .map(|e| format!("{} at {}", e, e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{event_type} v{version} #{seq}: {errors:?}\n{}",
            typed.payload
        );
        if let Err(e) = pos_sync::parse_payload(&event_type, version, &typed.payload) {
            panic!("{event_type} v{version} #{seq}: {e}\n{}", typed.payload);
        }
        // Master data carries the version it was based on; facts never do.
        assert_eq!(
            typed.base_version.is_some(),
            pos_sync::events::is_master(&event_type),
            "{event_type}: baseVersion presence"
        );
        seen.insert(event_type);
    }
    seen
}
