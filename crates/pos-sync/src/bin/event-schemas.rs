//! `event-schemas write|check|compat|release` (E17-T01), run through `cargo xtask`:
//!
//! - `write`: writes every catalogue entry's JSON Schema, and the protocol's, to
//!   `contracts/events/current/`.
//! - `check`: fails if `current/` is out of date.
//! - `compat`: fails if a schema in `current/` breaks the one in `released/` at the same
//!   version (a field removed, made required, retyped...). Bump `schemaVersion` instead.
//! - `release`: copies `current/` to `released/` (done when an app version ships).

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/events")
}

fn generated() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = pos_sync::CATALOGUE
        .iter()
        .map(|(t, v)| {
            let s = pos_sync::events::json_schema(t, *v).expect("catalogue entries have schemas");
            (format!("{t}.v{v}.json"), pretty(&s))
        })
        .collect();
    for (name, s) in pos_sync::events::protocol_schemas() {
        out.push((format!("{name}.json"), pretty(&s)));
    }
    out.sort();
    out
}

fn pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).expect("schemas serialize") + "\n"
}

fn main() -> std::process::ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let current = root().join("current");
    let released = root().join("released");
    match cmd.as_str() {
        "write" => {
            std::fs::create_dir_all(&current).expect("create contracts/events/current");
            for (name, body) in generated() {
                std::fs::write(current.join(&name), body).expect("write schema");
            }
            println!("wrote {}", current.display());
        }
        "check" => {
            let stale: Vec<String> = generated()
                .into_iter()
                .filter(|(name, body)| {
                    std::fs::read_to_string(current.join(name)).ok().as_deref() != Some(body)
                })
                .map(|(name, _)| name)
                .collect();
            if !stale.is_empty() {
                eprintln!(
                    "event schemas out of date ({}); run `cargo xtask event-schemas`",
                    stale.join(", ")
                );
                return std::process::ExitCode::FAILURE;
            }
            println!("event schemas up to date");
        }
        "compat" => {
            let mut problems = Vec::new();
            for entry in std::fs::read_dir(&released).expect("contracts/events/released") {
                let path = entry.expect("dir entry").path();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let old: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&path).expect("read released"))
                        .expect("json");
                match std::fs::read_to_string(current.join(&name)) {
                    // A version that is no longer generated: it must still be accepted, so its
                    // file stays in current/ until the upcaster's support window ends.
                    Err(_) => problems.push(format!("{name}: no longer generated")),
                    Ok(text) => {
                        let new: serde_json::Value = serde_json::from_str(&text).expect("json");
                        for c in pos_sync::compat::breaking_changes(&old, &new) {
                            problems.push(format!("{name}: {c}"));
                        }
                    }
                }
            }
            if !problems.is_empty() {
                eprintln!("breaking changes without a new schemaVersion:");
                for p in problems {
                    eprintln!("  {p}");
                }
                return std::process::ExitCode::FAILURE;
            }
            println!("event schemas compatible with the released ones");
        }
        "release" => {
            std::fs::create_dir_all(&released).expect("create released");
            for (name, body) in generated() {
                std::fs::write(released.join(&name), body).expect("write released schema");
            }
            println!("released {}", released.display());
        }
        _ => {
            eprintln!("usage: event-schemas write|check|compat|release");
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::SUCCESS
}
