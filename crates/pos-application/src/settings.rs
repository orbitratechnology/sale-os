//! The typed settings registry (E04-T03, SET-004/005). Every business setting is declared
//! here once: its key, type, default, where it shows in the settings screen and what it
//! does. Setup writes the defaults, `SettingsService` validates every change against its
//! declaration, and `docs/settings.md` is generated from it (a test keeps it in step).

use crate::audit::{self, AuditEntry};
use crate::context::{AppError, CommandContext};
use crate::queries::QueryError;
use crate::records::SettingRecord;
use crate::{LocalStore, OutboxDraft, WriteSet};
use pos_domain::{ErrorCode, Permission};
use serde_json::{Value, json};
use std::sync::Arc;

/// What values a setting takes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    /// A whole number within bounds.
    Int {
        min: i64,
        max: i64,
    },
    /// A rate in parts per million (100000 = 10%).
    Ppm,
    /// One of these JSON values (strings or numbers).
    Choice(&'static [&'static str]),
    /// Up to `max_lines` lines of at most `max_chars` characters each.
    Lines {
        max_lines: usize,
        max_chars: usize,
    },
}

#[derive(Copy, Clone, Debug)]
pub struct SettingDef {
    pub key: &'static str,
    pub kind: SettingKind,
    /// The default, as JSON text.
    pub default: &'static str,
    /// The settings screen page.
    pub group: &'static str,
    pub label: &'static str,
    /// What it does, in plain words.
    pub help: &'static str,
}

/// Every business setting. Order is the settings screen's order.
pub const REGISTRY: &[SettingDef] = &[
    SettingDef {
        key: "inventory.allow_negative",
        kind: SettingKind::Bool,
        default: "true",
        group: "Inventory",
        label: "Allow selling below zero stock",
        help: "On: sell anyway and flag the item for a stock check. Off: refuse the sale and list the items.",
    },
    SettingDef {
        key: "cash.require_session_for_sale",
        kind: SettingKind::Bool,
        default: "true",
        group: "Cash drawer",
        label: "Sales need an open drawer",
        help: "In live mode, a sale or a cash refund needs an open cash drawer session. Practice sales never do.",
    },
    SettingDef {
        key: "discount.cashier_max_ppm",
        kind: SettingKind::Ppm,
        default: "100000",
        group: "Security",
        label: "Largest discount a cashier may give",
        help: "Above this, a cashier needs a manager's approval and a reason.",
    },
    SettingDef {
        key: "returns.cashier_requires_approval",
        kind: SettingKind::Bool,
        default: "true",
        group: "Security",
        label: "Cashier returns need a manager",
        help: "On: a cashier's return asks for a manager's PIN. Off: cashiers make returns on their own.",
    },
    SettingDef {
        key: "payments.cashier_external_tender",
        kind: SettingKind::Bool,
        default: "true",
        group: "Security",
        label: "Cashiers may record card and other external payments",
        help: "Off: only managers and owners can record a card, transfer or QR payment; cashiers take cash.",
    },
    SettingDef {
        key: "security.lock_timeout_secs",
        kind: SettingKind::Int { min: 0, max: 3600 },
        default: "300",
        group: "Security",
        label: "Lock the till after this many idle seconds",
        help: "0 never locks. The cart is kept for the next sign-in.",
    },
    SettingDef {
        key: "receipt.paper_width_mm",
        kind: SettingKind::Choice(&["58", "80"]),
        default: "80",
        group: "Receipts",
        label: "Paper width (mm)",
        help: "58 mm prints 32 characters a line, 80 mm prints 48.",
    },
    SettingDef {
        key: "receipt.auto_print",
        kind: SettingKind::Bool,
        default: "false",
        group: "Receipts",
        label: "Print the receipt after every sale",
        help: "Off: the cashier prints when asked.",
    },
    SettingDef {
        key: "receipt.header_lines",
        kind: SettingKind::Lines {
            max_lines: 5,
            max_chars: 48,
        },
        default: "[]",
        group: "Receipts",
        label: "Extra lines at the top of receipts",
        help: "Up to 5 lines, e.g. opening hours.",
    },
    SettingDef {
        key: "receipt.footer_lines",
        kind: SettingKind::Lines {
            max_lines: 5,
            max_chars: 48,
        },
        default: "[]",
        group: "Receipts",
        label: "Lines at the bottom of receipts",
        help: "Up to 5 lines, e.g. the returns policy.",
    },
    SettingDef {
        key: "scanner.suffix",
        kind: SettingKind::Choice(&["\"Enter\"", "\"Tab\"", "\"none\""]),
        default: "\"Enter\"",
        group: "Scanner",
        label: "Key the scanner sends after a code",
        help: "Match the scanner's setting. \"none\" ends a code by the pause after it.",
    },
    SettingDef {
        key: "scanner.max_key_gap_ms",
        kind: SettingKind::Int { min: 10, max: 100 },
        default: "35",
        group: "Scanner",
        label: "Fastest typing that counts as a scan (ms between keys)",
        help: "Raise it for slow wireless scanners; people type far slower than any scanner.",
    },
];

pub fn def(key: &str) -> Option<&'static SettingDef> {
    REGISTRY.iter().find(|d| d.key == key)
}

/// The defaults written at setup.
pub fn defaults() -> Vec<(&'static str, Value)> {
    REGISTRY
        .iter()
        .map(|d| {
            (
                d.key,
                serde_json::from_str(d.default).expect("registry defaults are valid JSON"),
            )
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SettingError {
    #[error("there is no setting {0:?}")]
    UnknownKey(String),
    #[error("{key} must be {expected}")]
    WrongType { key: &'static str, expected: String },
}

/// Checks `value` against the declaration of `key`.
pub fn validate(key: &str, value: &Value) -> Result<&'static SettingDef, SettingError> {
    let d = def(key).ok_or_else(|| SettingError::UnknownKey(key.to_owned()))?;
    let wrong = |expected: String| Err(SettingError::WrongType { key: d.key, expected });
    match d.kind {
        SettingKind::Bool => {
            if !value.is_boolean() {
                return wrong("true or false".into());
            }
        }
        SettingKind::Int { min, max } => match value.as_i64() {
            Some(n) if (min..=max).contains(&n) => {}
            _ => return wrong(format!("a whole number from {min} to {max}")),
        },
        SettingKind::Ppm => match value.as_u64() {
            Some(n) if n <= 1_000_000 => {}
            _ => return wrong("a rate from 0 to 1000000 ppm (0% to 100%)".into()),
        },
        SettingKind::Choice(options) => {
            if !options
                .iter()
                .any(|o| serde_json::from_str::<Value>(o).is_ok_and(|v| &v == value))
            {
                return wrong(format!("one of {}", options.join(", ")));
            }
        }
        SettingKind::Lines { max_lines, max_chars } => {
            let ok = value.as_array().is_some_and(|a| {
                a.len() <= max_lines
                    && a.iter().all(|l| {
                        l.as_str()
                            .is_some_and(|s| s.chars().count() <= max_chars && !s.contains(['\n', '\r']))
                    })
            });
            if !ok {
                return wrong(format!(
                    "up to {max_lines} lines of at most {max_chars} characters"
                ));
            }
        }
    }
    Ok(d)
}

impl pos_domain::HasErrorCode for SettingError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::ValidationFailed
    }
}

/// A setting as the settings screen shows it.
#[derive(Clone, Debug)]
pub struct SettingValue {
    pub def: &'static SettingDef,
    pub value: Value,
    /// For `baseVersion` on update; 0 if never stored.
    pub version: u64,
}

#[async_trait::async_trait]
pub trait SettingsQueries: Send + Sync {
    /// Every stored setting: `(key, value, version)`.
    async fn stored_settings(&self) -> Result<Vec<(String, Value, u64)>, QueryError>;
}

pub struct SettingsService {
    pub store: Arc<dyn LocalStore>,
    pub settings: Arc<dyn SettingsQueries>,
}

/// What the till itself needs from the settings, for anyone signed in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub scanner_suffix: String,
    pub scanner_max_key_gap_ms: u32,
    pub lock_timeout_secs: u32,
}

impl SettingsService {
    /// Every registered setting with its current value (the default if never stored).
    pub async fn list(&self, ctx: &CommandContext) -> Result<Vec<SettingValue>, AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        self.current().await
    }

    async fn current(&self) -> Result<Vec<SettingValue>, AppError> {
        let stored = self.settings.stored_settings().await?;
        Ok(REGISTRY
            .iter()
            .map(|d| {
                let found = stored.iter().find(|(k, _, _)| k == d.key);
                SettingValue {
                    def: d,
                    // A stored value that no longer validates (an old version) shows the default.
                    value: found
                        .filter(|(_, v, _)| validate(d.key, v).is_ok())
                        .map(|(_, v, _)| v.clone())
                        .unwrap_or_else(|| serde_json::from_str(d.default).expect("valid default")),
                    version: found.map_or(0, |(_, _, n)| *n),
                }
            })
            .collect())
    }

    /// Changes one setting. `base_version` is the version the owner saw (0 if never stored);
    /// a newer change in between gives `CONFLICT_VERSION`.
    pub async fn update(
        &self,
        ctx: &CommandContext,
        key: &str,
        value: Value,
        base_version: u64,
    ) -> Result<SettingValue, AppError> {
        ctx.authorize(Permission::SettingsEdit)?;
        let d = validate(key, &value)?;
        let before = self
            .current()
            .await?
            .into_iter()
            .find(|s| s.def.key == d.key)
            .map(|s| s.value)
            .unwrap_or(Value::Null);
        let ids = ctx.ids.as_ref();
        let record = SettingRecord {
            business_id: ctx.register.business_id,
            key: d.key.into(),
            value: value.clone(),
            version: base_version + 1,
            updated_at: ctx.now(),
        };
        let entry = AuditEntry::new(ids, audit::SETTINGS_UPDATED, "setting", d.key)
            .with_metadata(json!({ "key": d.key, "from": before, "to": value }));
        let set = WriteSet::builder(ctx.write_context())
            .outbox(OutboxDraft::setting_updated(ids, &record)?)
            .put_setting(record.clone(), (base_version > 0).then_some(base_version))
            .audit(entry)
            .build()?;
        self.store.apply(set).await?;
        Ok(SettingValue {
            def: d,
            value,
            version: record.version,
        })
    }

    /// The till's own preferences (scanner, idle lock), for anyone signed in.
    pub async fn preferences(&self, ctx: &CommandContext) -> Result<Preferences, AppError> {
        ctx.actor()?;
        let all = self.current().await?;
        let get = |k: &str| {
            all.iter()
                .find(|s| s.def.key == k)
                .map(|s| s.value.clone())
                .unwrap_or(Value::Null)
        };
        Ok(Preferences {
            scanner_suffix: get("scanner.suffix").as_str().unwrap_or("Enter").to_owned(),
            scanner_max_key_gap_ms: get("scanner.max_key_gap_ms")
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(35),
            lock_timeout_secs: get("security.lock_timeout_secs")
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(300),
        })
    }
}

/// `docs/settings.md`, generated from the registry.
pub fn settings_doc() -> String {
    let mut out = String::from(
        "# Settings\n\n<!-- GENERATED from crates/pos-application/src/settings.rs (REGISTRY). Do not edit:\n     run `UPDATE_GOLDEN=1 cargo test -p pos-application settings_doc` after changing the registry. -->\n\nEvery business setting, its type and default. Owners change them in Settings (E04-T03);\neach change is one transaction with an audit row (`settings.updated`) and a `setting.updated`\noutbox event, and is refused with `CONFLICT_VERSION` if someone changed it meanwhile.\n",
    );
    let mut group = "";
    for d in REGISTRY {
        if d.group != group {
            group = d.group;
            out.push_str(&format!(
                "\n## {group}\n\n| Key | Type | Default | What it does |\n|---|---|---|---|\n"
            ));
        }
        let kind = match d.kind {
            SettingKind::Bool => "true / false".to_owned(),
            SettingKind::Int { min, max } => format!("whole number {min}–{max}"),
            SettingKind::Ppm => "rate (ppm)".to_owned(),
            SettingKind::Choice(o) => format!("one of {}", o.join(", ")),
            SettingKind::Lines { max_lines, max_chars } => {
                format!("up to {max_lines} lines × {max_chars} chars")
            }
        };
        out.push_str(&format!(
            "| `{}` | {} | `{}` | **{}.** {} |\n",
            d.key, kind, d.default, d.label, d.help
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_validates_and_keys_are_unique() {
        for d in REGISTRY {
            let v: Value = serde_json::from_str(d.default).unwrap();
            assert!(validate(d.key, &v).is_ok(), "{} default", d.key);
            assert_eq!(
                REGISTRY.iter().filter(|x| x.key == d.key).count(),
                1,
                "{} twice",
                d.key
            );
        }
    }

    #[test]
    fn unknown_keys_and_wrong_types_are_refused() {
        assert!(matches!(
            validate("nope", &json!(true)),
            Err(SettingError::UnknownKey(_))
        ));
        for (key, bad) in [
            ("inventory.allow_negative", json!("true")),
            ("security.lock_timeout_secs", json!(-1)),
            ("security.lock_timeout_secs", json!(3601)),
            ("security.lock_timeout_secs", json!(1.5)),
            ("discount.cashier_max_ppm", json!(1_000_001)),
            ("receipt.paper_width_mm", json!(70)),
            ("receipt.paper_width_mm", json!("58")),
            ("scanner.suffix", json!("Space")),
            ("receipt.footer_lines", json!(["a", "b", "c", "d", "e", "f"])),
            ("receipt.footer_lines", json!(["x".repeat(49)])),
            ("receipt.footer_lines", json!(["two\nlines"])),
        ] {
            assert!(
                matches!(validate(key, &bad), Err(SettingError::WrongType { .. })),
                "{key} = {bad}"
            );
        }
        for (key, good) in [
            ("security.lock_timeout_secs", json!(0)),
            ("receipt.paper_width_mm", json!(58)),
            ("scanner.suffix", json!("none")),
            ("receipt.footer_lines", json!(["No refunds without a receipt"])),
        ] {
            assert!(validate(key, &good).is_ok(), "{key} = {good}");
        }
    }

    #[test]
    fn settings_doc() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/settings.md");
        let doc = super::settings_doc();
        if std::env::var("UPDATE_GOLDEN").is_ok() {
            std::fs::write(&path, &doc).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            doc,
            "docs/settings.md is stale: UPDATE_GOLDEN=1"
        );
    }
}
