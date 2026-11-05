# 06 — Rust Interfaces

These are the **target signatures** that tasks implement against. They are precise enough to
review and to write tests from. Exact generics and lifetimes may change during
implementation, but the semantics may not change without updating this file.

Conventions:

- `pos-domain` is synchronous and has no IO.
- Ports in `pos-application` and `pos-device-ports` are `async` and object-safe, using
  `#[async_trait]`. Every port is held as `Arc<dyn Port>` in `AppServices`.
- Errors are `thiserror` enums. Each variant maps to exactly one catalogued error code
  ([07 §2](07-contracts.md#2-error-model)).

---

## 1. `pos-domain::primitives` (E03-T01, E03-T02, E03-T03)

```rust
/// ADR-005. One macro generates newtypes with Display/FromStr/serde/sqlx Type.
id_type!(BusinessId, LocationId, RegisterId, UserId, ProductId, VariantId, BarcodeId,
         CategoryId, TaxRateId, TaxCategoryId, CartId, CartLineId, SaleId, SaleLineId,
         TenderId, ReturnId, ReturnLineId, RefundId, MovementId, InventoryDocumentId,
         ReceiptId, PrintJobId, CashSessionId, AuditId, EventId, ScanEventId, ConflictId);

/// ADR-001. Minor units; currency is business-scoped context.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money(i64);
impl Money {
    pub const ZERO: Money = Money(0);
    pub fn from_minor(minor: i64) -> Self;
    pub fn minor(self) -> i64;
    pub fn checked_add(self, rhs: Money) -> Result<Money, ArithmeticError>;
    pub fn checked_sub(self, rhs: Money) -> Result<Money, ArithmeticError>;
    pub fn checked_neg(self) -> Result<Money, ArithmeticError>;
    pub fn is_negative(self) -> bool;
}
impl std::iter::Sum for Result<Money, ArithmeticError> { /* checked */ }

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Currency { pub code: CurrencyCode /* [u8;3] ISO-4217 */, pub exponent: u8 /* 0..=4 */ }
impl Currency { pub fn format(&self, m: Money, locale: &Locale) -> String; pub fn parse(&self, s: &str) -> Result<Money, ParseError>; }

/// ADR-002. Milli-units.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Quantity(i64);
impl Quantity {
    pub const ONE: Quantity = Quantity(1000);
    pub fn from_milli(m: i64) -> Self;
    pub fn milli(self) -> i64;
    pub fn is_whole(self) -> bool;                               // milli % 1000 == 0
    pub fn checked_add(self, rhs: Quantity) -> Result<Quantity, ArithmeticError>;
    pub fn validate_for_unit(self, unit: &Unit) -> Result<(), QuantityError>; // fraction allowed? > 0?
}

/// ADR-003.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RatePpm(u32); // 0..=1_000_000
impl RatePpm { pub fn new(ppm: u32) -> Result<Self, RateError>; pub fn as_decimal(self) -> Decimal; }

/// ADR-004.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoundingMode { HalfAwayFromZero, HalfEven }
pub fn round_to_minor(value: Decimal, mode: RoundingMode) -> Result<Money, ArithmeticError>;

/// Largest-remainder allocation; Σ result == total exactly; weights may be zero (then equal split).
pub fn allocate(total: Money, weights: &[Money]) -> Result<Vec<Money>, ArithmeticError>;

pub trait Clock: Send + Sync { fn now(&self) -> Timestamp; }        // UTC ms
pub trait IdGenerator: Send + Sync { fn new_uuid_v7(&self) -> uuid::Uuid; }
pub fn business_date(at: Timestamp, tz: chrono_tz::Tz) -> BusinessDate; // ADR-006
```

**Property tests** (proptest):

- `allocate` always sums exactly to the total.
- `round_to_minor` is symmetric for negative values.
- Money arithmetic never panics: overflow returns `Err`.
- `Quantity` parse→format round-trips.

## 2. `pos-domain::catalog` (E06-T02, E06-T03)

```rust
pub struct Product {                       // aggregate root (ADR-019)
    pub id: ProductId, pub name: String, pub description: Option<String>,
    pub category_id: Option<CategoryId>, pub product_type: ProductType, pub unit: UnitCode,
    pub tax_category_id: TaxCategoryId, pub quick_select: bool, pub status: Status,
    pub variants: Vec<Variant>, pub version: u64,
}
pub struct Variant {
    pub id: VariantId, pub is_default: bool, pub sku: Option<Sku>, pub options: BTreeMap<String,String>,
    pub price: Money, pub cost: Option<Money>, pub track_stock: bool,
    pub low_stock_threshold: Option<Quantity>, pub scan_qty: Quantity,
    pub barcodes: Vec<Barcode>, pub status: Status,
}
pub struct Barcode { pub id: BarcodeId, pub value: NormalizedBarcode, pub raw: String,
                     pub symbology: Symbology, pub qualifier: BarcodeQualifier, pub pack_qty: Quantity }

pub enum ProductType { Stock, NonStock, Service }  // NonStock/Service ⇒ every variant.track_stock = false

impl Product {
    pub fn create(cmd: CreateProduct, ids: &dyn IdGenerator) -> Result<Product, CatalogError>;
    pub fn apply_edit(&mut self, edit: ProductEdit) -> Result<ProductChangeSet, CatalogError>; // bumps version
    pub fn archive(&mut self) -> Result<(), CatalogError>;
    pub fn label_for(&self, v: &Variant) -> String;          // "M / Red"
}

/// E06-T03 normalization: trim, strip configured scanner prefix/suffix, uppercase alnum,
/// for numeric GTIN family: validate check digit when symbology known, zero-pad UPC-A→EAN-13
/// so 012345678905 and 0012345678905 resolve to the same item.
pub fn normalize_barcode(raw: &str, hint: Option<Symbology>) -> Result<NormalizedBarcode, BarcodeError>;

/// Uniqueness is checked by the use case via CatalogQueries + enforced by SQL unique index.
pub enum CatalogError { NameRequired, DuplicateSku(Sku), DuplicateBarcode{ value: NormalizedBarcode, owner: VariantId },
    NoVariants, MultipleDefaultVariants, FractionNotAllowedForUnit, NegativePrice, InvalidBarcode(BarcodeError), Archived }
```

## 3. `pos-domain::pricing` (E08-T01, E08-T02)

```rust
pub enum TaxMode { Inclusive, Exclusive }

pub struct TaxRateSnapshot { pub id: TaxRateId, pub name: String, pub rate: RatePpm }

pub enum DiscountValue { Percent(RatePpm), Fixed(Money) }
pub struct Discount { pub value: DiscountValue, pub reason: Option<String>, pub authorized_by: Option<UserId> }

pub struct PricingLine {
    pub line_id: CartLineId,
    pub quantity: Quantity,
    pub unit_price: Money,               // shelf price (incl. or excl. tax per TaxMode)
    pub line_discount: Option<Discount>,
    pub taxes: Vec<TaxRateSnapshot>,     // resolved for business_date (effective dates)
}

pub struct PricingPolicy { pub tax_mode: TaxMode, pub rounding: RoundingMode, pub cash_rounding_increment: Option<Money> }

pub struct PricedLine {
    pub line_id: CartLineId, pub gross: Money, pub line_discount: Money,
    pub order_discount_alloc: Money, pub tax_lines: Vec<TaxLine>, pub tax: Money,
    pub total: Money,                    // customer-facing
}
pub struct TaxLine { pub tax_rate_id: TaxRateId, pub name: String, pub rate: RatePpm, pub taxable: Money, pub tax: Money }

pub struct PricedCart {
    pub lines: Vec<PricedLine>,
    pub subtotal: Money,                 // Σ gross
    pub line_discount_total: Money,
    pub order_discount_total: Money,
    pub tax_total: Money,
    pub tax_breakdown: Vec<TaxLine>,     // aggregated by rate
    pub total: Money,                    // before cash rounding
}

pub fn price_cart(lines: &[PricingLine], order_discount: Option<&Discount>, policy: &PricingPolicy)
    -> Result<PricedCart, PricingError>;

/// Cash rounding only (ADR-004).
pub fn cash_rounding(total: Money, policy: &PricingPolicy) -> Money /* rounding delta */;

/// Cloud-side check of an ingested sale (02 §3). Recompute and compare; never overwrite.
pub fn verify_snapshot(snapshot: &SaleSnapshot) -> Result<(), SnapshotMismatch>;

pub enum PricingError { DiscountExceedsLine{ line: CartLineId }, DiscountExceedsOrder, NegativeTotal, Arithmetic(ArithmeticError) }
```

Algorithm, in normative order. The golden scenarios in `tests/fixtures/pricing/*.json` follow
it (E08-T03).

1. `gross = round(unit_price × qty / 1000)`.
2. `line_discount`:
   - Percent: `round(gross × p)`.
   - Fixed: the value. It must not exceed `gross`.
3. `net1 = gross − line_discount`.
4. `order_discount_total`:
   - Percent: `round(Σ net1 × p)`.
   - Fixed: the value. It must not exceed `Σ net1`.
5. Allocate `order_discount_total` across lines with `allocate(order_discount_total, [net1…])`.
6. `net2 = net1 − alloc`.
7. Tax for each rate on the line:
   - Inclusive: `tax_r = round(net2 × r / (1 + Σr))`. Any remainder from multiple rates is
     assigned to the largest rate.
   - Exclusive: `tax_r = round(net2 × r)`.
8. Line total:
   - Inclusive: `total = net2`.
   - Exclusive: `total = net2 + Σ tax_r`.
9. Cart total = `Σ line totals`. `subtotal` = `Σ gross`.

## 4. `pos-domain::cart` (E09-T01)

```rust
pub struct Cart { pub id: CartId, pub register_id: RegisterId, pub status: CartStatus,
                  pub lines: Vec<CartLine>, pub order_discount: Option<Discount>, pub revision: u64 }
pub struct CartLine { pub id: CartLineId, pub variant: SellableVariant, pub quantity: Quantity,
                      pub price_override: Option<Money>, pub line_discount: Option<Discount>,
                      pub added_via: AddVia, pub last_scan_event_id: Option<ScanEventId> }
/// Everything needed to price/display a line without another lookup.
pub struct SellableVariant { pub variant_id: VariantId, pub product_id: ProductId, pub name: String, pub label: String,
    pub sku: Option<Sku>, pub unit: Unit, pub price: Money, pub taxes: Vec<TaxRateSnapshot>,
    pub track_stock: bool, pub product_type: ProductType, pub cost: Option<Money> }

pub enum AddVia { Scan{ event_id: ScanEventId, barcode: NormalizedBarcode }, Search, QuickSelect, ManualCode }

impl Cart {
    /// BAR-005: a scan merges into the LAST line with the same variant if that line has no
    /// price override and no line discount; otherwise appends a new line.
    /// Qty added = barcode.pack_qty if qualifier=Pack else variant.scan_qty.
    pub fn add(&mut self, v: SellableVariant, qty: Quantity, via: AddVia, ids: &dyn IdGenerator) -> Result<CartChange, CartError>;
    pub fn set_quantity(&mut self, line: CartLineId, qty: Quantity) -> Result<CartChange, CartError>;
    pub fn remove_line(&mut self, line: CartLineId) -> Result<CartChange, CartError>;
    pub fn set_line_discount(&mut self, line: CartLineId, d: Option<Discount>) -> Result<CartChange, CartError>;
    pub fn set_order_discount(&mut self, d: Option<Discount>) -> Result<CartChange, CartError>;
    pub fn override_price(&mut self, line: CartLineId, price: Money) -> Result<CartChange, CartError>;
    pub fn clear(&mut self) -> Result<CartChange, CartError>;          // POS-003
    pub fn void(&mut self, reason: String) -> Result<CartChange, CartError>; // RET-001
    pub fn to_pricing_lines(&self) -> Vec<PricingLine>;
}
pub enum CartError { NotOpen, LineNotFound, InvalidQuantity(QuantityError), Empty, RevisionMismatch{ expected: u64, actual: u64 } }
```

## 5. `pos-domain::settlement` (E10-T01)

```rust
pub enum TenderKind { Cash, CardExternal, BankTransferExternal, QrExternal, OtherExternal }
pub struct TenderInput { pub kind: TenderKind, pub amount: Money, pub cash_received: Option<Money>,
                         pub external_reference: Option<String> }

pub struct Settlement { pub tenders: Vec<SettledTender>, pub rounding: Money, pub total_due: Money,
                        pub tendered_total: Money, pub change: Money }
pub struct SettledTender { pub kind: TenderKind, pub amount: Money, pub received: Option<Money>, pub change: Money,
                           pub status: TenderStatus /* Confirmed */, pub confirmation: ConfirmationSource /* Operator */,
                           pub external_reference: Option<String> }

/// Rules: Σ amount == total_due (after cash rounding when all-cash);
/// change only from cash and only from the LAST cash tender; change ≤ cash received;
/// non-cash amount may not exceed remaining due (no change from card); amounts > 0;
/// external tenders are recorded Confirmed + ConfirmationSource::Operator (PAY-003) — never Provider.
pub fn settle(total: Money, tenders: &[TenderInput], policy: &PricingPolicy) -> Result<Settlement, SettlementError>;

pub enum SettlementError { Insufficient{ remaining: Money }, OverTenderNonCash, ChangeFromNonCash, NonPositiveAmount, ReferenceTooLong }
```

## 6. `pos-domain::inventory` (E11-T01..T04)

```rust
pub enum StockBucket { Sellable, Damaged }
pub enum MovementType { Opening, Receipt, Sale, ReturnRestock, ReturnDamaged, Damage, Loss, Adjustment, WriteOff, Correction }
pub struct StockMovement { pub id: MovementId, pub location_id: LocationId, pub variant_id: VariantId, pub bucket: StockBucket,
    pub movement_type: MovementType, pub delta: Quantity /* signed */, pub unit_cost: Option<Money>,
    pub source: MovementSource, pub actor: UserId, pub occurred_at: Timestamp, pub business_date: BusinessDate }

pub enum NegativeStockPolicy { AllowAndFlag, Block }

pub fn movements_for_sale(sale: &SaleSnapshot, location: LocationId) -> Vec<StockMovement>;     // only track_stock lines
pub fn movements_for_return(ret: &ReturnPlan, location: LocationId) -> Vec<StockMovement>;      // by disposition
pub fn movements_for_document(doc: &InventoryDocumentDraft) -> Result<Vec<StockMovement>, InventoryError>;
/// Given current balances, decide: Ok(flags) listing variants that go negative, or Err when policy=Block.
pub fn check_negative(policy: NegativeStockPolicy, balances: &BalanceMap, movements: &[StockMovement])
    -> Result<Vec<NegativeStockFlag>, InventoryError>;
```

## 7. `pos-domain::returns` (E12-T01)

```rust
pub struct ReturnRequest { pub original_sale: SaleId, pub lines: Vec<ReturnLineRequest>, pub reason: ReturnReason,
                           pub note: Option<String>, pub refunds: Vec<TenderInput> }
pub struct ReturnLineRequest { pub sale_line_id: SaleLineId, pub quantity: Quantity, pub disposition: Disposition }
pub enum Disposition { Restock, Damaged, NoStockChange }

/// RET-004 + ADR-004 cumulative refund rule.
pub fn plan_return(original: &SaleSnapshot, already_returned: &HashMap<SaleLineId, Quantity>, req: &ReturnRequest)
    -> Result<ReturnPlan, ReturnError>;
pub struct ReturnPlan { pub lines: Vec<PlannedReturnLine>, pub subtotal: Money, pub tax: Money, pub total: Money,
                        pub refunds: Vec<SettledTender> }
pub enum ReturnError { QuantityExceedsUnreturned{ line: SaleLineId, available: Quantity }, LineNotInSale,
                       RefundMismatch{ expected: Money, got: Money }, SaleIsSandbox, EmptyReturn }
```

## 8. `pos-domain::access` (E05-T03)

```rust
pub enum Role { Owner, Manager, Cashier }

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumIter)]
pub enum Permission {
    SaleComplete, CartLineDiscount, CartOrderDiscount, DiscountAboveThreshold, PriceOverride, CartClear, CartVoid,
    ExternalTenderRecord, SalesView, ReceiptReprint,
    ReturnCreate, ReturnApprove,
    CatalogView, CatalogEdit, CatalogImport, CatalogExport, BarcodeCreateFromScan,
    InventoryView, InventoryReceive, InventoryAdjust,
    CashSessionOpen, CashSessionClose, CashSessionViewVariance,
    ReportsView, ReportsExport, AuditView,
    SettingsEdit, StaffManage, DeviceLink, SyncManage, ConflictResolve, DevtoolsUse,
}
pub fn role_grants(role: Role) -> &'static [Permission];   // matrix in 07-contracts §1.2
/// Some actions are granted to the actor's role but still need approval above a threshold.
pub fn approval_required(p: Permission, ctx: &ApprovalContext) -> Option<Permission /* approver must hold */>;
```

## 9. `pos-application` — context, authorization, WriteSet (E05-T03, E02-T03)

```rust
pub struct CommandContext {
    pub actor: Actor,                         // from the unlocked staff session held in Rust, never from IPC args
    pub register: RegisterContext,            // business/location/register ids, tz, currency, mode
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    pub approval: Option<ApprovalToken>,
}
pub struct Actor { pub user_id: UserId, pub role: Role, pub session_id: SessionId }

impl CommandContext {
    pub fn authorize(&self, p: Permission) -> Result<(), AppError>;                 // PERMISSION_DENIED
    pub fn authorize_with_approval(&self, p: Permission, ac: &ApprovalContext) -> Result<Option<UserId>, AppError>; // APPROVAL_REQUIRED
}

/// Manager override (E05-T04): verified PIN of a user holding `approver_permission`,
/// single-use, scoped to (action, entity, amount), expires after 120 s.
pub struct ApprovalToken { pub id: Uuid, pub approver: UserId, pub scope: ApprovalScope, pub expires_at: Timestamp }

pub struct WriteSet { /* private */ }
impl WriteSet { pub fn builder(ctx: &CommandContext) -> WriteSetBuilder; }
impl WriteSetBuilder {
    pub fn upsert_product(self, p: &Product, base_version: u64) -> Self;
    pub fn insert_sale(self, s: &SaleSnapshot) -> Self;
    pub fn insert_tenders(self, t: &[SettledTender]) -> Self;
    pub fn append_movements(self, m: &[StockMovement]) -> Self;          // also updates stock_balances in apply()
    pub fn insert_return(self, r: &ReturnPlan) -> Self;
    pub fn insert_inventory_document(self, d: &InventoryDocument) -> Self;
    pub fn insert_receipt(self, r: &ReceiptDocument, source: ReceiptSource) -> Self;
    pub fn put_cart(self, c: &Cart) -> Self;
    pub fn close_cart(self, id: CartId, sale: SaleId) -> Self;
    pub fn allocate_number(self, counter: CounterName, into: NumberSlot) -> Self; // receipt/return/doc numbers
    pub fn precondition(self, p: Precondition) -> Self;                   // e.g. ReturnQtyAvailable, CartRevision, NoOpenSession
    pub fn audit(self, a: AuditEntry) -> Self;
    pub fn outbox<E: SyncEvent>(self, e: E) -> Self;                      // envelope fields filled by apply()
    pub fn build(self) -> Result<WriteSet, WriteSetError>;               // ADR-008 validation
}

#[async_trait]
pub trait LocalStore: Send + Sync {
    async fn apply(&self, ws: WriteSet) -> Result<AppliedWriteSet, StoreError>;
}
pub struct AppliedWriteSet { pub numbers: HashMap<NumberSlot, String>, pub outbox: Vec<(EventId, u64 /*local_sequence*/)> }
pub enum StoreError { PreconditionFailed(Precondition), UniqueViolation{ constraint: String }, Busy, Io(String), Corrupt(String) }
```

### Query ports (implemented in `pos-local-db`)

```rust
#[async_trait] pub trait CatalogQueries: Send + Sync {
    async fn find_by_barcode(&self, b: &NormalizedBarcode) -> Result<Option<(SellableVariant, Barcode)>, StoreError>;
    async fn find_by_sku(&self, sku: &str) -> Result<Option<SellableVariant>, StoreError>;
    async fn search(&self, q: &str, limit: u32) -> Result<Vec<SearchHit>, StoreError>;
    async fn get_product(&self, id: ProductId) -> Result<Option<Product>, StoreError>;
    async fn list_products(&self, f: ProductFilter, page: Page) -> Result<Paged<ProductRow>, StoreError>;
    async fn quick_select(&self) -> Result<Vec<SellableVariant>, StoreError>;
    async fn barcode_owner(&self, b: &NormalizedBarcode) -> Result<Option<VariantId>, StoreError>;
}
#[async_trait] pub trait CartQueries     { async fn open_cart(&self, r: RegisterId) -> Result<Option<Cart>, StoreError>; async fn get(&self, id: CartId) -> Result<Option<Cart>, StoreError>; }
#[async_trait] pub trait SalesQueries    { async fn get_sale(&self, id: SaleId) -> Result<Option<SaleSnapshot>, StoreError>;
                                           async fn find_by_receipt(&self, no: &str) -> Result<Option<SaleSnapshot>, StoreError>;
                                           async fn returned_quantities(&self, id: SaleId) -> Result<HashMap<SaleLineId, Quantity>, StoreError>;
                                           async fn list(&self, f: SalesFilter, page: Page) -> Result<Paged<SaleRow>, StoreError>; }
#[async_trait] pub trait InventoryQueries{ async fn balances(&self, loc: LocationId, vs: &[VariantId]) -> Result<BalanceMap, StoreError>;
                                           async fn overview(&self, f: StockFilter, page: Page) -> Result<Paged<StockRow>, StoreError>;
                                           async fn history(&self, v: VariantId, page: Page) -> Result<Paged<StockMovement>, StoreError>; }
#[async_trait] pub trait ReportQueries   { async fn sales_summary(&self, r: DateRange, g: Grouping) -> Result<SalesSummary, StoreError>;
                                           async fn tender_mix(&self, r: DateRange) -> Result<Vec<TenderTotal>, StoreError>;
                                           async fn top_products(&self, r: DateRange, by: TopBy, n: u32) -> Result<Vec<TopProduct>, StoreError>; }
#[async_trait] pub trait OutboxQueries   { async fn next_batch(&self, max: u32) -> Result<Vec<OutboxEvent>, StoreError>;
                                           async fn mark_results(&self, r: &[PushResultItem], ack: u64) -> Result<(), StoreError>;
                                           async fn counts(&self) -> Result<OutboxCounts, StoreError>; }
```

### Application services (one per bounded area; each method = one IPC command)

```rust
impl SetupService     { async fn create_workspace(&self, input: CreateWorkspace) -> Result<WorkspaceDto, AppError>; /* first run, no actor */ }
impl AuthService      { async fn login(&self, user: UserId, pin: Pin) -> Result<SessionDto, AppError>;
                        async fn lock(&self) -> Result<(), AppError>; async fn unlock(&self, pin: Pin) -> Result<SessionDto, AppError>;
                        async fn request_approval(&self, ctx: &CommandContext, approver: UserId, pin: Pin, scope: ApprovalScope) -> Result<ApprovalToken, AppError>; }
impl StaffService     { async fn create(..); async fn update(..); async fn deactivate(..); async fn reset_pin(..); }
impl CatalogService   { async fn create_product(..); async fn update_product(..); async fn archive_product(..); async fn add_barcode(..); }
impl ImportService    { async fn parse(..); async fn set_mapping(..); async fn validate(..); async fn commit(..); async fn error_report_csv(..); }
impl ScanService      { async fn submit(&self, ctx: &CommandContext, scan: ScanInput) -> Result<ScanOutcome, AppError>; }
impl CartService      { async fn current(..); async fn add_variant(..); async fn set_qty(..); async fn remove(..); async fn discount(..); async fn clear(..); async fn void(..); }
impl CheckoutService  { async fn complete_sale(&self, ctx: &CommandContext, input: CompleteSale) -> Result<CompletedSaleDto, AppError>; }
impl InventoryService { async fn post_document(&self, ctx: &CommandContext, d: InventoryDocumentDraft) -> Result<DocumentDto, AppError>; }
impl ReturnService    { async fn create_return(&self, ctx: &CommandContext, r: ReturnRequest) -> Result<ReturnDto, AppError>; }
impl ReceiptService   { async fn preview(..); async fn export_pdf(&self, ctx, id: ReceiptId, path: SaveTarget) -> Result<(), AppError>; async fn print(..); async fn reprint(..); }
impl CashSessionService { async fn open(..); async fn close(..); async fn current(..); }
impl ReportService    { async fn summary(..); async fn export_csv(..); }
impl SyncService      { async fn status(..); async fn link(..); async fn retry_now(..); async fn conflicts(..); async fn resolve(..); async fn repair_queue(..); }
```

## 10. Device ports — `pos-device-ports` (E07, E13, E10)

```rust
pub struct ScanEvent {
    pub id: ScanEventId,               // unique per physical/simulated scan; duplicates of the SAME delivery share it
    pub source: ScanSource,            // KeyboardWedge | Virtual | Hid{device} | Serial{port}
    pub raw: String,
    pub symbology_hint: Option<Symbology>,
    pub received_at: Timestamp,
}
pub enum DeviceStatus { Disconnected, Connecting, Ready, Error(DeviceError) }

#[async_trait]
pub trait BarcodeInputPort: Send + Sync {
    async fn start(&self) -> Result<(), DeviceError>;
    async fn stop(&self) -> Result<(), DeviceError>;
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ScanEvent>;
    fn status(&self) -> watch::Receiver<DeviceStatus>;
}

pub struct PrinterCapabilities { pub paper_width_mm: u8 /*58|80*/, pub chars_per_line: u8, pub cut: CutSupport,
                                 pub drawer_kick: bool, pub qr: bool, pub raster_image: bool, pub codepages: Vec<CodePage> }
pub enum PrintStatus { Queued, Submitted, Completed, Failed(PrintFailure), Unknown }
pub enum PrintFailure { PaperOut, CoverOpen, Disconnected, Timeout, Rejected(String) }
pub struct PrintPayload { pub job_id: PrintJobId, pub bytes: Vec<u8>, pub kind: PayloadKind /* EscPos | Pdf | Text */ }

#[async_trait]
pub trait ReceiptOutputPort: Send + Sync {
    fn capabilities(&self) -> PrinterCapabilities;
    async fn submit(&self, p: PrintPayload, timeout: Duration) -> Result<PrintStatus, DeviceError>;
    async fn status(&self, job: PrintJobId) -> Result<PrintStatus, DeviceError>;
}

#[async_trait]
pub trait CashDrawerPort: Send + Sync { async fn open(&self) -> Result<(), DeviceError>; }   // Stage 5

pub enum PaymentStatus { Pending, Approved{ provider_ref: String }, Declined{ reason: String }, Reversed, Unknown, TimedOut }
#[async_trait]
pub trait PaymentPort: Send + Sync {
    async fn initiate(&self, req: PaymentRequest /* amount, idempotency_key */) -> Result<PaymentAttempt, DeviceError>;
    async fn query(&self, attempt: PaymentAttemptId) -> Result<PaymentStatus, DeviceError>;
    async fn cancel(&self, attempt: PaymentAttemptId) -> Result<PaymentStatus, DeviceError>;
}
```

The **virtual implementations** in `pos-device-virtual` add a scenario API used by e2e tests
and by the dev panel:

```rust
impl VirtualScanner { pub fn scan(&self, raw: &str); pub fn scan_with_suffix(&self, raw: &str, s: Suffix);
                      pub fn burst(&self, codes: &[&str], gap: Duration); pub fn duplicate_last_delivery(&self);
                      pub fn malformed(&self, partial: &str); pub fn disconnect(&self); pub fn reconnect(&self); }
impl VirtualPrinter { pub fn script(&self, outcomes: Vec<PrintStatus>); pub fn captured(&self) -> Vec<CapturedJob>;
                      pub fn set_capabilities(&self, c: PrinterCapabilities); }  // writes captures to target/virtual-printer/*.bin + .txt
impl MockPaymentProvider { pub fn script(&self, s: PaymentScenario /* Approve|Decline|Timeout|DuplicateCallback|Delayed(d)|Reverse|Unknown */); }
```

## 11. Sync ports — `pos-application::sync` (E17)

```rust
#[async_trait]
pub trait SyncTransport: Send + Sync {
    async fn push(&self, batch: PushRequest) -> Result<PushResponse, TransportError>;
    async fn pull(&self, cursor: Option<Cursor>, limit: u32) -> Result<PullResponse, TransportError>;
    async fn bootstrap(&self, page: Option<BootstrapToken>) -> Result<BootstrapPage, TransportError>;
}
pub enum TransportError { Offline, Timeout, Unauthorized, DeviceRevoked, UpgradeRequired{ min_version: String },
                          Server{ status: u16, problem: Option<Problem> }, Decode(String) }
impl TransportError { pub fn is_transient(&self) -> bool; }

#[async_trait]
pub trait CredentialStore: Send + Sync {          // OS keychain (ADR-013)
    async fn get_refresh_token(&self) -> Result<Option<SecretString>, CredError>;
    async fn set_refresh_token(&self, t: SecretString) -> Result<(), CredError>;
    async fn clear(&self) -> Result<(), CredError>;
}

pub struct Backoff { base: Duration /*2s*/, cap: Duration /*5m*/ }  // full jitter: sleep = rand(0, min(cap, base·2^n))
```

## 12. Cloud — `cloud-api` event appliers (E17-T04..T06)

```rust
pub struct Principal { pub business_id: BusinessId, pub register_id: RegisterId, pub device_id: DeviceId }

#[async_trait]
pub trait EventApplier: Send + Sync {
    fn event_type(&self) -> &'static str;
    fn supported_versions(&self) -> RangeInclusive<u32>;
    async fn apply(&self, tx: &mut TenantTx<'_>, p: &Principal, env: &EventEnvelope) -> Result<ApplyOutcome, ApplyError>;
}
pub enum ApplyOutcome { Applied{ server_seq: Option<i64>, new_version: Option<u64> },
                        Conflict{ server_version: u64, server_snapshot: serde_json::Value, reason: ConflictReason } }
pub enum ApplyError { Rejected{ code: &'static str, detail: String } /* permanent */, Transient(anyhow::Error) }

/// Ingestion loop per register (ADR-010): for each event in local_sequence order:
///   BEGIN; SET LOCAL app.business_id; lock register_sync_state row FOR UPDATE;
///   if inbox has event_id → return stored result (verify payload hash) ;
///   if local_sequence != ack+1 → OutOfOrder (stop batch);
///   outcome = applier.apply(...) ; insert inbox row ; ack = local_sequence ; COMMIT.
///   Rejected ⇒ still insert inbox row with status rejected + COMMIT (handled).
///   Transient ⇒ ROLLBACK, stop batch, return results so far.
```
