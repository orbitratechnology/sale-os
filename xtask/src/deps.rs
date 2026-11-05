//! Crate dependency rules (docs/backlog/02-architecture-and-modules.md §3).

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// Allowed *normal* dependencies between workspace crates.
pub const ALLOWED: &[(&str, &[&str])] = &[
    ("pos-domain", &[]),
    ("pos-sync", &["pos-domain"]),
    ("pos-receipts", &["pos-domain"]),
    ("pos-device-ports", &["pos-domain"]),
    (
        "pos-application",
        &["pos-domain", "pos-sync", "pos-receipts", "pos-device-ports"],
    ),
    ("pos-local-db", &["pos-application", "pos-domain", "pos-sync"]),
    (
        "pos-device-virtual",
        &["pos-device-ports", "pos-domain", "pos-receipts"],
    ),
    ("pos-cloud-db", &["pos-domain", "pos-sync"]),
    ("pos-testkit", &["pos-domain"]),
    ("pos-observability", &[]),
    (
        "desktop-tauri",
        &[
            "pos-domain",
            "pos-application",
            "pos-local-db",
            "pos-sync",
            "pos-device-ports",
            "pos-device-virtual",
            "pos-receipts",
            "pos-observability",
        ],
    ),
    (
        "cloud-api",
        &["pos-domain", "pos-sync", "pos-cloud-db", "pos-observability"],
    ),
    ("xtask", &[]),
];

/// External crates that must never appear in a crate's normal dependency tree.
pub const FORBIDDEN_EXTERNAL: &[(&str, &[&str])] = &[
    ("pos-domain", &["sqlx", "tauri", "tokio", "axum"]),
    ("cloud-api", &["tauri", "libsqlite3-sys", "sqlx-sqlite"]),
];

/// `crate -> set of workspace crates it depends on (normal deps only)`.
pub type Graph = BTreeMap<String, BTreeSet<String>>;

pub fn violations(graph: &Graph) -> Vec<String> {
    let allowed: BTreeMap<&str, BTreeSet<&str>> = ALLOWED
        .iter()
        .map(|(k, v)| (*k, v.iter().copied().collect()))
        .collect();
    let mut out = Vec::new();
    for (krate, deps) in graph {
        let Some(ok) = allowed.get(krate.as_str()) else {
            out.push(format!(
                "{krate}: not listed in xtask ALLOWED rules; add it (and update docs/backlog/02 §3)"
            ));
            continue;
        };
        for d in deps {
            if !ok.contains(d.as_str()) {
                out.push(format!("{krate} -> {d}: forbidden dependency"));
            }
        }
    }
    out
}

fn workspace_graph(root: &Path) -> Result<Graph> {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .context("cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let meta: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let packages = meta["packages"].as_array().context("packages")?;
    let names: BTreeSet<String> = packages
        .iter()
        .filter_map(|p| p["name"].as_str().map(str::to_owned))
        .collect();
    let mut graph = Graph::new();
    for p in packages {
        let name = p["name"].as_str().context("package name")?.to_owned();
        let deps = p["dependencies"]
            .as_array()
            .context("dependencies")?
            .iter()
            .filter(|d| d["kind"].is_null()) // normal deps; dev/build deps are not layering edges
            .filter_map(|d| d["name"].as_str())
            .filter(|n| names.contains(*n))
            .map(str::to_owned)
            .collect();
        graph.insert(name, deps);
    }
    Ok(graph)
}

fn external_violations(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (krate, banned) in FORBIDDEN_EXTERNAL {
        let output = Command::new(env!("CARGO"))
            .current_dir(root)
            .args([
                "tree", "-p", krate, "-e", "normal", "--prefix", "none", "--format", "{p}",
            ])
            .output()
            .with_context(|| format!("cargo tree -p {krate}"))?;
        if !output.status.success() {
            bail!("cargo tree failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        let tree = String::from_utf8_lossy(&output.stdout);
        for line in tree.lines() {
            let name = line.split_whitespace().next().unwrap_or_default();
            if banned.contains(&name) {
                out.push(format!(
                    "{krate} depends (transitively) on forbidden crate {name}"
                ));
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn run(root: &Path) -> Result<()> {
    let mut problems = violations(&workspace_graph(root)?);
    problems.extend(external_violations(root)?);
    if problems.is_empty() {
        println!("dependency rules OK");
        return Ok(());
    }
    for p in &problems {
        eprintln!("error: {p}");
    }
    bail!("{} dependency rule violation(s)", problems.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(&str, &[&str])]) -> Graph {
        edges
            .iter()
            .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    #[test]
    fn allowed_graph_passes() {
        let g = graph(&[
            ("pos-domain", &[]),
            ("pos-local-db", &["pos-domain"]),
            ("cloud-api", &["pos-cloud-db"]),
        ]);
        assert!(violations(&g).is_empty());
    }

    #[test]
    fn domain_depending_on_storage_is_rejected() {
        let g = graph(&[("pos-domain", &["pos-local-db"])]);
        assert_eq!(
            violations(&g),
            vec!["pos-domain -> pos-local-db: forbidden dependency"]
        );
    }

    #[test]
    fn cloud_depending_on_local_db_is_rejected() {
        let g = graph(&[("cloud-api", &["pos-local-db"])]);
        assert_eq!(violations(&g).len(), 1);
    }

    #[test]
    fn testkit_as_normal_dependency_is_rejected() {
        let g = graph(&[("pos-application", &["pos-testkit"])]);
        assert_eq!(violations(&g).len(), 1);
    }

    #[test]
    fn unknown_crates_must_be_added_to_the_rules() {
        let g = graph(&[("pos-new-thing", &[])]);
        assert!(violations(&g)[0].contains("not listed"));
    }
}
