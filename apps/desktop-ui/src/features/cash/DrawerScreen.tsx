import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type IpcError, type Session, type Workspace } from "../../ipc";
import type { CashStateDto_Serialize as CashState, ClosedCashDto_Serialize as Closed, SessionTotalsDto } from "../../ipc/bindings";
import { scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { TENDER_LABEL } from "../sales/labels";
import { describeError } from "../shell/errors";

/**
 * The cash drawer (E15-T02): open with a float, see the session, close with a count.
 * A cashier's count is blind: Rust sends no expected cash or variance to that role, and
 * this screen shows none. Closing is final, so it asks first.
 */
export function DrawerScreen({ workspace, session, onBack }: { workspace: Workspace; session: Session; onBack: () => void }) {
  const [state, setState] = useState<CashState | null>(null);
  const [amount, setAmount] = useState("");
  const [note, setNote] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [closed, setClosed] = useState<Closed | null>(null);
  const [error, setError] = useState<IpcError | string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showZ, setShowZ] = useState(false);
  const input = useRef<HTMLInputElement | null>(null);
  const exp = state?.session?.currencyExponent ?? workspace.currencyExponent;
  const money = (minor: string) => formatMoney(minor, { exponent: exp });
  const manager = session.role !== "cashier"; // UI only: Rust decides what each role receives

  const refresh = useCallback(() => {
    void commands.cashSessionCurrent().then((res) => {
      if (res.status === "ok") setState(res.data);
      else setError(res.error);
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
    if (minor === null) setError("Enter an amount, e.g. 100.00.");
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
      <h1>Cash drawer</h1>
      <button type="button" className="secondary" onClick={onBack}>
        Back to till <kbd>Esc</kbd>
      </button>
    </header>
  );

  if (closed) {
    const v = closed.varianceMinor;
    return (
      <main className="sales">
        {header}
        <section className="card return-done" aria-label="Drawer closed">
          <p>
            Session <strong className="mono">{closed.session.number}</strong> is closed.
          </p>
          <p className="small">Counted</p>
          <output className="change-amount" data-testid="counted">
            {money(closed.countedMinor)}
          </output>
          {closed.expectedMinor !== null && v !== null ? (
            <dl className="totals" aria-label="Count against expected">
              <dt>Expected</dt>
              <dd data-testid="expected">{money(closed.expectedMinor)}</dd>
              <dt>Variance</dt>
              <dd data-testid="variance">
                {v === "0" ? "None" : v.startsWith("-") ? `${money(v.slice(1))} short` : `${money(v)} over`}
              </dd>
            </dl>
          ) : (
            <p className="small">A manager will check the count against the expected amount.</p>
          )}
          {manager && (
            <>
              <PrintControls source={{ kind: "z", id: closed.session.cashSessionId }} autoPrint={false} />
              {showZ && <ReceiptPreview source={{ kind: "z", id: closed.session.cashSessionId }} />}
            </>
          )}
          <div className="actions">
            <button type="button" onClick={onBack} autoFocus>
              Back to till <kbd>Enter</kbd>
            </button>
          </div>
        </section>
      </main>
    );
  }

  if (!state) return <main className="sales" aria-busy="true" />;
  const s = state.session;

  return (
    <main className="sales">
      {header}
      {!s ? (
        <form className="card return-find" aria-label="Open the drawer" onSubmit={(e) => void open(e)}>
          <p>
            The drawer is closed.
            {state.required ? " Sales need an open drawer on this till." : ""}
          </p>
          <label>
            Opening float ({workspace.currencyCode})
            <input ref={input} inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="e.g. 100.00" autoComplete="off" />
          </label>
          <button type="submit" disabled={busy}>
            Open drawer
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
          aria-label="Close the drawer"
          onSubmit={(e) => {
            e.preventDefault();
            if (parsed() !== null) setConfirming(true);
          }}
        >
          <p>
            Session <strong className="mono">{s.number}</strong> · opened {s.openedLocal} by {s.openedByName} · float{" "}
            {money(s.openingFloatMinor)}
          </p>
          {state.totals && state.expectedMinor !== null && <Running totals={state.totals} expected={state.expectedMinor} money={money} />}
          <label>
            Counted cash ({workspace.currencyCode})
            <input ref={input} inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="Count the drawer" autoComplete="off" />
          </label>
          <label>
            Note (optional)
            <input value={note} onChange={(e) => setNote(e.target.value)} maxLength={200} />
          </label>
          {errorText && (
            <p role="alert" className="field-error">
              {errorText}
            </p>
          )}
          <div className="actions">
            <button type="submit" className="pay" disabled={busy || !amount.trim()}>
              Close drawer
            </button>
          </div>
        </form>
      )}

      {confirming && (
        <div className="backdrop">
          <div role="dialog" aria-modal="true" aria-label="Close the drawer?" className="dialog card">
            <h2>Close the drawer?</h2>
            <p>
              Counted {money(scaleDecimal(amount || "0", exp) ?? "0")}. Closing is final: the session cannot be reopened.
            </p>
            <div className="actions">
              <button type="button" className="secondary" onClick={() => setConfirming(false)}>
                Cancel
              </button>
              <button type="button" className="pay" onClick={() => void close()} autoFocus>
                Close drawer
              </button>
            </div>
          </div>
        </div>
      )}
    </main>
  );
}

function Running({ totals, expected, money }: { totals: SessionTotalsDto; expected: string; money: (m: string) => string }) {
  return (
    <dl className="totals" aria-label="So far">
      <dt>Sales ({totals.salesCount})</dt>
      <dd>{money(totals.salesTotalMinor)}</dd>
      {totals.tenders.map((t) => (
        <Row key={t.kind} label={`  ${TENDER_LABEL[t.kind]} (${t.count})`} value={money(t.amountMinor)} />
      ))}
      <dt>Returns ({totals.returnsCount})</dt>
      <dd>{totals.refundsTotalMinor === "0" ? money("0") : `-${money(totals.refundsTotalMinor)}`}</dd>
      <dt>
        <strong>Expected in the drawer</strong>
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
  const [show, setShow] = useState(false);
  return (
    <div>
      <button type="button" className="secondary" onClick={() => setShow((s) => !s)}>
        {show ? "Hide last Z summary" : "Last Z summary"}
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
