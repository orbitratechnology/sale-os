import {
  BadgePercent,
  Ban,
  Banknote,
  Boxes,
  ChartColumn,
  CircleExclamationPoint,
  CirclePause,
  ClipboardList,
  FlaskConical,
  Hash,
  Info,
  LayoutGrid,
  ListX,
  Lock,
  Package,
  PackageSearch,
  Percent,
  ReceiptText,
  ScanBarcode,
  Settings,
  Store,
  Tag,
  Trash,
  TriangleExclamationPoint,
  Undo2,
  Wallet,
  ArrowRightLeft,
  LockOpen,
} from "../../ui/icons";
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type Session, type Workspace } from "../../ipc";
import type { CashStateDto_Serialize as CashState, RecoveredCartDto } from "../../ipc/bindings";
import { formatMilli, percentToPpm, ppmToPercent, scaleDecimal, stepMilli } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { ApprovalDialog } from "../auth/ApprovalDialog";
import { GoLiveDialog } from "../setup/GoLiveDialog";
import { useScanCapture } from "../scan/useScanCapture";
import { useSerialScans } from "../scan/useSerialScans";
import { describeError } from "../shell/errors";
import { CartTable, type Editing } from "./CartTable";
import { Dialog } from "./Dialog";
import { ParkedDialog } from "./ParkedDialog";
import { QuickSelectDialog } from "./QuickSelectDialog";
import { RecoveryDialog } from "./RecoveryDialog";
import { errorBeep } from "../../lib/beep";
import { GettingStarted } from "../setup/GettingStarted";
import { PaymentPanel } from "./PaymentPanel";
import type { ExchangeCredit } from "../returns/exchange";
import { SearchPanel, type SearchHandle } from "./SearchPanel";
import { StatusBar } from "./StatusBar";
import { scanHintText, useCart } from "./useCart";
import { useT, type T } from "../../i18n";

type Modal =
  | "goLive"
  | "pay"
  | "clear"
  | "void"
  | "orderDiscount"
  | "price"
  | "park"
  | "parked"
  | "recovered"
  | "start"
  | "quick"
  | { reason: string; then: (reason: string) => void }
  | null;

/** The key map (E09-T05). Shown in the action bar; documented in docs/keyboard.md. */
export const KEYS = {
  search: "F2",
  quick: "Shift+F2",
  quantity: "F3",
  discount: "F4",
  remove: "Delete",
  park: "F5",
  parked: "Shift+F5",
  returns: "F7",
  sales: "F8",
  drawer: "F10",
  stock: "F11",
  pay: "F9",
  lock: "F12",
} as const;

function inTextField(): boolean {
  const el = document.activeElement;
  return el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement;
}

/**
 * Checkout (E09-T03..T06). Everything happens on this screen: scan, search, change
 * quantities and discounts inline, clear or void. Totals come from Rust; the UI formats.
 */
export function Checkout({
  active = true,
  workspace,
  session,
  onSignedOut,
  canEditCatalog,
  onProducts,
  onReturns,
  onSales,
  onDrawer,
  onStock,
  onReports,
  onWentLive,
  onSettings,
  onConflicts,
  onCreateProduct,
  gettingStarted = false,
  onGettingStartedSeen,
  errorSound = true,
  exchange = null,
  onExchangeEnd,
}: {
  /** A return brought here as credit for new items (from the return screen). */
  exchange?: ExchangeCredit | null;
  /** The exchange was settled or cancelled. */
  onExchangeEnd?: () => void;
  /**
   * The till stays mounted while another screen is open (coming back is then instant). While
   * inactive it is hidden: no scanning, no keys, no dialogs; on return it refreshes its data.
   */
  active?: boolean;
  workspace: Workspace;
  session: Session;
  onSignedOut: () => void;
  canEditCatalog: boolean;
  onProducts: () => void;
  onReturns: () => void;
  onSales: () => void;
  onDrawer: () => void;
  /** Receive and adjust stock; `null` for roles that cannot (UI only: Rust checks). */
  onStock: (() => void) | null;
  /** Reports; `null` for roles without ReportsView (UI only: Rust checks). */
  onReports: (() => void) | null;
  /** Owner only: the till went live (refresh the workspace). `null` hides "Go live". */
  onWentLive: (() => void) | null;
  /** Owner only: the settings screen, optionally on one page ("Staff", "Business"). */
  onSettings: ((group?: string) => void) | null;
  /** `scanner.error_sound`: a tone with not-found and refusal messages (E07-T04). */
  errorSound?: boolean;
  /** E04-T02: open "Getting started" now (the wizard just finished). */
  gettingStarted?: boolean;
  onGettingStartedSeen?: () => void;
  /** Managers and owners: the sync conflicts screen (E18). `null` for cashiers. */
  onConflicts: (() => void) | null;
  /** E07-T04: open the editor with an unknown barcode pre-filled. */
  /** `approvalTokenId`: a manager approved a cashier creating this one product (E07-T04). */
  onCreateProduct: (barcode: string, approvalTokenId?: string) => void;
}) {
  const c = useCart(onSignedOut);
  const [drawer, setDrawer] = useState<CashState | null>(null);
  useEffect(() => {
    let live = true;
    void commands.cashSessionCurrent().then((res) => live && res.status === "ok" && setDrawer(res.data));
    return () => {
      live = false;
    };
  }, []);
  const drawerNeeded = drawer !== null && drawer.required && drawer.session === null;
  const [editing, setEditing] = useState<Editing>(null);
  const [modal, setModal] = useState<Modal>(gettingStarted && onWentLive ? "start" : null);
  const [code, setCode] = useState("");
  const [scannerReady, setScannerReady] = useState(true);
  const search = useRef<SearchHandle | null>(null);
  const [parkedCount, setParkedCount] = useState(0);
  const countParked = useCallback(() => {
    void commands.cartParkedList().then((res) => res.status === "ok" && setParkedCount(res.data.length));
  }, []);
  useEffect(countParked, [countParked]);
  // E09-T02: a sale left open when the till was closed is offered back once, after sign-in.
  const [recovered, setRecovered] = useState<RecoveredCartDto | null>(null);
  useEffect(() => {
    let live = true;
    void commands.cartRecovery().then((res) => {
      if (!live || res.status !== "ok" || !res.data) return;
      setRecovered(res.data);
      setModal("recovered");
    });
    return () => {
      live = false;
    };
  }, []);
  const exponent = c.cart?.currencyExponent ?? 2;
  // Startup timing for scripts/ui-latency.mjs: the till has its cart and can take a scan.
  const loaded = c.cart !== null;
  useEffect(() => {
    if (loaded && performance.getEntriesByName("sale-os:till-ready").length === 0) performance.mark("sale-os:till-ready");
  }, [loaded]);
  // E07-T04: the error cue. The message is always on screen; the tone is the setting's.
  const noticeKind = c.notice?.kind;
  useEffect(() => {
    if (errorSound && (noticeKind === "notFound" || noticeKind === "error")) errorBeep();
  }, [c.notice, noticeKind, errorSound]);
  const money = (minor: string) => formatMoney(minor, { exponent });
  const selectedLine = c.line(c.selected);
  const t = useT();

  // Wedge scans: in scanner-ready mode, or typed into the code field.
  // Paused while a dialog owns the keyboard (approval PIN pad, reason, confirmations).
  useScanCapture(
    (scan) => {
      if (scan.inScanField) setCode("");
      void c.scan(scan.raw, scan.scanEventId, "scanner");
    },
    {},
    active && modal === null && c.approval === null,
    (d) => {
      if (d === "missing_suffix")
        c.setNotice({
          kind: "info",
          text: t("That looked like a scan, but the scanner did not end it with the key set in Settings → Scanner. Match the setting to the scanner, or set it to “none”."),
        });
      else if (d === "interrupted_scan")
        c.setNotice({ kind: "info", text: t("A scan was interrupted part-way and nothing was added. Scan the item again.") });
    },
  );

  // Serial scanners deliver through Rust, exactly like wedge scans (E21-T04).
  useSerialScans((scan) => void c.scan(scan.raw, scan.scanEventId, "scanner"), modal === null && c.approval === null);

  useEffect(() => {
    // Only while the till is showing: hidden, it does no work at all.
    if (!active) return;
    const update = () => setScannerReady(!inTextField());
    // On focusout the next element is not focused yet: check after the event.
    const later = () => setTimeout(update, 0);
    document.addEventListener("focusin", update);
    document.addEventListener("focusout", later);
    return () => {
      document.removeEventListener("focusin", update);
      document.removeEventListener("focusout", later);
    };
  }, [active]);

  /** "No sale" (E20-T05): opens the drawer with a reason; a cashier needs a manager. */
  const noSale = useCallback(
    (reason: string) => {
      const attempt = async (approvalTokenId: string | null): Promise<boolean> => {
        const res = await commands.drawerOpen(reason, approvalTokenId);
        if (res.status === "ok") {
          c.setNotice({ kind: "info", text: t("Cash drawer opened.") });
          return true;
        }
        if (res.error.code === "APPROVAL_REQUIRED" && res.error.details?.scope) {
          c.setApproval({ scope: res.error.details.scope, retry: (token) => attempt(token) });
          return false;
        }
        c.setNotice({ kind: "error", error: res.error });
        return false;
      };
      return attempt(null);
    },
    [c, t],
  );

  const lock = useCallback(async () => {
    await commands.authLogout();
    onSignedOut();
  }, [onSignedOut]);

  const commit = (lineId: string, field: "qty" | "discount", value: string) => {
    setEditing(null);
    if (field === "qty") {
      const milli = scaleDecimal(value, 3);
      if (milli === null || milli === "0") {
        c.setNotice({ kind: "info", text: t("Enter a quantity above zero. To take an item off, press Delete.") });
        return;
      }
      void c.setQuantity(lineId, milli);
    } else {
      if (!value.trim()) return void c.setLineDiscount(lineId, null);
      const ppm = percentToPpm(value.replace("%", ""));
      if (ppm === null) {
        c.setNotice({ kind: "info", text: t("Enter the discount as a percentage, e.g. 10.") });
        return;
      }
      withReason(ppm, t("Reason for {pct}% off", { pct: ppmToPercent(ppm) }), (reason) =>
        void c.setLineDiscount(lineId, { kind: "percent", ratePpm: ppm, reason }),
      );
    }
  };

  /** Above the cashier limit, a discount needs a reason (E08-T02): ask before sending. */
  const withReason = (ppm: number, title: string, send: (reason: string | null) => void) => {
    if (ppm > (c.cart?.discountLimitPpm ?? 1_000_000)) setModal({ reason: title, then: (r) => send(r.trim()) });
    else send(null);
  };

  // Global keys. Ignored while a dialog is open (the dialog owns Esc and Enter).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!active || modal || c.approval || e.defaultPrevented) return;
      const typing = inTextField();
      const sel = c.selected;
      switch (e.key) {
        case "F2":
          e.preventDefault();
          if (e.shiftKey) setModal("quick");
          else search.current?.focus();
          return;
        case "F3":
          e.preventDefault();
          if (sel && c.line(sel)?.scaleLabel) {
            c.setNotice({ kind: "info", text: t("This item's weight or price comes from its scale label. Remove the line and scan the label again.") });
          } else if (sel) setEditing({ lineId: sel, field: "qty" });
          return;
        case "F4":
          e.preventDefault();
          if (sel) setEditing({ lineId: sel, field: "discount" });
          return;
        case "F5":
          // Also stops the WebView reloading the page.
          e.preventDefault();
          if (e.shiftKey) setModal("parked");
          else if (c.cart?.lines.length) setModal("park");
          return;
        case "F6":
          e.preventDefault();
          onProducts();
          return;
        case "F7":
          e.preventDefault();
          onReturns();
          return;
        case "F8":
          e.preventDefault();
          onSales();
          return;
        case "F10":
          e.preventDefault();
          onDrawer();
          return;
        case "F11":
          e.preventDefault();
          onStock?.();
          return;
        case "F9":
          e.preventDefault();
          if (c.cart?.lines.length) setModal("pay");
          return;
        case "F12":
          e.preventDefault();
          void lock();
          return;
        case "Escape":
          setEditing(null);
          c.setNotice(null);
          (document.activeElement as HTMLElement | null)?.blur();
          return;
      }
      if (typing) return;
      const lines = c.cart?.lines ?? [];
      const at = lines.findIndex((l) => l.lineId === sel);
      if (e.key === "Delete" && sel) {
        e.preventDefault();
        void c.removeLine(sel);
      } else if (e.key === "ArrowUp" && at > 0) {
        e.preventDefault();
        c.setSelected(lines[at - 1]?.lineId ?? sel);
      } else if (e.key === "ArrowDown" && at >= 0 && at < lines.length - 1) {
        e.preventDefault();
        c.setSelected(lines[at + 1]?.lineId ?? sel);
      } else if ((e.code === "NumpadAdd" || e.code === "NumpadSubtract") && sel) {
        e.preventDefault();
        const line = c.line(sel);
        if (line && line.unit === "ea" && !line.scaleLabel) void c.setQuantity(sel, stepMilli(line.quantityMilli, e.code === "NumpadAdd" ? 1 : -1));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, c, modal, lock, onProducts, onReturns, onSales, onDrawer, onStock, t]);

  // Leaving the till closes what was open on it (adjusted while rendering, the React way to
  // reset state when a prop changes).
  const [wasActive, setWasActive] = useState(active);
  if (wasActive !== active) {
    setWasActive(active);
    if (!active) {
      setModal(null);
      setEditing(null);
      c.setNotice(null);
    }
  }
  // Coming back shows the last state at once, then refreshes it: another screen may have
  // changed the cart (a return, a new product), the drawer or the parked carts.
  const returned = useRef(false);
  const { refresh } = c;
  useEffect(() => {
    if (!active) {
      returned.current = true;
      return;
    }
    if (!returned.current) return;
    returned.current = false;
    void refresh();
    countParked();
    void commands.cashSessionCurrent().then((res) => res.status === "ok" && setDrawer(res.data));
  }, [active, refresh, countParked]);
  // Dialogs only ever show on the active till: a modal in the hidden till would block the
  // screen that is open.
  const shown = active ? modal : null;

  const submitCode = (e: FormEvent) => {
    e.preventDefault();
    const raw = code.trim();
    if (!raw) return;
    setCode("");
    void c.scan(raw, crypto.randomUUID(), "manual");
  };

  const cart = c.cart;
  const hasLines = (cart?.lines.length ?? 0) > 0;

  return (
    <div className="checkout">
      {/* Landmarks: header (mode banner and screens), main (adding items and the cart), aside
          (totals and actions), footer (status). The wrappers lay out as their children. */}
      <header className="till-head">
      {workspace.mode === "sandbox" && (
        <div className="sandbox-banner" role="note">
          <FlaskConical />
          {t("SANDBOX: practice mode. Sales here are excluded from reports.")}
          {onWentLive && (
            <>
              {" "}
              <button type="button" className="link" onClick={() => setModal("start")}>
                {t("Getting started")}
              </button>{" "}
              <button type="button" className="link" onClick={() => setModal("goLive")}>
                {t("Go live…")}
              </button>
            </>
          )}
        </div>
      )}
      <div className="topbar">
        <div className="brand">
          <span className="brand-mark">
            <Store />
          </span>
          <span className="brand-text">
            <strong>{workspace.businessName}</strong>
            <span>
              {workspace.registerName} · {workspace.registerCode}
            </span>
          </span>
        </div>
        <nav className="screen-nav" aria-label={t("Screens")}>
          <button type="button" onClick={onProducts}>
            <Package />
            {t("Products")} <kbd>F6</kbd>
          </button>
          <button type="button" onClick={onSales}>
            <ReceiptText />
            {t("Sales")} <kbd>F8</kbd>
          </button>
          <button type="button" onClick={onReturns}>
            <Undo2 />
            {t("Returns")} <kbd>F7</kbd>
          </button>
          <button type="button" onClick={onDrawer}>
            <Banknote />
            {t("Drawer")} <kbd>F10</kbd>
          </button>
          <button
            type="button"
            onClick={() => setModal({ reason: t("Why open the cash drawer without a sale?"), then: (r) => void noSale(r.trim()) })}
          >
            <LockOpen />
            {t("No sale")}
          </button>
          {onStock && (
            <button type="button" onClick={onStock}>
              <Boxes />
              {t("Stock")} <kbd>F11</kbd>
            </button>
          )}
          {onReports && (
            <button type="button" onClick={onReports}>
              <ChartColumn />
              {t("Reports")}
            </button>
          )}
          {onSettings && (
            <button type="button" onClick={() => onSettings()}>
              <Settings />
              {t("Settings")}
            </button>
          )}
        </nav>
        <button type="button" className="secondary lock" onClick={() => void lock()}>
          <Lock />
          {t("Lock")} <kbd>F12</kbd>
        </button>
      </div>
      </header>
      <main className="till-main">
      <h1 className="visually-hidden">{t("Till")}</h1>
      {/* Arrow keys move the selected line without moving focus (the page keeps it for the
          scanner): say which line is selected, for screen-reader users. */}
      <p className="visually-hidden" aria-live="polite" data-testid="selected-line">
        {selectedLine
          ? t("Selected: {name}, quantity {qty}, {total}", {
              name: selectedLine.name,
              qty: formatMilli(selectedLine.quantityMilli),
              total: money(selectedLine.totalMinor),
            })
          : ""}
      </p>
      <section className="entry" aria-label={t("Add items")}>
        <form onSubmit={submitCode} className="code-form">
          <ScanBarcode />
          <input
            data-scan-field
            aria-label={t("Scan or type a code")}
            placeholder={t("Scan or type a code, then Enter")}
            value={code}
            onChange={(e) => setCode(e.target.value)}
            autoComplete="off"
          />
        </form>
        <SearchPanel ref={search} exponent={exponent} onPick={(hit) => void c.addVariant(hit.variantId)} />
        <button type="button" className="secondary quick-open" onClick={() => setModal("quick")}>
          <LayoutGrid />
          {t("Quick items")} <kbd>⇧F2</kbd>
        </button>
      </section>

      <section className="cart-area" aria-label={t("Cart lines")}>
        {cart && (
          <CartTable
            cart={cart}
            selected={c.selected}
            highlight={c.highlight}
            editing={editing}
            onSelect={c.setSelected}
            onCommit={commit}
            onCancel={() => setEditing(null)}
          />
        )}
      </section>
      </main>

      <aside className="side" aria-label={t("Totals and actions")}>
        {cart && (
          <dl className="totals">
            <dt>{t("Subtotal")}</dt>
            <dd>{money(cart.subtotalMinor)}</dd>
            {cart.lineDiscountTotalMinor !== "0" && (
              <>
                <dt>{t("Line discounts")}</dt>
                <dd>-{money(cart.lineDiscountTotalMinor)}</dd>
              </>
            )}
            {cart.orderDiscountTotalMinor !== "0" && (
              <>
                <dt>{t("Order discount")}</dt>
                <dd>-{money(cart.orderDiscountTotalMinor)}</dd>
              </>
            )}
            {cart.taxBreakdown.map((tax) => (
              <TaxRow key={tax.name} name={tax.name} amount={money(tax.taxMinor)} included={cart.taxMode === "inclusive"} t={t} />
            ))}
          </dl>
        )}
        <div className="total-due" aria-live="polite">
          <span className="total-label">{t("Total due {currency}", { currency: cart?.currencyCode ?? "" })}</span>
          <output className="total-amount" data-testid="total">
            {cart ? money(cart.totalMinor) : ""}
          </output>
        </div>
        <div className="action-group" role="group" aria-labelledby="line-actions">
          <h2 id="line-actions">{t("This line")}</h2>
          <div className="actions-grid">
            <button type="button" className="secondary" disabled={!selectedLine || !!selectedLine.scaleLabel} onClick={() => c.selected && setEditing({ lineId: c.selected, field: "qty" })}>
              <Hash />
              {t("Quantity")} <kbd>F3</kbd>
            </button>
            <button type="button" className="secondary" disabled={!selectedLine} onClick={() => c.selected && setEditing({ lineId: c.selected, field: "discount" })}>
              <Percent />
              {t("Discount")} <kbd>F4</kbd>
            </button>
            <button type="button" className="secondary" disabled={!selectedLine} onClick={() => setModal("price")}>
              <Tag />
              {t("Change price")}
            </button>
            <button type="button" className="secondary" disabled={!selectedLine} onClick={() => c.selected && void c.removeLine(c.selected)}>
              <Trash />
              {t("Remove")} <kbd>Del</kbd>
            </button>
          </div>
        </div>
        <div className="action-group" role="group" aria-labelledby="sale-actions">
          <h2 id="sale-actions">{t("This sale")}</h2>
          <div className="actions-grid">
            <button type="button" className="secondary" disabled={!hasLines} onClick={() => setModal("orderDiscount")}>
              <BadgePercent />
              {t("Order discount")}
            </button>
            <button type="button" className="secondary" disabled={!hasLines} onClick={() => setModal("park")}>
              <CirclePause />
              {t("Park")} <kbd>F5</kbd>
            </button>
            <button type="button" className="secondary wide" onClick={() => setModal("parked")}>
              <ClipboardList />
              {t("Parked ({n})", { n: parkedCount })} <kbd>⇧F5</kbd>
            </button>
            <button type="button" className="secondary quiet-danger" disabled={!hasLines} onClick={() => setModal("clear")}>
              <ListX />
              {t("Clear cart")}
            </button>
            <button type="button" className="secondary quiet-danger" disabled={!hasLines} onClick={() => setModal("void")}>
              <Ban />
              {t("Void sale")}
            </button>
          </div>
        </div>
        {exchange && (
          <div className="exchange-banner" role="status" data-testid="exchange-banner">
            <ArrowRightLeft />
            <p>
              <strong>{t("Exchange")}</strong> · {t("{items} from", { items: exchange.items })} <span className="mono">{exchange.receiptNumber}</span>
              <br />
              {t("Credit {amount} towards the new items", {
                amount: formatMoney(exchange.creditMinor, { exponent: workspace.currencyExponent }),
              })}
            </p>
            <button type="button" className="link" onClick={onExchangeEnd}>
              {t("Cancel exchange")}
            </button>
          </div>
        )}
        {drawerNeeded && (
          <p role="status" className="drawer-needed">
            <TriangleExclamationPoint />
            {t("Open the cash drawer before selling.")}{" "}
            <button type="button" className="link" onClick={onDrawer}>
              {t("Open drawer (F10)")}
            </button>
          </p>
        )}
        <Notice
          c={c}
          onSearch={(text) => search.current?.focus(text)}
          onCreate={canEditCatalog ? onCreateProduct : null}
          onAskManager={
            canEditCatalog
              ? null
              : (code) =>
                  c.setApproval({
                    scope: { permission: "barcode_create_from_scan", entityId: code.trim(), maxPpm: null },
                    retry: (token) => {
                      c.setNotice(null);
                      onCreateProduct(code, token);
                      return Promise.resolve(true);
                    },
                  })
          }
        />
        <button type="button" className="pay" disabled={!hasLines} onClick={() => setModal("pay")}>
          {exchange ? <ArrowRightLeft /> : <Wallet />}
          {exchange ? t("Settle exchange") : t("Pay")} <kbd>F9</kbd>
        </button>
      </aside>

      <StatusBar workspace={workspace} session={session} scannerReady={scannerReady} drawer={drawer} onConflicts={onConflicts} />

      {shown === "pay" && cart && (
        <PaymentPanel
          cart={cart}
          exchange={exchange}
          requesterId={session.userId}
          onClose={() => setModal(null)}
          onDone={() => {
            setModal(null);
            if (exchange) onExchangeEnd?.();
            void c.refresh();
          }}
        />
      )}
      {shown === "goLive" && onWentLive && (
        <GoLiveDialog
          onCancel={() => setModal(null)}
          onDone={() => {
            setModal(null);
            onWentLive();
          }}
        />
      )}
      {shown === "clear" && (
        <Dialog
          title={t("Clear the cart?")}
          confirmLabel={t("Clear cart")}
          danger
          onCancel={() => setModal(null)}
          onConfirm={() => {
            setModal(null);
            void c.clear();
          }}
        >
          <p>{t("All {n} lines will be removed.", { n: cart?.lines.length ?? 0 })}</p>
        </Dialog>
      )}
      {shown === "void" && (
        <Dialog
          title={t("Void this sale?")}
          confirmLabel={t("Void sale")}
          danger
          input={{ label: t("Reason") }}
          onCancel={() => setModal(null)}
          onConfirm={(reason) => {
            setModal(null);
            void c.void(reason.trim());
          }}
        >
          <p>{t("The cart is closed and kept for the audit trail. A new, empty cart opens.")}</p>
        </Dialog>
      )}
      {shown === "park" && (
        <Dialog
          title={t("Park this cart?")}
          confirmLabel={t("Park")}
          input={{ label: t("Label, to find it again (optional)"), optional: true, maxLength: 40 }}
          onCancel={() => setModal(null)}
          onConfirm={(label) => {
            setModal(null);
            void c.park(label.trim() || null).then((ok) => ok && countParked());
          }}
        >
          <p>
            {cart?.lines.length === 1
              ? t("The 1 line is set aside and an empty cart opens for the next customer.")
              : t("The {n} lines are set aside and an empty cart opens for the next customer.", { n: cart?.lines.length ?? 0 })}{" "}
            {t("Resume it from Parked (⇧F5); prices are checked again then.")}
          </p>
        </Dialog>
      )}
      {shown === "start" && onSettings && (
        <GettingStarted
          onProducts={() => {
            onGettingStartedSeen?.();
            onProducts();
          }}
          onStaff={() => {
            onGettingStartedSeen?.();
            onSettings("Staff");
          }}
          onBusiness={() => {
            onGettingStartedSeen?.();
            onSettings("Business");
          }}
          onGoLive={() => {
            onGettingStartedSeen?.();
            setModal("goLive");
          }}
          onClose={() => {
            onGettingStartedSeen?.();
            setModal(null);
          }}
        />
      )}
      {shown === "quick" && (
        <QuickSelectDialog exponent={exponent} onPick={(variantId) => c.addQuick(variantId)} onClose={() => setModal(null)} />
      )}
      {shown === "recovered" && recovered && (
        <RecoveryDialog
          recovered={recovered}
          exponent={exponent}
          onResume={() => {
            setModal(null);
            void c.resumeRecovered();
          }}
          onDiscard={() => {
            setModal(null);
            void c.discardRecovered(recovered.revision);
          }}
        />
      )}
      {shown === "parked" && (
        <ParkedDialog
          exponent={exponent}
          canResume={!hasLines}
          paused={c.approval !== null}
          onCount={setParkedCount}
          onApproval={c.setApproval}
          onClose={() => setModal(null)}
          onResume={(cartId) => {
            setModal(null);
            void c.resume(cartId).then(() => countParked());
          }}
        />
      )}
      {shown === "orderDiscount" && (
        <Dialog
          title={t("Order discount")}
          confirmLabel={t("Apply")}
          input={{ label: t("Discount % (0 removes it)"), inputMode: "decimal" }}
          onCancel={() => setModal(null)}
          onConfirm={(value) => {
            setModal(null);
            const ppm = percentToPpm(value.replace("%", ""));
            if (ppm === null) return c.setNotice({ kind: "info", text: t("Enter the discount as a percentage, e.g. 10.") });
            if (ppm === 0) return void c.setOrderDiscount(null);
            withReason(ppm, t("Reason for {pct}% off the order", { pct: ppmToPercent(ppm) }), (reason) =>
              void c.setOrderDiscount({ kind: "percent", ratePpm: ppm, reason }),
            );
          }}
        />
      )}
      {shown === "price" && selectedLine && (
        <Dialog
          title={t("New price for {name}", { name: selectedLine.name })}
          confirmLabel={t("Change price")}
          input={{
            label: t("Unit price ({currency})", { currency: cart?.currencyCode ?? "" }),
            initial: money(selectedLine.unitPriceMinor).replace(/,/g, ""),
            inputMode: "decimal",
          }}
          onCancel={() => setModal(null)}
          onConfirm={(value) => {
            setModal(null);
            const minor = scaleDecimal(value, exponent);
            if (minor === null) return c.setNotice({ kind: "info", text: t("Enter a price like 1.20.") });
            const lineId = selectedLine.lineId;
            setModal({
              reason: t("Reason for changing the price of {name}", { name: selectedLine.name }),
              then: (r) => void c.overridePrice(lineId, minor, r.trim()),
            });
          }}
        />
      )}
      {shown !== null && typeof shown === "object" && (
        <Dialog
          title={shown.reason}
          confirmLabel={t("Continue")}
          input={{ label: t("Reason (kept in the audit log)") }}
          onCancel={() => setModal(null)}
          onConfirm={(reason) => {
            setModal(null);
            shown.then(reason);
          }}
        />
      )}
      {active && c.approval && (
        <ApprovalDialog
          scope={c.approval.scope}
          requesterId={session.userId}
          onCancel={() => c.setApproval(null)}
          onApproved={(token) => {
            const pending = c.approval;
            c.setApproval(null);
            if (pending) void pending.retry(token);
          }}
        />
      )}
    </div>
  );
}

function TaxRow({ name, amount, included, t }: { name: string; amount: string; included: boolean; t: T }) {
  return (
    <>
      <dt>{included ? t("{tax} (included)", { tax: name }) : name}</dt>
      <dd>{amount}</dd>
    </>
  );
}

function Notice({
  c,
  onSearch,
  onCreate,
  onAskManager,
}: {
  c: ReturnType<typeof useCart>;
  onSearch: (text: string) => void;
  /** Only for roles that may create products from a scan (BarcodeCreateFromScan). */
  onCreate: ((barcode: string) => void) | null;
  /** Everyone else: a manager approves on the spot, then the editor opens (E07-T04). */
  onAskManager: ((barcode: string) => void) | null;
}) {
  const n = c.notice;
  const t = useT();
  if (!n) return <div className="notice-slot" aria-live="assertive" />;
  return (
    <div className="notice-slot" aria-live="assertive">
      <div role="alert" className={n.kind === "info" ? "toast" : "toast warn"}>
        {n.kind === "notFound" && (
          <>
            <p>
              <PackageSearch />
              <span>
                {t("No product with code")} <strong className="mono">{n.code}</strong>.
              </span>
            </p>
            {n.hint && <p className="small">{scanHintText(n.hint, t)}</p>}
            <button
              type="button"
              className="secondary"
              onClick={() => {
                c.setNotice(null);
                onSearch(n.code);
              }}
            >
              {t("Search for it")}
            </button>
            {onCreate ? (
              <button type="button" className="secondary" onClick={() => onCreate(n.code)}>
                {t("Create product")}
              </button>
            ) : onAskManager ? (
              <button type="button" className="secondary" onClick={() => onAskManager(n.code)}>
                {t("Ask a manager")}
              </button>
            ) : null}
          </>
        )}
        {n.kind === "error" && (
          <p>
            <CircleExclamationPoint />
            <span>
              {describeError(n.error)} <span className="mono small">{t("ref")} {n.error.correlationId.slice(-8)}</span>
            </span>
          </p>
        )}
        {n.kind === "info" && (
          <p>
            <Info />
            <span>{n.text}</span>
          </p>
        )}
      </div>
    </div>
  );
}
