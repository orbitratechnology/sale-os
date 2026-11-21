import { ArrowLeft, ArrowRightLeft, Undo2 } from "../../ui/icons";
import { Fragment, useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import { commands, type ApprovalScope, type IpcError, type Session, type Workspace } from "../../ipc";
import type {
  CompletedReturnDto,
  Disposition,
  ReturnLineInputDto,
  ReturnPlanDto,
  ReturnReason,
  ReturnableSaleDto,
  TenderKind,
} from "../../ipc/bindings";
import { formatMilli, scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { ApprovalDialog } from "../auth/ApprovalDialog";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { describeError } from "../shell/errors";
import type { ExchangeCredit } from "./exchange";
import { useT } from "../../i18n";

/** The reasons offered, in this order (English: shown through `t(…)`). */
const REASON_COPY: Record<ReturnReason, string> = {
  changed_mind: "Changed mind",
  defective: "Faulty",
  wrong_item: "Wrong item",
  damaged_in_transit: "Damaged",
  other: "Other",
};

/** The ways to refund, in this order (English: shown through `t(…)`). */
const REFUND_COPY: Partial<Record<TenderKind, string>> = {
  cash: "Cash",
  card_external: "Card (separate terminal)",
  bank_transfer_external: "Bank transfer",
  other_external: "Other",
};

/**
 * A translated sentence with elements in it: each `{name}` in `text` becomes `parts[name]`,
 * wherever the language puts it.
 */
function withParts(text: string, parts: Record<string, ReactNode>): ReactNode[] {
  return text.split(/(\{\w+\})/).map((piece, i) => {
    const name = /^\{(\w+)\}$/.exec(piece)?.[1];
    return name !== undefined && name in parts ? <Fragment key={i}>{parts[name]}</Fragment> : piece;
  });
}

/** Per sale line: what the cashier typed and where the goods go. */
interface Pick {
  qty: string;
  disposition: Disposition;
}

/**
 * Returns (E12-T04). Find the sale by the number on its receipt (typed, or scanned from the
 * receipt's barcode), choose what comes back and where it goes, then refund. Rust plans the
 * refund (the cumulative rounding rule) and commits everything in one transaction; the
 * amounts here are its preview. A cashier's return asks for a manager's approval.
 */
export function ReturnScreen({
  workspace,
  session,
  initialReceipt,
  onBack,
  onExchange,
}: {
  workspace: Workspace;
  session: Session;
  /** From a sale's detail: look this receipt up straight away. */
  initialReceipt?: string;
  onBack: () => void;
  /**
   * "Exchange for other items": take the chosen lines to the till as credit for new items.
   * Nothing is recorded here; the till records the return and the new sale together.
   */
  onExchange?: (credit: ExchangeCredit) => void;
}) {
  const t = useT();
  const [number, setNumber] = useState(initialReceipt ?? "");
  const [sale, setSale] = useState<ReturnableSaleDto | null>(null);
  const [picks, setPicks] = useState<Record<string, Pick>>({});
  const [reason, setReason] = useState<ReturnReason>("changed_mind");
  const [note, setNote] = useState("");
  const [method, setMethod] = useState<TenderKind>("cash");
  const [reference, setReference] = useState("");
  // The preview, tagged with the selection it was computed for: a stale one is never shown
  // or used as the refund amount.
  const [preview, setPreview] = useState<{ key: string; plan: ReturnPlanDto } | null>(null);
  const [error, setError] = useState<IpcError | string | null>(null);
  const [busy, setBusy] = useState(false);
  const [approval, setApproval] = useState<ApprovalScope | null>(null);
  const [done, setDone] = useState<CompletedReturnDto | null>(null);
  const [showReceipt, setShowReceipt] = useState(true);
  const findInput = useRef<HTMLInputElement | null>(null);
  const exp = sale?.currencyExponent ?? workspace.currencyExponent;
  const money = (minor: string) => formatMoney(minor, { exponent: exp });

  useEffect(() => {
    if (!sale && !done) findInput.current?.focus();
  }, [sale, done]);

  const reset = useCallback(() => {
    setSale(null);
    setPicks({});
    setPreview(null);
    setNumber("");
    setNote("");
    setReference("");
    setMethod("cash");
    setReason("changed_mind");
    setError(null);
    setDone(null);
  }, []);

  // Esc: back out one step (the approval dialog owns Esc while open).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || approval) return;
      e.preventDefault();
      if (done || sale) reset();
      else onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [approval, done, sale, reset, onBack]);

  const lookUp = useCallback(async (receiptNumber: string) => {
    setBusy(true);
    setError(null);
    const res = await commands.returnFind(receiptNumber);
    setBusy(false);
    if (res.status !== "ok") {
      return setError(res.error.code === "VALIDATION_FAILED" ? t("No sale with that receipt number on this till.") : res.error);
    }
    setSale(res.data);
    setPicks(
      Object.fromEntries(
        res.data.lines.map((l) => [l.saleLineId, { qty: "", disposition: l.trackStock ? "restock" : "no_stock_change" } as Pick]),
      ),
    );
  }, [t]);

  function find(e: FormEvent) {
    e.preventDefault();
    if (!number.trim() || busy) return;
    void lookUp(number.trim());
  }

  const started = useRef(false);
  useEffect(() => {
    if (initialReceipt && !started.current) {
      started.current = true;
      void lookUp(initialReceipt);
    }
  }, [initialReceipt, lookUp]);

  /** The lines with a quantity, as Rust wants them; null when something typed is not a number. */
  const lines = useMemo((): ReturnLineInputDto[] | null => {
    if (!sale) return [];
    const out: ReturnLineInputDto[] = [];
    for (const l of sale.lines) {
      const p = picks[l.saleLineId];
      if (!p || !p.qty.trim()) continue;
      const milli = scaleDecimal(p.qty, 3);
      if (milli === null) return null;
      if (milli !== "0") out.push({ saleLineId: l.saleLineId, quantityMilli: milli, disposition: p.disposition });
    }
    return out;
  }, [sale, picks]);

  const key = lines && lines.length > 0 ? JSON.stringify(lines) : null;
  const plan = preview && preview.key === key ? preview.plan : null;

  // The refund preview follows every change (no writes).
  useEffect(() => {
    if (!sale || !lines || key === null) return;
    let live = true;
    void commands.returnPreview(sale.saleId, lines).then((res) => {
      if (!live) return;
      if (res.status === "ok") {
        setPreview({ key, plan: res.data });
        setError(null);
      } else {
        setPreview(null);
        setError(res.error);
      }
    });
    return () => {
      live = false;
    };
  }, [sale, lines, key]);

  async function create(approvalTokenId: string | null) {
    if (!sale || !lines || !plan || busy) return;
    setBusy(true);
    setError(null);
    const refund = { kind: method, amountMinor: plan.totalMinor, reference: method === "cash" ? null : reference.trim() || null };
    const res = await commands.returnCreate(sale.saleId, lines, reason, note.trim() || null, [refund], approvalTokenId);
    setBusy(false);
    if (res.status === "ok") return setDone(res.data);
    if (res.error.code === "APPROVAL_REQUIRED" && res.error.details?.scope) return setApproval(res.error.details.scope);
    setError(res.error);
  }

  function exchange() {
    if (!sale || !lines || !plan || !onExchange) return;
    const items = lines
      .map((x) => {
        const l = sale.lines.find((y) => y.saleLineId === x.saleLineId);
        const name = l ? (l.label ? `${l.name} ${l.label}` : l.name) : "?";
        return `${name} ×${formatMilli(x.quantityMilli)}`;
      })
      .join(", ");
    onExchange({
      saleId: sale.saleId,
      receiptNumber: sale.receiptNumber,
      lines,
      reason,
      note: note.trim() || null,
      creditMinor: plan.totalMinor,
      items,
    });
  }

  const setPick = (id: string, change: Partial<Pick>) => {
    setError(null);
    setPicks((all) => ({ ...all, [id]: { ...(all[id] ?? { qty: "", disposition: "restock" }), ...change } }));
  };

  const errorText = error === null ? null : typeof error === "string" ? error : describeError(error);

  if (done) {
    return (
      <main className="returns">
        <section className="card return-done" aria-label={t("Return completed")}>
          <p className="small">{t("Refund")}</p>
          <output className="change-amount" data-testid="refund">
            {money(done.totalMinor)}
          </output>
          <p>
            {withParts(t("Return {number} for receipt {receipt}"), {
              number: <strong className="mono">{done.returnNumber}</strong>,
              receipt: <span className="mono">{done.originalReceiptNumber}</span>,
            })}
            {done.approved ? ` · ${t("approved by a manager")}` : ""}
          </p>
          <PrintControls source={{ kind: "return", id: done.returnId }} autoPrint={false} />
          {showReceipt && <ReceiptPreview source={{ kind: "return", id: done.returnId }} />}
          <div className="actions">
            <button type="button" className="secondary" onClick={() => setShowReceipt((s) => !s)}>
              {showReceipt ? t("Hide receipt") : t("Show receipt")}
            </button>
            <button type="button" className="secondary" onClick={reset}>
              {t("Another return")}
            </button>
            <button type="button" onClick={onBack} autoFocus>
              <ArrowLeft />
              {t("Back to till")} <kbd>Enter</kbd>
            </button>
          </div>
        </section>
      </main>
    );
  }

  return (
    <main className="returns">
      <header className="returns-header">
        <h1 className="page-title">
          <Undo2 />
          {t("Returns")}
        </h1>
        <button type="button" className="secondary" onClick={onBack}>
          <ArrowLeft />
          {t("Back to till")} <kbd>Esc</kbd>
        </button>
      </header>

      {!sale ? (
        <form className="card return-find" onSubmit={find}>
          <label>
            {t("Receipt number")}
            <input
              ref={findInput}
              value={number}
              onChange={(e) => setNumber(e.target.value)}
              placeholder={t("Scan the receipt or type e.g. {receipt}", { receipt: "R01-000123" })}
              autoComplete="off"
              spellCheck={false}
            />
          </label>
          <button type="submit" disabled={busy || !number.trim()}>
            {t("Find sale")}
          </button>
          {errorText && (
            <p role="alert" className="field-error">
              {errorText}
            </p>
          )}
        </form>
      ) : (
        <form
          className="card return-form"
          onSubmit={(e) => {
            e.preventDefault();
            void create(null);
          }}
        >
          <p>
            {t("Sale")} <strong className="mono">{sale.receiptNumber}</strong>
            {sale.mode === "sandbox" ? ` · ${t("SANDBOX")}` : ""}
          </p>
          <table className="return-lines">
            <thead>
              <tr>
                <th scope="col">{t("Item")}</th>
                <th scope="col" className="num">{t("Sold")}</th>
                <th scope="col" className="num">{t("Returned")}</th>
                <th scope="col" className="num">{t("Paid")}</th>
                <th scope="col">{t("Return now")}</th>
                <th scope="col">{t("Goods")}</th>
              </tr>
            </thead>
            <tbody>
              {sale.lines.map((l) => {
                // Keyboard first: the first line that can still come back takes focus.
                const first = sale.lines.find((x) => x.availableMilli !== "0")?.saleLineId === l.saleLineId;
                const p = picks[l.saleLineId];
                const none = l.availableMilli === "0";
                const label = l.label ? `${l.name} ${l.label}` : l.name;
                return (
                  <tr key={l.saleLineId} className={none ? "muted" : undefined}>
                    <th scope="row">{label}</th>
                    <td className="num">{formatMilli(l.soldMilli)}</td>
                    <td className="num">{formatMilli(l.returnedMilli)}</td>
                    <td className="num">{money(l.totalMinor)}</td>
                    <td>
                      <input
                        className="qty-input"
                        aria-label={t("Quantity to return: {name}", { name: label })}
                        inputMode="decimal"
                        autoFocus={first} // the next step after finding the sale
                        disabled={none}
                        placeholder={none ? t("all returned") : t("up to {qty}", { qty: formatMilli(l.availableMilli) })}
                        value={p?.qty ?? ""}
                        onChange={(e) => setPick(l.saleLineId, { qty: e.target.value })}
                      />
                    </td>
                    <td>
                      {l.trackStock ? (
                        <select
                          aria-label={t("Where it goes: {name}", { name: label })}
                          disabled={none}
                          value={p?.disposition ?? "restock"}
                          onChange={(e) => setPick(l.saleLineId, { disposition: e.target.value as Disposition })}
                        >
                          <option value="restock">{t("Back to stock")}</option>
                          <option value="damaged">{t("Damaged")}</option>
                          <option value="no_stock_change">{t("Not kept")}</option>
                        </select>
                      ) : (
                        <span className="small">{t("No stock")}</span>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          {lines === null && <p className="field-error">{t("Quantities must be numbers, e.g. 1 or 0.5.")}</p>}

          <div className="return-options">
            <label>
              {t("Reason")}
              <select value={reason} onChange={(e) => setReason(e.target.value as ReturnReason)}>
                {(Object.keys(REASON_COPY) as ReturnReason[]).map((r) => (
                  <option key={r} value={r}>
                    {t(REASON_COPY[r])}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {t("Note (optional)")}
              <input value={note} onChange={(e) => setNote(e.target.value)} maxLength={200} />
            </label>
            <label>
              {t("Refund by")}
              <select value={method} onChange={(e) => setMethod(e.target.value as TenderKind)}>
                {(Object.entries(REFUND_COPY) as [TenderKind, string][]).map(([kind, label]) => (
                  <option key={kind} value={kind}>
                    {t(label)}
                  </option>
                ))}
              </select>
            </label>
            {method !== "cash" && (
              <label>
                {t("Reference (optional)")}
                <input
                  value={reference}
                  onChange={(e) => setReference(e.target.value)}
                  placeholder={t("Terminal approval code — never the card number")}
                  autoComplete="off"
                />
              </label>
            )}
          </div>

          {plan && (
            <dl className="totals" aria-label={t("Refund")}>
              <dt>{t("Tax refunded (included)")}</dt>
              <dd>{money(plan.taxMinor)}</dd>
            </dl>
          )}
          <div className="total-due" aria-live="polite">
            <span className="total-label">{t("Refund {currency}", { currency: workspace.currencyCode })}</span>
            <output className="total-amount" data-testid="refund-total">
              {plan ? money(plan.totalMinor) : money("0")}
            </output>
          </div>
          {method !== "cash" && plan && (
            <p className="small">
              {t("Refund {amount} on the separate terminal first, then confirm here.", { amount: money(plan.totalMinor) })}
            </p>
          )}
          {errorText && (
            <p role="alert" className="field-error">
              {errorText}
            </p>
          )}
          <div className="actions">
            <button type="button" className="secondary" onClick={reset}>
              {t("Different sale")}
            </button>
            {onExchange && (
              <button type="button" className="secondary" disabled={!plan || busy} onClick={exchange}>
                <ArrowRightLeft />
                {t("Exchange for other items")}
              </button>
            )}
            <button type="submit" className="pay" disabled={!plan || busy}>
              {plan ? t("Refund {amount}", { amount: money(plan.totalMinor) }) : t("Refund")}
            </button>
          </div>
        </form>
      )}

      {approval && (
        <ApprovalDialog
          scope={approval}
          requesterId={session.userId}
          onCancel={() => setApproval(null)}
          onApproved={(token) => {
            setApproval(null);
            void create(token);
          }}
        />
      )}
    </main>
  );
}
