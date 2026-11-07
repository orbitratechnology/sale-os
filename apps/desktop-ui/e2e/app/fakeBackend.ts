/**
 * A scripted stand-in for the Rust commands, for browser tests of the screens.
 *
 * It records every IPC call with its arguments (what the tests assert) and returns
 * well-formed DTOs. Pricing here is deliberately simple: totals are Rust's job and are
 * tested there (pricing goldens, cart integration tests, the IPC harness).
 */
import type { CartLine, CartView, CatalogHit, IpcError, Session, SignInUser, Workspace } from "../../src/ipc";
import type { Permission, ProductDto_Serialize as ProductDto, ProductInputDto_Deserialize as ProductInput } from "../../src/ipc/bindings";

export interface FakeProduct {
  variantId: string;
  productId: string;
  name: string;
  sku: string;
  barcodes: Record<string, number>; // code → units per scan
  priceMinor: bigint;
}

interface StaffUser extends SignInUser {
  pin: string;
  failed: number;
}

export interface FakeState {
  setupComplete: boolean;
  users: StaffUser[];
  session: Session | null;
  products: FakeProduct[];
  lines: { lineId: string; variantId: string; qtyMilli: bigint; discountPpm: number | null }[];
  orderDiscountPpm: number | null;
  cartId: string;
  revision: number;
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
  drawer: {
    required: boolean;
    seq: number;
    session: { id: string; number: string; floatMinor: bigint; cashInMinor: bigint; sales: number; openedBy: string } | null;
    lastClosed: { id: string; number: string } | null;
  };
  /** Inventory documents posted so far (for their numbers). */
  documents: number;
  /** CSV exports made. */
  exports: number;
  /** Completed returns: id → number. */
  returns: Map<string, { number: string; totalMinor: bigint }>;
  /** Outcomes for the next print submissions: "printed", "paper_out" or "timeout". */
  printScript: string[];
  printJobs: Map<string, { status: string; reprint: boolean; errorCode: string | null; attempts: number }>;
  printed: number;
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
}

export type Preset = "fresh" | "signIn" | "checkout";

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
        lowStockThresholdMilli: null,
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

export function initialState(preset: Preset): FakeState {
  const s = initialStateInner(preset);
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
    exports: 0,
    printScript: [],
    printJobs: new Map(),
    printed: 0,
  };
}

function fail(code: IpcError["code"], details: IpcError["details"] = null): never {
  const e: IpcError = { code, message: code, retryable: false, correlationId: id(), details };
  throw e;
}

const div = (a: bigint, b: bigint) => (a >= 0n ? (a + b / 2n) / b : (a - b / 2n) / b);

/** Sellable on hand per product name, matching the fake stock report. */
const FAKE_STOCK: Record<string, bigint> = { "Chocolate bar": -2000n, "Cola 330ml": 40000n, Crisps: 3000n };

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
  return { productId: p.productId, variantId: p.variantId, name: p.name, label: "", sku: p.sku, priceMinor: p.priceMinor.toString(), unit: "ea", trackStock: true, scanQtyMilli: "1000", matchedBy: "search" };
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
export function handle(s: FakeState, cmd: string, a: Record<string, unknown>): unknown {
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
      return s.users.map(({ userId, displayName, role }) => ({ userId, displayName, role }));
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
    case "tax_list":
      needSession(s);
      return [{ taxCategoryId: TAX, name: "Standard", isDefault: true, rates: [{ name: "VAT 20%", ratePpm: 200000 }] }];
    case "product_list": {
      needSession(s);
      const f = a.filter as { query: string; status: string; trackStock: boolean | null };
      const offset = Number(a.offset);
      const limit = Number(a.limit);
      if (s.bigCatalog !== null) {
        const total = s.bigCatalog;
        const rows = Array.from({ length: Math.max(0, Math.min(limit, total - offset)) }, (_, i) => {
          const n = offset + i;
          return { productId: `big-${n}`, name: `Item ${String(n).padStart(6, "0")}`, archived: false, version: 1, variantCount: 1, sku: `SKU${n}`, priceMinMinor: "100", priceMaxMinor: "100", barcodeCount: 1, trackStock: true, quickSelect: false, onHandMilli: "0", negativeStock: false };
        });
        return { rows, total };
      }
      const q = f.query.trim().toLowerCase();
      const all = [...s.catalog.values()]
        .filter((p) => (f.status === "all" ? true : f.status === "archived" ? p.archived : !p.archived))
        .filter((p) => f.trackStock === null || p.variants.some((v) => v.trackStock) === f.trackStock)
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
        };
      });
      return { rows, total: all.length };
    }
    case "product_get":
      needSession(s);
      return s.catalog.get(String(a.productId)) ?? fail("CATALOG_NOT_FOUND");
    case "product_create":
    case "product_update": {
      if (needSession(s).role === "cashier") fail("PERMISSION_DENIED");
      const input = a.product as ProductInput;
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
        productId, name: input.name, description: input.description ?? null, categoryId: null, productType: input.productType, unit: input.unit,
        taxCategoryId: input.taxCategoryId, quickSelect: input.quickSelect, archived: false, version: (existing?.version ?? 0) + 1,
        variants: input.variants.map((v, i) => ({
          variantId: v.variantId ?? id(), isDefault: i === 0, sku: v.sku ?? null, options: v.options, priceMinor: v.priceMinor, costMinor: null,
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
      // The same rules as pos_domain::settlement::settle.
      const total = BigInt(view(s).totalMinor);
      let remaining = total;
      let tendered = 0n;
      const tenders = a.tenders as { kind: string; amountMinor: string; reference: string | null }[];
      for (const [i, t] of tenders.entries()) {
        const amount = BigInt(t.amountMinor);
        if (amount <= 0n || remaining === 0n) fail("VALIDATION_FAILED");
        if (t.kind !== "cash" && amount > remaining) fail("TENDER_OVERPAY_NON_CASH");
        if (t.kind === "cash" && amount > remaining && i < tenders.length - 1) fail("TENDER_CHANGE_FROM_NON_CASH");
        if (t.reference && /\d{13,19}/.test(t.reference.replace(/[ -]/g, ""))) fail("VALIDATION_FAILED");
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
      s.lines = [];
      s.orderDiscountPpm = null;
      s.cartId = id();
      s.revision = 0;
      return {
        saleId, receiptNumber, totalMinor: total.toString(), tenderedMinor: tendered.toString(),
        changeMinor: (tendered - total).toString(), roundingMinor: "0", currencyExponent: 2, mode: "sandbox", syncPending: true, negativeStockItems: 0, autoPrint: false,
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
      return [
        { movementId: id(), localTime: "2026-11-07 10:05", movementType: "sale", bucket: "sellable", deltaMilli: "-2000", sourceType: "sale", sourceId: id(), sourceNumber: "R01-000004", actorName: "Cal" },
        { movementId: id(), localTime: "2026-11-07 08:30", movementType: "receipt", bucket: "sellable", deltaMilli: "24000", sourceType: "inventory_document", sourceId: id(), sourceNumber: "R01-IN-000001", actorName: "Mia" },
      ];
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
      return { path: `/data/exports/${name}`, fileName: name, rows };
    }
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
    case "cash_session_z_print":
    case "return_receipt_print":
    case "receipt_print":
    case "print_job_retry":
    case "print_job_resolve": {
      needSession(s);
      let jobId = String(a.jobId ?? "");
      let job = s.printJobs.get(jobId);
      if (cmd === "receipt_print" || cmd === "return_receipt_print" || cmd === "cash_session_z_print") {
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
      if (!p) return { outcome: "notFound", code: raw };
      const before = s.lines.length;
      const lineId = add(s, p, BigInt(p.barcodes[raw] ?? 1) * 1000n);
      return { outcome: s.lines.length > before ? "added" : "incremented", cart: view(s), lineId };
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
      return s.products.filter((p) => p.name.toLowerCase().includes(q) || p.sku.toLowerCase().startsWith(q)).map(hit);
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
    case "cart_clear":
      needSession(s);
      if (a.revision !== s.revision) fail("CART_REVISION_MISMATCH");
      s.lines = [];
      s.orderDiscountPpm = null;
      s.revision += 1;
      return view(s);
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
    default:
      throw new Error(`fake backend: unhandled command ${cmd}`);
  }
}
