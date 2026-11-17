//! Upcasters (07 §4 versioning): when a payload's shape changes, the event type gets a new
//! `schemaVersion`, and an upcaster turns the old version into the next one, so appliers only
//! ever see the current version. The server keeps them for at least two app releases.
//!
//! None are needed yet: every event is at v1.

use serde_json::Value;

/// Turns `event_type` at version `from` into version `from + 1`.
#[derive(Clone, Copy)]
pub struct Upcaster {
    pub event_type: &'static str,
    pub from: u32,
    pub up: fn(Value) -> Value,
}

/// The registry. Add an entry when a version is bumped.
pub const UPCASTERS: &[Upcaster] = &[];

/// Applies the registry's upcasters from `version` as far as they go.
pub fn to_current(event_type: &str, version: u32, value: Value) -> (u32, Value) {
    upcast_with(UPCASTERS, event_type, version, value)
}

pub fn upcast_with(
    registry: &[Upcaster],
    event_type: &str,
    mut version: u32,
    mut value: Value,
) -> (u32, Value) {
    while let Some(u) = registry
        .iter()
        .find(|u| u.event_type == event_type && u.from == version)
    {
        value = (u.up)(value);
        version += 1;
    }
    (version, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_upcasters_to_the_newest_version() {
        // A made-up history: v1 had "total", v2 renamed it "totalMinor", v3 added "currency".
        let registry = [
            Upcaster {
                event_type: "demo.happened",
                from: 1,
                up: |mut v| {
                    let t = v["total"].take();
                    v.as_object_mut().unwrap().remove("total");
                    v["totalMinor"] = t;
                    v
                },
            },
            Upcaster {
                event_type: "demo.happened",
                from: 2,
                up: |mut v| {
                    v["currency"] = serde_json::json!("GBP");
                    v
                },
            },
        ];
        let (v, out) = upcast_with(&registry, "demo.happened", 1, serde_json::json!({"total": 5}));
        assert_eq!(
            (v, out),
            (3, serde_json::json!({"totalMinor": 5, "currency": "GBP"}))
        );
        // Already current, or another type: untouched.
        let (v, _) = upcast_with(&registry, "demo.happened", 3, serde_json::json!({}));
        assert_eq!(v, 3);
        let (v, _) = upcast_with(&registry, "other", 1, serde_json::json!({}));
        assert_eq!(v, 1);
    }
}
