import { useCallback, useEffect, useRef, useState } from "react";
import { commands, type IpcError, type SignInUser, type Workspace } from "../../ipc";
import type { SaleRowDto, SalesFilterDto_Deserialize as SalesFilter, TenderKind } from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { describeError } from "../shell/errors";
import { SaleDetail } from "./SaleDetail";
import { TENDER_LABEL } from "./labels";

const EMPTY: SalesFilter = { from: null, to: null, receipt: null, cashierUserId: null, tender: null };

/**
 * Sales history (E12-T03): newest first, filtered by date, receipt number, cashier and
 * payment type, a page at a time ("Show more" follows Rust's page token). Opening a sale
 * shows its detail; Esc goes back one level.
 */
export function SalesScreen({
  workspace,
  onBack,
  onReturn,
}: {
  workspace: Workspace;
  onBack: () => void;
  /** Start a return for this receipt number. */
  onReturn: (receiptNumber: string) => void;
}) {
  const [filter, setFilter] = useState<SalesFilter>(EMPTY);
  const [receipt, setReceipt] = useState("");
  const [rows, setRows] = useState<SaleRowDto[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<IpcError | null>(null);
  const [users, setUsers] = useState<SignInUser[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const request = useRef(0);
  const receiptInput = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    void commands.authListUsers().then((res) => res.status === "ok" && setUsers(res.data));
    receiptInput.current?.focus();
  }, []);

  // The receipt field filters after a short pause in typing (or at once on Enter).
  useEffect(() => {
    const t = window.setTimeout(() => setFilter((f) => ({ ...f, receipt: receipt.trim() || null })), 250);
    return () => window.clearTimeout(t);
  }, [receipt]);

  const load = useCallback((f: SalesFilter, after: string | null) => {
    const mine = ++request.current;
    return commands.salesList(f, after, null).then((res) => {
      if (mine !== request.current) return; // a newer filter won
      setLoaded(true);
      if (res.status !== "ok") return setError(res.error);
      setError(null);
      setRows((r) => (after ? [...r, ...res.data.rows] : res.data.rows));
      setNext(res.data.next);
    });
  }, []);

  useEffect(() => {
    const mine = ++request.current;
    void commands.salesList(filter, null, null).then((res) => {
      if (mine !== request.current) return;
      setLoaded(true);
      if (res.status !== "ok") return setError(res.error);
      setError(null);
      setRows(res.data.rows);
      setNext(res.data.next);
    });
  }, [filter]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || open) return;
      e.preventDefault();
      onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onBack]);

  if (open) {
    return (
      <SaleDetail
        saleId={open}
        onBack={() => {
          setOpen(null);
          setFilter((f) => ({ ...f })); // reload: a return may have been made
        }}
        onReturn={onReturn}
      />
    );
  }

  const money = (minor: string, exp: number) => formatMoney(minor, { exponent: exp });
  const set = (change: Partial<SalesFilter>) => setFilter((f) => ({ ...f, ...change }));

  return (
    <main className="sales">
      <header className="returns-header">
        <h1>Sales</h1>
        <button type="button" className="secondary" onClick={onBack}>
          Back to till <kbd>Esc</kbd>
        </button>
      </header>
      <form
        className="card sales-filters"
        role="search"
        aria-label="Filter sales"
        onSubmit={(e) => {
          e.preventDefault();
          set({ receipt: receipt.trim() || null });
        }}
      >
        <label>
          Receipt number
          <input
            ref={receiptInput}
            value={receipt}
            onChange={(e) => setReceipt(e.target.value)}
            placeholder="e.g. R01-000123 or 123"
            autoComplete="off"
            spellCheck={false}
          />
        </label>
        <label>
          From
          <input type="date" value={filter.from ?? ""} onChange={(e) => set({ from: e.target.value || null })} />
        </label>
        <label>
          To
          <input type="date" value={filter.to ?? ""} onChange={(e) => set({ to: e.target.value || null })} />
        </label>
        <label>
          Cashier
          <select value={filter.cashierUserId ?? ""} onChange={(e) => set({ cashierUserId: e.target.value || null })}>
            <option value="">Anyone</option>
            {users.map((u) => (
              <option key={u.userId} value={u.userId}>
                {u.displayName}
              </option>
            ))}
          </select>
        </label>
        <label>
          Payment
          <select value={filter.tender ?? ""} onChange={(e) => set({ tender: (e.target.value || null) as TenderKind | null })}>
            <option value="">Any</option>
            {(Object.keys(TENDER_LABEL) as TenderKind[]).map((k) => (
              <option key={k} value={k}>
                {TENDER_LABEL[k]}
              </option>
            ))}
          </select>
        </label>
      </form>

      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <section className="card" aria-label="Sales list">
        <table className="sales-table">
          <thead>
            <tr>
              <th scope="col">Time</th>
              <th scope="col">Receipt</th>
              <th scope="col">Cashier</th>
              <th scope="col">Items</th>
              <th scope="col">Payment</th>
              <th scope="col" className="num">
                Total {workspace.currencyCode}
              </th>
              <th scope="col" className="num">
                Refunded
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.saleId} data-testid="sale-row">
                <td>{r.localTime}</td>
                <td>
                  <button type="button" className="link mono" onClick={() => setOpen(r.saleId)}>
                    {r.receiptNumber}
                  </button>
                  {r.mode === "sandbox" && <span className="small"> · sandbox</span>}
                </td>
                <td>{r.cashierName}</td>
                <td>{formatMilli(r.itemCountMilli)}</td>
                <td>{r.tenders.map((k) => TENDER_LABEL[k]).join(" + ")}</td>
                <td className="num">{money(r.totalMinor, r.currencyExponent)}</td>
                <td className="num">{r.refundedMinor === "0" ? "" : `-${money(r.refundedMinor, r.currencyExponent)}`}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {loaded && rows.length === 0 && !error && <p className="small">No sales match.</p>}
        {next && (
          <div className="actions">
            <button type="button" className="secondary" onClick={() => void load(filter, next)}>
              Show more
            </button>
          </div>
        )}
      </section>
    </main>
  );
}
