//! E19-T01: the PRD §14 performance targets, measured on a full-size register database.
//!
//! Builds a register with 50,000 variants (100,000 barcodes) through the real catalogue write
//! path (search index included) and 500,000 sale lines over the past year, then times each
//! operation the targets name, through the same services the app calls, and reports p50, p95
//! and the maximum against each target.
//!
//! ```text
//! cargo run --release -p pos-bench -- [--scale full|small] [--samples N]
//!     [--markdown FILE] [--json FILE] [--baseline FILE] [--max-regression PCT]
//! ```
//!
//! `--baseline` compares with an earlier `--json` result and fails when a median is more than
//! `--max-regression` percent slower (default 20) and by at least 0.5 ms (see [`regressions`]).
//! Without a baseline it fails when a target is missed.

use anyhow::{Context, Result, anyhow, bail};
use pos_application::approval::Approvals;
use pos_application::auth::{Argon2PinHasher, AuthService, PinHasher};
use pos_application::cart::CartService;
use pos_application::cash::CashService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::context::{Actor, CommandContext};
use pos_application::queries::{CatalogQueries, SalesFilter};
use pos_application::reports::ReportService;
use pos_application::sales::SalesService;
use pos_application::scan::{ScanOutcome, ScanSource};
use pos_application::setup::{CreateWorkspace, NewTaxRate, SetupService};
use pos_application::sync::SyncStore;
use pos_domain::cart::AddVia;
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::staff::Pin;
use pos_domain::{
    BusinessDate, Clock, IdGenerator, Money, RatePpm, Role, ScanEventId, SessionId, SystemClock, Timestamp,
    UuidV7Generator, VariantId,
};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_sync::pull::{BootstrapEntity, BootstrapPage};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How big the database is and how many samples each measure takes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Scale {
    variants: usize,
    sale_lines: usize,
    samples: usize,
}

const FULL: Scale = Scale {
    variants: 50_000,
    sale_lines: 500_000,
    samples: 1_000,
};
/// The weekly CI regression run: the same measures on a tenth of the data.
const SMALL: Scale = Scale {
    variants: 5_000,
    sale_lines: 50_000,
    samples: 1_000,
};

/// One measure's result, in microseconds.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Measure {
    key: String,
    name: String,
    target_ms: u64,
    samples: usize,
    p50_us: u64,
    p95_us: u64,
    max_us: u64,
    /// What exactly is timed.
    what: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Run {
    date: String,
    scale: Scale,
    machine: String,
    build: String,
    dataset_seconds: u64,
    measures: Vec<Measure>,
}

struct Args {
    scale: Scale,
    markdown: Option<PathBuf>,
    json: Option<PathBuf>,
    baseline: Option<PathBuf>,
    max_regression: u64,
}

fn args() -> Result<Args> {
    let mut a = Args {
        scale: FULL,
        markdown: None,
        json: None,
        baseline: None,
        max_regression: 20,
    };
    let mut it = std::env::args().skip(1);
    let mut samples = None;
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--scale" => {
                a.scale = match value()?.as_str() {
                    "full" => FULL,
                    "small" => SMALL,
                    other => bail!("--scale full|small, not {other}"),
                }
            }
            "--samples" => samples = Some(value()?.parse()?),
            "--markdown" => a.markdown = Some(value()?.into()),
            "--json" => a.json = Some(value()?.into()),
            "--baseline" => a.baseline = Some(value()?.into()),
            "--max-regression" => a.max_regression = value()?.parse()?,
            other => bail!("unknown argument {other}; see the crate docs"),
        }
    }
    if let Some(n) = samples {
        a.scale.samples = n;
    }
    Ok(a)
}

/// p50, p95 and the maximum, by rank (no interpolation).
fn summarise(mut d: Vec<Duration>) -> (u64, u64, u64) {
    d.sort_unstable();
    let at = |pct: usize| {
        let i = (d.len() * pct).div_ceil(100).saturating_sub(1);
        u64::try_from(d[i.min(d.len() - 1)].as_micros()).unwrap_or(u64::MAX)
    };
    (at(50), at(95), at(100))
}

/// A GTIN-13 with its check digit.
fn gtin13(prefix: u64, n: u64) -> String {
    let body = format!("{prefix:02}{n:010}");
    let sum: u32 = body
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    format!("{body}{}", (10 - sum % 10) % 10)
}

/// A small deterministic generator (xorshift), so every run asks the same questions.
struct Rng(u64);
impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % (n as u64)).unwrap_or(0)
    }
}

struct Bench {
    _dir: tempfile::TempDir,
    path: PathBuf,
    store: SqliteStore,
    owner: CommandContext,
    hasher: Arc<dyn PinHasher>,
    variants: Vec<VariantId>,
    barcodes: Vec<String>,
    today: BusinessDate,
    /// Generated sales, numbered from 1,000,001.
    sales: usize,
}

async fn build(scale: Scale) -> Result<Bench> {
    let dir = tempfile::TempDir::new()?;
    let path = dir.path().to_path_buf();
    let store = SqliteStore::new(LocalDb::open(&DbOptions::in_dir(&path)).await?);
    // Cheap PIN hashing: sign-in speed is not one of the targets.
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidV7Generator);
    let ws = SetupService {
        store: Arc::new(store.clone()),
        setup: Arc::new(store.clone()),
        hasher: hasher.clone(),
        clock: clock.clone(),
        ids: ids.clone(),
    }
    .create_workspace(CreateWorkspace {
        business_name: "Bench Shop".into(),
        legal_name: None,
        address: json!({}),
        phone: None,
        email: None,
        country_code: "GB".into(),
        timezone: "Europe/London".into(),
        currency_code: "GBP".into(),
        prices_include_tax: true,
        location_name: "Main".into(),
        register_name: "Till".into(),
        owner_name: "Owner".into(),
        owner_pin: Pin::parse("1234").map_err(|e| anyhow!("{e:?}"))?,
        tax_rates: vec![NewTaxRate {
            name: "VAT 20%".into(),
            rate: RatePpm::new(200_000).map_err(|e| anyhow!("{e:?}"))?,
        }],
    })
    .await
    .map_err(|e| anyhow!("setup: {e}"))?;
    let owner = CommandContext {
        register: ws.register,
        actor: Some(Actor {
            user_id: ws.owner_user_id,
            role: Role::Owner,
            session_id: SessionId::generate(&UuidV7Generator),
        }),
        clock: clock.clone(),
        ids,
        approval: None,
    };
    let tax: String = sqlx::query_scalar("SELECT tax_category_id FROM tax_categories")
        .fetch_one(store.db().readers())
        .await?;

    // The catalogue, through the same write path as a pulled catalogue (products, variants,
    // barcodes, stock rows and the search index), in pages of 2,000.
    let words = [
        "Cola", "Bread", "Milk", "Crisps", "Soap", "Rice", "Tea", "Coffee", "Juice", "Biscuits",
    ];
    let mut variants = Vec::with_capacity(scale.variants);
    let mut barcodes = Vec::with_capacity(scale.variants * 2);
    let at = Timestamp::from_millis(clock.now().as_millis());
    let pages = scale.variants.div_ceil(2000);
    for p in 0..pages {
        let entities: Vec<BootstrapEntity> = (p * 2000..((p + 1) * 2000).min(scale.variants))
            .map(|n| {
                let id = Uuid::now_v7();
                let variant = Uuid::now_v7();
                variants.push(VariantId::from_uuid(variant));
                let codes = [gtin13(20, n as u64), gtin13(21, n as u64)];
                barcodes.extend(codes.iter().cloned());
                let barcodes: Vec<_> = codes
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        json!({"id": Uuid::now_v7(), "value": c, "raw": c, "symbology": "ean13",
                               "qualifier": if i == 0 { "primary" } else { "alias" }, "packQty": 1000})
                    })
                    .collect();
                let word = words[n % words.len()];
                let product = json!({"id": id, "name": format!("{word} {n}"), "description": null,
                    "categoryId": null, "productType": "stock", "unit": "ea", "taxCategoryId": tax,
                    "quickSelect": false, "archived": false, "version": 1,
                    "variants": [{"id": variant, "isDefault": true, "sku": format!("SKU-{n}"), "options": {},
                        "price": 100 + (n % 900), "cost": null, "trackStock": true, "lowStockThreshold": null,
                        "scanQty": 1000, "barcodes": barcodes, "archived": false}]});
                BootstrapEntity {
                    aggregate_type: "product".into(),
                    aggregate_id: id.to_string(),
                    event_type: "product.upserted".into(),
                    schema_version: 1,
                    version: 1,
                    snapshot: product,
                }
            })
            .collect();
        let page = BootstrapPage {
            snapshot_cursor: "v1:0".into(),
            entities,
            next_page: (p + 1 < pages).then(|| format!("p{}", p + 1)),
        };
        store
            .apply_bootstrap(&page, at)
            .await
            .map_err(|e| anyhow!("catalogue: {e:?}"))?;
    }
    // The register was never linked; the bootstrap only reused its write path.
    sqlx::query(
        "UPDATE sync_state SET bootstrap_page = NULL, last_pull_cursor = NULL, last_pull_ok_at = NULL",
    )
    .execute(store.db().writer())
    .await?;

    // A year of trading: sales of 4 lines spread over the 365 days up to now, each paid once
    // (every 3rd by card, every 50th by QR), written straight into the tables. Receipt
    // numbers start at 1,000,000 so the timed sales (numbered from 1) never meet them.
    let reg = &owner.register;
    let cashier = ws.owner_user_id.to_string();
    let sales = scale.sale_lines / 4;
    let now = clock.now().as_millis();
    let year = 365 * 86_400_000_i64;
    let step = year / i64::try_from(sales).unwrap_or(1);
    let start = now - year;
    let mut tx = store.db().writer().begin().await?;
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?6)
         INSERT INTO sales (sale_id, business_id, location_id, register_id, receipt_seq, receipt_number, mode,
           currency_code, currency_exponent, prices_include_tax, subtotal_minor, line_discount_total_minor,
           order_discount_total_minor, tax_total_minor, total_minor, tendered_total_minor, change_minor,
           tax_breakdown_json, cashier_user_id, occurred_at, business_date, timezone, created_at)
         SELECT printf('00000000-0000-7000-8000-%012d', i), ?1, ?2, ?3, 1000000 + i, printf('R01-%07d', 1000000 + i),
           'live', 'GBP', 2, 1, 1000, 0, 0, 167, 1000, 1000, 0, '[]', ?4,
           ?5 + i * ?7, date((?5 + i * ?7) / 1000, 'unixepoch'), 'Europe/London', ?5 + i * ?7
         FROM n",
    )
    .bind(reg.business_id.to_string())
    .bind(reg.location_id.to_string())
    .bind(reg.register_id.to_string())
    .bind(&cashier)
    .bind(start)
    .bind(i64::try_from(sales)?)
    .bind(step)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1)
         INSERT INTO sale_lines (sale_line_id, sale_id, business_id, line_no, product_id, variant_id, name_snapshot,
           variant_label_snapshot, unit_code, product_type, quantity_milli, unit_price_minor, gross_minor,
           line_discount_minor, order_discount_alloc_minor, tax_minor, total_minor, tax_lines_json)
         SELECT printf('00000000-0000-7000-9000-%012d', i), printf('00000000-0000-7000-8000-%012d', i / 4 + 1), ?1,
           i % 4 + 1, 'p' || (i % 997), 'v' || (i % 997), 'Item ' || (i % 997), '', 'ea', 'stock', 1000, 250, 250,
           0, 0, 42, 250, '[]'
         FROM n",
    )
    .bind(reg.business_id.to_string())
    .bind(i64::try_from(sales * 4)?)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tenders (tender_id, sale_id, business_id, seq, tender_type, amount_minor, status,
           confirmation_source, recorded_by, occurred_at)
         SELECT 't' || sale_id, sale_id, business_id, 1,
           CASE WHEN receipt_seq % 50 = 0 THEN 'qr_external' WHEN receipt_seq % 3 = 0 THEN 'card_external' ELSE 'cash' END,
           total_minor, 'confirmed', 'operator', cashier_user_id, occurred_at
         FROM sales WHERE receipt_seq >= 1000000",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Trading for real: live, with an open drawer.
    sqlx::query("UPDATE device_identity SET mode = 'live'")
        .execute(store.db().writer())
        .await?;
    let s = Arc::new(store.clone());
    CashService {
        store: s.clone(),
        cash: s.clone(),
        receipts: s.clone(),
        setup: s.clone(),
        pricing: s,
    }
    .open(&owner, Money::from_minor(10_000))
    .await
    .map_err(|e| anyhow!("drawer: {e}"))?;
    sqlx::query("ANALYZE").execute(store.db().writer()).await?;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(store.db().writer())
        .await?;
    let today = pos_domain::business_date(clock.now(), "Europe/London".parse().map_err(|e| anyhow!("{e}"))?)
        .map_err(|e| anyhow!("{e:?}"))?;
    Ok(Bench {
        _dir: dir,
        path,
        store,
        owner,
        hasher,
        variants,
        barcodes,
        today,
        sales,
    })
}

fn days_before(d: BusinessDate, n: i64) -> Result<BusinessDate> {
    (d.as_naive() - chrono::Duration::days(n))
        .format("%Y-%m-%d")
        .to_string()
        .parse()
        .map_err(|e| anyhow!("{e:?}"))
}

#[allow(clippy::too_many_lines)]
async fn measure(b: &Bench, samples: usize) -> Result<Vec<Measure>> {
    let a = Arc::new(b.store.clone());
    let carts = Arc::new(CartService::new(a.clone(), a.clone(), a.clone()).with_stock(a.clone()));
    let checkout = CheckoutService {
        carts: carts.clone(),
        store: a.clone(),
        setup: a.clone(),
        stock: a.clone(),
        receipts: a.clone(),
    };
    let reports = ReportService { reports: a.clone() };
    let sales = SalesService { sales: a.clone() };
    let ctx = &b.owner;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = Vec::new();
    let mut record = |key: &str, name: &str, target_ms: u64, what: &str, d: Vec<Duration>| {
        let (p50_us, p95_us, max_us) = summarise(d.clone());
        eprintln!(
            "{name}: p50 {p50_us} µs, p95 {p95_us} µs, max {max_us} µs ({} samples)",
            d.len()
        );
        out.push(Measure {
            key: key.into(),
            name: name.into(),
            target_ms,
            samples: d.len(),
            p50_us,
            p95_us,
            max_us,
            what: what.into(),
        });
    };
    let warm = samples / 10 + 1;

    // Barcode lookup.
    let mut d = Vec::with_capacity(samples);
    for i in 0..samples + warm {
        let code = &b.barcodes[rng.below(b.barcodes.len())];
        let t = Instant::now();
        let hit = b.store.lookup_code(code).await.map_err(|e| anyhow!("{e:?}"))?;
        let e = t.elapsed();
        hit.ok_or_else(|| anyhow!("barcode {code} not found"))?;
        if i >= warm {
            d.push(e);
        }
    }
    record(
        "barcode_lookup",
        "Barcode lookup",
        50,
        "`lookup_code` for a random one of the barcodes (normalise, look up, load the variant)",
        d,
    );

    // Add item to visible cart: a scan, which looks the code up, adds the line, prices the
    // cart, saves it and returns what the screen shows. The cart is cleared every 20 items.
    let mut d = Vec::with_capacity(samples);
    for i in 0..samples + warm {
        if i % 20 == 0 {
            let rev = carts
                .current(ctx)
                .await
                .map_err(|e| anyhow!("{e}"))?
                .cart
                .revision;
            carts.clear(ctx, rev).await.map_err(|e| anyhow!("{e}"))?;
        }
        let code = &b.barcodes[rng.below(b.barcodes.len())];
        let t = Instant::now();
        let r = carts
            .scan(
                ctx,
                code,
                ScanEventId::generate(&UuidV7Generator),
                ScanSource::Scanner,
            )
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let e = t.elapsed();
        if !matches!(r, ScanOutcome::Added { .. }) {
            bail!("scan of {code} did not add: {r:?}");
        }
        if i >= warm {
            d.push(e);
        }
    }
    record(
        "add_to_cart",
        "Add item to visible cart",
        100,
        "a scan through `CartService::scan` on a cart of 1 to 20 lines, returning the priced cart",
        d,
    );
    let rev = carts
        .current(ctx)
        .await
        .map_err(|e| anyhow!("{e}"))?
        .cart
        .revision;
    carts.clear(ctx, rev).await.map_err(|e| anyhow!("{e}"))?;

    // Product search: what a cashier types (a word, a word and a number, a SKU prefix).
    let words = [
        "cola", "bread 12", "milk", "SKU-4", "tea 4", "coffee", "juice 49", "soap 3", "rice", "biscu",
    ];
    let mut d = Vec::with_capacity(samples);
    for i in 0..samples + warm {
        let q = words[i % words.len()];
        let t = Instant::now();
        let hits = b.store.search(q, 20).await.map_err(|e| anyhow!("{e:?}"))?;
        let e = t.elapsed();
        if hits.is_empty() {
            bail!("search {q:?} found nothing");
        }
        if i >= warm {
            d.push(e);
        }
    }
    record(
        "product_search",
        "Product search",
        150,
        "`search` for 20 results, cycling through typical queries",
        d,
    );

    // Cash-sale local commit: completing a 3-line sale paid in exact cash (the cart is filled
    // first, untimed). Everything in one transaction: the sale, its lines and tender, stock
    // movements, the receipt number and document, the audit row and the outbox event.
    let mut d = Vec::with_capacity(samples);
    for i in 0..samples + warm {
        for _ in 0..3 {
            let v = b.variants[rng.below(b.variants.len())];
            carts
                .add_variant(ctx, v, None, AddVia::Search)
                .await
                .map_err(|e| anyhow!("{e}"))?;
        }
        let view = carts.current(ctx).await.map_err(|e| anyhow!("{e}"))?;
        let due = view.priced.total;
        let t = Instant::now();
        checkout
            .complete_sale(
                ctx,
                CompleteSale {
                    cart_id: view.cart.id,
                    revision: view.cart.revision,
                    tenders: vec![TenderInput {
                        kind: TenderKind::Cash,
                        amount: due,
                        external_reference: None,
                    }],
                },
            )
            .await
            .map_err(|e| anyhow!("sale: {e}"))?;
        let e = t.elapsed();
        if i >= warm {
            d.push(e);
        }
    }
    record(
        "cash_sale_commit",
        "Cash-sale local commit",
        500,
        "`complete_sale` for 3 lines paid in exact cash (one durable transaction, `synchronous=FULL`)",
        d,
    );

    // 30-day sales report.
    let from = days_before(b.today, 29)?;
    let mut d = Vec::with_capacity(samples);
    for i in 0..samples + warm {
        let t = Instant::now();
        let s = reports
            .sales_summary(ctx, from, b.today)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let e = t.elapsed();
        if s.total.sales_count == 0 {
            bail!("the 30-day report is empty");
        }
        if i >= warm {
            d.push(e);
        }
    }
    record(
        "report_30_days",
        "30-day sales report",
        1000,
        "`sales_summary` for the last 30 days (figures per day and totals)",
        d,
    );

    // Sales list page: the newest 50, then filtered by a date range, a tender and a receipt.
    let filters = [
        SalesFilter::default(),
        SalesFilter {
            from: Some(days_before(b.today, 200)?),
            to: Some(days_before(b.today, 170)?),
            ..SalesFilter::default()
        },
        SalesFilter {
            tender: Some(TenderKind::QrExternal),
            ..SalesFilter::default()
        },
        SalesFilter {
            receipt: Some((1_000_000 + b.sales / 2).to_string()),
            ..SalesFilter::default()
        },
    ];
    let mut d = Vec::with_capacity(samples);
    for i in 0..samples + warm {
        let f = &filters[i % filters.len()];
        let t = Instant::now();
        let page = sales
            .list(ctx, f, None, Some(50))
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let e = t.elapsed();
        if page.rows.is_empty() {
            bail!("the sales list is empty for {f:?}");
        }
        if i >= warm {
            d.push(e);
        }
    }
    record(
        "sales_list_page",
        "Sales list page",
        300,
        "`SalesService::list`, 50 rows: newest, a date range, a tender, a receipt number",
        d,
    );

    // Cold launch (the app's part before the window shows): open the database, check its
    // migrations and pragmas, and answer the sign-in screen (who can sign in). The stock
    // check runs in the background and is not waited for, as in the app.
    let mut d = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t = Instant::now();
        let db = LocalDb::open(&DbOptions::in_dir(&b.path)).await?;
        db.schema_version().await?;
        let s = Arc::new(SqliteStore::new(db.clone()));
        let staff = AuthService {
            store: s.clone(),
            staff: s,
            hasher: b.hasher.clone(),
            approvals: Arc::new(Approvals::default()),
        }
        .sign_in_list()
        .await
        .map_err(|e| anyhow!("{e}"))?;
        let e = t.elapsed();
        if staff.is_empty() {
            bail!("nobody to sign in");
        }
        db.close().await;
        d.push(e);
    }
    record(
        "cold_launch",
        "Cold launch (backend)",
        5000,
        "open the database (migration check, pragmas) and load the sign-in screen's staff list; the WebView's own start is not included",
        d,
    );
    Ok(out)
}

fn machine() -> String {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|m| m.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown CPU".into());
    let threads = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    let mem_gb = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map_or(0, |kb| kb / 1_048_576);
    format!(
        "{cpu}, {threads} threads, {mem_gb} GB RAM, {}",
        std::env::consts::OS
    )
}

fn ms(us: u64) -> String {
    format!("{}.{:01}", us / 1000, (us % 1000) / 100)
}

fn markdown(run: &Run) -> String {
    let mut s = format!(
        "# Performance run {}\n\nE19-T01, PRD §14. Generated by `cargo run --release -p pos-bench`.\n\n\
         - **Machine:** {}\n- **Build:** {}\n- **Data:** {} variants, {} barcodes, {} sale lines; built in {} s\n\
         - **Samples:** {} per measure (after a 10% warm-up, except cold launch)\n\n\
         | Measure | Target | p50 | p95 | Max | Samples | Met |\n|---|---|---|---|---|---|---|\n",
        run.date,
        run.machine,
        run.build,
        run.scale.variants,
        run.scale.variants * 2,
        run.scale.sale_lines,
        run.dataset_seconds,
        run.scale.samples,
    );
    for m in &run.measures {
        s.push_str(&format!(
            "| {} | < {} ms | {} ms | {} ms | {} ms | {} | {} |\n",
            m.name,
            m.target_ms,
            ms(m.p50_us),
            ms(m.p95_us),
            ms(m.max_us),
            m.samples,
            if m.p95_us < m.target_ms * 1000 {
                "yes"
            } else {
                "**no**"
            }
        ));
    }
    s.push_str("\n## Read with care\n\n");
    if !run.machine.contains("windows") {
        s.push_str(
            "- **Not the reference machine.** D-05 names Windows 11 on a 4-core i3-12100-class CPU with \
             8 GB RAM and an SSD; this run is on the machine above. The verdict for the pilot is a \
             run there (`pos-bench.exe --scale full`).\n",
        );
    }
    s.push_str(
        "- The operating system's file cache is warm: the database was just written. A first scan \
         after a reboot reads from disk.\n\
         - Times are the services the app's commands call, without the WebView and IPC (a few \
         milliseconds more per command), and cold launch excludes the process and window start.\n",
    );
    s.push_str("\n## What each measure times\n\n");
    for m in &run.measures {
        s.push_str(&format!("- **{}:** {}.\n", m.name, m.what));
    }
    s
}

/// Measures whose median is more than `pct`% slower than the baseline's, and by at least
/// 0.5 ms. The median, not p95: on a shared CI runner p95 over the same code moves by 40%
/// from run to run, the median by about 10% (measured when this was written), so an alert on
/// p95 would mostly be noise. The targets themselves are still judged at p95.
fn regressions(now: &Run, base: &Run, pct: u64) -> Vec<String> {
    now.measures
        .iter()
        .filter_map(|m| {
            let b = base.measures.iter().find(|b| b.key == m.key)?;
            let worse = m.p50_us.saturating_sub(b.p50_us);
            (worse >= 500 && worse * 100 > b.p50_us * pct).then(|| {
                format!(
                    "{}: median {} ms, was {} ms (+{}%)",
                    m.name,
                    ms(m.p50_us),
                    ms(b.p50_us),
                    worse * 100 / b.p50_us.max(1)
                )
            })
        })
        .collect()
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = args()?;
    let built = Instant::now();
    let bench = build(a.scale).await?;
    let dataset_seconds = built.elapsed().as_secs();
    eprintln!("dataset ready in {dataset_seconds} s");
    let measures = measure(&bench, a.scale.samples).await?;
    let run = Run {
        date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        scale: a.scale,
        machine: machine(),
        build: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
        .into(),
        dataset_seconds,
        measures,
    };
    let md = markdown(&run);
    println!("{md}");
    if let Some(p) = &a.markdown {
        write(p, &md)?;
    }
    if let Some(p) = &a.json {
        write(p, &serde_json::to_string_pretty(&run)?)?;
    }
    match &a.baseline {
        Some(p) if p.exists() => {
            let base: Run = serde_json::from_str(&std::fs::read_to_string(p)?)?;
            if base.scale != run.scale {
                eprintln!("baseline is at another scale; not compared");
                return Ok(());
            }
            let worse = regressions(&run, &base, a.max_regression);
            if !worse.is_empty() {
                bail!(
                    "slower than the baseline by more than {}%:\n{}",
                    a.max_regression,
                    worse.join("\n")
                );
            }
            eprintln!(
                "no regression above {}% against {}",
                a.max_regression,
                p.display()
            );
        }
        Some(p) => eprintln!("no baseline at {} yet; this run becomes it", p.display()),
        None => {
            let missed: Vec<_> = run
                .measures
                .iter()
                .filter(|m| m.p95_us >= m.target_ms * 1000)
                .map(|m| m.name.clone())
                .collect();
            if !missed.is_empty() && run.scale == FULL {
                bail!("targets missed at p95: {}", missed.join(", "));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_by_rank() {
        let d: Vec<Duration> = (1..=100).map(Duration::from_micros).collect();
        assert_eq!(summarise(d), (50, 95, 100));
        assert_eq!(summarise(vec![Duration::from_micros(7)]), (7, 7, 7));
    }

    #[test]
    fn check_digits() {
        // Known-valid GTIN-13s: 4006381333931 and 2000000000008.
        assert_eq!(gtin13(40, 638_133_393), "4006381333931");
        assert_eq!(gtin13(20, 0), "2000000000008");
    }

    #[test]
    fn a_regression_is_a_median_twenty_percent_and_half_a_millisecond_slower() {
        let m = |p50_us| Measure {
            key: "k".into(),
            name: "K".into(),
            target_ms: 50,
            samples: 1,
            p50_us,
            p95_us: 0,
            max_us: 0,
            what: String::new(),
        };
        let run = |p50| Run {
            date: String::new(),
            scale: SMALL,
            machine: String::new(),
            build: String::new(),
            dataset_seconds: 0,
            measures: vec![m(p50)],
        };
        assert_eq!(regressions(&run(5_000), &run(4_000), 20).len(), 1, "+25%");
        assert!(regressions(&run(4_500), &run(4_000), 20).is_empty(), "+12.5%");
        assert!(
            regressions(&run(700), &run(400), 20).is_empty(),
            "+75% but under 0.5 ms"
        );
    }
}
