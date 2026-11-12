//! Repository automation. Run as `cargo xtask <command>`.
//!
//! - `check-deps`            enforce the crate dependency rules (docs/backlog/02 §3)
//! - `gen-bindings [--check]` regenerate (or verify) the TypeScript IPC bindings (ADR-012)
//! - `gen-schemas [--check]`  regenerate (or verify) sync event JSON Schemas (E17-T01)
//! - `schema-compat`         fail on a breaking schema change without a new schemaVersion
//! - `schema-release`        record the current schemas as released (when an app version ships)
//! - `trace`                 verify every PRD P0 requirement is traced to a task
//! - `crash-harness [--kills N] [--report FILE]`  kill-and-verify durability campaign (E02-T04)

mod deps;
mod trace;

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.iter().any(|a| a == "--check");
    match args.first().map(String::as_str) {
        Some("check-deps") => deps::run(&root()),
        Some("gen-bindings") => gen_bindings(check),
        Some("gen-schemas") => gen_schemas(check),
        Some("schema-compat") => event_schemas("check").and_then(|()| event_schemas("compat")),
        Some("schema-release") => event_schemas("release"),
        Some("trace") => trace::run(&root()),
        Some("crash-harness") => crash_harness(&args[1..]),
        _ => {
            eprintln!(
                "usage: cargo xtask <check-deps | gen-bindings [--check] | gen-schemas [--check] | schema-compat | schema-release | trace>"
            );
            std::process::exit(2);
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}

/// Builds the harness in release mode (realistic timing) and runs it.
fn crash_harness(args: &[String]) -> Result<()> {
    let status = Command::new(env!("CARGO"))
        .current_dir(root())
        .args([
            "run",
            "--quiet",
            "--release",
            "-p",
            "pos-local-db",
            "--bin",
            "crash-harness",
            "--",
            "run",
        ])
        .args(if args.is_empty() {
            vec!["--kills".to_owned(), "100".to_owned()]
        } else {
            args.to_vec()
        })
        .status()
        .context("running crash-harness")?;
    if !status.success() {
        bail!("crash harness found a violation (see output above)");
    }
    Ok(())
}

const BINDINGS: &str = "apps/desktop-ui/src/ipc/bindings.ts";

fn gen_bindings(check: bool) -> Result<()> {
    let root = root();
    let committed = root.join(BINDINGS);
    let target = if check {
        root.join("target/xtask/bindings.ts")
    } else {
        committed.clone()
    };
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "run",
            "--quiet",
            "-p",
            "desktop-tauri",
            "--bin",
            "export-bindings",
            "--",
        ])
        .arg(&target)
        .status()
        .context("running export-bindings")?;
    if !status.success() {
        bail!("export-bindings failed");
    }
    if check {
        let fresh = std::fs::read_to_string(&target)?;
        let current = std::fs::read_to_string(&committed).unwrap_or_default();
        if fresh != current {
            bail!("{BINDINGS} is out of date. Run `cargo xtask gen-bindings` and commit the result.");
        }
        println!("bindings up to date");
    } else {
        println!("wrote {BINDINGS}");
    }
    Ok(())
}

fn gen_schemas(check: bool) -> Result<()> {
    event_schemas(if check { "check" } else { "write" })
}

/// Runs `pos-sync`'s `event-schemas` tool (write, check, compat or release).
fn event_schemas(cmd: &str) -> Result<()> {
    let status = Command::new(env!("CARGO"))
        .current_dir(root())
        .args([
            "run",
            "--quiet",
            "-p",
            "pos-sync",
            "--bin",
            "event-schemas",
            "--",
            cmd,
        ])
        .status()
        .context("running event-schemas")?;
    if !status.success() {
        bail!("event-schemas {cmd} failed");
    }
    Ok(())
}
