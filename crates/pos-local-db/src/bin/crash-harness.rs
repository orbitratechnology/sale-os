//! Crash-consistency harness (E02-T04, and AT-05 for E09-T08).
//!
//! `crash-harness run --kills N [--dir D] [--report FILE]` repeatedly starts a child
//! process that sells through the **real** services (set up a shop once, then scan items
//! into the cart and complete the sale with `CheckoutService`), kills it at a random moment
//! (sometimes during startup or migration), reopens the database and checks:
//!
//! 1. `PRAGMA quick_check` is `ok`;
//! 2. no partial sale: every sale has its lines, tenders, stock movements, `sale.completed`
//!    event (carrying its receipt number) and audit row, and nothing exists without its sale;
//! 3. every sale the child acknowledged (written to an fsynced file *after*
//!    `checkout_complete` returned: what the cashier saw as "completed") is present with its
//!    pending outbox event (AT-05);
//! 4. receipt numbers and outbox sequences are gapless and the counters agree;
//! 5. every stock balance equals the sum of its movements, and at most one cart is open.
//!
//! Process kills test transaction atomicity and SQLite's WAL recovery. They do not test
//! fsync durability against power loss; that is the VM power-off campaign in E19-T02.

use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::context::{Actor, CommandContext};
use pos_application::queries::{CatalogQueries, SetupQueries};
use pos_application::scan::ScanSource;
use pos_application::setup::{CreateWorkspace, NewTaxRate, SetupService};
use pos_application::{Counter, LocalStore, NumberSlot, WriteSet};
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::staff::Pin;
use pos_domain::{
    Clock, IdGenerator, Money, RatePpm, Role, ScanEventId, SessionId, SystemClock, UserId, UuidV7Generator,
};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// SKU, name, price, stock item?
const PRODUCTS: [(&str, &str, i64, bool); 3] = [
    ("TEA", "Tea", 250, true),
    ("MUG", "Mug", 800, true),
    ("WRAP", "Gift wrap", 100, false),
];

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
        let s = |e: &dyn std::fmt::Display| e.to_string();
        let db = LocalDb::open(&DbOptions::in_dir(db_dir))
            .await
            .map_err(|e| s(&e))?;
        let store = SqliteStore::new(db.clone());
        let a = Arc::new(store.clone());
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ids: Arc<dyn IdGenerator> = Arc::new(UuidV7Generator);
        // Cheap Argon2: the harness measures the write path, not PIN hashing.
        let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));

        // Set up once (one transaction, so a kill leaves all or nothing).
        if a.register_context().await.map_err(|e| s(&e))?.is_none() {
            let setup = SetupService {
                store: a.clone(),
                setup: a.clone(),
                hasher,
                clock: clock.clone(),
                ids: ids.clone(),
            };
            setup
                .create_workspace(CreateWorkspace {
                    business_name: "Crash Harness Shop".into(),
                    legal_name: None,
                    address: serde_json::json!({}),
                    phone: None,
                    email: None,
                    country_code: "GB".into(),
                    timezone: "Europe/London".into(),
                    currency_code: "GBP".into(),
                    prices_include_tax: true,
                    location_name: "Main".into(),
                    register_name: "Till 1".into(),
                    owner_name: "Owner".into(),
                    owner_pin: Pin::parse("1234").map_err(|e| s(&e))?,
                    tax_rates: vec![NewTaxRate {
                        name: "VAT 20%".into(),
                        rate: RatePpm::new(200_000).map_err(|e| s(&e))?,
                    }],
                })
                .await
                .map_err(|e| s(&e))?;
        }
        let register = a
            .register_context()
            .await
            .map_err(|e| s(&e))?
            .ok_or("no register after setup")?;
        let owner: String =
            sqlx::query_scalar("SELECT user_id FROM staff_users WHERE role = 'owner' LIMIT 1")
                .fetch_one(db.readers())
                .await
                .map_err(|e| s(&e))?;
        let ctx = CommandContext {
            register,
            actor: Some(Actor {
                user_id: owner.parse::<UserId>().map_err(|e| s(&e))?,
                role: Role::Owner,
                session_id: SessionId::generate(ids.as_ref()),
            }),
            clock,
            ids: ids.clone(),
            approval: None,
        };

        // Products, each in its own transaction: create whichever a killed child did not.
        let catalog = CatalogService {
            store: a.clone(),
            catalog: a.clone(),
        };
        let tax: String =
            sqlx::query_scalar("SELECT tax_category_id FROM tax_categories WHERE is_default = 1")
                .fetch_one(db.readers())
                .await
                .map_err(|e| s(&e))?;
        for (sku, name, price, stock) in PRODUCTS {
            if a.sku_owner(sku).await.map_err(|e| s(&e))?.is_none() {
                let mut v = VariantInput::simple(Money::from_minor(price));
                v.sku = Some(sku.into());
                catalog
                    .create(
                        &ctx,
                        ProductInput {
                            name: name.into(),
                            description: None,
                            category_id: None,
                            product_type: if stock {
                                ProductType::Stock
                            } else {
                                ProductType::Service
                            },
                            unit: Unit::Ea,
                            tax_category_id: tax.parse().map_err(|e| s(&e))?,
                            quick_select: false,
                            variants: vec![v],
                        },
                    )
                    .await
                    .map_err(|e| s(&e))?;
            }
        }

        let carts = Arc::new(CartService::new(a.clone(), a.clone(), a.clone()));
        let checkout = CheckoutService {
            carts: carts.clone(),
            store: a.clone(),
            setup: a.clone(),
            stock: a.clone(),
        };

        // A previous child may have been killed mid-write, leaving an unterminated line.
        // Terminate it so it cannot merge with the next acknowledgement; the verifier
        // ignores incomplete lines (a lost acknowledgement is allowed, a false one is not).
        let existing = std::fs::read(ack_path).unwrap_or_default();
        let mut ack = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(ack_path)
            .map_err(|e| s(&e))?;
        if existing.last().is_some_and(|b| *b != b'\n') {
            ack.write_all(b"\n").map_err(|e| s(&e))?;
        }
        println!("ready");
        let _ = std::io::stdout().flush();
        // Self-test: CRASH_HARNESS_SABOTAGE=orphan allocates a receipt number outside a sale
        // after each sale; the verifier must report the gap.
        let sabotage = std::env::var("CRASH_HARNESS_SABOTAGE").is_ok_and(|v| v == "orphan");
        let mut n: usize = 0;
        loop {
            // 1-3 lines per sale (a killed child's open cart is simply carried on).
            for (sku, ..) in PRODUCTS.iter().take(1 + n % 3) {
                let id = ScanEventId::generate(ids.as_ref());
                carts
                    .scan(&ctx, sku, id, ScanSource::Manual)
                    .await
                    .map_err(|e| s(&e))?;
            }
            let view = carts.current(&ctx).await.map_err(|e| s(&e))?;
            let paid = Money::from_minor(view.priced.total.minor() + 500);
            let done = checkout
                .complete_sale(
                    &ctx,
                    CompleteSale {
                        cart_id: view.cart.id,
                        revision: view.cart.revision,
                        tenders: vec![TenderInput {
                            kind: TenderKind::Cash,
                            amount: paid,
                            external_reference: None,
                        }],
                    },
                )
                .await
                .map_err(|e| s(&e))?;
            // Acknowledge only after the command returned: this is what the UI shows.
            // One write call per acknowledgement (`writeln!` would issue two).
            ack.write_all(format!("{}\n", done.receipt_number).as_bytes())
                .map_err(|e| s(&e))?;
            ack.sync_data().map_err(|e| s(&e))?;
            if sabotage {
                let set = WriteSet::builder(ctx.write_context())
                    .allocate_number(Counter::Receipt, NumberSlot("orphan"))
                    .build()
                    .map_err(|e| s(&e))?;
                store.apply(set).await.map_err(|e| s(&e))?;
            }
            n += 1;
        }
    })
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
    sales: i64,
    lines: i64,
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
        // Half the time kill relative to spawn (may land during open, migration or setup),
        // otherwise after the child reports it is selling.
        if rng.below(2) == 0 {
            std::thread::sleep(Duration::from_millis(rng.below(150)));
        } else {
            let stdout = child.stdout.take().ok_or("no stdout")?;
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            std::thread::sleep(Duration::from_millis(1 + rng.below(200)));
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
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
        totals.sales = verdict.sales;
        totals.lines = verdict.lines;
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
        "{} kills, 0 violations; {} real sales committed ({} lines, {} acknowledged); {:.1}s; seed {seed}; {} {}",
        totals.kills,
        totals.sales,
        totals.lines,
        totals.acked,
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
    lines: i64,
    acked: usize,
}

/// A query that must return 0 for a consistent database, with what it means otherwise.
const MUST_BE_ZERO: &[(&str, &str)] = &[
    ("sale without lines", "SELECT count(*) FROM sales s WHERE NOT EXISTS (SELECT 1 FROM sale_lines l WHERE l.sale_id = s.sale_id)"),
    ("sale without tenders", "SELECT count(*) FROM sales s WHERE NOT EXISTS (SELECT 1 FROM tenders t WHERE t.sale_id = s.sale_id)"),
    (
        "tenders that do not add up to the total",
        "SELECT count(*) FROM sales s WHERE total_minor != (SELECT sum(amount_minor) FROM tenders t WHERE t.sale_id = s.sale_id)",
    ),
    (
        "lines that do not add up to the total",
        "SELECT count(*) FROM sales s WHERE total_minor != rounding_minor + (SELECT sum(total_minor) FROM sale_lines l WHERE l.sale_id = s.sale_id)",
    ),
    (
        "sale without its sale.completed event (or a wrong receipt number in it)",
        "SELECT count(*) FROM sales s WHERE NOT EXISTS (SELECT 1 FROM outbox_events o WHERE o.event_type = 'sale.completed'
           AND o.aggregate_id = s.sale_id AND json_extract(o.payload_json, '$.receiptNumber') = s.receipt_number)",
    ),
    (
        "sale.completed event without its sale",
        "SELECT count(*) FROM outbox_events o WHERE o.event_type = 'sale.completed'
           AND NOT EXISTS (SELECT 1 FROM sales s WHERE s.sale_id = o.aggregate_id)",
    ),
    (
        "sale without its audit row",
        "SELECT count(*) FROM sales s WHERE NOT EXISTS (SELECT 1 FROM audit_events a WHERE a.action = 'sale.completed' AND a.entity_id = s.sale_id)",
    ),
    (
        "audit row without its sale",
        "SELECT count(*) FROM audit_events a WHERE a.action = 'sale.completed' AND NOT EXISTS (SELECT 1 FROM sales s WHERE s.sale_id = a.entity_id)",
    ),
    (
        "stock-tracked line without its movement",
        "SELECT count(*) FROM sale_lines l WHERE l.product_type = 'stock'
           AND NOT EXISTS (SELECT 1 FROM stock_movements m WHERE m.source_type = 'sale' AND m.source_line_id = l.sale_line_id)",
    ),
    (
        "movement without its sale",
        "SELECT count(*) FROM stock_movements m WHERE m.source_type = 'sale' AND NOT EXISTS (SELECT 1 FROM sales s WHERE s.sale_id = m.source_id)",
    ),
    (
        "balance that differs from the sum of its movements",
        "SELECT count(*) FROM stock_balances b WHERE on_hand_milli != (SELECT coalesce(sum(quantity_delta_milli), 0) FROM stock_movements m
           WHERE m.location_id = b.location_id AND m.variant_id = b.variant_id AND m.bucket = b.bucket)",
    ),
    (
        "completed cart without its sale",
        "SELECT count(*) FROM carts c WHERE c.status = 'completed' AND NOT EXISTS (SELECT 1 FROM sales s WHERE s.sale_id = c.sale_id)",
    ),
    ("sale whose cart is not completed", "SELECT count(*) FROM sales s JOIN carts c ON c.cart_id = s.cart_id WHERE c.status != 'completed'"),
    ("more than one open cart", "SELECT max(0, count(*) - 1) FROM carts WHERE status = 'open'"),
];

async fn verify(dir: &Path, ack: &Path) -> Result<Verdict, String> {
    if !dir.join("pos.db").exists() {
        return Ok(Verdict {
            sales: 0,
            lines: 0,
            acked: 0,
        });
    }
    let db = LocalDb::open(&DbOptions::in_dir(dir))
        .await
        .map_err(|e| format!("reopen failed: {e}"))?;
    let r = db.readers();
    let e = |e: sqlx::Error| e.to_string();

    // 1. Integrity.
    let check: Vec<String> = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_all(r)
        .await
        .map_err(e)?;
    if check != ["ok"] {
        return Err(format!("quick_check: {check:?}"));
    }

    // 2 and 5. All-or-nothing per sale; projections agree.
    for (what, sql) in MUST_BE_ZERO {
        let n: i64 = sqlx::query_scalar(sql).fetch_one(r).await.map_err(e)?;
        if n != 0 {
            return Err(format!("{what}: {n}"));
        }
    }

    // 4. Gapless receipt numbers and outbox.
    let seqs: Vec<i64> = sqlx::query_scalar("SELECT receipt_seq FROM sales ORDER BY receipt_seq")
        .fetch_all(r)
        .await
        .map_err(e)?;
    if let Some((i, s)) = seqs.iter().enumerate().find(|(i, s)| **s != *i as i64 + 1) {
        return Err(format!("receipt number gap: position {} holds {s}", i + 1));
    }
    let counter: Option<i64> =
        sqlx::query_scalar("SELECT value FROM register_counters WHERE name = 'receipt_seq'")
            .fetch_optional(r)
            .await
            .map_err(e)?;
    if counter.unwrap_or(0) != seqs.len() as i64 {
        return Err(format!(
            "receipt counter {counter:?} != {} sales: a number was used outside a sale",
            seqs.len()
        ));
    }
    let (count, max, outbox_counter): (i64, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT count(*), max(local_sequence), (SELECT value FROM register_counters WHERE name = 'outbox_seq') FROM outbox_events",
    )
    .fetch_one(r)
    .await
    .map_err(e)?;
    if max.unwrap_or(0) != count || outbox_counter.unwrap_or(0) != count {
        return Err(format!(
            "outbox not gapless: count {count}, max {max:?}, counter {outbox_counter:?}"
        ));
    }

    // 3. AT-05: every acknowledged sale is present, with its event still pending sync.
    let text = std::fs::read_to_string(ack).unwrap_or_default();
    // Only newline-terminated lines are complete acknowledgements.
    let complete = text.rfind('\n').map_or("", |i| &text[..i]);
    let acked: Vec<&str> = complete
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    for receipt in &acked {
        let found: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sales s JOIN outbox_events o ON o.aggregate_id = s.sale_id AND o.event_type = 'sale.completed'
             WHERE s.receipt_number = ?",
        )
        .bind(receipt)
        .fetch_one(r)
        .await
        .map_err(e)?;
        if found != 1 {
            return Err(format!(
                "acknowledged sale {receipt} is missing after the crash (or its event is)"
            ));
        }
    }
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM sale_lines")
        .fetch_one(r)
        .await
        .map_err(e)?;
    db.close().await;
    Ok(Verdict {
        sales: seqs.len() as i64,
        lines,
        acked: acked.len(),
    })
}
