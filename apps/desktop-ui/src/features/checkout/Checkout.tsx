import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type Session, type Workspace } from "../../ipc";
import { percentToPpm, ppmToPercent, scaleDecimal, stepMilli } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { ApprovalDialog } from "../auth/ApprovalDialog";
import { useScanCapture } from "../scan/useScanCapture";
import { describeError } from "../shell/errors";
import { CartTable, type Editing } from "./CartTable";
import { Dialog } from "./Dialog";
import { PaymentPanel } from "./PaymentPanel";
import { SearchPanel, type SearchHandle } from "./SearchPanel";
import { StatusBar } from "./StatusBar";
import { useCart } from "./useCart";

type Modal =
  | "pay"
  | "clear"
  | "void"
  | "orderDiscount"
  | "price"
  | { reason: string; then: (reason: string) => void }
  | null;

/** The key map (E09-T05). Shown in the action bar; documented in docs/keyboard.md. */
export const KEYS = {
  search: "F2",
  quantity: "F3",
  discount: "F4",
  remove: "Delete",
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
  workspace,
  session,
  onSignedOut,
  canEditCatalog,
  onProducts,
  onCreateProduct,
}: {
  workspace: Workspace;
  session: Session;
  onSignedOut: () => void;
  canEditCatalog: boolean;
  onProducts: () => void;
  /** E07-T04: open the editor with an unknown barcode pre-filled. */
  onCreateProduct: (barcode: string) => void;
}) {
  const c = useCart(onSignedOut);
  const [editing, setEditing] = useState<Editing>(null);
  const [modal, setModal] = useState<Modal>(null);
  const [code, setCode] = useState("");
  const [scannerReady, setScannerReady] = useState(true);
  const search = useRef<SearchHandle | null>(null);
  const exponent = c.cart?.currencyExponent ?? 2;
  const money = (minor: string) => formatMoney(minor, { exponent });
  const selectedLine = c.line(c.selected);

  // Wedge scans: in scanner-ready mode, or typed into the code field.
  // Paused while a dialog owns the keyboard (approval PIN pad, reason, confirmations).
  useScanCapture(
    (scan) => {
      if (scan.inScanField) setCode("");
      void c.scan(scan.raw, scan.scanEventId, "scanner");
    },
    {},
    modal === null && c.approval === null,
  );

  useEffect(() => {
    const update = () => setScannerReady(!inTextField());
    // On focusout the next element is not focused yet: check after the event.
    const later = () => setTimeout(update, 0);
    document.addEventListener("focusin", update);
    document.addEventListener("focusout", later);
    return () => {
      document.removeEventListener("focusin", update);
      document.removeEventListener("focusout", later);
    };
  }, []);

  const lock = useCallback(async () => {
    await commands.authLogout();
    onSignedOut();
  }, [onSignedOut]);

  const commit = (lineId: string, field: "qty" | "discount", value: string) => {
    setEditing(null);
    if (field === "qty") {
      const milli = scaleDecimal(value, 3);
      if (milli === null || milli === "0") {
        c.setNotice({ kind: "info", text: "Enter a quantity above zero. To take an item off, press Delete." });
        return;
      }
      void c.setQuantity(lineId, milli);
    } else {
      if (!value.trim()) return void c.setLineDiscount(lineId, null);
      const ppm = percentToPpm(value.replace("%", ""));
      if (ppm === null) {
        c.setNotice({ kind: "info", text: "Enter the discount as a percentage, e.g. 10." });
        return;
      }
      withReason(ppm, `Reason for ${ppmToPercent(ppm)}% off`, (reason) =>
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
      if (modal || c.approval || e.defaultPrevented) return;
      const typing = inTextField();
      const sel = c.selected;
      switch (e.key) {
        case "F2":
          e.preventDefault();
          search.current?.focus();
          return;
        case "F3":
          e.preventDefault();
          if (sel) setEditing({ lineId: sel, field: "qty" });
          return;
        case "F4":
          e.preventDefault();
          if (sel) setEditing({ lineId: sel, field: "discount" });
          return;
        case "F6":
          e.preventDefault();
          onProducts();
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
        if (line && line.unit === "ea") void c.setQuantity(sel, stepMilli(line.quantityMilli, e.code === "NumpadAdd" ? 1 : -1));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [c, modal, lock, onProducts]);

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
      {workspace.mode === "sandbox" && (
        <div className="sandbox-banner" role="note">
          SANDBOX: practice mode. Sales here are excluded from reports.
        </div>
      )}
      <section className="entry" aria-label="Add items">
        <form onSubmit={submitCode} className="code-form">
          <input
            data-scan-field
            aria-label="Scan or type a code"
            placeholder="Scan or type a code, then Enter"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            autoComplete="off"
          />
        </form>
        <SearchPanel ref={search} exponent={exponent} onPick={(hit) => void c.addVariant(hit.variantId)} />
      </section>

      <section className="cart-area" aria-label="Cart lines">
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

      <aside className="side" aria-label="Totals and actions">
        {cart && (
          <dl className="totals">
            <dt>Subtotal</dt>
            <dd>{money(cart.subtotalMinor)}</dd>
            {cart.lineDiscountTotalMinor !== "0" && (
              <>
                <dt>Line discounts</dt>
                <dd>-{money(cart.lineDiscountTotalMinor)}</dd>
              </>
            )}
            {cart.orderDiscountTotalMinor !== "0" && (
              <>
                <dt>Order discount</dt>
                <dd>-{money(cart.orderDiscountTotalMinor)}</dd>
              </>
            )}
            {cart.taxBreakdown.map((t) => (
              <TaxRow key={t.name} name={t.name} amount={money(t.taxMinor)} included={cart.taxMode === "inclusive"} />
            ))}
          </dl>
        )}
        <div className="total-due" aria-live="polite">
          <span className="total-label">Total due {cart?.currencyCode}</span>
          <output className="total-amount" data-testid="total">
            {cart ? money(cart.totalMinor) : ""}
          </output>
        </div>
        <div className="actions-grid">
          <button type="button" disabled={!selectedLine} onClick={() => c.selected && setEditing({ lineId: c.selected, field: "qty" })}>
            Quantity <kbd>F3</kbd>
          </button>
          <button type="button" disabled={!selectedLine} onClick={() => c.selected && setEditing({ lineId: c.selected, field: "discount" })}>
            Discount <kbd>F4</kbd>
          </button>
          <button type="button" disabled={!selectedLine} onClick={() => c.selected && void c.removeLine(c.selected)}>
            Remove <kbd>Del</kbd>
          </button>
          <button type="button" disabled={!hasLines} onClick={() => setModal("orderDiscount")}>
            Order discount
          </button>
          <button type="button" className="secondary" disabled={!selectedLine} onClick={() => setModal("price")}>
            Change price
          </button>
          <button type="button" className="secondary" disabled={!hasLines} onClick={() => setModal("clear")}>
            Clear cart
          </button>
          <button type="button" className="secondary" disabled={!hasLines} onClick={() => setModal("void")}>
            Void sale
          </button>
          <button type="button" className="secondary" onClick={onProducts}>
            Products <kbd>F6</kbd>
          </button>
          <button type="button" className="secondary" onClick={() => void lock()}>
            Lock <kbd>F12</kbd>
          </button>
          <button type="button" className="pay" disabled={!hasLines} onClick={() => setModal("pay")}>
            Pay <kbd>F9</kbd>
          </button>
        </div>
        <Notice c={c} onSearch={(text) => search.current?.focus(text)} onCreate={canEditCatalog ? onCreateProduct : null} />
      </aside>

      <StatusBar workspace={workspace} session={session} scannerReady={scannerReady} />

      {modal === "pay" && cart && (
        <PaymentPanel
          cart={cart}
          onClose={() => setModal(null)}
          onDone={() => {
            setModal(null);
            void c.refresh();
          }}
        />
      )}
      {modal === "clear" && (
        <Dialog
          title="Clear the cart?"
          confirmLabel="Clear cart"
          danger
          onCancel={() => setModal(null)}
          onConfirm={() => {
            setModal(null);
            void c.clear();
          }}
        >
          <p>All {cart?.lines.length} lines will be removed.</p>
        </Dialog>
      )}
      {modal === "void" && (
        <Dialog
          title="Void this sale?"
          confirmLabel="Void sale"
          danger
          input={{ label: "Reason" }}
          onCancel={() => setModal(null)}
          onConfirm={(reason) => {
            setModal(null);
            void c.void(reason.trim());
          }}
        >
          <p>The cart is closed and kept for the audit trail. A new, empty cart opens.</p>
        </Dialog>
      )}
      {modal === "orderDiscount" && (
        <Dialog
          title="Order discount"
          confirmLabel="Apply"
          input={{ label: "Discount % (0 removes it)", inputMode: "decimal" }}
          onCancel={() => setModal(null)}
          onConfirm={(value) => {
            setModal(null);
            const ppm = percentToPpm(value.replace("%", ""));
            if (ppm === null) return c.setNotice({ kind: "info", text: "Enter the discount as a percentage, e.g. 10." });
            if (ppm === 0) return void c.setOrderDiscount(null);
            withReason(ppm, `Reason for ${ppmToPercent(ppm)}% off the order`, (reason) =>
              void c.setOrderDiscount({ kind: "percent", ratePpm: ppm, reason }),
            );
          }}
        />
      )}
      {modal === "price" && selectedLine && (
        <Dialog
          title={`New price for ${selectedLine.name}`}
          confirmLabel="Change price"
          input={{ label: `Unit price (${cart?.currencyCode ?? ""})`, initial: money(selectedLine.unitPriceMinor).replace(/,/g, ""), inputMode: "decimal" }}
          onCancel={() => setModal(null)}
          onConfirm={(value) => {
            setModal(null);
            const minor = scaleDecimal(value, exponent);
            if (minor === null) return c.setNotice({ kind: "info", text: "Enter a price like 1.20." });
            const lineId = selectedLine.lineId;
            setModal({ reason: `Reason for changing the price of ${selectedLine.name}`, then: (r) => void c.overridePrice(lineId, minor, r.trim()) });
          }}
        />
      )}
      {modal !== null && typeof modal === "object" && (
        <Dialog
          title={modal.reason}
          confirmLabel="Continue"
          input={{ label: "Reason (kept in the audit log)" }}
          onCancel={() => setModal(null)}
          onConfirm={(reason) => {
            setModal(null);
            modal.then(reason);
          }}
        />
      )}
      {c.approval && (
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

function TaxRow({ name, amount, included }: { name: string; amount: string; included: boolean }) {
  return (
    <>
      <dt>
        {name}
        {included ? " (included)" : ""}
      </dt>
      <dd>{amount}</dd>
    </>
  );
}

function Notice({
  c,
  onSearch,
  onCreate,
}: {
  c: ReturnType<typeof useCart>;
  onSearch: (text: string) => void;
  /** Only for roles that may create products from a scan (BarcodeCreateFromScan). */
  onCreate: ((barcode: string) => void) | null;
}) {
  const n = c.notice;
  if (!n) return <div className="notice-slot" aria-live="assertive" />;
  return (
    <div className="notice-slot" aria-live="assertive">
      <div role="alert" className={n.kind === "info" ? "toast" : "toast warn"}>
        {n.kind === "notFound" && (
          <>
            <p>
              No product with code <strong className="mono">{n.code}</strong>.
            </p>
            <button
              type="button"
              className="secondary"
              onClick={() => {
                c.setNotice(null);
                onSearch(n.code);
              }}
            >
              Search for it
            </button>
            {onCreate ? (
              <button type="button" className="secondary" onClick={() => onCreate(n.code)}>
                Create product
              </button>
            ) : (
              <p className="small">A manager can add it as a new product.</p>
            )}
          </>
        )}
        {n.kind === "error" && (
          <p>
            {describeError(n.error)} <span className="mono small">ref {n.error.correlationId.slice(-8)}</span>
          </p>
        )}
        {n.kind === "info" && <p>{n.text}</p>}
      </div>
    </div>
  );
}
