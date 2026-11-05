//! Repository automation. Run as `cargo xtask <command>`.
//!
//! - `check-deps`            enforce the crate dependency rules (docs/backlog/02 §3)
//! - `gen-bindings [--check]` regenerate (or verify) the TypeScript IPC bindings (ADR-012)
//! - `gen-schemas [--check]`  regenerate (or verify) sync event JSON Schemas (E17-T01)
//! - `trace`                 verify every PRD P0 requirement is traced to a task

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
        Some("trace") => trace::run(&root()),
        _ => {
            eprintln!(
                "usage: cargo xtask <check-deps | gen-bindings [--check] | gen-schemas [--check] | trace>"
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
    // Event payload types and their schemars generator arrive with E17-T01.
    let dir = root().join("contracts/events");
    // A missing directory means "no schemas" (git does not track empty directories).
    let stray: Vec<_> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".schema.json"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    if check && !stray.is_empty() {
        bail!("contracts/events contains schemas but no generator exists yet: {stray:?}");
    }
    println!(
        "no sync event types defined yet (E17-T01); nothing to {}",
        if check { "check" } else { "generate" }
    );
    Ok(())
}
