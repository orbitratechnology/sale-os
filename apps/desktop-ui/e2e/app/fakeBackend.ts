/**
 * A scripted stand-in for the Rust commands, for browser tests of the screens.
 *
 * It records every IPC call with its arguments (what the tests assert) and returns
 * well-formed DTOs. Pricing here is deliberately simple: totals are Rust's job and are
 * tested there (pricing goldens, cart integration tests, the IPC harness).
 */
import type { CartLine, CartView, CatalogHit, IpcError, Session, SignInUser, Workspace } from "../../src/ipc";
import type {
  CategoryDto_Serialize as CategoryDto,
  Permission,
  Role,
  ProductDto_Serialize as ProductDto,
  ProductInputDto_Deserialize as ProductInput,
  TaxCategoryConfigDto,
  TaxRateConfigDto_Serialize as TaxRateConfigDto,
} from "../../src/ipc/bindings";

export interface FakeProduct {
  variantId: string;
  productId: string;
  name: string;
  sku: string;
  barcodes: Record<string, number>; // code → units per scan
  priceMinor: bigint;
  /** E09-T04: on the quick-select grid, in this category; `label` names a variant. */
  quick?: boolean;
  category?: { categoryId: string; name: string };
  label?: string;
}

interface StaffUser extends SignInUser {
  pin: string;
  failed: number;
  /** Staff management (E05-T06); absent means active, version 1. */
  active?: boolean;
  version?: number;
  mustChangePin?: boolean;
}

export interface FakeState {
  setupComplete: boolean;
  users: StaffUser[];
  session: Session | null;
  products: FakeProduct[];
  lines: { lineId: string; variantId: string; qtyMilli: bigint; discountPpm: number | null }[];
  orderDiscountPpm: number | null;
  /** E09-T02: the cart was left open by the last run (offered once); these lines were repriced. */
  recoverable?: boolean;
  repricedOnRecovery?: string[];
  cartId: string;
  revision: number;
  /** ADR-023: the data is encrypted and this computer lacks the key. */
  locked?: boolean;
  recoveryKeyConfirmed?: boolean;
  /** POS-012: carts set aside, oldest first. */
  parked: { cartId: string; label: string | null; parkedAt: string; parkedByName: string; lines: FakeState["lines"]; orderDiscountPpm: number | null }[];
  seenScans: Set<string>;
  lockedUntil: number | null;
  /** Issued approval tokens: id → scope (single use). */
  tokens: Map<string, { permission: Permission; entityId: string; maxPpm: number | null }>;
  /** The catalogue as the editor sees it. */
  catalog: Map<string, ProductDto>;
  /** When set, product_list answers with this many generated rows (virtualization tests). */
  bigCatalog: number | null;
  /** Completed sales: receipt numbers in order. */
  receipts: string[];
  /** Completed sales with their lines, for returns. */
  sales: FakeSale[];
  /** The cash drawer (E15-T02). Totals are kept simply: cash net of change, and the count. */
  /** E19-T03: backups (newest first), the second folder, and whether a restore restarted the app. */
  backups?: { id: string; kind: string; createdAt: string; sizeKb: number; sales: number; secondFolderOnly: boolean }[];
  backupFolder?: string | null;
  restartedFrom?: string | null;
  /** E19-T02: the disk is full, so nothing can be saved. */
  diskFull?: boolean;
  drawer: {
    required: boolean;
    seq: number;
    session: { id: string; number: string; floatMinor: bigint; cashInMinor: bigint; sales: number; openedBy: string } | null;
    lastClosed: { id: string; number: string } | null;
  };
  /** Inventory documents posted so far (for their numbers). */
  documents: number;
  /** E11-T06: stock figures that differ from their movements (what `stock_verify` finds), and
   * the rebuilds made (each clears them, as Rust's does). */
  stockDrift: { itemName: string; sku: string | null; locationName: string; bucket: string; recordedMilli: string; fromMovementsMilli: string }[];
  stockRebuilds: number;
  /** CSV exports made. */
  exports: number;
  /** Business settings: key → value (JSON text) and version. */
  settings: Map<string, { valueJson: string; version: number }>;
  /** Completed returns: id → number. */
  returns: Map<string, { number: string; totalMinor: bigint }>;
  /** Outcomes for the next print submissions: "printed", "paper_out" or "timeout". */
  printScript: string[];
  /** This till's devices (E20-T11). */
  devices: { deviceId: string; kind: string; role: string; name: string; configJson: string; enabled: boolean; version: number }[];
  /** Drawer pulses sent (no sale and cash sales). */
  drawerOpens: string[];
  /** Card-terminal attempts (E22-T02, sandbox rules: .01 declines, .02 uncertain). */
  attempts: Map<string, { cartId: string; amountMinor: string; status: string; polls: number; used: boolean; reason: string | null }>;
  printJobs: Map<string, { status: string; reprint: boolean; errorCode: string | null; attempts: number }>;
  printed: number;
  /** What the next "Save as" dialogs answer: a file name, or null for Cancel. */
  saveAs: (string | null)[];
  /** What the next "Open" dialog picks: CSV text and a file name, or null for Cancel. */
  openFile: { name: string; text: string } | null;
  /** Import jobs by id: the rows and what validation decided. */
  imports: Map<string, { name: string; header: string[]; rows: string[][]; status: string; valid: number[]; done?: number }>;
  /** A commit writes one row per this many ms (0: at once), so its progress bar can be seen. */
  importStepMs: number;
  /** The business profile besides the name and till name (which live in `workspace`). */
  business: { legalName: string | null; taxRegistrationNo: string | null; addressLines: string[]; phone: string | null; email: string | null; version: number; registerVersion: number; logo?: string | null };
  /** `scanner.error_sound` (E07-T04). */
  errorSound?: boolean;
  /** SET-001: what the logo dialog hands over next (`null`: the owner cancels). */
  logoFile?: "image" | "notAnImage" | null;
  /** Product categories (E06-T05). */
  categories: CategoryDto[];
  /** Tax configuration (E06-T05). */
  taxRates: TaxRateConfigDto[];
  taxCategories: TaxCategoryConfigDto[];
  /** Sync and linking (E17-T08). */
  cloud: {
    configured: boolean;
    linkState: string;
    registerCode: string;
    pending: number;
    mayJoin: boolean;
    signedIn: boolean;
    /** Refused events and conflicts shown on the sync page (E17-T09). */
    refused: { eventId: string; localSequence: number; eventType: string; code: string; kind: string }[];
    conflicts: number;
    /** Sales the cloud has applied (AT-13): sale id → applied. */
    synced: Set<string>;
    /** Consolidated reports (E14-T05): whether the cloud answers, how many registers are
     * behind, and the reports kept from the last fetch (range and grouping → report). */
    online: boolean;
    unsyncedRegisters: number;
    kept: Record<string, unknown>;
  };
}

export interface FakeSale {
  saleId: string;
  receiptNumber: string;
  lines: { saleLineId: string; name: string; qtyMilli: bigint; totalMinor: bigint; returnedMilli: bigint }[];
  localTime: string;
  cashierUserId: string;
  cashierName: string;
  tenders: { kind: string; amountMinor: bigint; reference: string | null }[];
  totalMinor: bigint;
  changeMinor: bigint;
  returns: { returnId: string; returnNumber: string; totalMinor: bigint; reason: string; refunds: string[] }[];
  /** D-02: the tax invoice issued for it, if any. */
  taxInvoice?: { number: string; purchaserTin: string; purchaserName: string; purchaserAddress: string[] };
}

export type Preset = "fresh" | "signIn" | "checkout" | "locked";

let n = 0;
const id = () => `0190a5b0-0000-7000-8000-${String(++n).padStart(12, "0")}`;

export function products(): FakeProduct[] {
  return [
    { variantId: id(), productId: id(), name: "Cola 330ml", sku: "COLA", barcodes: { "4006381333931": 1, CASE12: 12 }, priceMinor: 120n },
    { variantId: id(), productId: id(), name: "Crisps", sku: "CRISP", barcodes: { "5000159407236": 1 }, priceMinor: 85n },
    { variantId: id(), productId: id(), name: "Chocolate bar", sku: "CHOC", barcodes: { "7622210449283": 1 }, priceMinor: 99n },
  ];
}

function dtoFor(p: FakeProduct): ProductDto {
  return {
    productId: p.productId,
    name: p.name,
    description: null,
    categoryId: null,
    productType: "stock",
    unit: "ea",
    taxCategoryId: TAX,
    quickSelect: false,
    archived: false,
    version: 1,
    variants: [
      {
        variantId: p.variantId,
        isDefault: true,
        sku: p.sku,
        options: {},
        priceMinor: p.priceMinor.toString(),
        costMinor: null,
        trackStock: true,
        // As in the stock report below.
        lowStockThresholdMilli: ({ "Cola 330ml": "12000", Crisps: "5000" } as Record<string, string>)[p.name] ?? null,
        scanQtyMilli: "1000",
        barcodes: Object.entries(p.barcodes).map(([raw, n]) => ({
          value: raw,
          raw,
          symbology: "ean13",
          qualifier: n > 1 ? "pack" : "primary",
          packQtyMilli: String(n * 1000),
        })),
        archived: false,
      },
    ],
  };
}

const TAX = "0190a5b0-0000-7000-8000-00000000ta01";
const RATE = "0190a5b0-0000-7000-8000-00000000ra01";

export function initialState(preset: Preset): FakeState {
  const s = initialStateInner(preset === "locked" ? "signIn" : preset);
  if (preset === "locked") s.locked = true;
  for (const p of s.products) s.catalog.set(p.productId, dtoFor(p));
  return s;
}

function initialStateInner(preset: Preset): FakeState {
  const owner: StaffUser = { userId: id(), displayName: "Olu", role: "owner", pin: "1234", failed: 0 };
  const cashier: StaffUser = { userId: id(), displayName: "Cal", role: "cashier", pin: "3333", failed: 0 };
  const manager: StaffUser = { userId: id(), displayName: "Mia", role: "manager", pin: "2222", failed: 0 };
  return {
    setupComplete: preset !== "fresh",
    users: preset === "fresh" ? [] : [owner, manager, cashier],
    session: preset === "checkout" ? { userId: owner.userId, displayName: owner.displayName, role: owner.role, mustChangePin: false } : null,
    products: products(),
    lines: [],
    orderDiscountPpm: null,
    cartId: id(),
    revision: 0,
    parked: [],
    seenScans: new Set(),
    lockedUntil: null,
    tokens: new Map(),
    catalog: new Map(),
    bigCatalog: null,
    receipts: [],
    sales: [],
    returns: new Map(),
    drawer: { required: false, seq: 0, session: null, lastClosed: null },
    documents: 0,
    stockDrift: [],
    stockRebuilds: 0,
    exports: 0,
    settings: new Map(FAKE_SETTINGS.map((d) => [d.key, { valueJson: d.defaultJson, version: 1 }])),
    printScript: [],
    devices: [],
    drawerOpens: [],
    attempts: new Map(),
    printJobs: new Map(),
    printed: 0,
    saveAs: [],
    openFile: null,
    importStepMs: 0,
    imports: new Map(),
    business: { legalName: null, taxRegistrationNo: "GB123456789", addressLines: ["1 High Street", "Leeds LS1 1AA"], phone: null, email: null, version: 1, registerVersion: 1 },
    categories: [],
    taxRates: [{ taxRateId: RATE, name: "VAT 20%", ratePpm: 200000, from: null, to: null, archived: false, version: 1 }],
    taxCategories: [{ taxCategoryId: TAX, name: "Standard", isDefault: true, rateIds: [RATE], archived: false, version: 1 }],
    cloud: { configured: true, linkState: "unlinked", registerCode: "R01", pending: 12, mayJoin: false, signedIn: false, refused: [], conflicts: 0, synced: new Set(), online: true, unsyncedRegisters: 1, kept: {} },
  };
}

const staffDto = (u: StaffUser) => ({
  userId: u.userId,
  displayName: u.displayName,
  role: u.role,
  active: u.active !== false,
  mustChangePin: u.mustChangePin ?? false,
  version: u.version ?? 1,
});

function ownerOnly(s: FakeState) {
  if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
}

/** Rust's name rules: required, at most 40 characters, unique among active staff. */
function staffName(s: FakeState, raw: string, except: string | null): string {
  const name = raw.trim();
  if (!name) fail("VALIDATION_FAILED", { field: "displayName" });
  if ([...name].length > 40) fail("VALIDATION_FAILED", { field: "displayName", value: "40" });
  if (s.users.some((u) => u.active !== false && u.userId !== except && u.displayName.toLowerCase() === name.toLowerCase()))
    fail("VALIDATION_FAILED", { field: "displayName", value: "taken" });
  return name;
}

function staffAt(s: FakeState, userId: string, baseVersion: number): StaffUser {
  const u = s.users.find((x) => x.userId === userId) ?? fail("VALIDATION_FAILED");
  if ((u.version ?? 1) !== baseVersion) fail("CONFLICT_VERSION");
  return u;
}

function lastOwner(s: FakeState, u: StaffUser) {
  if (!s.users.some((x) => x.userId !== u.userId && x.role === "owner" && x.active !== false))
    fail("VALIDATION_FAILED", { field: "role", value: "lastOwner" });
}

function fail(code: IpcError["code"], details: IpcError["details"] = null): never {
  const e: IpcError = { code, message: code, retryable: false, correlationId: id(), details };
  throw e;
}

function attemptDto(attemptId: string, x: { cartId: string; amountMinor: string; status: string; used: boolean; reason: string | null }) {
  const ok = x.status === "approved";
  return {
    attemptId, cartId: x.cartId, provider: "sandbox", amountMinor: x.amountMinor, status: x.status, reason: x.reason,
    providerRef: ok ? `SBX-${attemptId.slice(-6)}` : null, authCode: ok ? "123456" : null, cardBrand: ok ? "VISA" : null, maskedPan: ok ? "****4242" : null,
    usable: ok && !x.used,
  };
}

const div = (a: bigint, b: bigint) => (a >= 0n ? (a + b / 2n) / b : (a - b / 2n) / b);

/** Sellable on hand per product name, matching the fake stock report. */
const FAKE_DOCUMENT = "0190a5b0-0000-7000-8000-00000000d0c1";
const FAKE_STOCK: Record<string, bigint> = { "Chocolate bar": -2000n, "Cola 330ml": 40000n, Crisps: 3000n };

/** A few registry entries, shaped like Rust's (crates/pos-application/src/settings.rs). */
const FAKE_SETTINGS = [
  { key: "inventory.allow_negative", group: "Inventory", label: "Allow selling below zero stock", help: "On: sell anyway.", kind: { type: "bool" }, defaultJson: "true" },
  { key: "catalog.default_unit", group: "Products", label: "Unit for new products", help: "What a new product is sold by.", kind: { type: "choice", options: ['"ea"', '"kg"', '"g"', '"l"', '"ml"', '"m"', '"cm"', '"box"', '"pack"'] }, defaultJson: '"ea"' },
  { key: "catalog.default_track_stock", group: "Products", label: "Track stock for new products", help: "Off: not stock-tracked.", kind: { type: "bool" }, defaultJson: "true" },
  { key: "discount.cashier_max_ppm", group: "Security", label: "Largest discount a cashier may give", help: "Above this, a manager approves.", kind: { type: "ppm" }, defaultJson: "100000" },
  { key: "security.lock_timeout_secs", group: "Security", label: "Lock the till after this many idle seconds", help: "0 never locks.", kind: { type: "int", min: 0, max: 3600 }, defaultJson: "300" },
  { key: "receipt.paper_width_mm", group: "Receipts", label: "Paper width (mm)", help: "58 or 80.", kind: { type: "choice", options: ["58", "80"] }, defaultJson: "80" },
  { key: "receipt.footer_lines", group: "Receipts", label: "Lines at the bottom of receipts", help: "Up to 5 lines.", kind: { type: "lines", maxLines: 5, maxChars: 48 }, defaultJson: "[]" },
  { key: "scanner.suffix", group: "Scanner", label: "Key the scanner sends after a code", help: "Match the scanner.", kind: { type: "choice", options: ['"Enter"', '"Tab"', '"none"'] }, defaultJson: '"Enter"' },
  { key: "tax.registered", group: "Taxes", label: "Registered for VAT", help: "Off: sales carry no tax.", kind: { type: "bool" }, defaultJson: "true" },
] as const;

function parkedList(s: FakeState) {
  return s.parked.map((p) => {
    const total = view({ ...s, lines: p.lines, orderDiscountPpm: p.orderDiscountPpm }).totalMinor;
    return { cartId: p.cartId, label: p.label, parkedAt: p.parkedAt, parkedByName: p.parkedByName, lines: p.lines.length, totalMinor: total };
  });
}

export function view(s: FakeState): CartView {
  const lines: CartLine[] = s.lines.map((l) => {
    const p = s.products.find((x) => x.variantId === l.variantId);
    if (!p) throw new Error("unknown variant");
    const gross = div(p.priceMinor * l.qtyMilli, 1000n);
    const disc = l.discountPpm === null ? 0n : div(gross * BigInt(l.discountPpm), 1_000_000n);
    const total = gross - disc;
    return {
      lineId: l.lineId,
      variantId: p.variantId,
      productId: p.productId,
      name: p.name,
      label: "",
      sku: p.sku,
      unit: "ea",
      quantityMilli: l.qtyMilli.toString(),
      unitPriceMinor: p.priceMinor.toString(),
      priceOverridden: false,
      lineDiscount: l.discountPpm === null ? null : { kind: "percent", ratePpm: l.discountPpm, reason: null },
      grossMinor: gross.toString(),
      discountMinor: disc.toString(),
      taxMinor: div(total, 6n).toString(),
      totalMinor: total.toString(),
      addedVia: "scan",
      // The same on-hand figures as the fake stock report.
      onlyInStockMilli: (() => {
        const have = FAKE_STOCK[p.name];
        return have !== undefined && l.qtyMilli > have ? have.toString() : null;
      })(),
      scaleLabel: null,
    };
  });
  const subtotal = lines.reduce((a, l) => a + BigInt(l.grossMinor), 0n);
  const lineDisc = lines.reduce((a, l) => a + BigInt(l.discountMinor), 0n);
  const afterLines = subtotal - lineDisc;
  const orderDisc = s.orderDiscountPpm === null ? 0n : div(afterLines * BigInt(s.orderDiscountPpm), 1_000_000n);
  const total = afterLines - orderDisc;
  return {
    cartId: s.cartId,
    revision: s.revision,
    currencyCode: "GBP",
    currencyExponent: 2,
    taxMode: "inclusive",
    discountLimitPpm: 100_000,
    lines,
    orderDiscount: s.orderDiscountPpm === null ? null : { kind: "percent", ratePpm: s.orderDiscountPpm, reason: null },
    subtotalMinor: subtotal.toString(),
    lineDiscountTotalMinor: lineDisc.toString(),
    orderDiscountTotalMinor: orderDisc.toString(),
    taxTotalMinor: div(total, 6n).toString(),
    taxBreakdown: lines.length ? [{ name: "VAT 20%", ratePpm: 200000, taxableMinor: (total - div(total, 6n)).toString(), taxMinor: div(total, 6n).toString() }] : [],
    totalMinor: total.toString(),
    itemCountMilli: s.lines.reduce((a, l) => a + l.qtyMilli, 0n).toString(),
    repricedLines: [],
  };
}

function hit(p: FakeProduct): CatalogHit {
  return { productId: p.productId, variantId: p.variantId, name: p.name, label: p.label ?? "", sku: p.sku, priceMinor: p.priceMinor.toString(), unit: "ea", trackStock: true, scanQtyMilli: "1000", matchedBy: "search", onHandMilli: null };
}

/** The approval rule Rust applies (authorize_with_approval): cashiers need a covering token. */
function approve(s: FakeState, permission: Permission, maxPpm: number | null, token: unknown, entityId: string = s.cartId) {
  if (needSession(s).role !== "cashier") return;
  const needed = { permission, entityId, maxPpm };
  if (token === null || token === undefined) fail("APPROVAL_REQUIRED", { remainingAttempts: null, retryAtMs: null, scope: needed });
  const granted = s.tokens.get(String(token));
  s.tokens.delete(String(token));
  const covers =
    granted?.permission === permission && granted.entityId === entityId && (maxPpm === null || (granted.maxPpm ?? -1) >= maxPpm);
  if (!covers) fail("APPROVAL_INVALID");
}

function needSession(s: FakeState): Session {
  return s.session ?? fail("SESSION_LOCKED");
}

function add(s: FakeState, p: FakeProduct, qtyMilli: bigint): string {
  const last = [...s.lines].reverse().find((l) => l.variantId === p.variantId);
  s.revision += 1;
  if (last && last.discountPpm === null) {
    last.qtyMilli += qtyMilli;
    return last.lineId;
  }
  const lineId = id();
  s.lines.push({ lineId, variantId: p.variantId, qtyMilli, discountPpm: null });
  return lineId;
}

// Typed, so a new field in the contract fails the typecheck here instead of at run time.
const workspace: Workspace = { businessName: "Corner Shop", registerName: "Till 1", registerCode: "R01", currencyCode: "GBP", currencyExponent: 2, mode: "sandbox" };

/** Handles one command. Throws an IpcError-shaped object for refusals, as Tauri does. */
/** The real app's IPC card-data guard (E19-T05), simplified: any Luhn-valid 13–19 digit run. */
function cardLike(v: unknown, key = ""): boolean {
  if (typeof v === "string") {
    if (["pin", "ownerPin", "password", "raw", "query", "barcode", "recoveryKey", "id"].includes(key)) return false;
    if (/(Id|Ids|Ms)$/.test(key) || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v)) return false;
    return v
      .replace(/[ -]/g, "")
      .split(/\D+/)
      .some((run) => {
        if (run.length < 13 || run.length > 19) return false;
        let sum = 0;
        [...run].reverse().forEach((c, i) => {
          let d = Number(c);
          if (i % 2) d = d * 2 > 9 ? d * 2 - 9 : d * 2;
          sum += d;
        });
        return sum % 10 === 0;
      });
  }
  if (Array.isArray(v)) return v.some((x) => cardLike(x, key));
  if (v && typeof v === "object") return Object.entries(v).some(([k, x]) => cardLike(x, k));
  return false;
}

export const RECOVERY_KEY = "0123ABCD-4567EF01-89ABCDEF-01234567-89ABCDEF-01234567-89ABCDEF-01234567";

export function handle(s: FakeState, cmd: string, a: Record<string, unknown>): unknown {
  if (cardLike(a)) fail("CARD_DATA_REJECTED");
  if (s.locked) {
    if (cmd !== "db_unlock") fail("STORE_LOCKED");
    if (String(a.recoveryKey).replace(/[\s-]/g, "").toUpperCase() !== RECOVERY_KEY.replace(/-/g, "")) fail("STORE_LOCKED");
    s.locked = false;
    return null;
  }
  switch (cmd) {
    case "app_bootstrap_state":
      return { setupComplete: s.setupComplete, workspace: s.setupComplete ? workspace : null, session: s.session };
    case "setup_create_workspace": {
      const input = a.input as { ownerName: string; ownerPin: string; businessName: string };
      if (s.setupComplete) fail("PERMISSION_DENIED");
      workspace.businessName = input.businessName;
      const owner: StaffUser = { userId: id(), displayName: input.ownerName, role: "owner", pin: input.ownerPin, failed: 0 };
      s.users.push(owner);
      s.setupComplete = true;
      return { ownerUserId: owner.userId, registerCode: "R01" };
    }
    case "auth_list_users":
      return s.users.filter((u) => u.active !== false).map(({ userId, displayName, role }) => ({ userId, displayName, role }));
    case "staff_list":
      ownerOnly(s);
      return s.users.map(staffDto);
    case "staff_create": {
      ownerOnly(s);
      const input = a.input as { displayName: string; role: Role; pin: string };
      const name = staffName(s, input.displayName, null);
      if (!/^\d{4,8}$/.test(input.pin)) fail("VALIDATION_FAILED", { field: "pin" });
      const u: StaffUser = { userId: id(), displayName: name, role: input.role, pin: input.pin, failed: 0, active: true, version: 1 };
      s.users.push(u);
      return staffDto(u);
    }
    case "staff_update": {
      ownerOnly(s);
      const input = a.input as { userId: string; baseVersion: number; displayName: string | null; role: Role | null };
      const u = staffAt(s, input.userId, input.baseVersion);
      if (input.displayName !== null) u.displayName = staffName(s, input.displayName, u.userId);
      if (input.role !== null && input.role !== "owner" && u.role === "owner") lastOwner(s, u);
      if (input.role !== null) u.role = input.role;
      u.version = (u.version ?? 1) + 1;
      return staffDto(u);
    }
    case "staff_deactivate": {
      ownerOnly(s);
      const u = staffAt(s, String(a.userId), Number(a.baseVersion));
      if (u.role === "owner") lastOwner(s, u);
      u.active = false;
      u.version = (u.version ?? 1) + 1;
      return staffDto(u);
    }
    case "staff_reactivate": {
      ownerOnly(s);
      const u = staffAt(s, String(a.userId), Number(a.baseVersion));
      if (u.active !== false) fail("VALIDATION_FAILED");
      staffName(s, u.displayName, u.userId);
      u.active = true;
      u.version = (u.version ?? 1) + 1;
      return staffDto(u);
    }
    case "staff_reset_pin": {
      ownerOnly(s);
      const u = staffAt(s, String(a.userId), Number(a.baseVersion));
      if (!/^\d{4,8}$/.test(String(a.pin))) fail("VALIDATION_FAILED", { field: "pin" });
      u.pin = String(a.pin);
      u.mustChangePin = true;
      u.version = (u.version ?? 1) + 1;
      return staffDto(u);
    }
    case "auth_login": {
      const u = s.users.find((x) => x.userId === a.userId) ?? fail("VALIDATION_FAILED");
      if (s.lockedUntil !== null && s.lockedUntil > Date.now()) fail("AUTH_LOCKED_OUT", { remainingAttempts: null, retryAtMs: String(s.lockedUntil) });
      if (a.pin !== u.pin) {
        u.failed += 1;
        if (u.failed >= 5) {
          s.lockedUntil = Date.now() + 30_000;
          fail("AUTH_LOCKED_OUT", { remainingAttempts: null, retryAtMs: String(s.lockedUntil) });
        }
        fail("AUTH_PIN_INVALID", { remainingAttempts: 5 - u.failed, retryAtMs: null });
      }
      u.failed = 0;
      s.session = { userId: u.userId, displayName: u.displayName, role: u.role, mustChangePin: false };
      return s.session;
    }
    case "auth_request_approval": {
      const me = needSession(s);
      const u = s.users.find((x) => x.userId === a.approverUserId) ?? fail("VALIDATION_FAILED");
      if (u.userId === me.userId || u.role === "cashier") fail("APPROVAL_INVALID");
      if (a.pin !== u.pin) fail("AUTH_PIN_INVALID", { remainingAttempts: 4, retryAtMs: null });
      const tokenId = id();
      s.tokens.set(tokenId, a.scope as { permission: Permission; entityId: string; maxPpm: number | null });
      return { approvalTokenId: tokenId, expiresAtMs: String(Date.now() + 120_000) };
    }
    case "auth_logout":
      needSession(s);
      s.session = null;
      return null;
    case "catalog_new_product_defaults": {
      needSession(s);
      return {
        unit: JSON.parse(s.settings.get("catalog.default_unit")?.valueJson ?? '"ea"') as string,
        trackStock: (s.settings.get("catalog.default_track_stock")?.valueJson ?? "true") !== "false",
      };
    }
    case "tax_list":
      needSession(s);
      return s.taxCategories
        .filter((c) => !c.archived)
        .map((c) => ({
          taxCategoryId: c.taxCategoryId,
          name: c.name,
          isDefault: c.isDefault,
          rates: s.taxRates.filter((r) => c.rateIds.includes(r.taxRateId)).map((r) => ({ name: r.name, ratePpm: r.ratePpm })),
        }));
    case "categories_list":
      needSession(s);
      return s.categories;
    case "category_save":
    case "category_archive": {
      if (needSession(s).role === "cashier") fail("PERMISSION_DENIED");
      const existing = s.categories.find((c) => c.categoryId === a.categoryId);
      if (a.categoryId && !existing) fail("VALIDATION_FAILED");
      if ((existing?.version ?? null) !== (a.baseVersion ?? null)) fail("CONFLICT_VERSION");
      if (cmd === "category_archive") {
        if (s.categories.some((c) => c.parentId === a.categoryId && !c.archived)) fail("VALIDATION_FAILED");
        Object.assign(existing ?? fail("VALIDATION_FAILED"), { archived: true, version: (existing?.version ?? 0) + 1 });
        return null;
      }
      const name = String(a.name).trim();
      if (!name) fail("VALIDATION_FAILED", { field: "name" });
      const parent = (a.parentId as string | null) ?? null;
      // Levels above the new position (a cycle refuses), plus the levels the category brings with it.
      let above = 0;
      for (let c = parent; c; c = s.categories.find((x) => x.categoryId === c)?.parentId ?? null) {
        if (c === a.categoryId || ++above > 5) fail("VALIDATION_FAILED", { field: "parent" });
      }
      const levels = (cid: string): number => 1 + Math.max(0, ...s.categories.filter((c) => c.parentId === cid && !c.archived).map((c) => levels(c.categoryId)));
      if (above + (existing ? levels(existing.categoryId) : 1) > 3) fail("VALIDATION_FAILED", { field: "parent" });
      if (existing) {
        Object.assign(existing, { name, parentId: parent, version: existing.version + 1 });
        return existing;
      }
      const created: CategoryDto = { categoryId: id(), parentId: parent, name, archived: false, version: 1 };
      s.categories.push(created);
      return created;
    }
    case "tax_config_get":
      if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
      return { rates: s.taxRates, categories: s.taxCategories };
    case "tax_rate_save":
    case "tax_rate_archive": {
      if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
      const existing = s.taxRates.find((r) => r.taxRateId === a.taxRateId);
      if (a.taxRateId && !existing) fail("VALIDATION_FAILED");
      if ((existing?.version ?? null) !== (a.baseVersion ?? null)) fail("CONFLICT_VERSION");
      if (cmd === "tax_rate_archive") {
        const users = s.taxCategories.filter((c) => !c.archived && c.rateIds.includes(String(a.taxRateId)));
        if (users.length) fail("VALIDATION_FAILED", { field: "categories", value: users.map((c) => c.name).join(", ") });
        Object.assign(existing ?? fail("VALIDATION_FAILED"), { archived: true, version: (existing?.version ?? 0) + 1 });
        return null;
      }
      const name = String(a.name).trim();
      if (!name) fail("VALIDATION_FAILED", { field: "name" });
      const from = (a.from as string | null) ?? null;
      const to = (a.to as string | null) ?? null;
      if (from && to && to <= from) fail("VALIDATION_FAILED", { field: "period" });
      const overlap = s.taxRates.find(
        (r) => !r.archived && r.taxRateId !== a.taxRateId && r.name.toLowerCase() === name.toLowerCase() && (!r.to || !from || from < r.to) && (!to || !r.from || r.from < to),
      );
      if (overlap) fail("VALIDATION_FAILED", { field: "name", value: overlap.name });
      const row = { name, ratePpm: Number(a.ratePpm), from, to };
      if (existing) return Object.assign(existing, row, { version: existing.version + 1 });
      const created: TaxRateConfigDto = { taxRateId: id(), ...row, archived: false, version: 1 };
      s.taxRates.push(created);
      return created;
    }
    case "tax_category_save": {
      if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
      const existing = s.taxCategories.find((c) => c.taxCategoryId === a.taxCategoryId);
      if (a.taxCategoryId && !existing) fail("VALIDATION_FAILED");
      if ((existing?.version ?? null) !== (a.baseVersion ?? null)) fail("CONFLICT_VERSION");
      const name = String(a.name).trim();
      if (!name) fail("VALIDATION_FAILED", { field: "name" });
      const rateIds = a.rateIds as string[];
      if (rateIds.some((r) => !s.taxRates.some((x) => x.taxRateId === r && !x.archived))) fail("VALIDATION_FAILED", { field: "rates" });
      if (existing?.isDefault && !a.isDefault) fail("VALIDATION_FAILED", { field: "default" });
      if (a.isDefault) for (const c of s.taxCategories.filter((c) => c.isDefault && c !== existing)) Object.assign(c, { isDefault: false, version: c.version + 1 });
      const row = { name, rateIds, isDefault: Boolean(a.isDefault) };
      if (existing) return Object.assign(existing, row, { version: existing.version + 1 });
      const created: TaxCategoryConfigDto = { taxCategoryId: id(), ...row, archived: false, version: 1 };
      s.taxCategories.push(created);
      return created;
    }
    case "product_list": {
      needSession(s);
      const f = a.filter as { query: string; status: string; trackStock: boolean | null; categoryId: string | null; lowStock?: boolean };
      // Low, like Rust's rule: a tracked variant below zero, or at or below its threshold.
      const level = (p: { name: string; variants: { trackStock: boolean; lowStockThresholdMilli?: string | null }[] }) => {
        const have = FAKE_STOCK[p.name] ?? 0n;
        const tracked = p.variants.filter((v) => v.trackStock);
        if (tracked.length === 0) return "none";
        if (have < 0n) return "negative";
        return tracked.some((v) => v.lowStockThresholdMilli != null && have <= BigInt(v.lowStockThresholdMilli)) ? "low" : "ok";
      };
      const offset = Number(a.offset);
      const limit = Number(a.limit);
      if (s.bigCatalog !== null) {
        const total = s.bigCatalog;
        const rows = Array.from({ length: Math.max(0, Math.min(limit, total - offset)) }, (_, i) => {
          const n = offset + i;
          return { productId: `big-${n}`, name: `Item ${String(n).padStart(6, "0")}`, archived: false, version: 1, variantCount: 1, sku: `SKU${n}`, priceMinMinor: "100", priceMaxMinor: "100", barcodeCount: 1, trackStock: true, quickSelect: false, onHandMilli: "0", negativeStock: false, lowStock: false, syncConflict: false };
        });
        return { rows, total };
      }
      const q = f.query.trim().toLowerCase();
      const all = [...s.catalog.values()]
        .filter((p) => (f.status === "all" ? true : f.status === "archived" ? p.archived : !p.archived))
        .filter((p) => f.trackStock === null || p.variants.some((v) => v.trackStock) === f.trackStock)
        .filter((p) => !f.categoryId || p.categoryId === f.categoryId)
        .filter((p) => !f.lowStock || level(p) === "low" || level(p) === "negative")
        .filter((p) => !q || p.name.toLowerCase().includes(q) || p.variants.some((v) => v.sku?.toLowerCase().startsWith(q) || v.barcodes.some((b) => b.raw.includes(q))))
        .sort((x, y) => x.name.localeCompare(y.name));
      const rows = all.slice(offset, offset + limit).map((p) => {
        const prices = p.variants.map((v) => BigInt(v.priceMinor));
        const min = prices.reduce((m, x) => (x < m ? x : m));
        const max = prices.reduce((m, x) => (x > m ? x : m));
        return {
          productId: p.productId, name: p.name, archived: p.archived, version: p.version, variantCount: p.variants.length,
          sku: p.variants[0]?.sku ?? null, priceMinMinor: min.toString(), priceMaxMinor: max.toString(),
          barcodeCount: p.variants.reduce((n, v) => n + v.barcodes.length, 0), trackStock: p.variants.some((v) => v.trackStock), quickSelect: p.quickSelect,
          onHandMilli: p.variants.some((v) => v.trackStock) ? (FAKE_STOCK[p.name] ?? 0n).toString() : null,
          negativeStock: (FAKE_STOCK[p.name] ?? 0n) < 0n,
          lowStock: level(p) === "low",
          syncConflict: s.cloud.conflicts > 0 && p.name === "Cola 330ml",
        };
      });
      return { rows, total: all.length };
    }
    case "product_get":
      needSession(s);
      return s.catalog.get(String(a.productId)) ?? fail("CATALOG_NOT_FOUND");
    case "product_create":
    case "product_create_from_scan":
    case "product_update": {
      const input = a.product as ProductInput;
      if (cmd === "product_create_from_scan") {
        // E07-T04: a cashier needs a manager's approval for this barcode; the product carries it.
        const code = String(a.barcode).trim();
        approve(s, "barcode_create_from_scan", null, a.approvalTokenId, code);
        if (!input.variants.some((v) => (v.barcodes ?? []).some((b) => b.raw.trim() === code))) fail("VALIDATION_FAILED");
      } else if (needSession(s).role === "cashier") fail("PERMISSION_DENIED");
      const existing = cmd === "product_update" ? (s.catalog.get(String(a.productId)) ?? fail("CATALOG_NOT_FOUND")) : null;
      if (existing && existing.version !== a.baseVersion) fail("CONFLICT_VERSION");
      if (!input.name.trim()) fail("VALIDATION_FAILED", { field: "name" });
      const productId = existing?.productId ?? id();
      for (const v of input.variants)
        for (const b of v.barcodes ?? []) {
          const owner = [...s.catalog.values()].find((p) => p.productId !== productId && !p.archived && p.variants.some((x) => x.barcodes.some((y) => y.raw === b.raw)));
          if (owner) fail("CATALOG_DUPLICATE_BARCODE", { field: "barcode", value: b.raw, ownerProductId: owner.productId });
        }
      const dto: ProductDto = {
        productId, name: input.name, description: input.description ?? null, categoryId: input.categoryId ?? null, productType: input.productType, unit: input.unit,
        taxCategoryId: input.taxCategoryId, quickSelect: input.quickSelect, archived: false, version: (existing?.version ?? 0) + 1,
        variants: input.variants.map((v, i) => ({
          variantId: v.variantId ?? id(), isDefault: i === 0, sku: v.sku ?? null, options: v.options, priceMinor: v.priceMinor, costMinor: v.costMinor ?? null,
          trackStock: v.trackStock, lowStockThresholdMilli: v.lowStockThresholdMilli ?? null, scanQtyMilli: v.scanQtyMilli ?? "1000",
          barcodes: (v.barcodes ?? []).map((b) => ({ value: b.raw, raw: b.raw, symbology: "code128", qualifier: b.qualifier, packQtyMilli: b.packQtyMilli ?? "1000" })),
          archived: false,
        })),
      };
      s.catalog.set(productId, dto);
      // Make it scannable at the till.
      s.products = s.products.filter((p) => p.productId !== productId);
      for (const v of dto.variants) {
        s.products.push({ variantId: v.variantId, productId, name: dto.name, sku: v.sku ?? v.variantId, priceMinor: BigInt(v.priceMinor), barcodes: Object.fromEntries(v.barcodes.map((b) => [b.raw, parseInt(b.packQtyMilli, 10) / 1000])) });
      }
      return dto;
    }
    case "product_archive": {
      if (needSession(s).role === "cashier") fail("PERMISSION_DENIED");
      const p = s.catalog.get(String(a.productId)) ?? fail("CATALOG_NOT_FOUND");
      p.archived = true;
      p.version += 1;
      return p;
    }
    case "checkout_complete": {
      needSession(s);
      if (a.cartId !== s.cartId || a.revision !== s.revision) fail("CART_REVISION_MISMATCH");
      if (s.lines.length === 0) fail("CART_EMPTY");
      if (s.drawer.required && !s.drawer.session) fail("CASH_SESSION_REQUIRED");
      // Nothing is written: the cart stays as it was.
      if (s.diskFull) fail("STORE_DISK_FULL");
      // The same rules as pos_domain::settlement::settle.
      const total = BigInt(view(s).totalMinor);
      let remaining = total;
      let tendered = 0n;
      const terminal = (a.terminal ?? []) as { attemptId: string; amountMinor: string }[];
      for (const t of terminal) {
        const att = s.attempts.get(t.attemptId);
        if (!att || att.status !== "approved" || att.used || att.cartId !== s.cartId || att.amountMinor !== t.amountMinor) fail("VALIDATION_FAILED");
      }
      if ([...s.attempts.values()].some((x) => x.cartId === s.cartId && (x.status === "pending" || x.status === "reconciliation_required"))) fail("VALIDATION_FAILED");
      const tenders = [
        ...(terminal.map((t) => ({ kind: "card_external", amountMinor: t.amountMinor, reference: `SBX-${t.attemptId.slice(-6)}` }))),
        ...(a.tenders as { kind: string; amountMinor: string; reference: string | null }[]),
      ];
      for (const [i, t] of tenders.entries()) {
        const amount = BigInt(t.amountMinor);
        if (amount <= 0n || remaining === 0n) fail("VALIDATION_FAILED");
        if (t.kind !== "cash" && amount > remaining) fail("TENDER_OVERPAY_NON_CASH");
        if (t.kind === "cash" && amount > remaining && i < tenders.length - 1) fail("TENDER_CHANGE_FROM_NON_CASH");
        if (t.reference && /\d{13,19}/.test(t.reference.replace(/[ -]/g, ""))) fail("CARD_DATA_REJECTED");
        remaining -= amount < remaining ? amount : remaining;
        tendered += amount;
      }
      if (remaining > 0n) fail("TENDER_INSUFFICIENT", { remainingMinor: remaining.toString() });
      if (s.drawer.session) {
        s.drawer.session.sales += 1;
        const cashIn = tenders.filter((t) => t.kind === "cash").reduce((n, t) => n + BigInt(t.amountMinor), 0n);
        s.drawer.session.cashInMinor += cashIn - (tendered - total);
      }
      const receiptNumber = `R01-${String(s.receipts.length + 1).padStart(6, "0")}`;
      s.receipts.push(receiptNumber);
      const saleId = id();
      const me = needSession(s);
      s.sales.push({
        saleId,
        receiptNumber,
        lines: view(s).lines.map((l) => ({ saleLineId: id(), name: l.name, qtyMilli: BigInt(l.quantityMilli), totalMinor: BigInt(l.totalMinor), returnedMilli: 0n })),
        localTime: `2026-11-07 09:${String(s.sales.length).padStart(2, "0")}`,
        cashierUserId: me.userId,
        cashierName: me.displayName,
        tenders: tenders.map((t) => ({ kind: t.kind, amountMinor: BigInt(t.amountMinor), reference: t.reference })),
        totalMinor: total,
        changeMinor: tendered - total,
        returns: [],
      });
      for (const t of terminal) {
        const att = s.attempts.get(t.attemptId);
        if (att) att.used = true;
      }
      s.lines = [];
      s.orderDiscountPpm = null;
      s.cartId = id();
      s.revision = 0;
      const tookCash = tenders.some((t) => t.kind === "cash");
      const hasDrawer = s.devices.some((d) => d.kind === "receipt_printer");
      if (tookCash && hasDrawer) s.drawerOpens.push("cash_sale");
      return {
        saleId, receiptNumber, totalMinor: total.toString(), tenderedMinor: tendered.toString(),
        changeMinor: (tendered - total).toString(), roundingMinor: "0", currencyExponent: 2, mode: "sandbox", syncPending: true, negativeStockItems: 0, autoPrint: false,
        drawer: tookCash && hasDrawer ? "opened" : "none",
      };
    }
    case "return_find": {
      needSession(s);
      const sale = s.sales.find((x) => x.receiptNumber.toLowerCase() === String(a.receiptNumber).trim().toLowerCase()) ?? fail("VALIDATION_FAILED");
      return {
        saleId: sale.saleId, receiptNumber: sale.receiptNumber, mode: "sandbox", currencyExponent: 2,
        lines: sale.lines.map((l, i) => ({
          saleLineId: l.saleLineId, lineNo: i + 1, name: l.name, label: "", unit: "ea", trackStock: true,
          soldMilli: l.qtyMilli.toString(), returnedMilli: l.returnedMilli.toString(), availableMilli: (l.qtyMilli - l.returnedMilli).toString(), totalMinor: l.totalMinor.toString(),
        })),
      };
    }
    case "exchange_complete": {
      // The same rules as pos_application::exchange::complete; Rust records both in one
      // transaction, the fake checks everything before writing either.
      needSession(s);
      const sale = s.sales.find((x) => x.saleId === a.saleId) ?? fail("VALIDATION_FAILED");
      approve(s, "return_create", null, a.approvalTokenId, sale.saleId);
      const plan = handle(s, "return_preview", { saleId: a.saleId, lines: a.lines }) as { totalMinor: string };
      const refunds = a.refunds as { kind: string; amountMinor: string; reference: string | null }[];
      const tenders = a.tenders as { kind: string; amountMinor: string; reference: string | null }[];
      const credit = BigInt(plan.totalMinor) - refunds.reduce((t, r) => t + BigInt(r.amountMinor), 0n);
      if (credit <= 0n) fail("VALIDATION_FAILED");
      if (tenders.length > 0 && credit >= BigInt(view(s).totalMinor)) fail("VALIDATION_FAILED");
      const cash = { kind: "cash", amountMinor: credit.toString(), reference: null };
      const sold = handle(s, "checkout_complete", { cartId: a.cartId, revision: a.revision, tenders: [cash, ...tenders] });
      const ret = handle(s, "exchange_return", { ...a, refunds: [cash, ...refunds] });
      return { return: ret, sale: sold };
    }
    case "exchange_return":
    case "return_preview":
    case "return_create": {
      needSession(s);
      const sale = s.sales.find((x) => x.saleId === a.saleId) ?? fail("VALIDATION_FAILED");
      if (cmd === "return_create") approve(s, "return_create", null, a.approvalTokenId, sale.saleId);
      // The same rules as pos_domain::returns::plan_return: cumulative rounded shares.
      const asked = a.lines as { saleLineId: string; quantityMilli: string; disposition: string }[];
      const planned = asked.map((r) => {
        const l = sale.lines.find((x) => x.saleLineId === r.saleLineId) ?? fail("VALIDATION_FAILED");
        const qty = BigInt(r.quantityMilli);
        if (qty > l.qtyMilli - l.returnedMilli) fail("RETURN_QTY_EXCEEDS");
        const share = (n: bigint) => div(l.totalMinor * n, l.qtyMilli);
        return { l, qty, refund: share(l.returnedMilli + qty) - share(l.returnedMilli), disposition: r.disposition };
      });
      if (planned.length === 0) fail("VALIDATION_FAILED");
      const total = planned.reduce((t, p) => t + p.refund, 0n);
      const tax = div(total, 6n);
      if (cmd === "return_preview") {
        return {
          subtotalMinor: (total - tax).toString(), taxMinor: tax.toString(), totalMinor: total.toString(),
          lines: planned.map((p) => ({ saleLineId: p.l.saleLineId, name: p.l.name, quantityMilli: p.qty.toString(), refundMinor: p.refund.toString(), disposition: p.disposition })),
        };
      }
      const refunds = a.refunds as { kind: string; amountMinor: string; reference: string | null }[];
      if (refunds.some((r) => r.reference && /\d{13,19}/.test(r.reference.replace(/[ -]/g, "")))) fail("VALIDATION_FAILED");
      if (refunds.reduce((t, r) => t + BigInt(r.amountMinor), 0n) !== total) fail("RETURN_REFUND_MISMATCH");
      for (const p of planned) p.l.returnedMilli += p.qty;
      const returnId = id();
      const number = `R01-R-${String(s.returns.size + 1).padStart(6, "0")}`;
      s.returns.set(returnId, { number, totalMinor: total });
      sale.returns.push({ returnId, returnNumber: number, totalMinor: total, reason: String(a.reason), refunds: refunds.map((r) => r.kind) });
      return {
        returnId, returnNumber: number, originalReceiptNumber: sale.receiptNumber, totalMinor: total.toString(), currencyExponent: 2,
        approved: a.approvalTokenId !== null && a.approvalTokenId !== undefined,
      };
    }
    case "sales_list": {
      needSession(s);
      const f = a.filter as { from: string | null; to: string | null; receipt: string | null; cashierUserId: string | null; tender: string | null };
      if (f.from && !/^\d{4}-\d{2}-\d{2}$/.test(f.from)) fail("VALIDATION_FAILED");
      const receipt = f.receipt?.trim().toUpperCase() ?? "";
      const match = (x: FakeSale) =>
        (!receipt || (/^\d+$/.test(receipt) ? Number(x.receiptNumber.slice(-6)) === Number(receipt) : x.receiptNumber === receipt)) &&
        (!f.cashierUserId || x.cashierUserId === f.cashierUserId) &&
        (!f.tender || x.tenders.some((t) => t.kind === f.tender));
      const all = [...s.sales].reverse().filter(match);
      const start = a.after ? Number(a.after) : 0;
      const limit = Number(a.limit ?? 50);
      const page = all.slice(start, start + limit);
      return {
        rows: page.map((x) => ({
          saleId: x.saleId, receiptNumber: x.receiptNumber, localTime: x.localTime, businessDate: "2026-11-07", cashierName: x.cashierName,
          mode: "sandbox", currencyExponent: 2, totalMinor: x.totalMinor.toString(),
          itemCountMilli: x.lines.reduce((t, l) => t + l.qtyMilli, 0n).toString(),
          tenders: [...new Set(x.tenders.map((t) => t.kind))],
          refundedMinor: x.returns.reduce((t, r) => t + r.totalMinor, 0n).toString(),
        })),
        next: start + limit < all.length ? String(start + limit) : null,
      };
    }
    case "sales_get": {
      needSession(s);
      const x = s.sales.find((y) => y.saleId === a.saleId) ?? fail("VALIDATION_FAILED");
      const tax = div(x.totalMinor, 6n);
      return {
        saleId: x.saleId, receiptNumber: x.receiptNumber, localTime: x.localTime, businessDate: "2026-11-07", cashierName: x.cashierName, approverName: null,
        mode: "sandbox", currencyCode: "GBP", currencyExponent: 2, taxMode: "inclusive",
        subtotalMinor: x.totalMinor.toString(), lineDiscountTotalMinor: "0", orderDiscountTotalMinor: "0", taxTotalMinor: tax.toString(), roundingMinor: "0",
        totalMinor: x.totalMinor.toString(), tenderedMinor: (x.totalMinor + x.changeMinor).toString(), changeMinor: x.changeMinor.toString(),
        taxBreakdown: [{ name: "VAT 20%", ratePpm: 200000, taxableMinor: (x.totalMinor - tax).toString(), taxMinor: tax.toString() }],
        lines: x.lines.map((l, i) => ({
          lineNo: i + 1, name: l.name, label: "", sku: null, unit: "ea", quantityMilli: l.qtyMilli.toString(),
          unitPriceMinor: div(l.totalMinor * 1000n, l.qtyMilli).toString(), priceOverridden: false, grossMinor: l.totalMinor.toString(),
          discountMinor: "0", taxMinor: div(l.totalMinor, 6n).toString(), totalMinor: l.totalMinor.toString(), returnedMilli: l.returnedMilli.toString(),
        })),
        // As Rust records it: the amount applied to the sale; cash also keeps what was handed over.
        tenders: x.tenders.map((t, i) => {
          const change = t.kind === "cash" && i === x.tenders.length - 1 ? x.changeMinor : 0n;
          return {
          kind: t.kind, amountMinor: (t.amountMinor - change).toString(), receivedMinor: t.kind === "cash" ? t.amountMinor.toString() : null,
          changeMinor: change.toString(),
          status: "confirmed", confirmationSource: "operator", reference: t.reference,
          };
        }),
        returns: x.returns.map((r) => ({ returnId: r.returnId, returnNumber: r.returnNumber, localTime: "2026-11-07 10:00", reason: r.reason, totalMinor: r.totalMinor.toString(), refunds: r.refunds })),
      };
    }
    case "reports_sales_summary":
    case "reports_tender_mix":
    case "reports_top_products":
    case "reports_stock":
    case "stock_overview": {
      const me = needSession(s);
      if (me.role === "cashier" && cmd !== "stock_overview") fail("PERMISSION_DENIED");
      const range = a.range as { from: string; to: string } | undefined;
      if (range && range.from > range.to) fail("VALIDATION_FAILED");
      const scope = { scope: "device", asOfMs: String(Date.UTC(2026, 10, 7, 9, 30)) };
      // Every fake sale is on the range's last day.
      const sum = (f: (x: FakeSale) => bigint) => s.sales.reduce((t, x) => t + f(x), 0n);
      if (cmd === "reports_sales_summary") {
        const total = sum((x) => x.totalMinor);
        const refunds = sum((x) => x.returns.reduce((t, r) => t + r.totalMinor, 0n));
        const tax = div(total, 6n);
        const rtax = div(refunds, 6n);
        const fig = {
          salesCount: s.sales.length, grossMinor: total.toString(), lineDiscountsMinor: "0", orderDiscountsMinor: "0", discountsMinor: "0",
          taxMinor: tax.toString(), roundingMinor: "0", salesTotalMinor: total.toString(),
          returnsCount: s.sales.reduce((n, x) => n + x.returns.length, 0), refundsMinor: refunds.toString(), refundedTaxMinor: rtax.toString(),
          netInclTaxMinor: (total - refunds).toString(), netExclTaxMinor: (total - refunds - (tax - rtax)).toString(),
        };
        return { days: s.sales.length ? [{ date: range?.to, figures: fig }] : [], total: fig, pricesIncludeTax: true, currencyExponent: 2, scope };
      }
      if (cmd === "reports_tender_mix") {
        const kinds = [...new Set(s.sales.flatMap((x) => x.tenders.map((t) => t.kind)))].sort();
        return {
          rows: kinds.map((k) => {
            const ts = s.sales.flatMap((x) => x.tenders.filter((t) => t.kind === k));
            const taken = s.sales.reduce((n, x) => n + x.tenders.filter((t) => t.kind === k).reduce((m, t) => m + t.amountMinor, 0n) - (k === "cash" ? x.changeMinor : 0n), 0n);
            return { kind: k, confirmationSource: "operator", count: ts.length, takenMinor: taken.toString(), refundCount: 0, refundedMinor: "0", netMinor: taken.toString() };
          }),
          scope,
        };
      }
      if (cmd === "reports_top_products") {
        const by = new Map<string, { qty: bigint; value: bigint }>();
        for (const l of s.sales.flatMap((x) => x.lines)) {
          const e = by.get(l.name) ?? { qty: 0n, value: 0n };
          e.qty += l.qtyMilli - l.returnedMilli;
          e.value += l.totalMinor;
          by.set(l.name, e);
        }
        const rows = [...by.entries()].sort((x, y) => (a.by === "quantity" ? Number(y[1].qty - x[1].qty) : Number(y[1].value - x[1].value)));
        return { rows: rows.map(([name, e]) => ({ variantId: id(), name, label: "", quantityMilli: e.qty.toString(), valueMinor: e.value.toString() })), scope };
      }
      const stock = [
        { name: "Chocolate bar", sellable: "-2000", status: "negative", low: null },
        { name: "Cola 330ml", sellable: "40000", status: "ok", low: "12000" },
        { name: "Crisps", sellable: "3000", status: "low", low: "5000" },
      ];
      return {
        rows: stock
          .filter((r) => !a.lowOnly || r.status !== "ok")
          .map((r) => ({ variantId: id(), name: r.name, label: "", sku: null, unit: "ea", sellableMilli: r.sellable, damagedMilli: "0", lowStockThresholdMilli: r.low, status: r.status, lastMovementMs: null })),
        scope,
      };
    }
    case "stock_history": {
      needSession(s);
      // The sale row points at the latest sale here, when there is one, so it can be opened.
      const sale = s.sales[s.sales.length - 1];
      return [
        { movementId: id(), localTime: "2026-11-07 10:05", movementType: "sale", bucket: "sellable", deltaMilli: "-2000", sourceType: "sale", sourceId: sale?.saleId ?? id(), sourceNumber: sale?.receiptNumber ?? "R01-000004", actorName: "Cal" },
        { movementId: id(), localTime: "2026-11-07 08:30", movementType: "receipt", bucket: "sellable", deltaMilli: "24000", sourceType: "inventory_document", sourceId: FAKE_DOCUMENT, sourceNumber: "R01-IN-000001", actorName: "Mia" },
      ];
    }
    case "stock_verify":
    case "stock_rebuild": {
      const me = needSession(s);
      // Verify needs InventoryView (every role); rebuild is the owner's (SettingsEdit).
      if (cmd === "stock_rebuild" && me.role !== "owner") fail("PERMISSION_DENIED");
      const found = s.stockDrift.map((d) => ({ ...d, locationId: "0190a5b0-0000-7000-8000-0000000010c1", variantId: id() }));
      if (cmd === "stock_rebuild") {
        s.stockDrift = [];
        s.stockRebuilds += 1;
      }
      return found;
    }
    case "stock_document_get": {
      needSession(s);
      if (a.documentId !== FAKE_DOCUMENT) fail("VALIDATION_FAILED");
      return {
        documentId: FAKE_DOCUMENT, documentNo: "R01-IN-000001", docType: "receipt", reason: null, supplierName: "Wholesale Ltd", reference: "INV-8812", note: null,
        localTime: "2026-11-07 08:30", actorName: "Mia", approverName: null,
        lines: [
          { variantId: id(), name: "Cola 330ml", sku: "COLA", movementType: "receipt", bucket: "sellable", deltaMilli: "24000", unitCostMinor: "35" },
          { variantId: id(), name: "Crisps", sku: "CRISP", movementType: "receipt", bucket: "sellable", deltaMilli: "12000", unitCostMinor: null },
        ],
      };
    }
    case "audit_list": {
      const me = needSession(s);
      if (me.role !== "owner") fail("PERMISSION_DENIED");
      const f = a.filter as { action: string | null; userId: string | null; from: string | null; to: string | null };
      if (f.from && f.to && f.from > f.to) fail("VALIDATION_FAILED");
      const firstProduct = [...s.catalog.values()][0];
      const rows = [
        ...[...s.sales].reverse().map((x) => ({
          auditId: id(), localTime: x.localTime, action: "sale.completed", actorName: x.cashierName, approverName: null,
          entityType: "sale", entityId: x.saleId, entityLabel: x.receiptNumber, metadata: JSON.stringify({ totalMinor: x.totalMinor.toString(), lines: x.lines.length }),
          userId: x.cashierUserId,
        })),
        {
          auditId: id(), localTime: "2026-11-07 08:00", action: "catalog.product_created", actorName: "Olu", approverName: null,
          entityType: "product", entityId: firstProduct?.productId ?? id(), entityLabel: firstProduct?.name ?? "?", metadata: "{}", userId: s.users[0]?.userId,
        },
        {
          auditId: id(), localTime: "2026-11-07 08:30", action: "inventory.receipt_posted", actorName: "Mia", approverName: null,
          entityType: "inventory_document", entityId: FAKE_DOCUMENT, entityLabel: "R01-IN-000001", metadata: "{}", userId: s.users[0]?.userId,
        },
        {
          auditId: id(), localTime: "2026-11-07 07:55", action: "auth.login", actorName: "Olu", approverName: null,
          entityType: "staff_user", entityId: id(), entityLabel: "Olu", metadata: "{}", userId: s.users[0]?.userId,
        },
      ].filter((r) => !f.action || (f.action.endsWith(".") ? r.action.startsWith(f.action) : r.action === f.action))
        .filter((r) => !f.userId || r.userId === f.userId)
        .map((r) => {
          const { userId, ...shown } = r;
          void userId; // used for filtering only
          return shown;
        });
      return { rows, next: null };
    }
    case "export_csv": {
      const me = needSession(s);
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      const dated = a.dataset !== "products" && a.dataset !== "stock";
      const range = a.range as { from: string; to: string } | null;
      if (dated && !range) fail("VALIDATION_FAILED");
      s.exports += 1;
      const name = dated && range ? `${String(a.dataset)}_${range.from}_${range.to}.csv` : `${String(a.dataset)}.csv`;
      const rows = a.dataset === "transactions" ? s.sales.reduce((n, x) => n + x.lines.length, 0) : a.dataset === "products" ? s.products.length : 3;
      const answer = s.saveAs.length ? s.saveAs.shift() : name;
      if (answer == null) return null;
      return { path: `/home/user/Documents/${answer}`, fileName: answer, rows };
    }
    case "reports_cloud_sales": {
      const me = needSession(s);
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      const range = a.range as { from: string; to: string };
      if (range.from > range.to) fail("VALIDATION_FAILED");
      if (s.cloud.linkState !== "linked") fail("SYNC_NOT_LINKED");
      const groupBy = String(a.groupBy);
      const key = `${groupBy}:${range.from}:${range.to}`;
      if (!s.cloud.online) {
        const kept = s.cloud.kept[key] as Record<string, unknown> | undefined;
        if (!kept) fail("SYNC_OFFLINE");
        return { ...kept, freshness: "stale", staleReason: "SYNC_OFFLINE" };
      }
      const row = (key: string, label: string, n: number, sales: number, refunds: number) => ({
        key, label, salesCount: n, salesMinor: String(sales), taxMinor: String(Math.floor(sales / 6)), discountMinor: "0",
        returnsCount: refunds ? 1 : 0, refundsMinor: String(refunds), netMinor: String(sales - refunds),
      });
      const rows =
        groupBy === "register"
          ? [row("r1", "R01", 3, 1200, 100), row("r2", "R02", 2, 750, 0)]
          : groupBy === "tender"
            ? [{ ...row("cash", "cash", 4, 1500, 100), taxMinor: "0" }, { ...row("card_external", "card_external", 1, 450, 0), taxMinor: "0" }]
            : [row(range.to, range.to, 5, 1950, 100)];
      const total = rows.reduce(
        (t, r) => ({
          ...t, salesCount: t.salesCount + r.salesCount, salesMinor: String(Number(t.salesMinor) + Number(r.salesMinor)),
          taxMinor: String(Number(t.taxMinor) + Number(r.taxMinor)), returnsCount: t.returnsCount + r.returnsCount,
          refundsMinor: String(Number(t.refundsMinor) + Number(r.refundsMinor)), netMinor: String(Number(t.netMinor) + Number(r.netMinor)),
        }),
        row("total", "Total", 0, 0, 0),
      );
      const behind = s.cloud.unsyncedRegisters;
      const report = {
        groupBy, from: range.from, to: range.to, currencyExponent: 2, rows, total,
        freshness: behind ? "partial" : "fully_synchronized", unsyncedRegisters: behind,
        lastSyncedAt: behind ? new Date(2026, 10, 5, 9, 5).toISOString() : null, staleReason: null,
        asOf: new Date(2026, 10, 5, 9, 30).toISOString(), fetchedAt: new Date(2026, 10, 5, 9, 30).toISOString(),
        registers: [
          { code: "R01", name: "Till 1", lastAckAt: new Date(2026, 10, 5, 9, 29).toISOString(), pendingKnown: false, current: true },
          { code: "R02", name: "Till 2", lastAckAt: new Date(2026, 10, 5, 9, 5).toISOString(), pendingKnown: behind > 0, current: behind === 0 },
        ],
      };
      s.cloud.kept[key] = report;
      return report;
    }
    case "backup_status":
    case "backup_now":
    case "backup_choose_folder":
    case "backup_clear_folder":
    case "backup_verify":
    case "backup_restore":
    case "data_export_full":
    case "security_recovery_key":
    case "security_recovery_key_confirmed": {
      const me = needSession(s);
      if (me.role !== "owner") fail("PERMISSION_DENIED");
      s.backups ??= [
        { id: "pos.daily.1793955600000.db", kind: "daily", createdAt: new Date(2026, 10, 5, 8, 0).toISOString(), sizeKb: 2310, sales: 40, secondFolderOnly: false },
        { id: "pos.hourly.1793952000000.db.damaged", kind: "hourly", createdAt: new Date(2026, 10, 5, 7, 0).toISOString(), sizeKb: 2290, sales: 38, secondFolderOnly: false },
      ];
      const status = () => ({
        backups: s.backups,
        secondFolder: s.backupFolder ?? null,
        secondFolderProblem: null,
        linked: s.cloud.linkState === "linked",
        encrypted: true,
        encryptionProblem: null,
        recoveryKeyConfirmed: s.recoveryKeyConfirmed ?? false,
      });
      if (cmd === "security_recovery_key") return RECOVERY_KEY;
      if (cmd === "security_recovery_key_confirmed") {
        s.recoveryKeyConfirmed = true;
        return status();
      }
      if (cmd === "backup_now") {
        const made = { id: `pos.manual.${Date.now()}.db`, kind: "manual", createdAt: new Date().toISOString(), sizeKb: 2320, sales: s.sales.length, secondFolderOnly: false };
        s.backups.unshift(made);
        return made;
      }
      if (cmd === "data_export_full") {
        const answer = s.saveAs.length ? s.saveAs.shift() : "sale-os-export-2026-11-05.zip";
        if (answer == null) return null;
        s.exports += 1;
        return { path: `/home/user/Documents/${answer}`, fileName: answer, tables: 44, rows: 1234 };
      }
      if (cmd === "backup_choose_folder") s.backupFolder = "/media/usb/SaleOS";
      if (cmd === "backup_clear_folder") s.backupFolder = null;
      if (cmd === "backup_verify") {
        const damaged = String(a.backupId).endsWith(".damaged");
        return { restorable: !damaged, problem: damaged ? "page 12 is never used" : null, tooNew: false };
      }
      if (cmd === "backup_restore") {
        if (s.cloud.linkState === "linked") {
          if (!s.cloud.online) fail("SYNC_OFFLINE");
        } else if (!a.confirmLoss) fail("VALIDATION_FAILED");
        s.restartedFrom = String(a.backupId);
        return null;
      }
      return status();
    }
    case "sync_status": {
      needSession(s);
      const c = s.cloud;
      return {
        linkState: c.linkState,
        cloudConfigured: c.configured,
        pending: c.pending,
        lastPushOkAt: c.linkState === "linked" ? "2026-11-05T09:20:00.000Z" : null,
        lastPullOkAt: c.linkState === "linked" ? "2026-11-05T09:20:01.000Z" : null,
        lastErrorCode: c.linkState === "revoked" ? "SYNC_DEVICE_REVOKED" : null,
        clockSkewWarning: false,
        registerCode: c.registerCode,
        indicator:
          c.linkState === "revoked"
            ? "revoked"
            : c.linkState !== "linked"
              ? "not_linked"
              : c.refused.length + c.conflicts > 0
                ? "attention"
                : c.pending > 0
                  ? "pending"
                  : "synced",
        rejected: c.refused.length,
        conflicts: c.conflicts,
      };
    }
    case "sync_repair_queue":
      if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
      return {
        events: s.cloud.refused.map((e) => ({
          ...e,
          aggregateType: "sale",
          aggregateId: e.eventId,
          detail: null,
          occurredAt: "2026-11-05T09:14:03.120Z",
        })),
        conflicts: Array.from({ length: s.cloud.conflicts }, (_, i) => ({
          conflictId: `c${i}`,
          aggregateType: "product",
          aggregateId: `p${i}`,
          localBaseVersion: 3,
          serverVersion: 4,
          createdAt: "2026-11-05T09:20:00.000Z",
        })),
      };
    case "sync_repair_action": {
      if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
      if (a.action === "view_payload") {
        return { payloadJson: JSON.stringify({ eventId: a.eventId, payload: { pinHash: "[REDACTED]" } }, null, 2), savedFileName: null };
      }
      const name = s.saveAs.shift() ?? null;
      return { payloadJson: null, savedFileName: name };
    }
    case "sync_conflicts_list": {
      if (needSession(s).role === "cashier") fail("PERMISSION_DENIED");
      if (s.cloud.conflicts === 0) return [];
      return [
        {
          conflictId: "c1", aggregateType: "product", aggregateId: "p1", title: "Cola 330ml", reason: "VERSION",
          localBaseVersion: 1, serverVersion: 2, createdAt: "2026-11-05T09:20:00.000Z",
          fields: [
            { field: "name", label: "Name", localJson: '"Cola 330ml"', serverJson: '"Cola Classic"' },
            { field: "variants/v1/price", label: "Price", localJson: "150", serverJson: "130" },
          ],
          taken: [], heldBy: null,
        },
      ];
    }
    case "sync_conflict_resolve": {
      if (needSession(s).role === "cashier") fail("PERMISSION_DENIED");
      s.cloud.conflicts = 0;
      return null;
    }
    case "sale_sync_state": {
      needSession(s);
      const linked = s.cloud.linkState === "linked";
      return { state: s.cloud.synced.has(String(a.saleId)) ? "synced" : linked ? "pending" : "local" };
    }
    case "sync_retry_now":
      needSession(s);
      s.cloud.pending = 0;
      return null;
    case "sync_link_start": {
      if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
      if (a.password !== "correct horse battery staple") fail("AUTH_CREDENTIALS_INVALID");
      s.cloud.signedIn = true;
      return {
        businesses: [{ businessId: "0190f0c2-0000-7000-8000-00000000b2b2", name: "Harbour Café" }],
        mayJoin: s.cloud.mayJoin,
        ownBusinessInCloud: false,
      };
    }
    case "sync_link_complete": {
      if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
      if (!s.cloud.signedIn) fail("AUTH_REQUIRED");
      s.cloud.signedIn = false;
      s.cloud.linkState = "linked";
      if (a.mode === "join") {
        s.cloud.registerCode = "R02";
        s.cloud.pending = 1;
        s.session = null;
        return { path: "join", businessId: String(a.businessId), registerCode: "R02", signInAgain: true };
      }
      s.cloud.pending = 0;
      return { path: "create", businessId: "0190f0c2-0000-7000-8000-00000000a1a1", registerCode: "R01", signInAgain: false };
    }
    case "settings_get":
    case "settings_update": {
      const me = needSession(s);
      if (me.role !== "owner") fail("PERMISSION_DENIED");
      const row = (d: (typeof FAKE_SETTINGS)[number]) => {
        const v = s.settings.get(d.key) ?? { valueJson: d.defaultJson, version: 0 };
        return { ...d, valueJson: v.valueJson, version: v.version };
      };
      if (cmd === "settings_get") return FAKE_SETTINGS.map(row);
      const d = FAKE_SETTINGS.find((x) => x.key === a.key) ?? fail("VALIDATION_FAILED");
      const current = s.settings.get(d.key);
      if ((current?.version ?? 0) !== a.baseVersion) fail("CONFLICT_VERSION");
      let value: unknown;
      try {
        value = JSON.parse(String(a.valueJson));
      } catch {
        fail("VALIDATION_FAILED");
      }
      const k = d.kind as { type: string; min?: number; max?: number; options?: readonly string[] };
      const ok =
        (k.type === "bool" && typeof value === "boolean") ||
        (k.type === "int" && Number.isInteger(value) && (value as number) >= (k.min ?? 0) && (value as number) <= (k.max ?? 0)) ||
        (k.type === "ppm" && Number.isInteger(value) && (value as number) >= 0 && (value as number) <= 1_000_000) ||
        (k.type === "choice" && (k.options ?? []).includes(String(a.valueJson))) ||
        (k.type === "lines" && Array.isArray(value) && value.length <= 5);
      if (!ok) fail("VALIDATION_FAILED");
      s.settings.set(d.key, { valueJson: String(a.valueJson), version: (current?.version ?? 0) + 1 });
      return row(d);
    }
    case "app_preferences": {
      needSession(s);
      const get = (k: string) => JSON.parse(s.settings.get(k)?.valueJson ?? "null") as unknown;
      return { scannerSuffix: get("scanner.suffix"), scannerMaxKeyGapMs: 35, lockTimeoutSecs: get("security.lock_timeout_secs"), errorSound: s.errorSound ?? true };
    }
    case "auth_lock":
      needSession(s);
      s.session = null;
      return null;
    case "register_go_live": {
      const me = needSession(s);
      if (me.role !== "owner") fail("PERMISSION_DENIED");
      const owner = s.users.find((u) => u.userId === me.userId) ?? fail("VALIDATION_FAILED");
      if (a.pinCode !== owner.pin) fail("AUTH_PIN_INVALID", { remainingAttempts: 4, retryAtMs: null });
      if (workspace.mode === "live" || s.lines.length > 0 || s.drawer.session) fail("VALIDATION_FAILED");
      workspace.mode = "live";
      s.drawer.required = true;
      return null;
    }
    case "inventory_post_document": {
      const me = needSession(s);
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      // The same checks as pos_domain::stock_docs::plan_document (the ones the screen can hit).
      const d = a.draft as { docType: string; reason: string | null; note: string | null; lines: { quantityMilli: string }[] };
      if (d.lines.length === 0) fail("VALIDATION_FAILED");
      if (d.docType === "adjustment" && !d.reason) fail("VALIDATION_FAILED");
      if (d.reason === "other" && !d.note) fail("VALIDATION_FAILED");
      const signed = d.reason === "count_correction" || d.reason === "other";
      if (d.lines.some((l) => BigInt(l.quantityMilli) === 0n || (!signed && BigInt(l.quantityMilli) < 0n))) fail("VALIDATION_FAILED");
      s.documents += 1;
      return {
        documentId: id(), documentNo: `R01-IN-${String(s.documents).padStart(6, "0")}`, docType: d.docType,
        movementCount: d.lines.length * (d.reason === "damage" ? 2 : 1),
      };
    }
    case "cash_session_current":
    case "cash_session_open": {
      const me = needSession(s);
      if (cmd === "cash_session_open") {
        if (s.drawer.session) fail("CASH_SESSION_ALREADY_OPEN");
        const float = BigInt(String(a.floatMinor));
        if (float < 0n) fail("VALIDATION_FAILED");
        s.drawer.seq += 1;
        s.drawer.session = { id: id(), number: `R01-Z-${String(s.drawer.seq).padStart(4, "0")}`, floatMinor: float, cashInMinor: 0n, sales: 0, openedBy: me.displayName };
      }
      const d = s.drawer.session;
      const manager = me.role !== "cashier";
      return {
        required: s.drawer.required,
        session: d && {
          cashSessionId: d.id, number: d.number, open: true, openedByName: d.openedBy, openedLocal: "2026-11-07 08:00",
          openingFloatMinor: d.floatMinor.toString(), businessDate: "2026-11-07", closedByName: null, closedLocal: null, currencyExponent: 2,
        },
        totals:
          d && manager
            ? {
                salesCount: d.sales, salesTotalMinor: "0", taxTotalMinor: "0", discountTotalMinor: "0", roundingTotalMinor: "0",
                tenders: d.cashInMinor ? [{ kind: "cash", count: d.sales, amountMinor: d.cashInMinor.toString() }] : [],
                returnsCount: 0, refundsTotalMinor: "0", refunds: [],
              }
            : null,
        expectedMinor: d && manager ? (d.floatMinor + d.cashInMinor).toString() : null,
      };
    }
    case "cash_session_close": {
      const me = needSession(s);
      const d = s.drawer.session ?? fail("CASH_SESSION_REQUIRED");
      const counted = BigInt(String(a.countedMinor));
      const expected = d.floatMinor + d.cashInMinor;
      s.drawer.session = null;
      s.drawer.lastClosed = { id: d.id, number: d.number };
      const manager = me.role !== "cashier";
      return {
        session: {
          cashSessionId: d.id, number: d.number, open: false, openedByName: d.openedBy, openedLocal: "2026-11-07 08:00",
          openingFloatMinor: d.floatMinor.toString(), businessDate: "2026-11-07", closedByName: me.displayName, closedLocal: "2026-11-07 18:00", currencyExponent: 2,
        },
        countedMinor: counted.toString(),
        expectedMinor: manager ? expected.toString() : null,
        varianceMinor: manager ? (counted - expected).toString() : null,
        totals: null,
      };
    }
    case "cash_session_z_get": {
      const me = needSession(s);
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      const z = s.drawer.lastClosed ?? fail("VALIDATION_FAILED");
      const cols = a.widthMm === 58 ? 32 : 48;
      return { receiptNumber: z.number, columns: cols, sandbox: true, lines: ["*** Z SUMMARY ***", `Session ${z.number}`, "EXPECTED CASH"].map((t) => ({ text: t, bold: t.startsWith("EXPECTED") })) };
    }
    case "return_receipt_get": {
      needSession(s);
      const r = s.returns.get(String(a.returnId)) ?? fail("VALIDATION_FAILED");
      const cols = a.widthMm === 58 ? 32 : 48;
      const pad = (l: string, rt: string) => l + " ".repeat(Math.max(1, cols - l.length - rt.length)) + rt;
      const amount = `${r.totalMinor / 100n}.${String(r.totalMinor % 100n).padStart(2, "0")}`;
      const text = ["Corner Shop".padStart(Math.floor((cols + 11) / 2)), "*** REFUND ***", "-".repeat(cols), `Return ${r.number}`, pad("REFUND TOTAL", amount)];
      return { receiptNumber: r.number, columns: cols, sandbox: true, lines: text.map((t) => ({ text: t, bold: t.startsWith("REFUND TOTAL") })) };
    }
    case "tax_invoice_issue":
    case "tax_invoice_get": {
      needSession(s);
      const sale = s.sales.find((x) => x.saleId === a.saleId) ?? fail("VALIDATION_FAILED");
      if (cmd === "tax_invoice_issue") {
        // The same rules as pos_application::tax_invoice::issue.
        if (sale.taxInvoice) fail("VALIDATION_FAILED");
        const tin = String(a.purchaserTin).replace(/[ -]/g, "");
        if (!/^\d{9}$/.test(tin)) fail("VALIDATION_FAILED");
        const address = (a.purchaserAddress as string[]).map((l) => l.trim()).filter(Boolean);
        if (!String(a.purchaserName).trim() || address.length === 0 || address.length > 4) fail("VALIDATION_FAILED");
        const seq = s.sales.filter((x) => x.taxInvoice).length + 1;
        sale.taxInvoice = { number: `26NOV_R01_${String(seq).padStart(5, "0")}`, purchaserTin: tin, purchaserName: String(a.purchaserName).trim(), purchaserAddress: address };
      }
      const ti = sale.taxInvoice;
      if (!ti) return null;
      const cols = a.widthMm === 58 ? 32 : 48;
      const pad = (l: string, r: string) => l + " ".repeat(Math.max(1, cols - l.length - r.length)) + r;
      const money = (m: bigint) => `${m / 100n}.${String(m % 100n).padStart(2, "0")}`;
      const vat = div(sale.totalMinor, 6n);
      const text = [
        "TAX INVOICE",
        "Supplier TIN: 104567890",
        ti.purchaserName,
        ...ti.purchaserAddress,
        `Purchaser TIN: ${ti.purchaserTin}`,
        pad("Invoice no.", ti.number),
        pad("Date of invoice", "11/20/2026"),
        pad("Total value of supply", money(sale.totalMinor - vat)),
        pad("VAT 20%", money(vat)),
        pad("TOTAL INCL. VAT", money(sale.totalMinor)),
      ];
      return { receiptNumber: ti.number, columns: cols, sandbox: true, lines: text.map((t) => ({ text: t, bold: t === "TAX INVOICE" || t.startsWith("TOTAL") })) };
    }
    case "tax_invoice_export_pdf": {
      needSession(s);
      const sale = s.sales.find((x) => x.saleId === a.saleId) ?? fail("VALIDATION_FAILED");
      if (!sale.taxInvoice) fail("VALIDATION_FAILED");
      const answer = s.saveAs.length ? s.saveAs.shift() : `tax-invoice-${sale.taxInvoice.number}.pdf`;
      return answer == null ? null : { path: `/home/user/Documents/${answer}`, fileName: answer };
    }
    case "cash_session_z_export_pdf": {
      const me = needSession(s);
      // As Rust: the Z summary needs CashSessionViewVariance, and a closed session.
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      const z = s.drawer.lastClosed ?? fail("VALIDATION_FAILED");
      if (a.sessionId != null && a.sessionId !== z.id) fail("VALIDATION_FAILED");
      const answer = s.saveAs.length ? s.saveAs.shift() : `z-summary-${z.number}${a.paper === "a4" ? "-A4" : ""}.pdf`;
      return answer == null ? null : { path: `/home/user/Documents/${answer}`, fileName: answer };
    }
    case "tax_invoice_print":
    case "cash_session_z_print":
    case "return_receipt_print":
    case "receipt_print":
    case "print_job_retry":
    case "print_job_resolve": {
      needSession(s);
      let jobId = String(a.jobId ?? "");
      let job = s.printJobs.get(jobId);
      if (cmd === "receipt_print" || cmd === "return_receipt_print" || cmd === "cash_session_z_print" || cmd === "tax_invoice_print") {
        jobId = id();
        job = { status: "queued", reprint: s.printed > 0, errorCode: null, attempts: 0 };
        s.printJobs.set(jobId, job);
      } else if (!job) fail("VALIDATION_FAILED");
      if (cmd === "print_job_resolve") {
        if (job.status !== "unknown") fail("VALIDATION_FAILED");
        if (a.printed) {
          job.status = "completed";
          return { jobId, ...job };
        }
        job.status = "failed";
        jobId = id();
        job = { status: "queued", reprint: true, errorCode: null, attempts: 0 };
        s.printJobs.set(jobId, job);
      }
      const outcome = s.printScript.shift() ?? "printed";
      job.attempts += 1;
      job.status = outcome === "printed" ? "completed" : outcome === "timeout" ? "unknown" : "failed";
      job.errorCode = outcome === "paper_out" ? "paper_out" : null;
      if (outcome !== "paper_out") s.printed += 1;
      return { jobId, ...job };
    }
    case "business_profile_get":
    case "business_logo_choose":
    case "business_logo_clear":
    case "business_logo_get":
    case "business_profile_update": {
      if (needSession(s).role !== "owner") fail("PERMISSION_DENIED");
      const b = s.business;
      const view = () => ({
        name: workspace.businessName, legalName: b.legalName, taxRegistrationNo: b.taxRegistrationNo, addressLines: b.addressLines,
        phone: b.phone, email: b.email, registerName: workspace.registerName, registerCode: workspace.registerCode, countryCode: "GB",
        timezone: "Europe/London", currencyCode: workspace.currencyCode, pricesIncludeTax: true, businessVersion: b.version, registerVersion: b.registerVersion,
        hasLogo: Boolean(b.logo),
      });
      if (cmd === "business_profile_get") return view();
      if (cmd === "business_logo_get") return b.logo ?? null;
      if (cmd === "business_logo_choose" || cmd === "business_logo_clear") {
        if (cmd === "business_logo_choose") {
          const file = s.logoFile ?? null;
          if (file === null) return null;
          if (file === "notAnImage") fail("VALIDATION_FAILED", { field: "logo", value: "notAnImage" });
        }
        if (a.businessVersion !== b.version) fail("CONFLICT_VERSION");
        // A 1×1 PNG stands for the stored logo.
        b.logo = cmd === "business_logo_choose" ? "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==" : null;
        b.version += 1;
        return view();
      }
      if (a.businessVersion !== b.version || a.registerVersion !== b.registerVersion) fail("CONFLICT_VERSION");
      const input = a.input as { name: string; legalName: string | null; taxRegistrationNo: string | null; addressLines: string[]; phone: string | null; email: string | null; registerName: string };
      const opt = (v: string | null) => v?.trim() || null;
      if (!input.name.trim()) fail("VALIDATION_FAILED", { field: "name" });
      if (input.name.trim().length > 80) fail("VALIDATION_FAILED", { field: "name", value: "80" });
      if (!input.registerName.trim()) fail("VALIDATION_FAILED", { field: "registerName" });
      const lines = input.addressLines.map((l) => l.trim()).filter(Boolean);
      if (lines.length > 4) fail("VALIDATION_FAILED", { field: "addressLines" });
      const email = opt(input.email);
      if (email && !/^[^\s@]+@[^\s@.][^\s@]*\.[^\s@]*[^\s@.]$/.test(email)) fail("VALIDATION_FAILED", { field: "email" });
      const next = { legalName: opt(input.legalName), taxRegistrationNo: opt(input.taxRegistrationNo), addressLines: lines, phone: opt(input.phone), email };
      const businessChanged =
        input.name.trim() !== workspace.businessName || JSON.stringify(next) !== JSON.stringify({ legalName: b.legalName, taxRegistrationNo: b.taxRegistrationNo, addressLines: b.addressLines, phone: b.phone, email: b.email });
      if (businessChanged) {
        workspace.businessName = input.name.trim();
        Object.assign(b, next, { version: b.version + 1 });
      }
      if (input.registerName.trim() !== workspace.registerName) {
        workspace.registerName = input.registerName.trim();
        b.registerVersion += 1;
      }
      return view();
    }
    case "import_products_open": {
      const me = needSession(s);
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      if (!s.openFile) return null;
      const [head, ...lines] = s.openFile.text.trim().split("\n").map((l) => l.split(","));
      const header = head ?? [];
      const jobId = id();
      s.imports.set(jobId, { name: s.openFile.name, header, rows: lines, status: "parsed", valid: [] });
      const synonyms: Record<string, string> = { name: "name", price: "price", sku: "sku", barcode: "barcode", variant: "variant", unit: "unit" };
      const mapping = header.flatMap((h, index) => (synonyms[h.toLowerCase()] ? [{ column: synonyms[h.toLowerCase()], index }] : []));
      return { jobId, fileName: s.openFile.name, encoding: "utf8", delimiter: ",", header, sample: lines.slice(0, 10), totalRows: lines.length, mapping };
    }
    case "import_products_progress": {
      needSession(s);
      const job = s.imports.get(String(a.jobId)) ?? fail("VALIDATION_FAILED");
      return { rowsDone: job.done ?? 0, rowsTotal: job.valid.length };
    }
    case "import_products_validate":
    case "import_products_commit":
    case "import_products_error_report":
    case "import_products_cancel": {
      const me = needSession(s);
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      const job = s.imports.get(String(a.jobId)) ?? fail("VALIDATION_FAILED");
      if (cmd === "import_products_cancel") {
        if (job.status === "committed") fail("VALIDATION_FAILED");
        job.status = "cancelled";
        return null;
      }
      if (cmd === "import_products_error_report") {
        const answer = s.saveAs.length ? s.saveAs.shift() : `${job.name.replace(/\.csv$/, "")}-errors.csv`;
        return answer == null ? null : { path: `/home/user/Documents/${answer}`, fileName: answer };
      }
      if (cmd === "import_products_validate") {
        if (!["parsed", "validated"].includes(job.status)) fail("VALIDATION_FAILED");
        const m = a.mapping as { column: string; index: number }[];
        const col = (c: string) => m.find((x) => x.column === c)?.index;
        const nameAt = col("name");
        const priceAt = col("price");
        if (nameAt === undefined || priceAt === undefined) fail("VALIDATION_FAILED");
        const problems: { line: number; status: string; messages: string[] }[] = [];
        const existing = new Set([...s.catalog.values()].map((p) => p.name.toLowerCase()));
        let dup = 0;
        let skipped = 0;
        job.valid = [];
        job.rows.forEach((r, i) => {
          const line = i + 2;
          const name = (r[nameAt] ?? "").trim();
          const price = (r[priceAt] ?? "").trim();
          if (!name) return problems.push({ line, status: "error", messages: ["name: required"] });
          if (!/^\d+([.,]\d{1,2})?$/.test(price)) return problems.push({ line, status: "error", messages: [price ? "price: cannot be negative" : "price: required"] });
          if (existing.has(name.toLowerCase())) {
            if (a.onDuplicate === "skip") return skipped++;
            if (a.onDuplicate === "error") {
              dup++;
              return problems.push({ line, status: "duplicate_existing", messages: [`"${name}" already exists`] });
            }
          }
          job.valid.push(i);
        });
        job.status = "validated";
        const updates = a.onDuplicate === "update" ? job.valid.filter((i) => existing.has((job.rows[i]?.[nameAt] ?? "").trim().toLowerCase())).length : 0;
        return {
          totalRows: job.rows.length, validRows: job.valid.length, errorRows: problems.length - dup, duplicateRows: dup, skippedRows: skipped,
          productsToCreate: job.valid.length - updates, productsToUpdate: updates, problems,
        };
      }
      if (job.status !== "validated") fail("VALIDATION_FAILED");
      job.status = "committed";
      for (const i of job.valid) {
        const r = job.rows[i] ?? [];
        const productId = id();
        const name = (r[0] ?? "").trim();
        if ([...s.catalog.values()].some((p) => p.name.toLowerCase() === name.toLowerCase())) continue;
        s.catalog.set(productId, {
          productId, name, description: null, categoryId: null, productType: "stock", unit: "ea", taxCategoryId: TAX, quickSelect: false, archived: false, version: 1,
          variants: [{ variantId: id(), isDefault: true, sku: null, options: {}, priceMinor: String(Math.round(parseFloat((r[1] ?? "0").replace(",", ".")) * 100)), costMinor: null, trackStock: true, lowStockThresholdMilli: null, scanQtyMilli: "1000", barcodes: [], archived: false }],
        });
      }
      const result = { productsCreated: job.valid.length, productsUpdated: 0, rowsCommitted: job.valid.length };
      if (s.importStepMs === 0) {
        job.done = job.valid.length;
        return result;
      }
      job.done = 0;
      return new Promise((resolve) => {
        const t = setInterval(() => {
          job.done = (job.done ?? 0) + 1;
          if (job.done >= job.valid.length) {
            clearInterval(t);
            resolve(result);
          }
        }, s.importStepMs);
      });
    }
    case "receipt_export_pdf": {
      needSession(s);
      if ((a.saleId == null) === (a.returnId == null)) fail("VALIDATION_FAILED");
      const answer = s.saveAs.length ? s.saveAs.shift() : "receipt.pdf";
      if (answer == null) return null;
      return { path: `/home/user/Documents/${answer}`, fileName: answer };
    }
    case "receipt_get": {
      needSession(s);
      const number = s.receipts.at(-1) ?? fail("VALIDATION_FAILED");
      const cols = a.widthMm === 58 ? 32 : 48;
      const pad = (l: string, r: string) => l + " ".repeat(Math.max(1, cols - l.length - r.length)) + r;
      const text = ["Corner Shop".padStart(Math.floor((cols + 11) / 2)), "-".repeat(cols), `Receipt ${number}`, pad("TOTAL", "2.19"), pad("Change", "0.00")];
      return { receiptNumber: number, columns: cols, sandbox: true, lines: text.map((t) => ({ text: t, bold: t.startsWith("TOTAL") })) };
    }
    case "cart_current":
      needSession(s);
      return view(s);
    case "scan_submit": {
      needSession(s);
      const raw = String(a.raw).trim();
      if (s.seenScans.has(String(a.scanEventId))) return { outcome: "duplicate", cart: view(s) };
      s.seenScans.add(String(a.scanEventId));
      const p = s.products.find((x) => raw in x.barcodes || x.sku.toLowerCase() === raw.toLowerCase());
      if (!p) return { outcome: "notFound", code: raw, hint: /[àéèçù]/.test(raw) ? "keyboard_layout:azerty_fr" : null };
      const before = s.lines.length;
      const lineId = add(s, p, BigInt(p.barcodes[raw] ?? 1) * 1000n);
      return { outcome: s.lines.length > before ? "added" : "incremented", cart: view(s), lineId, hint: null };
    }
    case "catalog_lookup_code": {
      needSession(s);
      const raw = String(a.code).trim();
      const p = s.products.find((x) => raw in x.barcodes || x.sku.toLowerCase() === raw.toLowerCase());
      if (!p) return null;
      return { ...hit(p), scanQtyMilli: (BigInt(p.barcodes[raw] ?? 1) * 1000n).toString(), matchedBy: raw in p.barcodes ? "barcode" : "sku" };
    }
    case "catalog_search": {
      needSession(s);
      const q = String(a.query).toLowerCase();
      return s.products
        .filter((p) => p.name.toLowerCase().includes(q) || p.sku.toLowerCase().startsWith(q))
        .map((p) => ({ ...hit(p), onHandMilli: String(FAKE_STOCK[p.name] ?? 0n) }));
    }
    case "catalog_quick_select": {
      needSession(s);
      const quick = s.products.filter((p) => p.quick);
      const categories = [...new Map(quick.filter((p) => p.category).map((p) => [p.category?.categoryId, p.category])).values()];
      const inTab = quick.filter((p) => a.categoryId === null || p.category?.categoryId === a.categoryId);
      const byProduct = new Map<string, FakeProduct[]>();
      for (const p of [...inTab].sort((x, y) => x.name.localeCompare(y.name))) byProduct.set(p.productId, [...(byProduct.get(p.productId) ?? []), p]);
      const all = [...byProduct.values()];
      const page = all.slice(Number(a.offset), Number(a.offset) + Number(a.limit));
      return {
        categories: categories.sort((x, y) => (x?.name ?? "").localeCompare(y?.name ?? "")),
        total: all.length,
        products: page.map((vs) => ({ productId: vs[0]?.productId, name: vs[0]?.name, variants: vs.map(hit) })),
      };
    }
    case "cart_add_variant": {
      needSession(s);
      const p = s.products.find((x) => x.variantId === a.variantId) ?? fail("CATALOG_NOT_FOUND");
      add(s, p, 1000n);
      return view(s);
    }
    case "cart_set_quantity": {
      needSession(s);
      const l = s.lines.find((x) => x.lineId === a.lineId) ?? fail("VALIDATION_FAILED");
      l.qtyMilli = BigInt(String(a.qtyMilli));
      s.revision += 1;
      return view(s);
    }
    case "cart_remove_line":
      needSession(s);
      s.lines = s.lines.filter((x) => x.lineId !== a.lineId);
      s.revision += 1;
      return view(s);
    case "cart_set_line_discount": {
      needSession(s);
      const l = s.lines.find((x) => x.lineId === a.lineId) ?? fail("VALIDATION_FAILED");
      const d = a.discount as { ratePpm: number; reason: string | null } | null;
      if (d && d.ratePpm > 100_000) {
        if (!d.reason) fail("VALIDATION_FAILED");
        approve(s, "discount_above_threshold", d.ratePpm, a.approvalTokenId);
      }
      l.discountPpm = d?.ratePpm ?? null;
      s.revision += 1;
      return view(s);
    }
    case "cart_set_order_discount":
      needSession(s);
      s.orderDiscountPpm = (a.discount as { ratePpm: number } | null)?.ratePpm ?? null;
      s.revision += 1;
      return view(s);
    case "cart_recovery": {
      needSession(s);
      if (!s.recoverable || s.lines.length === 0) {
        s.recoverable = false;
        return null;
      }
      const v = view(s);
      return { cartId: s.cartId, revision: s.revision, lines: s.lines.length, totalMinor: v.totalMinor, repricedLines: s.repricedOnRecovery?.length ?? 0 };
    }
    case "cart_recovery_resume":
      needSession(s);
      s.recoverable = false;
      return { ...view(s), repricedLines: s.repricedOnRecovery ?? [] };
    case "cart_recovery_discard":
      needSession(s);
      if (a.revision !== s.revision) fail("CART_REVISION_MISMATCH");
      s.recoverable = false;
      s.lines = [];
      s.orderDiscountPpm = null;
      s.revision += 1;
      return view(s);
    case "cart_clear":
      needSession(s);
      if (a.revision !== s.revision) fail("CART_REVISION_MISMATCH");
      s.lines = [];
      s.orderDiscountPpm = null;
      s.revision += 1;
      return view(s);
    case "diagnostics_export": {
      needSession(s);
      const answer = s.saveAs.length ? s.saveAs.shift() : "sale-os-diagnostics-2026-11-05T0920.zip";
      if (answer == null) return null;
      return { path: `/home/user/Documents/${answer}`, fileName: answer, files: 3, integrityOk: true, stockDrift: 0 };
    }
    case "cart_park": {
      const me = needSession(s);
      if (!s.lines.length) fail("CART_EMPTY");
      if (a.revision !== s.revision) fail("CART_REVISION_MISMATCH");
      if (s.parked.length >= 20) fail("VALIDATION_FAILED");
      s.parked.push({ cartId: s.cartId, label: (a.label as string | null) ?? null, parkedAt: new Date().toISOString(), parkedByName: me.displayName, lines: s.lines, orderDiscountPpm: s.orderDiscountPpm });
      s.lines = [];
      s.orderDiscountPpm = null;
      s.cartId = id();
      s.revision = 0;
      return view(s);
    }
    case "cart_parked_list":
      needSession(s);
      return parkedList(s);
    case "cart_resume": {
      needSession(s);
      const at = s.parked.findIndex((p) => p.cartId === a.cartId);
      if (at < 0) fail("CART_REVISION_MISMATCH");
      if (s.lines.length) fail("VALIDATION_FAILED");
      const [p] = s.parked.splice(at, 1);
      if (!p) fail("CART_REVISION_MISMATCH");
      s.cartId = p.cartId;
      s.lines = p.lines;
      s.orderDiscountPpm = p.orderDiscountPpm;
      s.revision = 1;
      return view(s);
    }
    case "cart_discard_parked": {
      needSession(s);
      approve(s, "cart_void", null, a.approvalTokenId, String(a.cartId));
      if (!String(a.reason ?? "").trim()) fail("VALIDATION_FAILED");
      const at = s.parked.findIndex((p) => p.cartId === a.cartId);
      if (at < 0) fail("CART_REVISION_MISMATCH");
      s.parked.splice(at, 1);
      return parkedList(s);
    }
    case "cart_void": {
      needSession(s);
      if (s.lines.length) approve(s, "cart_void", null, a.approvalTokenId);
      if (a.revision !== s.revision) fail("CART_REVISION_MISMATCH");
      s.lines = [];
      s.orderDiscountPpm = null;
      s.cartId = id();
      s.revision = 0;
      return view(s);
    }
    // ---------- devices, drawer, card terminal (E20-T11, E22-T02) ----------
    case "plugin:event|listen":
      return 1;
    case "plugin:event|unlisten":
      return null;
    case "printer_status": {
      needSession(s);
      const p = s.devices.find((d) => d.kind === "receipt_printer");
      if (!p) return { configured: false, adapter: "virtual", target: null, state: "unknown", message: null, drawer: false, drawerOpen: null, push: false };
      const c = JSON.parse(p.configJson) as { connection: { host?: string; port?: number } };
      return { configured: true, adapter: "escpos_net", target: `${c.connection.host}:${c.connection.port}`, state: "ready", message: null, drawer: true, drawerOpen: false, push: true };
    }
    case "devices_list": {
      const me = needSession(s);
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      return s.devices;
    }
    case "printer_profiles":
      needSession(s);
      return [
        { id: "generic-escpos-80mm-48", vendor: "Generic", name: "ESC/POS 80 mm (48 columns)", dialect: "EscPos", columns: 48, columnsFontB: 64, paperWidthMm: 80, cutter: true, drawer: true, barcode: true, qr: true, statusQuery: true, textSupported: true, verified: true, notes: null },
        { id: "TM-T88V", vendor: "Epson", name: "TM-T88V", dialect: "EscPos", columns: 42, columnsFontB: 56, paperWidthMm: 80, cutter: true, drawer: true, barcode: true, qr: true, statusQuery: true, textSupported: true, verified: true, notes: null },
        { id: "TSP143IIIU", vendor: "Star Micronics", name: "TSP143III", dialect: "StarLine", columns: 48, columnsFontB: null, paperWidthMm: 80, cutter: true, drawer: true, barcode: false, qr: false, statusQuery: false, textSupported: false, verified: false, notes: "Image-only" },
      ];
    case "devices_discover":
      needSession(s);
      return {
        serialPorts: [{ port: "COM5", kind: { kind: "usb", vid: 0x04b8, pid: 0x0e28, manufacturer: "EPSON", product: "TM-T20III", serialNumber: null }, vendor: "Epson", likely: "printer", likelySure: true, label: "COM5: Epson TM-T20III (likely printer)" }],
        deviceFiles: [],
        queues: ["EPSON TM-T20III Receipt"],
        network: a.subnet ? [{ host: "192.168.1.50", port: 9100, identified: { maker: "EPSON", model: "TM-T88V", modelId: null }, profileId: "TM-T88V" }] : [],
        networkProblem: null,
      };
    case "device_save": {
      const me = needSession(s);
      if (me.role === "cashier") fail("PERMISSION_DENIED");
      const input = a.input as { deviceId: string | null; kind: string; name: string; configJson: string; enabled: boolean; baseVersion: number | null };
      const config = JSON.parse(input.configJson) as { connection?: { kind: string; host?: string }; provider?: string };
      if (input.kind === "receipt_printer" && config.connection?.kind === "network" && !config.connection.host) fail("VALIDATION_FAILED", { remainingAttempts: null, retryAtMs: null, scope: null, field: "connection.host", value: null, ownerProductId: null, remainingMinor: null });
      const old = s.devices.find((d) => d.deviceId === input.deviceId);
      if (old && old.version !== input.baseVersion) fail("CONFLICT_VERSION");
      const dev = { deviceId: old?.deviceId ?? id(), kind: input.kind, role: "main", name: input.name || "Device", configJson: input.configJson, enabled: input.enabled, version: (old?.version ?? 0) + 1 };
      s.devices = [...s.devices.filter((d) => d.deviceId !== dev.deviceId && !(d.kind === dev.kind && !old)), dev];
      return dev;
    }
    case "device_remove":
      needSession(s);
      s.devices = s.devices.filter((d) => d.deviceId !== a.deviceId);
      return null;
    case "printer_test":
      needSession(s);
      s.printed += 1;
      return { outcome: "printed", message: "Printed. Check the ruler fits one line and the accents look right." };
    case "drawer_open": {
      const me = needSession(s);
      if (!s.devices.some((d) => d.kind === "receipt_printer")) fail("DEVICE_UNAVAILABLE");
      if (me.role === "cashier") {
        const token = a.approvalTokenId ? s.tokens.get(String(a.approvalTokenId)) : undefined;
        if (!token) fail("APPROVAL_REQUIRED", { remainingAttempts: null, retryAtMs: null, scope: { permission: "drawer_open", entityId: "register", maxPpm: null }, field: null, value: null, ownerProductId: null, remainingMinor: null });
        s.tokens.delete(String(a.approvalTokenId));
      }
      s.drawerOpens.push(`no_sale:${String(a.reason)}`);
      return null;
    }
    case "payment_attempts": {
      needSession(s);
      if (!s.devices.some((d) => d.kind === "payment_terminal")) fail("DEVICE_UNAVAILABLE");
      return [...s.attempts.entries()].filter(([, x]) => x.cartId === a.cartId).map(([k, x]) => attemptDto(k, x));
    }
    case "payment_start": {
      needSession(s);
      if (!s.devices.some((d) => d.kind === "payment_terminal")) fail("DEVICE_UNAVAILABLE");
      const attemptId = id();
      const att = { cartId: String(a.cartId), amountMinor: String(a.amountMinor), status: "pending", polls: 0, used: false, reason: null as string | null };
      s.attempts.set(attemptId, att);
      return attemptDto(attemptId, att);
    }
    case "payment_poll": {
      needSession(s);
      const att = s.attempts.get(String(a.attemptId)) ?? fail("VALIDATION_FAILED");
      if (att.status === "pending") {
        att.polls += 1;
        const cents = BigInt(att.amountMinor) % 100n;
        if (cents === 1n) [att.status, att.reason] = ["declined", "Declined by the sandbox terminal (amount ends in .01)"];
        else if (cents === 2n) [att.status, att.reason] = ["reconciliation_required", "the terminal timed out"];
        else att.status = "approved";
      }
      return attemptDto(String(a.attemptId), att);
    }
    case "payment_cancel": {
      needSession(s);
      const att = s.attempts.get(String(a.attemptId)) ?? fail("VALIDATION_FAILED");
      att.status = "cancelled";
      return attemptDto(String(a.attemptId), att);
    }
    case "payment_resolve": {
      const me = needSession(s);
      if (me.role === "cashier" && !a.approvalTokenId) fail("APPROVAL_REQUIRED", { remainingAttempts: null, retryAtMs: null, scope: { permission: "cart_void", entityId: String(a.attemptId), maxPpm: null }, field: null, value: null, ownerProductId: null, remainingMinor: null });
      const att = s.attempts.get(String(a.attemptId)) ?? fail("VALIDATION_FAILED");
      const r = a.resolution as { outcome: string };
      att.status = r.outcome === "approved" ? "approved" : r.outcome;
      att.reason = String(a.reason);
      return attemptDto(String(a.attemptId), att);
    }
    case "payment_reverse": {
      needSession(s);
      const att = s.attempts.get(String(a.attemptId)) ?? fail("VALIDATION_FAILED");
      att.status = "reversed";
      return attemptDto(String(a.attemptId), att);
    }
    default:
      throw new Error(`fake backend: unhandled command ${cmd}`);
  }
}
