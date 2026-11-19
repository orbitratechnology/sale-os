//! Crash-consistency harness (E02-T04, AT-05 for E09-T08, extended by E19-T02).
//!
//! `crash-harness run --kills N [--dir D] [--report FILE]` repeatedly starts a child
//! process that trades through the **real** services, kills it at a random moment
//! (sometimes during startup), reopens the database and checks it. The child sets up a shop
//! once, then loops over: sales (scan into the cart, `CheckoutService`), returns of part of a
//! recent sale (`ReturnService`), goods receipts (`StockService`), the cloud acknowledging the
//! outbox (the real `SyncWorker` against an in-process cloud that accepts everything), and
//! now and then a 1,200-product CSV import (`ImportService`, three chunks), which the next
//! child resumes if a kill interrupted it.
//!
//! `crash-harness disk-full --dir D` (D on a small file system, such as a 16 MB tmpfs or VHD)
//! trades until the disk is full, then checks the failure was reported as a full disk and the
//! database is whole, and that trading carries on once there is space.
//!
//! `crash-harness migrate --kills N` kills the app while it upgrades a populated database
//! (the last two migrations, including a data migration) and checks the database is either
//! untouched or fully upgraded, every time, with nothing lost.
//!
//! After each kill `run` checks:
//!
//! 1. `PRAGMA quick_check` is `ok`;
//! 2. no partial sale: every sale has its lines, tenders, stock movements, `sale.completed`
//!    event (carrying its receipt number) and audit row, and nothing exists without its sale;
//! 3. every sale the child acknowledged (written to an fsynced file *after*
//!    `checkout_complete` returned: what the cashier saw as "completed") is present with its
//!    pending outbox event (AT-05);
//! 4. receipt numbers and outbox sequences are gapless and the counters agree;
//! 5. every stock balance equals the sum of its movements, and at most one cart is open;
//! 6. returns, stock documents, imports and acknowledgements are each all-or-nothing, and
//!    every one the child reported done is there (see `MUST_BE_ZERO` and `verify`).
//!
//! Process kills test transaction atomicity and SQLite's WAL recovery. They do not test
//! fsync durability against power loss; that is the VM power-off campaign in E19-T02.

use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::context::{Actor, CommandContext};
use pos_application::import::{ImportService, OnDuplicate};
use pos_application::queries::{CatalogQueries, SetupQueries};
use pos_application::returns::{CreateReturn, ReturnService};
use pos_application::scan::ScanSource;
use pos_application::setup::{CreateWorkspace, NewTaxRate, SetupService};
use pos_application::stock::StockService;
use pos_application::sync::{OsRandom, SyncTransport, SyncWorker, TransportError};
use pos_application::{Counter, LocalStore, NumberSlot, WriteSet};
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::returns::{Disposition, ReturnLineRequest, ReturnReason};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::staff::Pin;
use pos_domain::stock_docs::{DocumentDraft, DocumentLine, DocumentType};
use pos_domain::{
    Clock, IdGenerator, Money, RatePpm, Role, ScanEventId, SessionId, SystemClock, UserId, UuidV7Generator,
};
use pos_domain::{ImportJobId, Quantity};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_sync::{EventResult, PushRequest, PushResponse, ResultStatus};
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

/// The database as the app has it: encrypted (ADR-023), here with a fixed test key so the
/// parent can open what the killed child wrote.
fn harness_opts(dir: &std::path::Path) -> DbOptions {
    DbOptions::in_dir(dir).with_key(pos_local_db::crypto::DbKey::from_bytes([0x5A; 32]))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("child") => child(Path::new(&args[2]), Path::new(&args[3])),
        Some("run") => run(&args[2..]),
        Some("migrate-child") => migrate_child(Path::new(&args[2])),
        Some("migrate") => migrate_campaign(&args[2..]),
        Some("disk-full") => disk_full(&args[2..]),
        _ => Err(
            "usage: crash-harness run|migrate --kills N [--dir DIR] [--report FILE] | disk-full --dir DIR_ON_A_SMALL_DISK"
                .into(),
        ),
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
        let db = LocalDb::open(&harness_opts(db_dir))
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
            receipts: a.clone(),
        };
        let returns = ReturnService {
            store: a.clone(),
            returns: a.clone(),
            setup: a.clone(),
            receipts: a.clone(),
            pricing: a.clone(),
        };
        let stock = StockService {
            store: a.clone(),
            carts: a.clone(),
            stock: a.clone(),
        };
        let imports = ImportService {
            store: a.clone(),
            imports: a.clone(),
            catalog: a.clone(),
            config: a.clone(),
            setup: a.clone(),
        };
        // The register is linked to a cloud that accepts everything, so acknowledgements are
        // written by the real worker.
        sqlx::query(
            "INSERT INTO sync_state (id, link_state) VALUES (1, 'linked')
             ON CONFLICT (id) DO UPDATE SET link_state = 'linked'",
        )
        .execute(db.writer())
        .await
        .map_err(|e| s(&e))?;
        let worker = SyncWorker::new(a.clone(), Arc::new(AcceptAll), ctx.clock.clone(), Arc::new(OsRandom));
        let tea = a
            .sku_owner("TEA")
            .await
            .map_err(|e| s(&e))?
            .ok_or("no TEA")?;
        let tea_variant: String = sqlx::query_scalar("SELECT variant_id FROM product_variants WHERE product_id = ?")
            .bind(tea.to_string())
            .fetch_one(db.readers())
            .await
            .map_err(|e| s(&e))?;

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
        // One write call per acknowledgement (`writeln!` would issue two), then fsync: what
        // the cashier saw as done.
        let mut acknowledge = |kind: &str, what: &str| -> Result<(), String> {
            ack.write_all(format!("{kind} {what}\n").as_bytes())
                .map_err(|e| e.to_string())?;
            ack.sync_data().map_err(|e| e.to_string())
        };

        // An import a killed child left half-committed is carried on, as the app does; one
        // it left validated is committed, as the owner finishing the wizard would.
        let open_jobs: Vec<String> =
            sqlx::query_scalar("SELECT job_id FROM import_jobs WHERE status IN ('committing', 'validated')")
                .fetch_all(db.readers())
                .await
                .map_err(|e| s(&e))?;
        for job in open_jobs {
            let id: ImportJobId = job.parse().map_err(|e| s(&e))?;
            imports.commit(&ctx, id).await.map_err(|e| s(&e))?;
            acknowledge("I", &job)?;
        }

        // The migration campaign's template wants a big catalogue: CRASH_HARNESS_SEED_IMPORTS=N
        // commits N imports of 1,200 products first.
        let seed_imports: usize = std::env::var("CRASH_HARNESS_SEED_IMPORTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        for i in 0..seed_imports {
            let mut csv = String::from(
                "name,variant,sku,barcode,price,cost,unit,tax_category,track_stock,low_stock_threshold\n",
            );
            for j in 0..1200 {
                csv.push_str(&format!("Seeded {i}-{j},,IMP-S{i}-{j},,1.00,,ea,Standard,yes,\n"));
            }
            let preview = imports
                .start(&ctx, &format!("seed-{i}.csv"), csv.as_bytes())
                .await
                .map_err(|e| s(&e))?;
            imports
                .validate(&ctx, preview.job_id, preview.mapping.clone(), OnDuplicate::Error)
                .await
                .map_err(|e| s(&e))?;
            imports.commit(&ctx, preview.job_id).await.map_err(|e| s(&e))?;
            acknowledge("I", &preview.job_id.to_string())?;
        }

        println!("ready");
        let _ = std::io::stdout().flush();
        // Self-test: CRASH_HARNESS_SABOTAGE=orphan allocates a receipt number outside a sale
        // after each sale; the verifier must report the gap.
        let sabotage = std::env::var("CRASH_HARNESS_SABOTAGE").is_ok_and(|v| v == "orphan");
        let mut n: usize = std::process::id() as usize;
        loop {
            n += 1;
            match n % 10 {
                // A return of one unit of the latest sale's first stock line still returnable.
                6 => {
                    let latest: Option<String> = sqlx::query_scalar(
                        "SELECT receipt_number FROM sales ORDER BY receipt_seq DESC LIMIT 1",
                    )
                    .fetch_optional(db.readers())
                    .await
                    .map_err(|e| s(&e))?;
                    let Some(receipt) = latest else { continue };
                    let sale = returns.find(&ctx, &receipt).await.map_err(|e| s(&e))?;
                    let Some(line) = sale.lines.iter().find(|l| l.track_stock && l.available().milli() >= 1000)
                    else {
                        continue;
                    };
                    let lines = vec![ReturnLineRequest {
                        sale_line_id: line.sale_line_id,
                        quantity: Quantity::from_milli(1000),
                        disposition: Disposition::Restock,
                    }];
                    let plan = returns
                        .preview(&ctx, sale.sale_id, &lines)
                        .await
                        .map_err(|e| s(&e))?;
                    let done = returns
                        .create(
                            &ctx,
                            CreateReturn {
                                sale_id: sale.sale_id,
                                lines,
                                reason: ReturnReason::ChangedMind,
                                note: None,
                                refunds: vec![TenderInput {
                                    kind: TenderKind::Cash,
                                    amount: plan.total,
                                    external_reference: None,
                                }],
                            },
                        )
                        .await
                        .map_err(|e| s(&e))?;
                    acknowledge("R", &done.return_number)?;
                }
                // A goods receipt of five teas.
                7 => {
                    let posted = stock
                        .post(
                            &ctx,
                            DocumentDraft {
                                doc_type: DocumentType::Receipt,
                                reason: None,
                                supplier_name: Some("Harness Supplies".into()),
                                reference: None,
                                note: None,
                                lines: vec![DocumentLine {
                                    variant_id: tea_variant.parse().map_err(|e| s(&e))?,
                                    quantity: Quantity::from_milli(5000),
                                    unit_cost: None,
                                }],
                            },
                        )
                        .await
                        .map_err(|e| s(&e))?;
                    acknowledge("D", &posted.document_no)?;
                }
                // The cloud acknowledges what is pending.
                8 => {
                    worker.push_once().await.map_err(|e| s(&e))?;
                    let acked: i64 = sqlx::query_scalar("SELECT last_ack_sequence FROM sync_state WHERE id = 1")
                        .fetch_one(db.readers())
                        .await
                        .map_err(|e| s(&e))?;
                    acknowledge("A", &acked.to_string())?;
                }
                // Now and then (one step in a hundred), an import of 1,200 products (three chunks).
                9 if n % 100 == 9 => {
                    let mut csv = String::from(
                        "name,variant,sku,barcode,price,cost,unit,tax_category,track_stock,low_stock_threshold\n",
                    );
                    for i in 0..1200 {
                        csv.push_str(&format!("Imported {n}-{i},,IMP-{n}-{i},,1.00,,ea,Standard,yes,\n"));
                    }
                    let preview = imports
                        .start(&ctx, &format!("harness-{n}.csv"), csv.as_bytes())
                        .await
                        .map_err(|e| s(&e))?;
                    imports
                        .validate(&ctx, preview.job_id, preview.mapping.clone(), OnDuplicate::Error)
                        .await
                        .map_err(|e| s(&e))?;
                    imports
                        .commit(&ctx, preview.job_id)
                        .await
                        .map_err(|e| s(&e))?;
                    acknowledge("I", &preview.job_id.to_string())?;
                }
                // Otherwise a sale of 1 to 3 lines (a killed child's open cart is carried on).
                _ => {
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
                    acknowledge("S", &done.receipt_number)?;
                    if sabotage {
                        let set = WriteSet::builder(ctx.write_context())
                            .allocate_number(Counter::Receipt, NumberSlot("orphan"))
                            .build()
                            .map_err(|e| s(&e))?;
                        store.apply(set).await.map_err(|e| s(&e))?;
                    }
                }
            }
        }
    })
}

/// A cloud that accepts every event it is sent.
struct AcceptAll;

#[async_trait::async_trait]
impl SyncTransport for AcceptAll {
    async fn push(&self, request: PushRequest) -> Result<PushResponse, TransportError> {
        Ok(PushResponse {
            ack_sequence: request.events.last().map_or(0, |e| e.local_sequence),
            server_time: SystemClock.now().to_rfc3339(),
            results: request
                .events
                .iter()
                .map(|e| EventResult::new(e.event_id, e.local_sequence, ResultStatus::Applied))
                .collect(),
        })
    }
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
    done: Done,
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
            // Mostly soon; one in four up to 3 s, so imports (over a second) get to commit,
            // and some are killed mid-commit.
            let wait = if rng.below(4) == 0 { 3000 } else { 200 };
            std::thread::sleep(Duration::from_millis(1 + rng.below(wait)));
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
        totals.done = verdict.done;
        if k % 50 == 0 || k == kills {
            println!(
                "  {k}/{kills} kills ok: {} sales committed, {} acknowledged",
                verdict.sales, verdict.acked
            );
        }
    }
    let elapsed = started.elapsed();
    let summary = format!(
        "{} kills, 0 violations; {} real sales committed ({} lines, {} acknowledged), {} returns, {} stock documents, \
         {} imports of 1,200 products, {} events acknowledged by the cloud; {:.1}s; seed {seed}; {} {}",
        totals.kills,
        totals.sales,
        totals.lines,
        totals.acked,
        totals.done.returns,
        totals.done.documents,
        totals.done.imports,
        totals.done.acknowledged_events,
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
    done: Done,
}

/// What else the child got done (committed, whether or not acknowledged).
#[derive(Default, Clone, Copy)]
struct Done {
    returns: i64,
    documents: i64,
    imports: i64,
    acknowledged_events: i64,
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
        "SELECT count(*) FROM sales s WHERE NOT EXISTS (SELECT 1 FROM outbox_events o WHERE o.aggregate_type = 'sale' AND o.event_type = 'sale.completed'
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
    // Returns (E19-T02): each with its lines, refunds that add up, event, audit row and the
    // stock it put back; nothing of a return without the return.
    ("return without lines", "SELECT count(*) FROM returns r WHERE NOT EXISTS (SELECT 1 FROM return_lines l WHERE l.return_id = r.return_id)"),
    (
        "refunds that do not add up to the return",
        "SELECT count(*) FROM returns r WHERE total_minor != (SELECT coalesce(sum(amount_minor), 0) FROM refunds f WHERE f.return_id = r.return_id)",
    ),
    (
        "return without its return.completed event (or a wrong number in it)",
        "SELECT count(*) FROM returns r WHERE NOT EXISTS (SELECT 1 FROM outbox_events o WHERE o.aggregate_type = 'return' AND o.event_type = 'return.completed'
           AND o.aggregate_id = r.return_id AND json_extract(o.payload_json, '$.receiptNumber') = r.return_number)",
    ),
    (
        "return.completed event without its return",
        "SELECT count(*) FROM outbox_events o WHERE o.event_type = 'return.completed' AND NOT EXISTS (SELECT 1 FROM returns r WHERE r.return_id = o.aggregate_id)",
    ),
    ("return without its audit row", "SELECT count(*) FROM returns r WHERE NOT EXISTS (SELECT 1 FROM audit_events a WHERE a.action = 'return.completed' AND a.entity_id = r.return_id)"),
    (
        "restocked return line without its movement",
        "SELECT count(*) FROM return_lines l WHERE l.disposition = 'restock' AND NOT EXISTS (SELECT 1 FROM stock_movements m WHERE m.source_type = 'return' AND m.source_line_id = l.return_line_id)",
    ),
    ("movement without its return", "SELECT count(*) FROM stock_movements m WHERE m.source_type = 'return' AND NOT EXISTS (SELECT 1 FROM returns r WHERE r.return_id = m.source_id)"),
    ("refund without its return", "SELECT count(*) FROM refunds f WHERE NOT EXISTS (SELECT 1 FROM returns r WHERE r.return_id = f.return_id)"),
    (
        "more returned than was sold",
        "SELECT count(*) FROM sale_lines s WHERE quantity_milli < (SELECT coalesce(sum(quantity_milli), 0) FROM return_lines l WHERE l.original_sale_line_id = s.sale_line_id)",
    ),
    // Stock documents: each with its movements, event and audit row.
    (
        "stock document without its movements",
        "SELECT count(*) FROM inventory_documents d WHERE NOT EXISTS (SELECT 1 FROM stock_movements m WHERE m.source_type = 'inventory_document' AND m.source_id = d.document_id)",
    ),
    (
        "stock document without its inventory.document_posted event",
        "SELECT count(*) FROM inventory_documents d WHERE NOT EXISTS (SELECT 1 FROM outbox_events o WHERE o.aggregate_type = 'inventory_document' AND o.event_type = 'inventory.document_posted' AND o.aggregate_id = d.document_id)",
    ),
    (
        "stock document without its audit row",
        "SELECT count(*) FROM inventory_documents d WHERE NOT EXISTS (SELECT 1 FROM audit_events a WHERE a.action LIKE 'inventory.%_posted' AND a.entity_id = d.document_id)",
    ),
    (
        "movement without its stock document",
        "SELECT count(*) FROM stock_movements m WHERE m.source_type = 'inventory_document' AND NOT EXISTS (SELECT 1 FROM inventory_documents d WHERE d.document_id = m.source_id)",
    ),
    // Imports: whole chunks only. A committed row's product exists with its event; a job
    // marked committed has nothing left to commit.
    (
        "imported row marked committed without its product",
        "SELECT count(*) FROM import_rows r WHERE r.status = 'committed'
           AND NOT EXISTS (SELECT 1 FROM products p WHERE p.product_id = json_extract(r.parsed_json, '$.productId'))",
    ),
    (
        "imported product without its row marked committed (a chunk half done)",
        "SELECT count(*) FROM product_variants v WHERE v.sku LIKE 'IMP-%' AND v.sku NOT IN
           (SELECT json_extract(r.parsed_json, '$.row.sku') FROM import_rows r WHERE r.status = 'committed')",
    ),
    (
        "product without its product.upserted event",
        "SELECT count(*) FROM products p WHERE NOT EXISTS (SELECT 1 FROM outbox_events o
           WHERE o.aggregate_type = 'product' AND o.aggregate_id = p.product_id AND o.event_type = 'product.upserted')",
    ),
    (
        "import job marked committed with rows left",
        "SELECT count(*) FROM import_jobs j WHERE j.status = 'committed' AND EXISTS (SELECT 1 FROM import_rows r WHERE r.job_id = j.job_id AND r.status = 'valid')",
    ),
    // Acknowledgements: nothing at or below the acknowledged sequence is still waiting, and
    // nothing above it is marked applied.
    (
        "event at or below the ack still pending",
        "SELECT count(*) FROM outbox_events WHERE local_sequence <= (SELECT coalesce(last_ack_sequence, 0) FROM sync_state WHERE id = 1)
           AND status IN ('pending','in_flight')",
    ),
    (
        "event above the ack marked applied",
        "SELECT count(*) FROM outbox_events WHERE local_sequence > (SELECT coalesce(last_ack_sequence, 0) FROM sync_state WHERE id = 1)
           AND status = 'applied'",
    ),
];

async fn verify(dir: &Path, ack: &Path) -> Result<Verdict, String> {
    if !dir.join("pos.db").exists() {
        return Ok(Verdict {
            sales: 0,
            lines: 0,
            acked: 0,
            done: Done::default(),
        });
    }
    let db = LocalDb::open(&harness_opts(dir))
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

    // Return and stock-document numbers are gapless too, and agree with their counters.
    for (what, numbers, counter) in [
        (
            "return",
            "SELECT return_seq FROM returns ORDER BY return_seq",
            "return_seq",
        ),
        (
            "stock document",
            "SELECT CAST(substr(document_no, length(document_no) - 5) AS INTEGER) FROM inventory_documents ORDER BY 1",
            "inventory_doc_seq",
        ),
    ] {
        let seqs: Vec<i64> = sqlx::query_scalar(numbers).fetch_all(r).await.map_err(e)?;
        if let Some((i, s)) = seqs.iter().enumerate().find(|(i, s)| **s != *i as i64 + 1) {
            return Err(format!("{what} number gap: position {} holds {s}", i + 1));
        }
        let value: Option<i64> = sqlx::query_scalar("SELECT value FROM register_counters WHERE name = ?")
            .bind(counter)
            .fetch_optional(r)
            .await
            .map_err(e)?;
        if value.unwrap_or(0) != seqs.len() as i64 {
            return Err(format!("{what} counter {value:?} != {} numbers used", seqs.len()));
        }
    }

    // 3. AT-05, and the same for every other kind of work: everything the child reported
    // done is there. Only newline-terminated lines are complete acknowledgements.
    let text = std::fs::read_to_string(ack).unwrap_or_default();
    let complete = text.rfind('\n').map_or("", |i| &text[..i]);
    let acked: Vec<(&str, &str)> = complete
        .lines()
        .filter_map(|l| l.trim().split_once(' '))
        .collect();
    let mut max_ack = 0i64;
    for (kind, what) in &acked {
        let (sql, label) = match *kind {
            "S" => (
                "SELECT count(*) FROM sales s JOIN outbox_events o ON o.aggregate_type = 'sale' AND o.aggregate_id = s.sale_id AND o.event_type = 'sale.completed'
                 WHERE s.receipt_number = ?",
                "sale",
            ),
            "R" => (
                "SELECT count(*) FROM returns r JOIN outbox_events o ON o.aggregate_type = 'return' AND o.aggregate_id = r.return_id AND o.event_type = 'return.completed'
                 WHERE r.return_number = ?",
                "return",
            ),
            "D" => ("SELECT count(*) FROM inventory_documents WHERE document_no = ?", "stock document"),
            "I" => ("SELECT count(*) FROM import_jobs WHERE job_id = ? AND status = 'committed'", "import"),
            "A" => {
                max_ack = max_ack.max(what.parse().map_err(|_| format!("bad ack line {what}"))?);
                continue;
            }
            other => return Err(format!("unknown acknowledgement kind {other}")),
        };
        let found: i64 = sqlx::query_scalar(sql).bind(what).fetch_one(r).await.map_err(e)?;
        if found != 1 {
            return Err(format!(
                "acknowledged {label} {what} is missing after the crash (or its event is)"
            ));
        }
    }
    let ack_now: Option<i64> = sqlx::query_scalar("SELECT last_ack_sequence FROM sync_state WHERE id = 1")
        .fetch_optional(r)
        .await
        .map_err(e)?;
    if ack_now.unwrap_or(0) < max_ack {
        return Err(format!(
            "the cloud's acknowledgement went back from {max_ack} to {ack_now:?} after the crash"
        ));
    }
    let count = |kind: &str| acked.iter().filter(|(k, _)| *k == kind).count();
    let done = Done {
        returns: sqlx::query_scalar("SELECT count(*) FROM returns")
            .fetch_one(r)
            .await
            .map_err(e)?,
        documents: sqlx::query_scalar("SELECT count(*) FROM inventory_documents")
            .fetch_one(r)
            .await
            .map_err(e)?,
        imports: sqlx::query_scalar("SELECT count(*) FROM import_jobs WHERE status = 'committed'")
            .fetch_one(r)
            .await
            .map_err(e)?,
        acknowledged_events: ack_now.unwrap_or(0),
    };
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM sale_lines")
        .fetch_one(r)
        .await
        .map_err(e)?;
    db.close().await;
    Ok(Verdict {
        sales: seqs.len() as i64,
        lines,
        acked: count("S"),
        done,
    })
}

// ------------------------------------------------------------ migration ---

/// Undoes migrations 0015 and 0014 on a database at head, so the real runner has a populated
/// database to upgrade (0014 includes a data migration: the search index rows). Kept next to
/// those files' contents; if a later edit breaks it, building the template fails loudly.
const DOWN_TO_0013: &str = "
ALTER TABLE cart_lines DROP COLUMN price_seen_minor;
ALTER TABLE cart_lines DROP COLUMN label_price_minor;
ALTER TABLE cart_lines DROP COLUMN label_kind;
DROP INDEX ix_carts_parked;
ALTER TABLE carts DROP COLUMN parked_by;
ALTER TABLE carts DROP COLUMN parked_at;
ALTER TABLE carts DROP COLUMN parked_label;
ALTER TABLE sync_state DROP COLUMN replay_after;
DROP TABLE cloud_report_cache;
DROP TABLE bootstrap_staging;
ALTER TABLE sync_state DROP COLUMN link_progress;
DROP TABLE search_index_rows;
DROP INDEX ix_outbox_aggregate;
ALTER TABLE sync_state DROP COLUMN bootstrap_page;
DELETE FROM _sqlx_migrations WHERE version IN (14, 15, 16, 17, 18, 19);
";

/// Opens (and so upgrades) the database, says so, and waits to be killed.
fn migrate_child(dir: &Path) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        LocalDb::open(&harness_opts(dir))
            .await
            .map_err(|e| e.to_string())?;
        println!("migrated");
        let _ = std::io::stdout().flush();
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    })
}

/// Counts that an upgrade must leave as they were.
const KEPT: &[&str] = &[
    "SELECT count(*) FROM sales",
    "SELECT count(*) FROM sale_lines",
    "SELECT count(*) FROM returns",
    "SELECT count(*) FROM inventory_documents",
    "SELECT count(*) FROM products",
    "SELECT count(*) FROM product_variants",
    "SELECT count(*) FROM outbox_events",
    "SELECT count(*) FROM audit_events",
    "SELECT count(*) FROM product_search",
    "SELECT coalesce(sum(on_hand_milli), 0) FROM stock_balances",
];

async fn kept_counts(db: &LocalDb) -> Result<Vec<i64>, String> {
    let mut out = Vec::new();
    for sql in KEPT {
        out.push(
            sqlx::query_scalar(sql)
                .fetch_one(db.readers())
                .await
                .map_err(|e| format!("{sql}: {e}"))?,
        );
    }
    Ok(out)
}

/// The schema version as the file says, read without upgrading it.
async fn version_on_disk(db: &Path) -> Result<i64, String> {
    use sqlx::ConnectOptions;
    let mut c = pos_local_db::crypto::keyed(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(db)
            .read_only(true),
        db,
        harness_opts(db).key.as_ref(),
    )
    .connect()
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM _sqlx_migrations WHERE success = 1")
        .fetch_one(&mut c)
        .await
        .map_err(|e| e.to_string())
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(to);
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for f in ["pos.db", "pos.db-wal", "pos.db-shm"] {
        if from.join(f).exists() {
            std::fs::copy(from.join(f), to.join(f)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn migrate_campaign(args: &[String]) -> Result<(), String> {
    let mut kills = 50usize;
    let mut report: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--kills" => kills = it.next().and_then(|v| v.parse().ok()).ok_or("--kills N")?,
            "--report" => report = it.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let root = std::env::temp_dir().join(format!("sale-os-migrate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (template, work) = (root.join("template"), root.join("work"));
    std::fs::create_dir_all(&template).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    // 1. A populated shop at head: 3,600 imported products, then a few seconds of trading.
    let ack = root.join("acked.txt");
    let mut child = Command::new(&exe)
        .args(["child"])
        .arg(&template)
        .arg(&ack)
        .env("CRASH_HARNESS_SEED_IMPORTS", "3")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    let _ = BufReader::new(child.stdout.take().ok_or("no stdout")?).read_line(&mut line);
    std::thread::sleep(Duration::from_secs(3));
    child.kill().map_err(|e| e.to_string())?;
    let _ = child.wait();
    rt.block_on(verify(&template, &ack))?;

    // 2. Back to schema 13, with everything in the main file.
    let before = rt.block_on(async {
        use sqlx::ConnectOptions;
        let file = template.join("pos.db");
        let mut c = pos_local_db::crypto::keyed(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&file),
            &file,
            harness_opts(&template).key.as_ref(),
        )
        .connect()
        .await
        .map_err(|e| e.to_string())?;
        sqlx::raw_sql(DOWN_TO_0013)
            .execute(&mut c)
            .await
            .map_err(|e| format!("undoing 0014-0019: {e}"))?;
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&mut c)
            .await
            .map_err(|e| e.to_string())?;
        let mut counts = Vec::new();
        for sql in KEPT {
            counts.push(
                sqlx::query_scalar::<_, i64>(sql)
                    .fetch_one(&mut c)
                    .await
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok::<_, String>(counts)
    })?;
    let from = rt.block_on(version_on_disk(&template.join("pos.db")))?;
    let head = pos_local_db::MIGRATOR
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap_or(0);
    println!(
        "crash-harness migrate: template at v{from} ({} products, {} sales), head v{head}",
        before[4], before[0]
    );

    // 3. How long an upgrade takes from spawn, so kills land across it.
    copy_dir(&template, &work)?;
    let t = Instant::now();
    let mut child = Command::new(&exe)
        .args(["migrate-child"])
        .arg(&work)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    let _ = BufReader::new(child.stdout.take().ok_or("no stdout")?).read_line(&mut line);
    let window = t.elapsed();
    child.kill().map_err(|e| e.to_string())?;
    let _ = child.wait();
    if line.trim() != "migrated" {
        return Err(format!("calibration upgrade did not finish: {line:?}"));
    }
    println!("  one upgrade takes {window:?} from spawn");

    // 4. Kill upgrades at random points, then check each database.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(42)
        | 1;
    let mut rng = Rng(seed);
    let mut landed = std::collections::BTreeMap::<i64, usize>::new();
    let started = Instant::now();
    let window_us = window.as_micros() as u64;
    for k in 1..=kills {
        copy_dir(&template, &work)?;
        let mut child = Command::new(&exe)
            .args(["migrate-child"])
            .arg(&work)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_micros(rng.below(window_us + window_us / 5)));
        child.kill().map_err(|e| e.to_string())?;
        let _ = child.wait();
        let at_kill = rt.block_on(version_on_disk(&work.join("pos.db")))?;
        *landed.entry(at_kill).or_default() += 1;
        rt.block_on(async {
            // The app starting again: it must open, finish the upgrade and keep everything.
            let db = LocalDb::open(&harness_opts(&work))
                .await
                .map_err(|e| format!("kill {k} (at v{at_kill}): reopen failed: {e}"))?;
            let v = db.schema_version().await.map_err(|e| e.to_string())?;
            if v != head {
                return Err(format!("kill {k}: reopened at v{v}, not v{head}"));
            }
            let after = kept_counts(&db).await?;
            if after != before {
                return Err(format!(
                    "kill {k} (at v{at_kill}): counts {before:?} became {after:?}"
                ));
            }
            let unmapped: i64 = sqlx::query_scalar(
                "SELECT (SELECT count(*) FROM product_search) - (SELECT count(*) FROM search_index_rows i
                   JOIN product_search s ON s.rowid = i.search_rowid AND s.variant_id = i.variant_id)",
            )
            .fetch_one(db.readers())
            .await
            .map_err(|e| e.to_string())?;
            if unmapped != 0 {
                return Err(format!(
                    "kill {k}: {unmapped} search rows without their index row"
                ));
            }
            db.close().await;
            Ok::<_, String>(())
        })?;
        rt.block_on(verify(&work, &root.join("none.txt")))
            .map_err(|e| format!("kill {k} (at v{at_kill}): {e}"))?;
        if k % 10 == 0 || k == kills {
            println!("  {k}/{kills} kills ok; killed at {landed:?} (schema version on disk)");
        }
    }
    let summary = format!(
        "{kills} kills during an upgrade v{from} -> v{head} ({} products, {} sales), 0 violations; \
         schema on disk at the kill: {landed:?}; {:.1}s; seed {seed}; {} {}",
        before[4],
        before[0],
        started.elapsed().as_secs_f64(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("crash-harness migrate: PASS: {summary}");
    if let Some(path) = report {
        std::fs::write(&path, format!("{summary}\n")).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

// ------------------------------------------------------------ disk full ---

/// Runs a child on `dir` until it exits by itself (or `limit` passes); its stderr.
fn run_child_until_exit(exe: &Path, dir: &Path, ack: &Path, limit: Duration) -> Result<String, String> {
    let mut child = Command::new(exe)
        .args(["child"])
        .arg(dir)
        .arg(ack)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let t = Instant::now();
    while t.elapsed() < limit {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            let mut err = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut s, &mut err);
            }
            return Ok(err);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().map_err(|e| e.to_string())?;
    let _ = child.wait();
    Err(format!(
        "the child was still trading after {limit:?}: the disk never filled"
    ))
}

/// Trades for `for_ms`, then kills the child (a running till).
fn trade_for(exe: &Path, dir: &Path, ack: &Path, for_ms: u64) -> Result<(), String> {
    let mut child = Command::new(exe)
        .args(["child"])
        .arg(dir)
        .arg(ack)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    let _ = BufReader::new(child.stdout.take().ok_or("no stdout")?).read_line(&mut line);
    std::thread::sleep(Duration::from_millis(for_ms));
    if child.try_wait().map_err(|e| e.to_string())?.is_some() || line.trim() != "ready" {
        let mut err = String::new();
        if let Some(mut s) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut s, &mut err);
        }
        let _ = child.kill();
        return Err(format!("the till stopped trading by itself ({line:?}): {err}"));
    }
    child.kill().map_err(|e| e.to_string())?;
    let _ = child.wait();
    Ok(())
}

fn disk_full(args: &[String]) -> Result<(), String> {
    let mut dir: Option<PathBuf> = None;
    let mut report: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => dir = it.next().map(PathBuf::from),
            "--report" => report = it.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let dir = dir.ok_or("--dir DIR (on a small file system) is required")?;
    let work = dir.join("till");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    // The acknowledgements live off the small disk, so they never fail for want of space.
    let ack = std::env::temp_dir().join(format!("sale-os-diskfull-ack-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&ack);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    // 1. A till that has traded for a while.
    trade_for(&exe, &work, &ack, 1500)?;
    rt.block_on(verify(&work, &ack))?;

    // 2. Fill the disk, then give back 4 MB: trading fills the rest part-way through a
    //    write.
    let filler = dir.join("filler");
    {
        let mut f = std::fs::File::create(&filler).map_err(|e| e.to_string())?;
        let block = vec![0u8; 64 * 1024];
        loop {
            match f.write_all(&block) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(28) || e.kind() == std::io::ErrorKind::StorageFull => {
                    break;
                }
                Err(e) => return Err(format!("filling the disk: {e}")),
            }
        }
        let _ = f.sync_all();
        let len = f.metadata().map_err(|e| e.to_string())?.len();
        f.set_len(len.saturating_sub(4 * 1024 * 1024))
            .map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    let before = rt.block_on(verify(&work, &ack))?;

    // 3. Trade until the disk is full. The child stops on the first error it gets: it must
    //    be the full disk, reported as such, not anything else.
    let err = run_child_until_exit(&exe, &work, &ack, Duration::from_secs(120))?;
    if !err.contains("disk is full") {
        return Err(format!(
            "the till stopped for another reason than a full disk: {err}"
        ));
    }
    let at_full = {
        let _ = std::fs::remove_file(&filler);
        rt.block_on(verify(&work, &ack))?
    };

    // 4. With space again, the same till trades on.
    trade_for(&exe, &work, &ack, 3000)?;
    let after = rt.block_on(verify(&work, &ack))?;
    if after.sales <= at_full.sales {
        return Err("no sale was saved once there was space again".into());
    }
    let summary = format!(
        "disk full: the till stopped with \"{}\" after {} more sales; nothing partial, every acknowledged \
         sale present; {} sales after space was freed; {} {}",
        err.trim().lines().last().unwrap_or("").trim(),
        at_full.sales - before.sales,
        after.sales - at_full.sales,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("crash-harness disk-full: PASS: {summary}");
    if let Some(path) = report {
        std::fs::write(&path, format!("{summary}\n")).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(&ack);
    Ok(())
}
