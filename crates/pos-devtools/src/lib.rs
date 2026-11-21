//! Developer tooling (E00-T04, PRD §16): reset a till database, or seed one with a
//! deterministic catalogue, **through the application services** (never raw SQL), so the
//! data has the same audit rows, outbox events and search entries a real till would write.
//!
//! - [`reset`] deletes the database and runs the migrations: the app then opens the setup
//!   wizard.
//! - [`seed`] creates a workspace and a catalogue for a [`Profile`]. The same seed gives the
//!   same IDs and data: IDs come from a counter, the clock steps by one millisecond per
//!   read, and the staff PIN salts are derived from the seed.
//!
//! Both write plain SQLite. The app encrypts it on its first open with the key the
//! computer's keychain holds for that folder (ADR-023), so nothing here touches the keychain.

use anyhow::{Context, Result, bail};
use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use pos_application::auth::{Argon2PinHasher, PinHasher};
use pos_application::catalog::CatalogService;
use pos_application::config::{ConfigService, RateInput, TaxCategoryInput};
use pos_application::context::{Actor, AppError, CommandContext};
use pos_application::setup::{CreateWorkspace, NewTaxRate, SetupService};
use pos_application::staff::{NewStaff, StaffService};
use pos_domain::catalog::{BarcodeInput, BarcodeQualifier, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::staff::Pin;
use pos_domain::{CategoryId, Clock, IdGenerator, Money, RatePpm, Role, SessionId, TaxCategoryId, Timestamp};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Tauri's bundle identifier: the app keeps its data in `<data dir>/<identifier>`.
pub const APP_IDENTIFIER: &str = "dev.saleos.pos";

/// The folder the desktop app uses on this computer (Tauri `app_data_dir`).
pub fn app_data_dir() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    Ok(base
        .context("cannot find the user's data folder; pass --dir")?
        .join(APP_IDENTIFIER))
}

/// The database file and its SQLite side files, as the app writes them.
fn database_files(dir: &Path) -> Vec<PathBuf> {
    let db = DbOptions::in_dir(dir).path;
    let name = db
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    ["", "-wal", "-shm", "-journal"]
        .iter()
        .map(|s| db.with_file_name(format!("{name}{s}")))
        .collect()
}

pub fn database_exists(dir: &Path) -> bool {
    DbOptions::in_dir(dir).path.exists()
}

/// Deletes the database and its side files. The backups folder and the logs are kept.
pub fn delete(dir: &Path) -> Result<()> {
    for f in database_files(dir) {
        match std::fs::remove_file(&f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("deleting {} (is the app still open?)", f.display()));
            }
        }
    }
    Ok(())
}

/// Deletes the database and creates an empty one with every migration applied.
pub async fn reset(dir: &Path) -> Result<PathBuf> {
    delete(dir)?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let opts = DbOptions::in_dir(dir);
    LocalDb::open(&opts)
        .await
        .context("creating the database")?
        .close()
        .await;
    Ok(opts.path)
}

// ------------------------------------------------------------- determinism ---

/// UUIDv7 values from a counter: the n-th uses timestamp `base_ms + n` and random bits
/// derived from `n`, so a seed always gives the same IDs (and they still sort by creation).
struct SeqIds {
    base_ms: u64,
    n: AtomicU64,
}

impl IdGenerator for SeqIds {
    fn new_uuid_v7(&self) -> uuid::Uuid {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        let mut random = [0u8; 10];
        random[2..].copy_from_slice(&n.to_be_bytes());
        uuid::Builder::from_unix_timestamp_millis(self.base_ms + n, &random).into_uuid()
    }
}

/// A clock that moves one millisecond each time it is read.
struct StepClock(AtomicI64);

impl Clock for StepClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.fetch_add(1, Ordering::SeqCst))
    }
}

/// Argon2id with the production parameters and a salt derived from the seed, so the stored
/// PIN hashes are the same on every run. The app verifies them like any other hash. A dev
/// database only: its PINs are printed by the tool anyway.
struct SeededHasher {
    seed: u64,
    n: AtomicU64,
    verify: Argon2PinHasher,
}

impl PinHasher for SeededHasher {
    fn hash(&self, pin: &Pin) -> Result<String, AppError> {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        let mut salt = [0u8; 16];
        salt[..8].copy_from_slice(&self.seed.to_be_bytes());
        salt[8..].copy_from_slice(&n.to_be_bytes());
        let salt = SaltString::encode_b64(&salt).map_err(|e| AppError::Internal(e.to_string()))?;
        let params = Params::new(64 * 1024, 3, 1, None).map_err(|e| AppError::Internal(e.to_string()))?;
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password(pin.expose().as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| AppError::Internal(format!("pin hashing failed: {e}")))
    }

    fn verify(&self, pin: &Pin, phc: &str) -> Result<bool, AppError> {
        self.verify.verify(pin, phc)
    }
}

/// xorshift64*: deterministic prices and picks, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A valid EAN-13: `prefix` (3 digits) + a 9-digit number + the check digit.
pub fn ean13(prefix: u32, n: u64) -> String {
    let body = format!("{prefix:03}{n:09}");
    let sum: u32 = body
        .bytes()
        .enumerate()
        .map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    format!("{body}{}", (10 - sum % 10) % 10)
}

// ------------------------------------------------------------------ profiles ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// 20 products for demos and manual testing: barcodes on every item, two tax rates, one
    /// product with size and colour variants, a manager and a cashier.
    Demo20,
    /// A typical shop: 5,000 variants, one barcode each.
    Shop5k,
    /// The benchmark catalogue: 50,000 variants and 100,000 barcodes.
    Bench50k,
}

impl Profile {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "demo20" => Self::Demo20,
            "shop5k" => Self::Shop5k,
            "bench50k" => Self::Bench50k,
            other => bail!("unknown profile {other:?}: demo20, shop5k or bench50k"),
        })
    }
}

/// The PINs the seeded staff sign in with (printed by the tool; a dev database only).
pub const STAFF: [(&str, Role, &str); 3] = [
    ("Owner", Role::Owner, "1234"),
    ("Manager", Role::Manager, "2580"),
    ("Cashier", Role::Cashier, "1357"),
];

#[derive(Debug, Default)]
pub struct Summary {
    pub products: usize,
    pub variants: usize,
    pub barcodes: usize,
    pub elapsed: Duration,
}

/// 2026-01-01T00:00:00Z: seeded records start here.
const BASE_MS: i64 = 1_767_225_600_000;

/// Creates the workspace and catalogue for `profile` in a new database under `dir`.
/// Fails if a database is already there (reset first).
pub async fn seed(dir: &Path, profile: Profile, seed: u64) -> Result<Summary> {
    let started = Instant::now();
    if database_exists(dir) {
        bail!("{} already has a database; reset it first", dir.display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let db = LocalDb::open(&DbOptions::in_dir(dir))
        .await
        .context("creating the database")?;
    let store = SqliteStore::new(db.clone());

    // The seed shifts every ID and timestamp, so two seeds never share IDs.
    let base = BASE_MS + i64::try_from(seed % 1_000_000_000).unwrap_or(0) * 1_000;
    let ids: Arc<dyn IdGenerator> = Arc::new(SeqIds {
        base_ms: u64::try_from(base).unwrap_or(0),
        n: AtomicU64::new(0),
    });
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicI64::new(base)));
    let hasher: Arc<dyn PinHasher> = Arc::new(SeededHasher {
        seed,
        n: AtomicU64::new(0),
        verify: Argon2PinHasher::new(),
    });

    let setup = SetupService {
        store: Arc::new(store.clone()),
        setup: Arc::new(store.clone()),
        hasher: hasher.clone(),
        clock: clock.clone(),
        ids: ids.clone(),
    };
    // Sri Lanka, the pilot country (D-02): LKR, prices include VAT 18%.
    let ws = setup
        .create_workspace(CreateWorkspace {
            business_name: "Kandy Corner Store (dev)".into(),
            legal_name: None,
            address: serde_json::json!({"line1": "12 Peradeniya Road", "city": "Kandy"}),
            phone: None,
            email: None,
            country_code: "LK".into(),
            timezone: "Asia/Colombo".into(),
            currency_code: "LKR".into(),
            prices_include_tax: true,
            location_name: "Main shop".into(),
            register_name: "Till 1".into(),
            owner_name: STAFF[0].0.into(),
            owner_pin: Pin::parse(STAFF[0].2)?,
            tax_rates: vec![NewTaxRate {
                name: "VAT 18%".into(),
                rate: RatePpm::new(180_000)?,
            }],
        })
        .await
        .map_err(|e| anyhow::anyhow!("creating the workspace: {e}"))?;
    let owner = CommandContext {
        register: ws.register,
        actor: Some(Actor {
            user_id: ws.owner_user_id,
            role: Role::Owner,
            session_id: SessionId::generate(ids.as_ref()),
        }),
        clock: clock.clone(),
        ids: ids.clone(),
        approval: None,
    };
    let err = |what: &'static str| move |e: AppError| anyhow::anyhow!("{what}: {e}");

    let staff = StaffService {
        store: Arc::new(store.clone()),
        staff: Arc::new(store.clone()),
        hasher,
    };
    for (name, role, pin) in &STAFF[1..] {
        staff
            .create(
                &owner,
                NewStaff {
                    display_name: (*name).into(),
                    role: *role,
                    pin: Pin::parse(pin)?,
                },
            )
            .await
            .map_err(err("adding staff"))?;
    }

    // The second rate: zero-rated goods in their own tax category.
    let config = ConfigService {
        store: Arc::new(store.clone()),
        config: Arc::new(store.clone()),
    };
    let zero = config
        .save_rate(
            &owner,
            None,
            RateInput {
                name: "Zero-rated 0%".into(),
                rate: RatePpm::new(0)?,
                from: None,
                to: None,
            },
            None,
        )
        .await
        .map_err(err("adding the zero rate"))?;
    let zero_cat = config
        .save_tax_category(
            &owner,
            None,
            TaxCategoryInput {
                name: "Zero-rated".into(),
                rates: vec![zero.tax_rate_id],
                is_default: false,
            },
            None,
        )
        .await
        .map_err(err("adding the zero-rated category"))?
        .tax_category_id;
    let standard = config
        .tax_config(&owner)
        .await
        .map_err(err("reading tax categories"))?
        .1
        .into_iter()
        .find(|c| c.is_default)
        .context("no default tax category")?
        .tax_category_id;
    let mut categories = BTreeMap::new();
    for name in ["Groceries", "Drinks", "Household", "Clothing", "Stationery"] {
        let c = config
            .save_category(&owner, None, name, None, None)
            .await
            .map_err(err("adding a category"))?;
        categories.insert(name, c.category_id);
    }

    let catalog = CatalogService {
        store: Arc::new(store.clone()),
        catalog: Arc::new(store.clone()),
    };
    let taxes = Taxes {
        standard,
        zero: zero_cat,
    };
    let products = match profile {
        Profile::Demo20 => demo20(&taxes, &categories),
        Profile::Shop5k => generated(seed, &taxes, &categories, 4_000, 200, 5, 1),
        Profile::Bench50k => generated(seed, &taxes, &categories, 0, 10_000, 5, 2),
    };
    let mut summary = Summary::default();
    for p in products {
        summary.products += 1;
        summary.variants += p.variants.len();
        summary.barcodes += p.variants.iter().map(|v| v.barcodes.len()).sum::<usize>();
        catalog.create(&owner, p).await.map_err(err("adding a product"))?;
    }
    db.close().await;
    summary.elapsed = started.elapsed();
    Ok(summary)
}

struct Taxes {
    standard: TaxCategoryId,
    zero: TaxCategoryId,
}

fn variant(sku: String, price: i64, barcodes: Vec<BarcodeInput>) -> VariantInput {
    let mut v = VariantInput::simple(Money::from_minor(price));
    v.sku = Some(sku);
    v.barcodes = barcodes;
    v
}

fn primary(code: String) -> BarcodeInput {
    BarcodeInput {
        qualifier: BarcodeQualifier::Primary,
        ..BarcodeInput::alias(&code)
    }
}

fn product(
    name: &str,
    category: Option<CategoryId>,
    tax: TaxCategoryId,
    unit: Unit,
    quick: bool,
    variants: Vec<VariantInput>,
) -> ProductInput {
    ProductInput {
        name: name.into(),
        description: None,
        category_id: category,
        product_type: ProductType::Stock,
        unit,
        tax_category_id: tax,
        quick_select: quick,
        variants,
    }
}

/// 20 recognisable products, prices in LKR cents. Barcodes use Sri Lanka's GS1 prefix 479.
fn demo20(t: &Taxes, cats: &BTreeMap<&str, CategoryId>) -> Vec<ProductInput> {
    // name, category, zero-rated, unit, price (cents), quick-select
    let items: [(&str, &str, bool, Unit, i64, bool); 19] = [
        ("Red rice 1 kg", "Groceries", true, Unit::Ea, 32_000, true),
        ("White sugar 1 kg", "Groceries", false, Unit::Ea, 29_500, true),
        ("Dhal (mysore) 500 g", "Groceries", true, Unit::Ea, 21_000, false),
        ("Coconut oil 750 ml", "Groceries", false, Unit::Ea, 89_000, false),
        ("Ceylon black tea 200 g", "Drinks", false, Unit::Ea, 65_000, true),
        ("Milk powder 400 g", "Groceries", false, Unit::Ea, 135_000, false),
        ("Fresh milk 1 l", "Drinks", true, Unit::Ea, 48_000, true),
        ("Bottled water 1.5 l", "Drinks", false, Unit::Ea, 13_000, true),
        ("Ginger beer 400 ml", "Drinks", false, Unit::Ea, 18_000, false),
        ("Bread loaf", "Groceries", true, Unit::Ea, 16_000, true),
        ("Eggs (10)", "Groceries", true, Unit::Ea, 52_000, false),
        ("Bananas (per kg)", "Groceries", true, Unit::Kg, 36_000, true),
        ("Laundry soap bar", "Household", false, Unit::Ea, 14_500, false),
        (
            "Dishwash liquid 500 ml",
            "Household",
            false,
            Unit::Ea,
            42_000,
            false,
        ),
        ("Toothpaste 120 g", "Household", false, Unit::Ea, 38_500, false),
        (
            "Exercise book 120 pages",
            "Stationery",
            false,
            Unit::Ea,
            22_000,
            false,
        ),
        ("Ballpoint pens (5)", "Stationery", false, Unit::Ea, 17_500, false),
        ("Matches (10 boxes)", "Household", false, Unit::Ea, 9_000, false),
        ("Biscuits 200 g", "Groceries", false, Unit::Ea, 24_000, true),
    ];
    let mut out: Vec<ProductInput> = items
        .iter()
        .enumerate()
        .map(|(i, (name, cat, zero, unit, price, quick))| {
            let n = i as u64 + 1;
            product(
                name,
                Some(cats[cat]),
                if *zero { t.zero } else { t.standard },
                *unit,
                *quick,
                vec![variant(
                    format!("DEMO-{n:03}"),
                    *price,
                    vec![primary(ean13(479, n))],
                )],
            )
        })
        .collect();
    // The variant product: a T-shirt in two sizes and two colours.
    let mut shirts = Vec::new();
    for (i, (size, colour)) in [("M", "White"), ("L", "White"), ("M", "Navy"), ("L", "Navy")]
        .iter()
        .enumerate()
    {
        let n = 100 + i as u64;
        let mut v = variant(
            format!("DEMO-TEE-{size}-{}", &colour[..1]),
            199_000,
            vec![primary(ean13(479, n))],
        );
        v.options = BTreeMap::from([
            ("Size".to_owned(), (*size).to_owned()),
            ("Colour".to_owned(), (*colour).to_owned()),
        ]);
        shirts.push(v);
    }
    out.push(product(
        "Cotton T-shirt",
        Some(cats["Clothing"]),
        t.standard,
        Unit::Ea,
        false,
        shirts,
    ));
    out
}

/// `simple` one-variant products, then `multi` products of `per` variants; each variant
/// has `codes` barcodes (a primary EAN-13 and, for 2, an in-store alias).
fn generated(
    seed: u64,
    t: &Taxes,
    cats: &BTreeMap<&str, CategoryId>,
    simple: u64,
    multi: u64,
    per: u64,
    codes: u64,
) -> Vec<ProductInput> {
    const WORDS: [&str; 16] = [
        "Fresh", "Classic", "Golden", "Island", "Spicy", "Sweet", "Royal", "Family", "Tropical", "Mild",
        "Premium", "Garden", "Ocean", "Village", "Hill", "Silver",
    ];
    const NOUNS: [&str; 12] = [
        "Rice",
        "Tea",
        "Biscuits",
        "Soap",
        "Noodles",
        "Juice",
        "Flour",
        "Spice mix",
        "Oil",
        "Candles",
        "Notebook",
        "Shampoo",
    ];
    let cat_list: Vec<CategoryId> = cats.values().copied().collect();
    let mut rng = Rng::new(seed);
    let mut serial = 0u64;
    let mut code = || -> Vec<BarcodeInput> {
        serial += 1;
        let mut v = vec![primary(ean13(479, serial))];
        if codes > 1 {
            v.push(BarcodeInput::alias(&ean13(200, serial)));
        }
        v
    };
    let mut out = Vec::with_capacity(usize::try_from(simple + multi).unwrap_or(0));
    for i in 0..simple + multi {
        let name = format!(
            "{} {} {}",
            WORDS[usize::try_from(rng.below(16)).unwrap_or(0)],
            NOUNS[usize::try_from(rng.below(12)).unwrap_or(0)],
            i + 1
        );
        let price = 5_000 + i64::try_from(rng.below(200_000)).unwrap_or(0);
        let count = if i < simple { 1 } else { per };
        let variants = (0..count)
            .map(|k| {
                let mut v = variant(
                    format!("P{:06}-{}", i + 1, k + 1),
                    price + i64::try_from(k).unwrap_or(0) * 500,
                    code(),
                );
                if count > 1 {
                    v.options = BTreeMap::from([("Size".to_owned(), format!("{}", k + 1))]);
                }
                v
            })
            .collect();
        let tax = if rng.below(5) == 0 { t.zero } else { t.standard };
        let cat = cat_list[usize::try_from(rng.below(cat_list.len() as u64)).unwrap_or(0)];
        out.push(product(&name, Some(cat), tax, Unit::Ea, false, variants));
    }
    out
}

/// Whole-database fingerprint for the determinism check: every row of every app table, in
/// primary-key order (the migration bookkeeping table is left out: it records install times).
pub async fn dump(dir: &Path) -> Result<Vec<String>> {
    let db = LocalDb::open(&DbOptions::in_dir(dir)).await?;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         AND name NOT LIKE '_sqlx_%' ORDER BY name",
    )
    .fetch_all(db.readers())
    .await?;
    let mut out = Vec::new();
    for t in tables {
        // `json_object` over every column gives one comparable string per row; `quote` renders
        // every type, the search index's BLOBs included, as a SQL literal.
        let cols: Vec<String> = sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{t}')"))
            .fetch_all(db.readers())
            .await?;
        if cols.is_empty() {
            continue;
        }
        let obj = cols
            .iter()
            .map(|c| format!("'{c}', quote(\"{c}\")"))
            .collect::<Vec<_>>()
            .join(", ");
        let rows: Vec<String> =
            sqlx::query_scalar(&format!("SELECT json_object({obj}) FROM \"{t}\" ORDER BY 1"))
                .fetch_all(db.readers())
                .await
                .with_context(|| format!("reading {t}"))?;
        let mut rows = rows;
        rows.sort();
        out.extend(rows.into_iter().map(|r| format!("{t} {r}")));
    }
    db.close().await;
    Ok(out)
}
