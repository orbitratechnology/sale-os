import { useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type CartView, type IpcError } from "../../ipc";
import type { CompletedSaleDto, TenderKind } from "../../ipc/bindings";
import { scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { describeError } from "../shell/errors";

interface Tender {
  kind: TenderKind;
  amountMinor: string;
  reference: string | null;
}

const METHODS: { kind: TenderKind; label: string }[] = [
  { kind: "card_external", label: "Card (separate terminal)" },
  { kind: "bank_transfer_external", label: "Bank transfer" },
  { kind: "qr_external", label: "QR payment" },
  { kind: "other_external", label: "Other" },
];
/** Notes offered as quick cash amounts, in whole currency units. */
const NOTES = [5n, 10n, 20n, 50n];

const label = (k: TenderKind) => (k === "cash" ? "Cash" : (METHODS.find((m) => m.kind === k)?.label ?? k));

/**
 * Taking payment (E10-T03). Tenders are added in order; the remaining due updates after
 * each; any tender can be removed before completing. External tenders are recorded by the
 * operator: they are never shown as approved by a provider (PAY-003). Rust settles and
 * commits; the amounts here are a preview of the same arithmetic (exact integers).
 */
export function PaymentPanel({
  cart,
  onDone,
  onClose,
}: {
  cart: CartView;
  /** After the cashier closes the "completed" view. */
  onDone: () => void;
  onClose: () => void;
}) {
  const [tenders, setTenders] = useState<Tender[]>([]);
  const [cash, setCash] = useState("");
  const [method, setMethod] = useState<TenderKind | null>(null);
  const [external, setExternal] = useState({ amount: "", reference: "" });
  const [error, setError] = useState<IpcError | string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<CompletedSaleDto | null>(null);
  const [showReceipt, setShowReceipt] = useState(false);
  const cashInput = useRef<HTMLInputElement | null>(null);
  const exp = cart.currencyExponent;
  const money = (minor: bigint | string) => formatMoney(minor.toString(), { exponent: exp });

  const total = BigInt(cart.totalMinor);
  const paid = tenders.reduce((a, t) => a + BigInt(t.amountMinor), 0n);
  const remaining = total - paid > 0n ? total - paid : 0n;
  const change = paid > total ? paid - total : 0n;

  useEffect(() => {
    cashInput.current?.focus();
  }, [tenders.length]);

  // Esc closes (or finishes after completion); the panel owns the keyboard while open.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (done) onDone();
        else if (method) setMethod(null);
        else onClose();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [done, method, onClose, onDone]);

  async function complete(list: Tender[]) {
    setBusy(true);
    setError(null);
    const res = await commands.checkoutComplete(cart.cartId, cart.revision, list);
    setBusy(false);
    if (res.status === "ok") setDone(res.data);
    else setError(res.error);
  }

  /** Cash: an empty field means "exact"; Enter then completes (the cash-exact flow). */
  function addCash(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const amount = cash.trim() ? scaleDecimal(cash, exp) : remaining.toString();
    if (amount === null || amount === "0") return setError("Enter the cash received, e.g. 20.00.");
    if (remaining === 0n) return setError("The sale is already paid. Remove a tender to change it.");
    const next = [...tenders, { kind: "cash" as const, amountMinor: amount, reference: null }];
    setCash("");
    setError(null);
    setTenders(next);
    // Cash covering the rest completes at once: the common case is one keystroke.
    if (BigInt(amount) >= remaining) void complete(next);
  }

  function addExternal(e: FormEvent) {
    e.preventDefault();
    if (!method) return;
    const amount = external.amount.trim() ? scaleDecimal(external.amount, exp) : remaining.toString();
    if (amount === null || amount === "0") return setError("Enter the amount charged.");
    if (BigInt(amount) > remaining) return setError("A card or transfer cannot be more than the amount due; the rest can be cash.");
    setTenders([...tenders, { kind: method, amountMinor: amount, reference: external.reference.trim() || null }]);
    setMethod(null);
    setExternal({ amount: "", reference: "" });
    setError(null);
  }

  if (done) {
    return (
      <div className="backdrop">
        <div role="dialog" aria-modal="true" aria-label="Sale completed" className="dialog card pay-done">
          <p className="small">Change due</p>
          <output className="change-amount" data-testid="change">
            {money(done.changeMinor)}
          </output>
          <p>
            Receipt <strong className="mono">{done.receiptNumber}</strong> · total {money(done.totalMinor)}
          </p>
          <p className="small" data-testid="saved">
            Saved on this device · {done.syncPending ? "Not linked: will sync when the cloud is linked" : "Synced"}
            {done.mode === "sandbox" ? " · SANDBOX" : ""}
          </p>
          {done.negativeStockItems > 0 && (
            <p className="small">
              {done.negativeStockItems === 1
                ? "1 item went below zero stock and was flagged for a stock check."
                : `${done.negativeStockItems} items went below zero stock and were flagged for a stock check.`}
            </p>
          )}
          <PrintControls source={{ kind: "sale", id: done.saleId }} autoPrint={done.autoPrint} />
          {showReceipt && <ReceiptPreview source={{ kind: "sale", id: done.saleId }} />}
          <div className="actions">
            <button type="button" className="secondary" onClick={() => setShowReceipt((s) => !s)}>
              {showReceipt ? "Hide receipt" : "Show receipt"}
            </button>
            <button type="button" autoFocus onClick={onDone}>
              New sale <kbd>Enter</kbd>
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="backdrop">
      <div role="dialog" aria-modal="true" aria-label="Payment" className="dialog card pay">
        <div className="pay-head">
          <div>
            <p className="small">Total</p>
            <output className="pay-total">{money(total)}</output>
          </div>
          <div>
            <p className="small">Still due</p>
            <output className="pay-remaining" data-testid="remaining">
              {money(remaining)}
            </output>
          </div>
          {change > 0n && (
            <div>
              <p className="small">Change</p>
              <output className="pay-remaining">{money(change)}</output>
            </div>
          )}
        </div>

        {tenders.length > 0 && (
          <ul className="tender-list" aria-label="Tenders">
            {tenders.map((t, i) => (
              <li key={i}>
                <span>
                  {label(t.kind)}
                  {t.kind !== "cash" && <span className="tag">Externally confirmed by operator</span>}
                  {t.reference && <span className="small mono"> {t.reference}</span>}
                </span>
                <span className="num">{money(t.amountMinor)}</span>
                <button type="button" className="secondary" aria-label={`Remove ${label(t.kind)} ${money(t.amountMinor)}`} disabled={busy} onClick={() => setTenders(tenders.filter((_, j) => j !== i))}>
                  Remove
                </button>
              </li>
            ))}
          </ul>
        )}

        {!method ? (
          <>
            <form onSubmit={addCash} className="cash-form">
              <label>
                Cash received ({cart.currencyCode}) · empty + Enter = exact
                <input ref={cashInput} inputMode="decimal" aria-label="Cash received" value={cash} onChange={(e) => setCash(e.target.value)} disabled={busy || remaining === 0n} />
              </label>
              <div className="quick-cash">
                <button type="button" disabled={busy || remaining === 0n} onClick={() => void complete([...tenders, { kind: "cash", amountMinor: remaining.toString(), reference: null }])}>
                  Exact {money(remaining)}
                </button>
                {quickAmounts(remaining, exp).map((n) => (
                  <button key={n.toString()} type="button" className="secondary" disabled={busy} onClick={() => void complete([...tenders, { kind: "cash", amountMinor: n.toString(), reference: null }])}>
                    {money(n)}
                  </button>
                ))}
              </div>
            </form>
            <div className="methods" role="group" aria-label="Other payment methods">
              {METHODS.map((m) => (
                <button key={m.kind} type="button" className="secondary" disabled={busy || remaining === 0n} onClick={() => setMethod(m.kind)}>
                  {m.label}
                </button>
              ))}
            </div>
          </>
        ) : (
          <form onSubmit={addExternal} className="external-form" aria-label={label(method)}>
            <p className="small">Take the payment on the {method === "card_external" ? "card terminal" : "customer's device"} first, then record it here.</p>
            <label>
              Amount ({cart.currencyCode}) · empty = the rest, {money(remaining)}
              <input autoFocus inputMode="decimal" value={external.amount} onChange={(e) => setExternal({ ...external, amount: e.target.value })} />
            </label>
            <label>
              Reference (optional: approval code or transfer ID, never a card number)
              <input value={external.reference} maxLength={64} autoComplete="off" onChange={(e) => setExternal({ ...external, reference: e.target.value })} />
            </label>
            <div className="actions">
              <button type="button" className="secondary" onClick={() => setMethod(null)}>
                Back
              </button>
              <button type="submit">Record {label(method)}</button>
            </div>
          </form>
        )}

        {error && (
          <p role="alert" className="field-error">
            {typeof error === "string" ? error : describeError(error)}
          </p>
        )}
        <div className="actions">
          <button type="button" className="secondary" onClick={onClose} disabled={busy}>
            Back to cart <kbd>Esc</kbd>
          </button>
          <button type="button" disabled={busy || remaining > 0n || tenders.length === 0} onClick={() => void complete(tenders)}>
            {busy ? "Saving…" : "Complete sale"}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * Round-up amounts a customer is likely to hand over: the next whole unit, then notes.
 * Exact integers in minor units, scaled by the currency's exponent.
 */
export function quickAmounts(due: bigint, exponent: number): bigint[] {
  if (due <= 0n) return [];
  const unit = 10n ** BigInt(exponent);
  const out = new Set<bigint>();
  const whole = ((due + unit - 1n) / unit) * unit;
  if (whole !== due) out.add(whole);
  for (const note of NOTES) {
    const n = note * unit;
    const up = ((due + n - 1n) / n) * n;
    if (up !== due) out.add(up);
  }
  return [...out].sort((a, b) => (a < b ? -1 : 1)).slice(0, 4);
}
