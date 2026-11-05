//! Crash-consistency harness (E02-T04, docs/backlog/tasks/stage-0-foundations.md).
//!
//! `crash-harness run --kills N [--dir D] [--report FILE]` repeatedly starts a child
//! process that commits multi-record "sales" through `LocalStore::apply`, kills it at a
//! random moment (sometimes during startup/migration), reopens the database and checks:
//!
//! 1. `PRAGMA quick_check` is `ok`;
//! 2. no partial sale: every sale has all its records, outbox events and its audit row,
//!    or none of them;
//! 3. every sale the child acknowledged (written to an fsynced file *after* `apply`
//!    returned) is present;
//! 4. sales, receipt numbers and outbox sequences are gapless and the counters agree.
//!
//! Process kills test transaction atomicity and SQLite's WAL recovery. They do not test
//! fsync durability against power loss; that is the VM power-off campaign in E19-T02.

use pos_application::audit::{self, AuditEntry};
use pos_application::records::{BusinessRecord, LocationRecord, RegisterRecord, SettingRecord};
use pos_application::{Counter, LocalStore, NumberSlot, OutboxDraft, WriteContext, WriteSet};
use pos_domain::{BusinessId, Currency, IdGenerator, LocationId, RegisterId, Timestamp, UuidV7Generator};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LINES_PER_SALE: usize = 5;

fn ctx() -> WriteContext {
    WriteContext {
        business_id: BusinessId::from_uuid(uuid_from(1)),
        location_id: LocationId::from_uuid(uuid_from(2)),
        register_id: RegisterId::from_uuid(uuid_from(3)),
        actor: None,
        occurred_at: Timestamp::from_millis(1_767_225_600_000),
    }
}

/// Fixed IDs so every child process writes to the same business and register.
fn uuid_from(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0000 | n)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("child") => child(Path::new(&args[2]), Path::new(&args[3])),
        Some("run") => run(&args[2..]),
        _ => Err("usage: crash-harness run --kills N [--dir DIR] [--report FILE]".into()),
    };
    if let Err(e) = result {
        eprintln!("crash-harness: {e}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------- child ---

fn child(db_dir: &Path, ack_path: &Path) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let db = LocalDb::open(&DbOptions::in_dir(db_dir)).await.map_err(|e| e.to_string())?;
        let store = SqliteStore::new(db.clone());
        let ids = UuidV7Generator;
        let has_business: i64 =
            sqlx::query_scalar("SELECT count(*) FROM businesses").fetch_one(db.readers()).await.map_err(|e| e.to_string())?;
        if has_business == 0 {
            store.apply(workspace(&ids)).await.map_err(|e| e.to_string())?;
        }
        let mut next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(CAST(entity_id AS INTEGER)), 0) + 1 FROM audit_events WHERE action = 'sale.completed'",
        )
        .fetch_one(db.readers())
        .await
        .map_err(|e| e.to_string())?;
        // A previous child may have been killed mid-write, leaving an unterminated line.
        // Terminate it so it cannot merge with the next acknowledgement; the verifier
        // ignores incomplete lines (a lost acknowledgement is allowed, a false one is not).
        let existing = std::fs::read(ack_path).unwrap_or_default();
        let mut ack = std::fs::OpenOptions::new().create(true).append(true).open(ack_path).map_err(|e| e.to_string())?;
        if existing.last().is_some_and(|b| *b != b'\n') {
            ack.write_all(b"\n").map_err(|e| e.to_string())?;
        }
        println!("ready");
        let _ = std::io::stdout().flush();
        // Self-test: CRASH_HARNESS_SABOTAGE=split commits each sale in two transactions, which
        // the verifier must detect as a partial sale.
        let sabotage = std::env::var("CRASH_HARNESS_SABOTAGE").is_ok_and(|v| v == "split");
        loop {
            if sabotage {
                store.apply(sale_lines_only(&ids, next)).await.map_err(|e| e.to_string())?;
                tokio::time::sleep(Duration::from_millis(2)).await;
                let header = WriteSet::builder(ctx())
                    .allocate_number(Counter::Receipt, NumberSlot("receipt"))
                    .audit(AuditEntry::new(&ids, audit::SALE_COMPLETED, "sale", next))
                    .build()
                    .map_err(|e| e.to_string())?;
                store.apply(header).await.map_err(|e| e.to_string())?;
            } else {
                store.apply(sale(&ids, next)).await.map_err(|e| e.to_string())?;
            }
            // Acknowledge only after apply returned: this is what the UI would show.
            // One write call per acknowledgement (`writeln!` would issue two).
            ack.write_all(format!("{next}\n").as_bytes()).map_err(|e| e.to_string())?;
            ack.sync_data().map_err(|e| e.to_string())?;
            next += 1;
        }
    })
}

fn workspace(ids: &dyn IdGenerator) -> WriteSet {
    let c = ctx();
    let business = BusinessRecord {
        business_id: c.business_id,
        name: "Crash Harness Shop".into(),
        legal_name: None,
        tax_registration_no: None,
        address: serde_json::json!({}),
        phone: None,
        email: None,
        logo_path: None,
        country_code: "GB".into(),
        timezone: "Europe/London".into(),
        currency: Currency::from_iso("GBP").expect("GBP"),
        prices_include_tax: true,
        version: 1,
        created_at: c.occurred_at,
        updated_at: c.occurred_at,
    };
    let location = LocationRecord {
        location_id: c.location_id,
        business_id: c.business_id,
        name: "Main".into(),
        address: serde_json::json!({}),
        archived: false,
        version: 1,
        created_at: c.occurred_at,
        updated_at: c.occurred_at,
    };
    let register = RegisterRecord {
        register_id: c.register_id,
        business_id: c.business_id,
        location_id: c.location_id,
        name: "Till 1".into(),
        code: "R01".into(),
        receipt_prefix: String::new(),
        retired: false,
        version: 1,
        created_at: c.occurred_at,
        updated_at: c.occurred_at,
    };
    WriteSet::builder(c.clone())
        .outbox(OutboxDraft::business_updated(ids, &business).expect("payload"))
        .outbox(OutboxDraft::location_upserted(ids, &location).expect("payload"))
        .outbox(OutboxDraft::register_upserted(ids, &register).expect("payload"))
        .put_business(business, None)
        .put_location(location, None)
        .put_register(register, None)
        .audit(AuditEntry::new(
            ids,
            audit::BUSINESS_CREATED,
            "business",
            c.business_id,
        ))
        .build()
        .expect("valid workspace set")
}

/// Until the sales tables land (E09-T08), a "sale" is a multi-record write set of the same
/// shape: N records, N outbox events, a receipt number and an audit row.
fn sale(ids: &dyn IdGenerator, n: i64) -> WriteSet {
    let c = ctx();
    let mut b = WriteSet::builder(c.clone()).allocate_number(Counter::Receipt, NumberSlot("receipt"));
    for line in 0..LINES_PER_SALE {
        let rec = SettingRecord {
            business_id: c.business_id,
            key: format!("sale.{n}.line.{line}"),
            value: serde_json::json!({"qty": line + 1}),
            version: 1,
            updated_at: c.occurred_at,
        };
        b = b
            .outbox(OutboxDraft::setting_updated(ids, &rec).expect("payload"))
            .put_setting(rec, None);
    }
    b.audit(AuditEntry::new(ids, audit::SALE_COMPLETED, "sale", n))
        .build()
        .expect("valid sale set")
}

fn sale_lines_only(ids: &dyn IdGenerator, n: i64) -> WriteSet {
    let c = ctx();
    let mut b = WriteSet::builder(c.clone());
    for line in 0..LINES_PER_SALE {
        let rec = SettingRecord {
            business_id: c.business_id,
            key: format!("sale.{n}.line.{line}"),
            value: serde_json::json!({"qty": line + 1}),
            version: 1,
            updated_at: c.occurred_at,
        };
        b = b
            .outbox(OutboxDraft::setting_updated(ids, &rec).expect("payload"))
            .put_setting(rec, None);
    }
    b.audit(AuditEntry::new(ids, audit::SETTINGS_UPDATED, "sale_lines", n))
        .build()
        .expect("valid set")
}

// --------------------------------------------------------------- parent ---

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Default)]
struct Totals {
    kills: usize,
    killed_before_ready: usize,
    sales: i64,
    acked: usize,
}

fn run(args: &[String]) -> Result<(), String> {
    let mut kills = 100usize;
    let mut dir: Option<PathBuf> = None;
    let mut report: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--kills" => kills = it.next().and_then(|v| v.parse().ok()).ok_or("--kills N")?,
            "--dir" => dir = it.next().map(PathBuf::from),
            "--report" => report = it.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let dir =
        dir.unwrap_or_else(|| std::env::temp_dir().join(format!("sale-os-crash-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ack = dir.join("acked.txt");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(42)
        | 1;
    let mut rng = Rng(seed);
    let started = Instant::now();
    let mut totals = Totals::default();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    println!("crash-harness: {kills} kills, dir {}, seed {seed}", dir.display());
    for k in 1..=kills {
        let mut child = Command::new(&exe)
            .args(["child"])
            .arg(&dir)
            .arg(&ack)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        // Half the time kill relative to spawn (may land during open/migration), otherwise
        // after the child reports it is committing.
        if rng.below(2) == 0 {
            std::thread::sleep(Duration::from_millis(rng.below(120)));
        } else {
            let stdout = child.stdout.take().ok_or("no stdout")?;
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            std::thread::sleep(Duration::from_millis(1 + rng.below(150)));
        }
        let exited_early = child.try_wait().map_err(|e| e.to_string())?;
        if let Some(status) = exited_early {
            let mut err = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut s, &mut err);
            }
            return Err(format!("child exited by itself ({status}) at kill {k}: {err}"));
        }
        child.kill().map_err(|e| e.to_string())?; // SIGKILL on Unix, TerminateProcess on Windows
        let _ = child.wait();
        totals.kills += 1;

        let verdict = rt
            .block_on(verify(&dir, &ack))
            .map_err(|e| format!("kill {k}: {e}"))?;
        if verdict.sales == 0 && verdict.acked == 0 {
            totals.killed_before_ready += 1;
        }
        totals.sales = verdict.sales;
        totals.acked = verdict.acked;
        if k % 50 == 0 || k == kills {
            println!(
                "  {k}/{kills} kills ok: {} sales committed, {} acknowledged",
                verdict.sales, verdict.acked
            );
        }
    }
    let elapsed = started.elapsed();
    let summary = format!(
        "{} kills, 0 violations; {} sales committed ({} acknowledged), {} lines each; {:.1}s; seed {seed}; {} {}",
        totals.kills,
        totals.sales,
        totals.acked,
        LINES_PER_SALE,
        elapsed.as_secs_f64(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("crash-harness: PASS: {summary}");
    if let Some(path) = report {
        std::fs::write(&path, format!("{summary}\n")).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

struct Verdict {
    sales: i64,
    acked: usize,
}

async fn verify(dir: &Path, ack: &Path) -> Result<Verdict, String> {
    let db_path = dir.join("pos.db");
    if !db_path.exists() {
        return Ok(Verdict { sales: 0, acked: 0 });
    }
    let db = LocalDb::open(&DbOptions::in_dir(dir))
        .await
        .map_err(|e| format!("reopen failed: {e}"))?;
    let r = db.readers();
    let e = |e: sqlx::Error| e.to_string();

    let check: Vec<String> = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_all(r)
        .await
        .map_err(e)?;
    if check != ["ok"] {
        return Err(format!("quick_check: {check:?}"));
    }

    // 2. All-or-nothing per sale.
    let sales: Vec<i64> = sqlx::query_scalar(
        "SELECT CAST(entity_id AS INTEGER) FROM audit_events WHERE action = 'sale.completed' ORDER BY 1",
    )
    .fetch_all(r)
    .await
    .map_err(e)?;
    let lines: Vec<(String,)> = sqlx::query_as("SELECT key FROM business_settings WHERE key LIKE 'sale.%'")
        .fetch_all(r)
        .await
        .map_err(e)?;
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM outbox_events WHERE aggregate_id LIKE 'sale.%'")
            .fetch_one(r)
            .await
            .map_err(e)?;
    let expected = sales.len() * LINES_PER_SALE;
    if lines.len() != expected || usize::try_from(events).unwrap_or(0) != expected {
        return Err(format!(
            "partial sale: {} sales but {} line records and {events} line events (expected {expected})",
            sales.len(),
            lines.len()
        ));
    }
    for (key,) in &lines {
        let n: i64 = key
            .split('.')
            .nth(1)
            .and_then(|v| v.parse().ok())
            .ok_or("bad key")?;
        if sales.binary_search(&n).is_err() {
            return Err(format!("line {key} exists without its sale"));
        }
    }

    // 4. Gapless sales and receipt numbers.
    for (i, n) in sales.iter().enumerate() {
        if *n != i as i64 + 1 {
            return Err(format!("sale sequence gap at {n}"));
        }
    }
    let receipt: Option<i64> =
        sqlx::query_scalar("SELECT value FROM register_counters WHERE name = 'receipt_seq'")
            .fetch_optional(r)
            .await
            .map_err(e)?;
    if receipt.unwrap_or(0) != sales.len() as i64 {
        return Err(format!("receipt counter {receipt:?} != {} sales", sales.len()));
    }
    let (count, max, counter): (i64, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT count(*), max(local_sequence), (SELECT value FROM register_counters WHERE name = 'outbox_seq')
         FROM outbox_events",
    )
    .fetch_one(r)
    .await
    .map_err(e)?;
    if max.unwrap_or(0) != count || counter.unwrap_or(0) != count {
        return Err(format!(
            "outbox not gapless: count {count}, max {max:?}, counter {counter:?}"
        ));
    }

    // 3. Every acknowledged sale is present.
    let text = std::fs::read_to_string(ack).unwrap_or_default();
    // Only newline-terminated lines are complete acknowledgements.
    let complete = match text.rfind('\n') {
        Some(i) => &text[..i],
        None => "",
    };
    let acked: Vec<i64> = complete.lines().filter_map(|l| l.trim().parse().ok()).collect();
    if let Some(missing) = acked.iter().find(|n| sales.binary_search(n).is_err()) {
        return Err(format!("acknowledged sale {missing} is missing after the crash"));
    }
    db.close().await;
    Ok(Verdict {
        sales: sales.len() as i64,
        acked: acked.len(),
    })
}
