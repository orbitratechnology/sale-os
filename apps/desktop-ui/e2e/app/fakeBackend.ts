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
  };
}

function fail(code: IpcError["code"], details: IpcError["details"] = null): never {
  const e: IpcError = { code, message: code, retryable: false, correlationId: id(), details };
  throw e;
}

const div = (a: bigint, b: bigint) => (a >= 0n ? (a + b / 2n) / b : (a - b / 2n) / b);

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
function approve(s: FakeState, permission: Permission, maxPpm: number | null, token: unknown) {
  if (needSession(s).role !== "cashier") return;
  const needed = { permission, entityId: s.cartId, maxPpm };
  if (token === null || token === undefined) fail("APPROVAL_REQUIRED", { remainingAttempts: null, retryAtMs: null, scope: needed });
  const granted = s.tokens.get(String(token));
  s.tokens.delete(String(token));
  const covers =
    granted?.permission === permission && granted.entityId === s.cartId && (maxPpm === null || (granted.maxPpm ?? -1) >= maxPpm);
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
          return { productId: `big-${n}`, name: `Item ${String(n).padStart(6, "0")}`, archived: false, version: 1, variantCount: 1, sku: `SKU${n}`, priceMinMinor: "100", priceMaxMinor: "100", barcodeCount: 1, trackStock: true, quickSelect: false };
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
