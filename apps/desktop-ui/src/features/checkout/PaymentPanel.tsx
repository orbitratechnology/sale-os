import { Modal } from "../../ui/Modal";
import type { LucideIcon } from "lucide-react";
import { Banknote, CircleCheck, CreditCard, Landmark, QrCode, Wallet } from "../../ui/icons";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type ApprovalScope, type CartView, type IpcError } from "../../ipc";
import type { CompletedReturnDto, CompletedSaleDto, PaymentAttemptDto, TenderKind } from "../../ipc/bindings";
import { ApprovalDialog } from "../auth/ApprovalDialog";
import type { ExchangeCredit } from "../returns/exchange";
import { scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { describeError } from "../shell/errors";
import { SaleSyncMark } from "../sync/SaleSyncMark";
import { TerminalPayment } from "./TerminalPayment";
import { useT } from "../../i18n";

interface Tender {
  kind: TenderKind;
  amountMinor: string;
  reference: string | null;
  /** Approved on the integrated terminal (E22-T02): completed through `terminal`. */
  attemptId?: string;
}

const METHODS: { kind: TenderKind; label: string; icon: LucideIcon }[] = [
  { kind: "card_external", label: "Card (separate terminal)", icon: CreditCard },
  { kind: "bank_transfer_external", label: "Bank transfer", icon: Landmark },
  { kind: "qr_external", label: "QR payment", icon: QrCode },
  { kind: "other_external", label: "Other", icon: Wallet },
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
  exchange = null,
  requesterId,
  onDone,
  onClose,
}: {
  cart: CartView;
  /**
   * Settling an exchange: the returned value pays first; the customer pays only the rest, or
   * gets the difference back as change. The return and the sale are recorded together.
   */
  exchange?: ExchangeCredit | null;
  /** The signed-in user, for a manager's approval (a cashier's exchange, or settling a card payment). */
  requesterId: string;
  /** After the cashier closes the "completed" view. */
  onDone: () => void;
  onClose: () => void;
}) {
  const [tenders, setTenders] = useState<Tender[]>([]);
  const [cash, setCash] = useState("");
  const [method, setMethod] = useState<TenderKind | "terminal" | null>(null);
  /** Whether a card terminal is set up, and an attempt to resume (E22-T02). */
  const [terminal, setTerminal] = useState(false);
  const [resume, setResume] = useState<PaymentAttemptDto | null>(null);
  const [external, setExternal] = useState({ amount: "", reference: "" });
  const [error, setError] = useState<IpcError | string | null>(null);
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<CompletedSaleDto | null>(null);
  const [returned, setReturned] = useState<CompletedReturnDto | null>(null);
  const [approval, setApproval] = useState<{ scope: ApprovalScope; tenders: Tender[] } | null>(null);
  const [showReceipt, setShowReceipt] = useState(false);
  const cashInput = useRef<HTMLInputElement | null>(null);
  const exp = cart.currencyExponent;
  const money = (minor: bigint | string) => formatMoney(minor.toString(), { exponent: exp });

  const credit = exchange ? BigInt(exchange.creditMinor) : 0n;
  // What the customer pays: the new items less what came back (never below zero; the
  // difference the other way is given back as change).
  const total = BigInt(cart.totalMinor) > credit ? BigInt(cart.totalMinor) - credit : 0n;
  const giveBack = credit > BigInt(cart.totalMinor) ? credit - BigInt(cart.totalMinor) : 0n;
  const paid = tenders.reduce((a, t) => a + BigInt(t.amountMinor), 0n);
  const remaining = total - paid > 0n ? total - paid : 0n;
  const change = paid > total ? paid - total : 0n;

  useEffect(() => {
    cashInput.current?.focus();
  }, [tenders.length]);

  // A terminal is set up when its attempts can be listed. Approved, unused attempts come
  // back as tenders; one still pending or uncertain reopens the terminal view.
  useEffect(() => {
    void commands.paymentAttempts(cart.cartId).then((res) => {
      if (res.status !== "ok") return;
      setTerminal(true);
      const usable = res.data.filter((a) => a.usable);
      if (usable.length > 0)
        setTenders((ts) => [
          ...ts.filter((t) => !t.attemptId),
          ...usable.map((a) => ({ kind: "card_external" as const, amountMinor: a.amountMinor, reference: a.providerRef, attemptId: a.attemptId })),
        ]);
      const open = res.data.find((a) => a.status === "pending" || a.status === "reconciliation_required");
      if (open) {
        setResume(open);
        setMethod("terminal");
      }
    });
  }, [cart.cartId]);

  // Esc closes (or finishes after completion); the panel owns the keyboard while open.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (approval) return; // the approval dialog owns Esc while open
        e.preventDefault();
        e.stopPropagation();
        if (done) onDone();
        else if (method) setMethod(null);
        else onClose();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [approval, done, method, onClose, onDone]);

  async function complete(list: Tender[], approvalTokenId: string | null = null) {
    setBusy(true);
    setError(null);
    const terminalTenders = list.filter((t) => t.attemptId).map((t) => ({ attemptId: t.attemptId ?? "", amountMinor: t.amountMinor }));
    const others = list.filter((t) => !t.attemptId).map(({ kind, amountMinor, reference }) => ({ kind, amountMinor, reference }));
    if (exchange) {
      // An exchange settles with tenders recorded at the till; the terminal flow is not offered for it.
      if (terminalTenders.length > 0) {
        setBusy(false);
        return setError(t("An exchange cannot be settled on the card terminal yet: record the card payment as Card (separate terminal)."));
      }
      const x = exchange;
      const res = await commands.exchangeComplete(x.saleId, x.lines, x.reason, x.note, [], approvalTokenId, cart.cartId, cart.revision, others);
      setBusy(false);
      if (res.status === "ok") {
        setReturned(res.data.return);
        return setDone(res.data.sale);
      }
      if (res.error.code === "APPROVAL_REQUIRED" && res.error.details?.scope) return setApproval({ scope: res.error.details.scope, tenders: list });
      return setError(res.error);
    }
    const res = await commands.checkoutComplete(cart.cartId, cart.revision, others, terminalTenders.length > 0 ? terminalTenders : null);
    setBusy(false);
    if (res.status === "ok") setDone(res.data);
    else setError(res.error);
  }

  const approvalDialog = approval && (
    <ApprovalDialog
      scope={approval.scope}
      requesterId={requesterId}
      onCancel={() => setApproval(null)}
      onApproved={(token) => {
        const list = approval.tenders;
        setApproval(null);
        void complete(list, token);
      }}
    />
  );

  /** Cash: an empty field means "exact"; Enter then completes (the cash-exact flow). */
  function addCash(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const amount = cash.trim() ? scaleDecimal(cash, exp) : remaining.toString();
    if (amount === null || amount === "0") return setError(t("Enter the cash received, e.g. 20.00."));
    if (remaining === 0n) return setError(t("The sale is already paid. Remove a tender to change it."));
    const next = [...tenders, { kind: "cash" as const, amountMinor: amount, reference: null }];
    setCash("");
    setError(null);
    setTenders(next);
    // Cash covering the rest completes at once: the common case is one keystroke.
    if (BigInt(amount) >= remaining) void complete(next);
  }

  function addExternal(e: FormEvent) {
    e.preventDefault();
    if (!method || method === "terminal") return;
    const amount = external.amount.trim() ? scaleDecimal(external.amount, exp) : remaining.toString();
    if (amount === null || amount === "0") return setError(t("Enter the amount charged."));
    if (BigInt(amount) > remaining) return setError(t("A card or transfer cannot be more than the amount due; the rest can be cash."));
    setTenders([...tenders, { kind: method, amountMinor: amount, reference: external.reference.trim() || null }]);
    setMethod(null);
    setExternal({ amount: "", reference: "" });
    setError(null);
  }

  if (done) {
    return (
      // Its own key: a new dialog, so focus starts on "New sale" (Enter), not on the page.
      <Modal key="done" label={returned ? t("Exchange completed") : t("Sale completed")} className="pay-done">
          <span className="done-icon">
            <CircleCheck />
          </span>
          {returned && (
            <p data-testid="exchange-return">
              {t("Return")} <strong className="mono">{returned.returnNumber}</strong> ·{" "}
              {t("{amount} back, spent on the new items", { amount: money(returned.totalMinor) })}
            </p>
          )}
          <p className="small">{returned ? t("Give back") : t("Change due")}</p>
          <output className="change-amount" data-testid="change">
            {money(done.changeMinor)}
          </output>
          <p>
            {t("Receipt")} <strong className="mono">{done.receiptNumber}</strong> · {t("total {amount}", { amount: money(done.totalMinor) })}
          </p>
          <p className="small" data-testid="saved" aria-live="polite">
            <SaleSyncMark saleId={done.saleId} />
            {done.mode === "sandbox" ? " · SANDBOX" : ""}
          </p>
          {done.drawer === "opened" && <p className="small">{t("Cash drawer opened.")}</p>}
          {done.drawer.startsWith("failed") && (
            <p role="alert" className="small">
              {t("The cash drawer did not open ({why}). Open it with No sale or the key.", { why: done.drawer.slice(8) })}
            </p>
          )}
          {done.negativeStockItems > 0 && (
            <p className="small">
              {done.negativeStockItems === 1
                ? t("1 item went below zero stock and was flagged for a stock check.")
                : t("{n} items went below zero stock and were flagged for a stock check.", { n: done.negativeStockItems })}
            </p>
          )}
          <PrintControls source={{ kind: "sale", id: done.saleId }} autoPrint={done.autoPrint} />
          {returned && <PrintControls source={{ kind: "return", id: returned.returnId }} autoPrint={false} />}
          {showReceipt && <ReceiptPreview source={{ kind: "sale", id: done.saleId }} />}
          <div className="actions">
            <button type="button" className="secondary" onClick={() => setShowReceipt((s) => !s)}>
              {showReceipt ? t("Hide receipt") : t("Show receipt")}
            </button>
            <button type="button" data-autofocus onClick={onDone}>
              {t("New sale")} <kbd>Enter</kbd>
            </button>
          </div>
      </Modal>
    );
  }

  return (
    <Modal key="pay" label={exchange ? t("Settle the exchange") : t("Payment")} className="pay">
        {exchange && (
          <dl className="totals" aria-label={t("Exchange")}>
            <dt>{t("New items")}</dt>
            <dd>{money(cart.totalMinor)}</dd>
            <dt>{t("Returned ({receipt})", { receipt: exchange.receiptNumber })}</dt>
            <dd data-testid="exchange-credit">-{money(credit)}</dd>
          </dl>
        )}
        <div className="pay-head">
          <div>
            <p className="small">{exchange ? t("To pay") : t("Total")}</p>
            <output className="pay-total">{money(total)}</output>
          </div>
          <div>
            <p className="small">{t("Still due")}</p>
            <output className="pay-remaining" data-testid="remaining">
              {money(remaining)}
            </output>
          </div>
          {change > 0n && (
            <div>
              <p className="small">{t("Change")}</p>
              <output className="pay-remaining">{money(change)}</output>
            </div>
          )}
          {giveBack > 0n && (
            <div>
              <p className="small">{t("Give back (cash)")}</p>
              <output className="pay-remaining" data-testid="give-back">
                {money(giveBack)}
              </output>
            </div>
          )}
        </div>

        {tenders.length > 0 && (
          <ul className="tender-list" aria-label={t("Tenders")}>
            {tenders.map((tender, i) => (
              <li key={i}>
                <span>
                  {t(label(tender.kind))}
                  {tender.attemptId ? (
                    <span className="tag">{t("Approved by terminal")}</span>
                  ) : (
                    tender.kind !== "cash" && <span className="tag">{t("Externally confirmed by operator")}</span>
                  )}
                  {tender.reference && <span className="small mono"> {tender.reference}</span>}
                </span>
                <span className="num">{money(tender.amountMinor)}</span>
                {!tender.attemptId && (
                  <button
                    type="button"
                    className="secondary"
                    aria-label={t("Remove {what} {amount}", { what: t(label(tender.kind)), amount: money(tender.amountMinor) })}
                    disabled={busy}
                    onClick={() => setTenders(tenders.filter((_, j) => j !== i))}
                  >
                    {t("Remove")}
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}

        {!method ? (
          <>
            <form onSubmit={addCash} className="cash-form">
              <label>
                {t("Cash received ({currency}) · empty + Enter = exact", { currency: cart.currencyCode })}
                <input ref={cashInput} inputMode="decimal" aria-label={t("Cash received")} value={cash} onChange={(e) => setCash(e.target.value)} disabled={busy || remaining === 0n} />
              </label>
              <div className="quick-cash">
                <button type="button" disabled={busy || remaining === 0n} onClick={() => void complete([...tenders, { kind: "cash", amountMinor: remaining.toString(), reference: null }])}>
                  <Banknote />
                  {t("Exact {amount}", { amount: money(remaining) })}
                </button>
                {quickAmounts(remaining, exp).map((n) => (
                  <button key={n.toString()} type="button" className="secondary" disabled={busy} onClick={() => void complete([...tenders, { kind: "cash", amountMinor: n.toString(), reference: null }])}>
                    {money(n)}
                  </button>
                ))}
              </div>
            </form>
            <div className="methods" role="group" aria-label={t("Other payment methods")}>
              {terminal && exchange === null && (
                <button type="button" disabled={busy || remaining === 0n} onClick={() => setMethod("terminal")}>
                  {t("Card (terminal)")}
                </button>
              )}
              {METHODS.map((m) => (
                <button key={m.kind} type="button" className="secondary" disabled={busy || remaining === 0n} onClick={() => setMethod(m.kind)}>
                  <m.icon />
                  {t(m.label)}
                </button>
              ))}
            </div>
          </>
        ) : method === "terminal" ? (
          <TerminalPayment
            cartId={cart.cartId}
            remainingMinor={remaining}
            exponent={exp}
            currencyCode={cart.currencyCode}
            requesterId={requesterId}
            resume={resume}
            onBack={() => {
              setResume(null);
              setMethod(null);
            }}
            onApproved={(a) => {
              setResume(null);
              setMethod(null);
              setTenders((ts) =>
                ts.some((t) => t.attemptId === a.attemptId)
                  ? ts
                  : [...ts, { kind: "card_external", amountMinor: a.amountMinor, reference: a.providerRef ?? null, attemptId: a.attemptId }],
              );
            }}
          />
        ) : (
          <form onSubmit={addExternal} className="external-form" aria-label={t(label(method))}>
            <p className="small">
              {method === "card_external"
                ? t("Take the payment on the card terminal first, then record it here.")
                : t("Take the payment on the customer's device first, then record it here.")}
            </p>
            <label>
              {t("Amount ({currency}) · empty = the rest, {amount}", { currency: cart.currencyCode, amount: money(remaining) })}
              <input autoFocus inputMode="decimal" value={external.amount} onChange={(e) => setExternal({ ...external, amount: e.target.value })} />
            </label>
            <label>
              {t("Reference (optional: approval code or transfer ID, never a card number)")}
              <input value={external.reference} maxLength={64} autoComplete="off" onChange={(e) => setExternal({ ...external, reference: e.target.value })} />
            </label>
            <div className="actions">
              <button type="button" className="secondary" onClick={() => setMethod(null)}>
                {t("Back")}
              </button>
              <button type="submit">{t("Record {what}", { what: t(label(method)) })}</button>
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
            {t("Back to cart")} <kbd>Esc</kbd>
          </button>
          {exchange && total === 0n ? (
            // The returned value covers the new items: nothing to pay, only a difference to give back.
            <button type="button" data-autofocus disabled={busy} onClick={() => void complete([])}>
              {busy ? t("Saving…") : t("Complete exchange")}
            </button>
          ) : (
            <button type="button" disabled={busy || remaining > 0n || tenders.length === 0} onClick={() => void complete(tenders)}>
              {busy ? t("Saving…") : exchange ? t("Complete exchange") : t("Complete sale")}
            </button>
          )}
        </div>
        {approvalDialog}
    </Modal>
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
