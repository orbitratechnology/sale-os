//! E20-T01: the hardware compatibility registry (`docs/hardware/registry.yaml`) is
//! well-formed, uses known values, and names only printer profiles that exist.

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema: u32,
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    vendor: String,
    model: String,
    kind: String,
    connection: String,
    profile_id: Option<String>,
    os: String,
    adapter: String,
    firmware: String,
    capabilities: Vec<String>,
    result: String,
    tested_on: String,
    tester: String,
    evidence: String,
    notes: String,
}

#[test]
fn the_registry_is_valid() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/hardware/registry.yaml");
    let text = std::fs::read_to_string(path).unwrap();
    let r: Registry = serde_yaml_ng::from_str(&text).unwrap();
    assert_eq!(r.schema, 1);
    assert!(!r.entries.is_empty());
    for e in &r.entries {
        let what = format!("{} {}", e.vendor, e.model);
        assert!(
            !e.vendor.trim().is_empty() && !e.model.trim().is_empty(),
            "{what}"
        );
        assert!(
            [
                "receipt_printer",
                "scanner",
                "payment_terminal",
                "cash_drawer",
                "customer_display",
                "scale"
            ]
            .contains(&e.kind.as_str()),
            "{what}: kind {}",
            e.kind
        );
        assert!(
            [
                "network",
                "serial",
                "device_file",
                "spooler",
                "usb_hid_keyboard",
                "usb_hid_pos",
                "bluetooth",
                "virtual"
            ]
            .contains(&e.connection.as_str()),
            "{what}: connection {}",
            e.connection
        );
        assert!(
            ["pass", "partial", "fail", "emulator-pass"].contains(&e.result.as_str()),
            "{what}: result {}",
            e.result
        );
        let d: Vec<&str> = e.tested_on.split('-').collect();
        assert!(
            d.len() == 3 && d[0].len() == 4 && d.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
            "{what}: tested_on must be YYYY-MM-DD"
        );
        assert!(!e.capabilities.is_empty(), "{what}");
        assert!(
            !e.os.is_empty() && !e.adapter.is_empty() && !e.tester.is_empty() && !e.evidence.is_empty(),
            "{what}"
        );
        let _ = (&e.firmware, &e.notes);
        if let Some(id) = &e.profile_id {
            assert!(
                pos_device_hw::profile(id).is_some(),
                "{what}: unknown profile {id}"
            );
        }
        if e.result != "emulator-pass" {
            assert!(
                e.firmware != "n/a",
                "{what}: a real-hardware entry records its firmware"
            );
        }
    }
}
