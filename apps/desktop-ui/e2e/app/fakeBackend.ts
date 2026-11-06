/**
 * A scripted stand-in for the Rust commands, for browser tests of the screens.
 *
 * It records every IPC call with its arguments (what the tests assert) and returns
 * well-formed DTOs. Pricing here is deliberately simple: totals are Rust's job and are
 * tested there (pricing goldens, cart integration tests, the IPC harness).
 */
import type { CartLine, CartView, CatalogHit, IpcError, Session, SignInUser } from "../../src/ipc";

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

export function initialState(preset: Preset): FakeState {
  const owner: StaffUser = { userId: id(), displayName: "Olu", role: "owner", pin: "1234", failed: 0 };
  const cashier: StaffUser = { userId: id(), displayName: "Cal", role: "cashier", pin: "3333", failed: 0 };
  return {
    setupComplete: preset !== "fresh",
    users: preset === "fresh" ? [] : [owner, cashier],
    session: preset === "checkout" ? { userId: owner.userId, displayName: owner.displayName, role: owner.role, mustChangePin: false } : null,
    products: products(),
    lines: [],
    orderDiscountPpm: null,
    cartId: id(),
    revision: 0,
    seenScans: new Set(),
    lockedUntil: null,
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

const workspace = { businessName: "Corner Shop", registerName: "Till 1", registerCode: "R01", currencyCode: "GBP", mode: "sandbox" };

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
    case "auth_logout":
      needSession(s);
      s.session = null;
      return null;
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
      l.discountPpm = (a.discount as { ratePpm: number } | null)?.ratePpm ?? null;
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
      const who = needSession(s);
      if (s.lines.length && who.role === "cashier") fail("PERMISSION_DENIED");
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
