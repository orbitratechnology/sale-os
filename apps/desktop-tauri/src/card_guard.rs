//! PAY-005 / E19-T05: no card number gets in through the IPC. Every command's arguments are
//! checked before the command runs: a string anywhere in them holding a Luhn-valid run of
//! 13–19 digits (spaces and dashes ignored) is refused with `CARD_DATA_REJECTED`, and the
//! command never sees it. So no card number can reach the database, the outbox, a receipt or
//! the logs, whichever free-text field it is typed into. The refusal names the field, never
//! the value.
//!
//! Scanned codes (`raw`) and search text (`query`) are different: about one EAN-13 barcode in
//! ten also passes Luhn. There, card *swipes* are refused (magnetic-stripe track data, as a
//! keyboard-wedge reader types it) and so are 15–19 digit Luhn runs (no retail barcode is that
//! long: GTINs are at most 14 digits). PINs and the cloud password are never stored or logged,
//! so they are not checked.

use crate::IpcError;
use pos_domain::ErrorCode;
use pos_domain::settlement::looks_like_card_number;
use serde_json::Value;

/// Keys holding a scan or a search (see the module comment).
const SCANNED: &[&str] = &["raw", "query"];
/// Keys never stored or logged.
const UNCHECKED: &[&str] = &["pin", "ownerPin", "password", "newPin", "currentPin"];

/// Magnetic-stripe track 1 (`%B<pan>^`) or track 2 (`;<pan>=`) as a reader types it.
fn looks_like_track_data(text: &str) -> bool {
    let track = |start: &str, end: char| {
        text.match_indices(start).any(|(i, _)| {
            let rest = &text[i + start.len()..];
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            (12..=19).contains(&digits) && rest[digits..].starts_with(end)
        })
    };
    track("%B", '^') || track("%b", '^') || track(";", '=')
}

fn long_card_run(text: &str) -> bool {
    let joined: String = text.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    joined
        .split(|c: char| !c.is_ascii_digit())
        .any(|run| run.len() >= 15 && looks_like_card_number(run))
}

/// The path (`input.lines[2].note`) of the first string that looks like card data.
pub fn find(value: &Value) -> Option<String> {
    fn walk(v: &Value, path: &str, key: &str) -> Option<String> {
        match v {
            Value::String(s) => {
                let bad = if UNCHECKED.contains(&key) {
                    false
                } else if SCANNED.contains(&key) {
                    looks_like_track_data(s) || long_card_run(s)
                } else {
                    looks_like_card_number(s) || looks_like_track_data(s)
                };
                bad.then(|| path.to_owned())
            }
            Value::Array(items) => items
                .iter()
                .enumerate()
                .find_map(|(i, x)| walk(x, &format!("{path}[{i}]"), key)),
            Value::Object(map) => map.iter().find_map(|(k, x)| {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                walk(x, &p, k)
            }),
            _ => None,
        }
    }
    walk(value, "", "")
}

/// The refusal for a command whose arguments hold card data at `path`.
pub fn refusal(cmd: &str, path: &str) -> IpcError {
    tracing::warn!(
        command = cmd,
        field = path,
        "card data refused at the IPC boundary"
    );
    let mut e = IpcError::coded(
        ErrorCode::CardDataRejected,
        "That looks like a card number. Card numbers must never be typed into Sale OS: take card payments on the card terminal.",
    );
    e.details = Some(Box::new(crate::error::ErrorDetails {
        field: Some(path.to_owned()),
        ..Default::default()
    }));
    e
}

/// Wraps the command handler with the check.
pub fn guard<R: tauri::Runtime>(
    inner: impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke: tauri::ipc::Invoke<R>| {
        if let tauri::ipc::InvokeBody::Json(args) = invoke.message.payload()
            && let Some(path) = find(args)
        {
            let e = refusal(invoke.message.command(), &path);
            invoke.resolver.reject(e);
            return true;
        }
        inner(invoke)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PAN: &str = "4111 1111 1111 1111";

    #[test]
    fn card_numbers_anywhere_are_found_by_path() {
        assert_eq!(find(&json!({"name": "Tea"})), None);
        assert_eq!(
            find(&json!({"input": {"name": PAN}})).as_deref(),
            Some("input.name")
        );
        assert_eq!(
            find(&json!({"input": {"lines": [{"note": "ok"}, {"note": format!("card {PAN}")}]}})).as_deref(),
            Some("input.lines[1].note")
        );
        assert_eq!(
            find(&json!({"note": "4111-1111-1111-1111"})).as_deref(),
            Some("note")
        );
        // Not Luhn-valid, or too short: fine.
        assert_eq!(find(&json!({"note": "4111 1111 1111 1112"})), None);
        assert_eq!(find(&json!({"note": "order 123456789012"})), None);
    }

    #[test]
    fn scans_allow_barcodes_but_not_swipes() {
        // An EAN-13 that also passes Luhn is a barcode.
        assert!(looks_like_card_number("5000000000104"));
        assert_eq!(find(&json!({"raw": "5000000000104"})), None);
        assert_eq!(find(&json!({"query": "5000000000104"})), None);
        // A swipe, or a 16-digit card number typed into the scan box, is not.
        assert!(find(&json!({"raw": "%B4111111111111111^DOE/JOHN^2512101"})).is_some());
        assert!(find(&json!({"raw": ";4111111111111111=2512101?"})).is_some());
        assert!(find(&json!({"raw": "4111111111111111"})).is_some());
        // Not checked: never stored or logged.
        assert_eq!(find(&json!({"pin": "4111111111111111"})), None);
    }
}
