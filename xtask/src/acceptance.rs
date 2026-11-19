//! `cargo xtask acceptance`: the MVP acceptance suite (E19-T12, PRD §16.4, test strategy §3).
//!
//! Each acceptance test AT-01…AT-13 is a list of named checks: Rust tests (by exact name, or
//! a name prefix where a family of scenarios belongs to the AT), browser tests (by title, in
//! Chromium) and the crash harness. The runner runs every check and writes a report
//! (`report.md` and `report.json`) for the pilot go/no-go decision.
//!
//! A check passes only when the tests it names **ran** and passed:
//! - a name that matches no test is `missing` (a renamed test cannot pass silently);
//! - the cloud and sync tests skip themselves without PostgreSQL, so without
//!   `TEST_DATABASE_URL` they are `not run`, never passed;
//! - browser tests need the frontend dependencies and Chromium; without them they are
//!   `not run`.
//!
//! Any AT that is not wholly passed makes the command fail.
//!
//! Options: `--report DIR` (default `target/acceptance`), `--only AT-01,AT-06`,
//! `--kills N` for the crash harness (default 50).

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

/// Which tests of a Rust package.
#[derive(Clone, Copy)]
enum Target {
    Lib,
    Test(&'static str),
}

#[derive(Clone, Copy)]
enum Check {
    /// Rust tests in `package`/`target` whose name is `name` (or starts with it, `prefix`).
    Rust {
        package: &'static str,
        target: Target,
        name: &'static str,
        prefix: bool,
        database: bool,
        /// Also run tests marked `#[ignore]` (the nightly fault matrix belongs to AT-06).
        ignored: bool,
    },
    /// A browser test, by its title, in an `apps/desktop-ui/e2e` file.
    Browser { file: &'static str, title: &'static str },
    /// The kill-and-verify durability campaign.
    CrashHarness,
}

const fn rust(package: &'static str, test: &'static str, name: &'static str) -> Check {
    Check::Rust {
        package,
        target: Target::Test(test),
        name,
        prefix: false,
        database: false,
        ignored: false,
    }
}
const fn lib(package: &'static str, name: &'static str) -> Check {
    Check::Rust {
        package,
        target: Target::Lib,
        name,
        prefix: true,
        database: false,
        ignored: false,
    }
}
/// Rust tests against the real cloud (PostgreSQL).
const fn cloud(package: &'static str, test: &'static str, name: &'static str, prefix: bool) -> Check {
    Check::Rust {
        package,
        target: Target::Test(test),
        name,
        prefix,
        database: true,
        ignored: false,
    }
}
const fn browser(file: &'static str, title: &'static str) -> Check {
    Check::Browser { file, title }
}

struct At {
    id: &'static str,
    statement: &'static str,
    checks: &'static [Check],
}

const SUITE: &[At] = &[
    At {
        id: "AT-01",
        statement: "A new business: profile, at least 20 products and a test cash sale, with no cloud",
        checks: &[
            rust(
                "desktop-tauri",
                "permissions",
                "at01_new_business_twenty_products_and_a_cash_sale",
            ),
            rust("pos-local-db", "import", "at01_demo20_creates_twenty_products"),
            browser(
                "screens.e2e.ts",
                "completes with the keyboard only, offline, well under 3 minutes",
            ),
        ],
    },
    At {
        id: "AT-02",
        statement: "Find a product by barcode, SKU and name",
        checks: &[
            rust(
                "pos-local-db",
                "catalog",
                "create_then_find_by_barcode_sku_and_name",
            ),
            browser(
                "screens.e2e.ts",
                "search: type, arrow down, Enter adds; manual code entry uses the scan path",
            ),
        ],
    },
    At {
        id: "AT-03",
        statement: "Repeated scans increment correctly; no rapid scan is lost",
        checks: &[
            rust("pos-local-db", "cart", "ten_fast_scans_are_ten_items"),
            rust("pos-local-db", "cart", "a_repeated_delivery_counts_once"),
            rust("pos-local-db", "cart", "concurrent_scans_are_never_lost"),
            browser(
                "scanCapture.e2e.ts",
                "delivers 20 scans typed at 5 ms/key: no merges, no drops, no truncation",
            ),
        ],
    },
    At {
        id: "AT-04",
        statement: "A completed sale and all its dependent records commit atomically",
        checks: &[
            rust(
                "pos-local-db",
                "checkout",
                "a_failure_at_any_statement_leaves_nothing_behind",
            ),
            rust("pos-local-db", "checkout", "refusals_write_nothing"),
            browser(
                "screens.e2e.ts",
                "a full disk: the sale is not saved, the cashier is told, the cart is kept (E19-T02)",
            ),
            browser(
                "screens.e2e.ts",
                "a refusal keeps the cart and the panel: a card number typed as the reference",
            ),
        ],
    },
    At {
        id: "AT-05",
        statement: "A restart loses no committed sale or pending event",
        checks: &[
            rust("pos-local-db", "cart", "the_cart_survives_a_restart"),
            Check::CrashHarness,
        ],
    },
    At {
        id: "AT-06",
        statement: "An offline sale syncs exactly once despite retries",
        // Every fault scenario: the per-PR smoke subset and the nightly matrix.
        checks: &[Check::Rust {
            package: "sync-e2e",
            target: Target::Test("faults"),
            name: "at06_",
            prefix: true,
            database: true,
            ignored: true,
        }],
    },
    At {
        id: "AT-07",
        statement: "A conflicting product edit is surfaced, not overwritten",
        checks: &[
            cloud(
                "sync-e2e",
                "conflicts",
                "at07_two_registers_reprice_offline_one_conflicts_and_nothing_is_overwritten",
                false,
            ),
            browser(
                "cloud.e2e.ts",
                "a manager resolves a conflict field by field, with a custom price in pounds",
            ),
        ],
    },
    At {
        id: "AT-08",
        statement: "Preview a receipt and export it as PDF without a printer",
        checks: &[
            rust(
                "pos-receipts",
                "pdf",
                "receipt_width_pdfs_say_what_the_printed_receipt_says",
            ),
            rust("pos-receipts", "pdf", "a4_pdfs_match_their_goldens"),
            rust("pos-receipts", "pdf", "fonts_are_embedded_noto_subsets"),
            rust("desktop-tauri", "permissions", "a_receipt_pdf_over_ipc"),
            browser(
                "screens.e2e.ts",
                "saves the receipt as a PDF with no printer: receipt width or A4; Cancel saves nothing (AT-08)",
            ),
        ],
    },
    At {
        id: "AT-09",
        statement: "ESC/POS output is captured by the virtual printer",
        checks: &[
            rust(
                "pos-local-db",
                "printing",
                "prints_the_stored_receipt_and_marks_later_copies_as_reprints",
            ),
            rust("pos-receipts", "golden", "golden_documents_and_text"),
        ],
    },
    At {
        id: "AT-10",
        statement: "A return creates a linked return and refund and the correct stock movement",
        checks: &[
            rust(
                "pos-local-db",
                "returns",
                "a_return_moves_stock_by_disposition_and_commits_everything_together",
            ),
            browser(
                "screens.e2e.ts",
                "a cashier's return: quantities, refund preview, manager approval, refund receipt",
            ),
        ],
    },
    At {
        id: "AT-11",
        statement: "A cashier cannot do manager-only operations by invoking IPC directly",
        checks: &[rust(
            "desktop-tauri",
            "permissions",
            "each_role_gets_exactly_what_the_matrix_grants",
        )],
    },
    At {
        id: "AT-12",
        statement: "A business cannot read another business's cloud data",
        checks: &[cloud("cloud-api", "isolation", "at12_", true)],
    },
    At {
        id: "AT-13",
        statement: "The UI tells pending from synchronized, and external confirmation from processor approval",
        checks: &[
            lib(
                "pos-domain",
                "settlement::tests::at13_till_input_is_only_ever_operator_confirmed",
            ),
            lib("pos-application", "sync::tests::a_sale_is_marked_by_its_event"),
            rust(
                "pos-local-db",
                "sync_push",
                "the_repair_queue_lists_refused_events_and_conflicts_and_sales_show_their_mark",
            ),
            browser("cloud.e2e.ts", "the status bar shows each sync state"),
            browser(
                "cloud.e2e.ts",
                "a completed sale says sync pending until the cloud has it, then synced (AT-13)",
            ),
            browser(
                "screens.e2e.ts",
                "split: card recorded by the operator, then cash with change; tenders can be removed",
            ),
        ],
    },
];

/// What one check found.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    /// This many tests (or crash-harness kills) ran and passed.
    Passed(usize),
    Failed(String),
    /// No test has this name.
    Missing,
    NotRun(String),
}

impl Outcome {
    fn label(&self) -> String {
        match self {
            Outcome::Passed(1) => "passed".into(),
            Outcome::Passed(n) => format!("passed ({n})"),
            Outcome::Failed(why) => format!("FAILED: {why}"),
            Outcome::Missing => "MISSING: no test has this name".into(),
            Outcome::NotRun(why) => format!("NOT RUN: {why}"),
        }
    }
    fn status(&self) -> &'static str {
        match self {
            Outcome::Passed(_) => "passed",
            Outcome::Failed(_) => "failed",
            Outcome::Missing => "missing",
            Outcome::NotRun(_) => "not_run",
        }
    }
}

struct Options {
    report: PathBuf,
    only: Option<Vec<String>>,
    kills: u32,
}

fn options(root: &Path, args: &[String]) -> Result<Options> {
    let mut o = Options {
        report: root.join("target/acceptance"),
        only: None,
        kills: 50,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--report" => o.report = PathBuf::from(it.next().context("--report DIR")?),
            "--only" => {
                o.only = Some(
                    it.next()
                        .context("--only AT-01,AT-02")?
                        .split(',')
                        .map(|s| s.trim().to_uppercase())
                        .collect(),
                );
            }
            "--kills" => o.kills = it.next().context("--kills N")?.parse().context("--kills N")?,
            other => bail!("unknown option {other}"),
        }
    }
    Ok(o)
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let opts = options(root, args)?;
    let started = Instant::now();
    let suite: Vec<&At> = SUITE
        .iter()
        .filter(|at| opts.only.as_ref().is_none_or(|o| o.iter().any(|id| id == at.id)))
        .collect();
    if suite.is_empty() {
        bail!("--only matched no acceptance test");
    }
    let mut runner = Runner {
        root,
        kills: opts.kills,
        browser_files: BTreeMap::new(),
    };
    let mut results: Vec<(&At, Vec<(Check, Outcome)>)> = Vec::new();
    for at in &suite {
        let mut outcomes = Vec::new();
        for check in at.checks {
            eprintln!("{}: {}", at.id, describe(check));
            let outcome = runner.run(check);
            eprintln!("  -> {}", outcome.label());
            outcomes.push((*check, outcome));
        }
        results.push((at, outcomes));
    }
    std::fs::create_dir_all(&opts.report)?;
    let env = environment(root, started.elapsed().as_secs());
    std::fs::write(opts.report.join("report.md"), markdown(&env, &results))?;
    std::fs::write(
        opts.report.join("report.json"),
        serde_json::to_vec_pretty(&report_json(&env, &results))?,
    )?;
    let failed: Vec<&str> = results
        .iter()
        .filter(|(_, o)| o.iter().any(|(_, x)| !matches!(x, Outcome::Passed(_))))
        .map(|(at, _)| at.id)
        .collect();
    println!(
        "acceptance: {}/{} passed; report in {}",
        results.len() - failed.len(),
        results.len(),
        opts.report.display()
    );
    if !failed.is_empty() {
        bail!("not passed: {}", failed.join(", "));
    }
    Ok(())
}

struct Runner<'a> {
    root: &'a Path,
    kills: u32,
    /// Browser results per file (title → passed?), each file run once.
    browser_files: BTreeMap<&'static str, Result<BTreeMap<String, bool>, String>>,
}

impl Runner<'_> {
    fn run(&mut self, check: &Check) -> Outcome {
        match *check {
            Check::Rust {
                package,
                target,
                name,
                prefix,
                database,
                ignored,
            } => {
                if database && std::env::var_os("TEST_DATABASE_URL").is_none() {
                    return Outcome::NotRun("needs PostgreSQL (TEST_DATABASE_URL is not set)".into());
                }
                self.rust(package, target, name, prefix, ignored)
            }
            Check::Browser { file, title } => {
                let ran = self
                    .browser_files
                    .entry(file)
                    .or_insert_with(|| run_browser_file(self.root, file));
                match ran {
                    Err(why) => Outcome::NotRun(why.clone()),
                    Ok(titles) => match titles.get(title) {
                        Some(true) => Outcome::Passed(1),
                        Some(false) => Outcome::Failed("the browser test failed or was skipped".into()),
                        None => Outcome::Missing,
                    },
                }
            }
            Check::CrashHarness => self.crash_harness(),
        }
    }

    fn rust(&self, package: &str, target: Target, name: &str, prefix: bool, ignored: bool) -> Outcome {
        let mut cmd = Command::new(env!("CARGO"));
        cmd.current_dir(self.root)
            .args(["test", "--locked", "-p", package]);
        match target {
            Target::Lib => cmd.arg("--lib"),
            Target::Test(t) => cmd.args(["--test", t]),
        };
        cmd.args(["--", name]);
        if !prefix {
            cmd.arg("--exact");
        }
        if ignored {
            cmd.arg("--include-ignored");
        }
        let out = match cmd.output() {
            Ok(o) => o,
            Err(e) => return Outcome::Failed(format!("could not run cargo: {e}")),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let (mut passed, mut failed) = (0usize, Vec::new());
        for line in stdout.lines() {
            let Some(rest) = line.strip_prefix("test ") else {
                continue;
            };
            let Some((test, result)) = rest.rsplit_once(" ... ") else {
                continue;
            };
            let matches = if prefix {
                test.starts_with(name) || test.rsplit("::").next().is_some_and(|t| t.starts_with(name))
            } else {
                test == name
            };
            if !matches {
                continue;
            }
            match result.trim() {
                "ok" => passed += 1,
                _ => failed.push(format!("{test} {}", result.trim())),
            }
        }
        if !failed.is_empty() {
            return Outcome::Failed(failed.join("; "));
        }
        if !out.status.success() {
            return Outcome::Failed(format!("cargo test failed:\n{}", tail(&out)));
        }
        if passed == 0 {
            return Outcome::Missing;
        }
        Outcome::Passed(passed)
    }

    fn crash_harness(&self) -> Outcome {
        let out = Command::new(env!("CARGO"))
            .current_dir(self.root)
            .args([
                "run",
                "--quiet",
                "--locked",
                "--release",
                "-p",
                "pos-local-db",
                "--bin",
                "crash-harness",
                "--",
                "run",
                "--kills",
                &self.kills.to_string(),
            ])
            .output();
        match out {
            Err(e) => Outcome::Failed(format!("could not run cargo: {e}")),
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.contains("crash-harness: PASS") {
                    Outcome::Passed(self.kills as usize)
                } else {
                    Outcome::Failed(format!("no PASS line:\n{}", tail(&o)))
                }
            }
            Ok(o) => Outcome::Failed(tail(&o)),
        }
    }
}

/// Runs one browser test file in Chromium and returns each test's title and whether it passed.
fn run_browser_file(root: &Path, file: &str) -> Result<BTreeMap<String, bool>, String> {
    let ui = root.join("apps/desktop-ui");
    let vitest = ui.join("node_modules/vitest/vitest.mjs");
    if !vitest.exists() {
        return Err("frontend dependencies are not installed (run pnpm install)".into());
    }
    let out_file = root.join("target/acceptance/vitest").join(format!("{file}.json"));
    if let Some(dir) = out_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(&out_file);
    let out = Command::new("node")
        .current_dir(&ui)
        .arg(&vitest)
        .args([
            "run",
            "--config",
            "vitest.e2e.config.ts",
            "--reporter=json",
            "--outputFile",
        ])
        .arg(&out_file)
        .arg(format!("e2e/{file}"))
        .output()
        .map_err(|e| format!("could not run node: {e}"))?;
    let Ok(text) = std::fs::read_to_string(&out_file) else {
        return Err(format!("vitest wrote no results:\n{}", tail(&out)));
    };
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut titles = BTreeMap::new();
    for file in v["testResults"].as_array().into_iter().flatten() {
        for t in file["assertionResults"].as_array().into_iter().flatten() {
            if let Some(title) = t["title"].as_str() {
                titles.insert(title.to_owned(), t["status"] == "passed");
            }
        }
    }
    if titles.is_empty() {
        // Chromium missing, or the file did not load: nothing ran.
        return Err(format!("no browser test ran:\n{}", tail(&out)));
    }
    Ok(titles)
}

fn tail(out: &Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

fn describe(check: &Check) -> String {
    match *check {
        Check::Rust {
            package,
            target,
            name,
            prefix,
            database,
            ignored: _,
        } => {
            let target = match target {
                Target::Lib => format!("{package} (unit)"),
                Target::Test(t) => format!("{package}/tests/{t}.rs"),
            };
            let name = if prefix {
                format!("{name}*")
            } else {
                name.to_owned()
            };
            let db = if database { " [PostgreSQL]" } else { "" };
            format!("`{target}` `{name}`{db}")
        }
        Check::Browser { file, title } => format!("browser `e2e/{file}`: \"{title}\""),
        Check::CrashHarness => "crash harness (kill during writes, verify on restart)".into(),
    }
}

struct Env {
    commit: String,
    os: String,
    rustc: String,
    node: String,
    database: bool,
    finished: String,
    seconds: u64,
}

fn capture(cmd: &str, args: &[&str], dir: &Path) -> String {
    Command::new(cmd)
        .current_dir(dir)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

fn environment(root: &Path, seconds: u64) -> Env {
    let dirty = !capture("git", &["status", "--porcelain"], root).is_empty();
    let commit = capture("git", &["rev-parse", "--short=12", "HEAD"], root);
    Env {
        commit: if dirty {
            format!("{commit} (with local changes)")
        } else {
            commit
        },
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        rustc: capture("rustc", &["--version"], root),
        node: capture("node", &["--version"], root),
        database: std::env::var_os("TEST_DATABASE_URL").is_some(),
        finished: chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string(),
        seconds,
    }
}

fn verdict(outcomes: &[(Check, Outcome)]) -> &'static str {
    if outcomes.iter().all(|(_, o)| matches!(o, Outcome::Passed(_))) {
        "PASS"
    } else if outcomes
        .iter()
        .any(|(_, o)| matches!(o, Outcome::Failed(_) | Outcome::Missing))
    {
        "FAIL"
    } else {
        "INCOMPLETE"
    }
}

fn markdown(env: &Env, results: &[(&At, Vec<(Check, Outcome)>)]) -> String {
    let mut s = String::new();
    let passed = results.iter().filter(|(_, o)| verdict(o) == "PASS").count();
    let _ = writeln!(s, "# MVP acceptance report (AT-01…AT-13)\n");
    let _ = writeln!(
        s,
        "**{passed} of {} acceptance tests passed.** Generated by `cargo xtask acceptance` (E19-T12).\n",
        results.len()
    );
    let _ = writeln!(s, "| | |\n|---|---|");
    let _ = writeln!(s, "| Commit | `{}` |", env.commit);
    let _ = writeln!(s, "| Machine | {} |", env.os);
    let _ = writeln!(s, "| Rust | {} |", env.rustc);
    let _ = writeln!(s, "| Node | {} |", env.node);
    let _ = writeln!(
        s,
        "| PostgreSQL | {} |",
        if env.database {
            "yes (TEST_DATABASE_URL)"
        } else {
            "no: the cloud checks did not run"
        }
    );
    let _ = writeln!(s, "| Finished | {} ({} s) |\n", env.finished, env.seconds);
    let _ = writeln!(s, "| AT | Statement | Result |\n|---|---|---|");
    for (at, outcomes) in results {
        let _ = writeln!(s, "| {} | {} | **{}** |", at.id, at.statement, verdict(outcomes));
    }
    for (at, outcomes) in results {
        let _ = writeln!(s, "\n## {} · {}\n", at.id, at.statement);
        for (check, outcome) in outcomes {
            let label = outcome.label().replace('\n', "\n  > ");
            let _ = writeln!(s, "- {}: {}", describe(check), label);
        }
    }
    s
}

fn report_json(env: &Env, results: &[(&At, Vec<(Check, Outcome)>)]) -> Value {
    json!({
        "commit": env.commit,
        "os": env.os,
        "rustc": env.rustc,
        "node": env.node,
        "postgres": env.database,
        "finished": env.finished,
        "seconds": env.seconds,
        "acceptanceTests": results.iter().map(|(at, outcomes)| json!({
            "id": at.id,
            "statement": at.statement,
            "verdict": verdict(outcomes),
            "checks": outcomes.iter().map(|(c, o)| json!({
                "check": describe(c),
                "status": o.status(),
                "detail": o.label(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_at_is_in_the_suite_once_in_order() {
        let ids: Vec<&str> = SUITE.iter().map(|a| a.id).collect();
        let want: Vec<String> = (1..=13).map(|n| format!("AT-{n:02}")).collect();
        assert_eq!(ids, want);
        assert!(SUITE.iter().all(|a| !a.checks.is_empty()));
    }

    #[test]
    fn not_run_is_never_a_pass() {
        let c = Check::CrashHarness;
        assert_eq!(
            verdict(&[(c, Outcome::Passed(1)), (c, Outcome::NotRun("x".into()))]),
            "INCOMPLETE"
        );
        assert_eq!(
            verdict(&[(c, Outcome::Missing), (c, Outcome::NotRun("x".into()))]),
            "FAIL"
        );
        assert_eq!(verdict(&[(c, Outcome::Passed(3))]), "PASS");
    }
}
