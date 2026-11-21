import { Modal } from "../../ui/Modal";
import { ArrowLeft, Banknote, TriangleExclamationPoint } from "../../ui/icons";
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type IpcError, type Session, type Workspace } from "../../ipc";
import type { CashStateDto_Serialize as CashState, ClosedCashDto_Serialize as Closed, SessionTotalsDto } from "../../ipc/bindings";
import { scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { TENDER_LABEL } from "../sales/labels";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";

/**
 * The cash drawer (E15-T02): open with a float, see the session, close with a count.
 * A cashier's count is blind: Rust sends no expected cash or variance to that role, and
 * this screen shows none. Closing is final, so it asks first.
 */
export function DrawerScreen({ workspace, session, onBack }: { workspace: Workspace; session: Session; onBack: () => void }) {
  const t = useT();
  const [state, setState] = useState<CashState | null>(null);
  const [amount, setAmount] = useState("");
  const [note, setNote] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [closed, setClosed] = useState<Closed | null>(null);
  const [error, setError] = useState<IpcError | string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showZ, setShowZ] = useState(false);
  const [parked, setParked] = useState(0);
  const input = useRef<HTMLInputElement | null>(null);
  const exp = state?.session?.currencyExponent ?? workspace.currencyExponent;
  const money = (minor: string) => formatMoney(minor, { exponent: exp });
  const manager = session.role !== "cashier"; // UI only: Rust decides what each role receives

  const refresh = useCallback(() => {
    void commands.cashSessionCurrent().then((res) => {
      if (res.status === "ok") setState(res.data);
      else setError(res.error);
    });
    // Parked carts outlive the drawer session; closing warns about them (POS-012).
    void commands.cartParkedList().then((res) => {
      if (res.status === "ok") setParked(res.data.length);
    });
  }, []);

  useEffect(refresh, [refresh]);
  useEffect(() => {
    if (state && !closed) input.current?.focus();
  }, [state, closed]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      if (confirming) setConfirming(false);
      else onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [confirming, onBack]);

  const parsed = () => {
    const minor = scaleDecimal(amount || "0", exp);
    if (minor === null) setError(t("Enter an amount, e.g. 100.00."));
    return minor;
  };

  async function open(e: FormEvent) {
    e.preventDefault();
    const minor = parsed();
    if (minor === null || busy) return;
    setBusy(true);
    const res = await commands.cashSessionOpen(minor);
    setBusy(false);
    if (res.status !== "ok") return setError(res.error);
    setError(null);
    setAmount("");
    setState(res.data);
  }

  async function close() {
    const minor = parsed();
    if (minor === null || busy) return;
    setBusy(true);
    const res = await commands.cashSessionClose(minor, note.trim() || null);
    setBusy(false);
    setConfirming(false);
    if (res.status !== "ok") return setError(res.error);
    setError(null);
    setClosed(res.data);
    setShowZ(manager);
  }

  const errorText = error === null ? null : typeof error === "string" ? error : describeError(error);
  const header = (
    <header className="returns-header">
      <h1 className="page-title">
          <Banknote />
          {t("Cash drawer")}
        </h1>
      <button type="button" className="secondary" onClick={onBack}>
        <ArrowLeft />
        {t("Back to till")} <kbd>Esc</kbd>
      </button>
    </header>
  );

  if (closed) {
    const v = closed.varianceMinor;
    const [closedBefore, closedAfter = ""] = t("Session {number} is closed.").split("{number}");
    return (
      <main className="sales">
        {header}
        <section className="card return-done" aria-label={t("Drawer closed")}>
          <p>
            {closedBefore}
            <strong className="mono">{closed.session.number}</strong>
            {closedAfter}
          </p>
          <p className="small">{t("Counted")}</p>
          <output className="change-amount" data-testid="counted">
            {money(closed.countedMinor)}
          </output>
          {closed.expectedMinor !== null && v !== null ? (
            <dl className="totals" aria-label={t("Count against expected")}>
              <dt>{t("Expected")}</dt>
              <dd data-testid="expected">{money(closed.expectedMinor)}</dd>
              <dt>{t("Variance")}</dt>
              <dd data-testid="variance">
                {v === "0"
                  ? t("None")
                  : v.startsWith("-")
                    ? t("{amount} short", { amount: money(v.slice(1)) })
                    : t("{amount} over", { amount: money(v) })}
              </dd>
            </dl>
          ) : (
            <p className="small">{t("A manager will check the count against the expected amount.")}</p>
          )}
          {manager && (
            <>
              <PrintControls source={{ kind: "z", id: closed.session.cashSessionId }} autoPrint={false} />
              {showZ && <ReceiptPreview source={{ kind: "z", id: closed.session.cashSessionId }} />}
            </>
          )}
          <div className="actions">
            <button type="button" onClick={onBack} autoFocus>
              <ArrowLeft />
              {t("Back to till")} <kbd>Enter</kbd>
            </button>
          </div>
        </section>
      </main>
    );
  }

  if (!state) return <main className="sales" aria-busy="true" />;
  const s = state.session;
  const [sessionBefore, sessionAfter = ""] = s
    ? t("Session {number} · opened {time} by {name} · float {amount}", {
        time: s.openedLocal,
        name: s.openedByName,
        amount: money(s.openingFloatMinor),
      }).split("{number}")
    : [];

  return (
    <main className="sales">
      {header}
      {!s ? (
        <form className="card return-find" aria-label={t("Open the drawer")} onSubmit={(e) => void open(e)}>
          <p>
            {t("The drawer is closed.")}
            {state.required ? ` ${t("Sales need an open drawer on this till.")}` : ""}
          </p>
          <label>
            {t("Opening float ({currency})", { currency: workspace.currencyCode })}
            <input ref={input} inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder={t("e.g. 100.00")} autoComplete="off" />
          </label>
          <button type="submit" disabled={busy}>
            {t("Open drawer")}
          </button>
          {errorText && (
            <p role="alert" className="field-error">
              {errorText}
            </p>
          )}
          {manager && <LastZ />}
        </form>
      ) : (
        <form
          className="card return-form"
          aria-label={t("Close the drawer")}
          onSubmit={(e) => {
            e.preventDefault();
            if (parsed() !== null) setConfirming(true);
          }}
        >
          <p>
            {sessionBefore}
            <strong className="mono">{s.number}</strong>
            {sessionAfter}
          </p>
          {state.totals && state.expectedMinor !== null && <Running totals={state.totals} expected={state.expectedMinor} money={money} />}
          {parked > 0 && <ParkedWarning count={parked} />}
          <label>
            {t("Counted cash ({currency})", { currency: workspace.currencyCode })}
            <input ref={input} inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder={t("Count the drawer")} autoComplete="off" />
          </label>
          <label>
            {t("Note (optional)")}
            <input value={note} onChange={(e) => setNote(e.target.value)} maxLength={200} />
          </label>
          {errorText && (
            <p role="alert" className="field-error">
              {errorText}
            </p>
          )}
          <div className="actions">
            <button type="submit" className="pay" disabled={busy || !amount.trim()}>
              {t("Close drawer")}
            </button>
          </div>
        </form>
      )}

      {confirming && (
        <Modal label={t("Close the drawer?")}>
            <h2>{t("Close the drawer?")}</h2>
            <p>
              {t("Counted {amount}. Closing is final: the session cannot be reopened.", {
                amount: money(scaleDecimal(amount || "0", exp) ?? "0"),
              })}
            </p>
            {parked > 0 && <ParkedWarning count={parked} />}
            <div className="actions">
              <button type="button" className="secondary" onClick={() => setConfirming(false)}>
                {t("Cancel")}
              </button>
              <button type="button" className="pay" onClick={() => void close()} data-autofocus>
                {t("Close drawer")}
              </button>
            </div>
        </Modal>
      )}
    </main>
  );
}

/** Parked carts are not sales: they stay parked, and sell into whichever drawer session is open then. */
function ParkedWarning({ count }: { count: number }) {
  const t = useT();
  return (
    <p className="callout-warning" data-testid="parked-warning">
      <TriangleExclamationPoint />
      <span>
        {count === 1
          ? t("1 cart is parked on this till and not in this count. It stays parked: resume and finish it, or void it, from the till.")
          : t("{n} carts are parked on this till and not in this count. They stay parked: resume and finish them, or void them, from the till.", { n: count })}
      </span>
    </p>
  );
}

function Running({ totals, expected, money }: { totals: SessionTotalsDto; expected: string; money: (m: string) => string }) {
  const t = useT();
  return (
    <dl className="totals" aria-label={t("So far")}>
      <dt>{t("Sales ({n})", { n: totals.salesCount })}</dt>
      <dd>{money(totals.salesTotalMinor)}</dd>
      {totals.tenders.map((x) => (
        <Row key={x.kind} label={`  ${t(TENDER_LABEL[x.kind])} (${x.count})`} value={money(x.amountMinor)} />
      ))}
      <dt>{t("Returns ({n})", { n: totals.returnsCount })}</dt>
      <dd>{totals.refundsTotalMinor === "0" ? money("0") : `-${money(totals.refundsTotalMinor)}`}</dd>
      <dt>
        <strong>{t("Expected in the drawer")}</strong>
      </dt>
      <dd data-testid="running-expected">
        <strong>{money(expected)}</strong>
      </dd>
    </dl>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}

/** The last Z summary on this till, for managers and owners. */
function LastZ() {
  const t = useT();
  const [show, setShow] = useState(false);
  return (
    <div>
      <button type="button" className="secondary" onClick={() => setShow((s) => !s)}>
        {show ? t("Hide last Z summary") : t("Last Z summary")}
      </button>
      {show && (
        <>
          <PrintControls source={{ kind: "z", id: null }} autoPrint={false} />
          <ReceiptPreview source={{ kind: "z", id: null }} />
        </>
      )}
    </div>
  );
}
