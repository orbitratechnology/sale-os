//! Schema compatibility (07 §4, E17-T01): what may change in a payload without a new
//! `schemaVersion`. Adding an optional field is fine (older servers ignore it). Removing or
//! renaming a field, making a field required, changing a type, or changing an enum's values
//! is breaking: it needs a new version (and an upcaster).

use serde_json::Value;

/// Breaking differences between the released schema and the new one, as readable paths.
pub fn breaking_changes(released: &Value, current: &Value) -> Vec<String> {
    let mut out = Vec::new();
    compare(released, released, current, current, "$", &mut out);
    out
}

/// Follows a local `$ref` (`#/$defs/Name`).
fn resolve<'a>(root: &'a Value, s: &'a Value) -> &'a Value {
    match s.get("$ref").and_then(Value::as_str) {
        Some(r) => r
            .strip_prefix("#/")
            .map(|p| p.split('/').fold(root, |v, k| &v[k]))
            .filter(|v| !v.is_null())
            .unwrap_or(s),
        None => s,
    }
}

fn types(s: &Value) -> Vec<String> {
    match s.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(a)) => {
            let mut v: Vec<String> = a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect();
            v.sort();
            v
        }
        _ => Vec::new(),
    }
}

fn strings(s: &Value, key: &str) -> Vec<String> {
    let mut v: Vec<String> = s
        .get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|x| x.to_string()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn compare(old_root: &Value, old: &Value, new_root: &Value, new: &Value, path: &str, out: &mut Vec<String>) {
    let (o, n) = (resolve(old_root, old), resolve(new_root, new));
    let (ot, nt) = (types(o), types(n));
    if !ot.is_empty() && ot != nt {
        out.push(format!("{path}: type changed from {ot:?} to {nt:?}"));
        return;
    }
    // A field that had a fixed set of values must keep exactly that set.
    let (oe, ne) = (strings(o, "enum"), strings(n, "enum"));
    if !oe.is_empty() && oe != ne {
        out.push(format!("{path}: allowed values changed from {oe:?} to {ne:?}"));
    }
    if let Some(oc) = o.get("const")
        && n.get("const") != Some(oc)
    {
        out.push(format!("{path}: constant changed"));
    }
    let (or_, nr) = (strings(o, "required"), strings(n, "required"));
    for r in &nr {
        if !or_.contains(r) {
            out.push(format!("{path}.{}: became required", r.trim_matches('"')));
        }
    }
    if let Some(props) = o.get("properties").and_then(Value::as_object) {
        let empty = serde_json::Map::new();
        let nprops = n.get("properties").and_then(Value::as_object).unwrap_or(&empty);
        for (k, os) in props {
            match nprops.get(k) {
                None => out.push(format!("{path}.{k}: removed")),
                Some(ns) => compare(old_root, os, new_root, ns, &format!("{path}.{k}"), out),
            }
        }
    }
    if let (Some(oi), Some(ni)) = (o.get("items"), n.get("items")) {
        compare(old_root, oi, new_root, ni, &format!("{path}[]"), out);
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(oa) = o.get(key).and_then(Value::as_array) {
            let na = n.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
            if oa.len() != na.len() {
                out.push(format!(
                    "{path}: {key} alternatives changed ({} to {})",
                    oa.len(),
                    na.len()
                ));
            } else {
                for (i, (a, b)) in oa.iter().zip(&na).enumerate() {
                    compare(old_root, a, new_root, b, &format!("{path}<{key}{i}>"), out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        crate::events::json_schema("sale.completed", 1).unwrap()
    }

    #[test]
    fn the_same_schema_is_compatible() {
        assert_eq!(breaking_changes(&schema(), &schema()), Vec::<String>::new());
    }

    #[test]
    fn removing_a_field_is_breaking() {
        let mut new = schema();
        new["properties"].as_object_mut().unwrap().remove("tenderedTotal");
        let changes = breaking_changes(&schema(), &new);
        assert!(
            changes.iter().any(|c| c == "$.tenderedTotal: removed"),
            "{changes:?}"
        );
        // Also inside a nested definition.
        let mut new = schema();
        new["$defs"]["TenderV1"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("received");
        let changes = breaking_changes(&schema(), &new);
        assert!(
            changes.iter().any(|c| c.ends_with(".received: removed")),
            "{changes:?}"
        );
    }

    #[test]
    fn adding_an_optional_field_is_not_breaking_but_a_required_one_is() {
        let mut new = schema();
        new["properties"]["loyaltyId"] = json!({"type": ["string", "null"]});
        assert!(breaking_changes(&schema(), &new).is_empty());
        new["required"].as_array_mut().unwrap().push(json!("loyaltyId"));
        assert_eq!(
            breaking_changes(&schema(), &new),
            vec!["$.loyaltyId: became required"]
        );
    }

    #[test]
    fn type_and_enum_changes_are_breaking() {
        let mut new = schema();
        new["properties"]["total"] = json!({"type": "string"});
        assert!(breaking_changes(&schema(), &new)[0].contains("type changed"));
        let mut new = schema();
        new["$defs"]["SaleMode"]["enum"] = json!(["live"]);
        assert!(
            breaking_changes(&schema(), &new)
                .iter()
                .any(|c| c.contains("allowed values"))
        );
    }
}
