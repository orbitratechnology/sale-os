//! Requirement traceability check (docs/backlog/01 §2): every P0 requirement ID in the PRD
//! must appear in the traceability table, and every task it names must exist.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::Path;

fn ids_matching(text: &str, pred: impl Fn(&str) -> bool) -> BTreeSet<String> {
    text.lines()
        .filter(|l| pred(l))
        .filter_map(|l| l.split('|').nth(1))
        .map(|c| c.trim().to_owned())
        .filter(|c| is_req_id(c))
        .collect()
}

fn is_req_id(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7
        && b[..3].iter().all(u8::is_ascii_uppercase)
        && b[3] == b'-'
        && b[4..].iter().all(u8::is_ascii_digit)
}

fn task_ids(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let b = text.as_bytes();
    for i in 0..b.len().saturating_sub(6) {
        if b[i] == b'E' && b[i + 1].is_ascii_digit() && b[i + 2].is_ascii_digit() && &b[i + 3..i + 5] == b"-T"
        {
            let mut j = i + 5;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 5 {
                out.insert(text[i..j].to_owned());
            }
        }
    }
    out
}

pub fn run(root: &Path) -> Result<()> {
    let prd = std::fs::read_to_string(root.join("docs/PRD.md")).context("docs/PRD.md")?;
    let traceability = std::fs::read_to_string(root.join("docs/backlog/01-epics-and-traceability.md"))?;
    let p0 = ids_matching(&prd, |l| l.contains("| P0 |"));
    let traced = ids_matching(&traceability, |_| true);
    let missing: Vec<_> = p0.difference(&traced).collect();

    let mut defined = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("docs/backlog/tasks"))? {
        let text = std::fs::read_to_string(entry?.path())?;
        for line in text.lines().filter(|l| l.starts_with("### E")) {
            defined.extend(task_ids(line));
        }
    }
    let undefined: Vec<_> = task_ids(&traceability)
        .into_iter()
        .filter(|t| !defined.contains(t))
        .collect();

    if missing.is_empty() && undefined.is_empty() {
        println!(
            "traceability OK: {} P0 requirements, {} tasks defined",
            p0.len(),
            defined.len()
        );
        return Ok(());
    }
    for m in &missing {
        eprintln!("error: P0 requirement {m} has no row in docs/backlog/01-epics-and-traceability.md");
    }
    for u in &undefined {
        eprintln!("error: traceability references undefined task {u}");
    }
    bail!("traceability check failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids() {
        assert!(is_req_id("CAT-008"));
        assert!(!is_req_id("CAT-08"));
        assert_eq!(
            task_ids("see E09-T08 and E06-T06a"),
            BTreeSet::from(["E09-T08".into(), "E06-T06".into()])
        );
    }
}
