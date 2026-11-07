import { useEffect, useRef, useState } from "react";
import { commands, type IpcError, type SignInUser } from "../../ipc";
import type { AuditFilterDto, AuditRowDto } from "../../ipc/bindings";
import { SaleDetail } from "../sales/SaleDetail";
import { describeError } from "../shell/errors";

const GROUPS: { value: string; label: string }[] = [
  { value: "", label: "Everything" },
  { value: "auth.", label: "Sign-in and approvals" },
  { value: "staff.", label: "Staff" },
  { value: "sale.", label: "Sales" },
  { value: "tender.", label: "External payments" },
  { value: "cart.", label: "Discounts, price changes, voids" },
  { value: "return.", label: "Returns" },
  { value: "refund.", label: "Refunds" },
  { value: "inventory.", label: "Stock" },
  { value: "cash_session.", label: "Cash drawer" },
  { value: "receipt.", label: "Reprints" },
  { value: "print.", label: "Printing" },
  { value: "catalog.", label: "Products" },
  { value: "report.", label: "Exports" },
  { value: "mode.", label: "Practice / live" },
];

/** Plain words for the catalogued actions (07-contracts §5); unknown ones show as stored. */
const ACTION: Record<string, string> = {
  "auth.login": "Signed in",
  "auth.login_failed": "Wrong PIN",
  "auth.locked_out": "Locked out after wrong PINs",
  "auth.logout": "Signed out",
  "auth.session_locked": "Till locked",
  "auth.approval_granted": "Manager approval",
  "staff.created": "Staff added",
  "staff.updated": "Staff changed",
  "staff.deactivated": "Staff deactivated",
  "staff.pin_reset": "PIN reset",
  "business.created": "Shop set up",
  "location.upserted": "Location saved",
  "mode.changed": "Practice / live switched",
  "catalog.product_created": "Product added",
  "catalog.product_updated": "Product changed",
  "catalog.product_archived": "Product archived",
  "cart.cleared": "Cart cleared",
  "cart.voided": "Sale voided",
  "cart.price_overridden": "Price changed at the till",
  "cart.discount_applied": "Discount",
  "sale.completed": "Sale",
  "tender.recorded_external": "External payment recorded",
  "return.completed": "Return",
  "refund.recorded": "Refund",
  "inventory.opening_posted": "Opening stock",
  "inventory.receipt_posted": "Goods received",
  "inventory.adjustment_posted": "Stock adjusted",
  "inventory.write_off_posted": "Stock written off",
  "inventory.negative_flagged": "Sold below zero stock",
  "inventory.projection_rebuilt": "Stock totals rebuilt",
  "receipt.reprinted": "Receipt reprinted",
  "print.unknown_resolved": "Print outcome resolved",
  "cash_session.opened": "Drawer opened",
  "cash_session.closed": "Drawer closed",
  "report.exported": "Exported",
};

/** A compact line from the stored details: `key value · key value`. */
function details(metadata: string): string {
  try {
    const m = JSON.parse(metadata) as Record<string, unknown>;
    return Object.entries(m)
      .filter(([, v]) => v !== null && typeof v !== "object")
      .map(([k, v]) => `${k} ${String(v)}`)
      .join(" · ");
  } catch {
    return "";
  }
}

/**
 * The audit log (E15-T01, owner only): every recorded action, newest first, filtered by
 * kind, person and dates. A sale opens its detail; a product opens the editor.
 */
export function AuditScreen({ onBack, onOpenProduct, onReturn }: { onBack: () => void; onOpenProduct: (productId: string) => void; onReturn: (receipt: string) => void }) {
  const [filter, setFilter] = useState<AuditFilterDto>({ action: null, userId: null, from: null, to: null });
  const [rows, setRows] = useState<AuditRowDto[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [users, setUsers] = useState<SignInUser[]>([]);
  const [error, setError] = useState<IpcError | null>(null);
  const [sale, setSale] = useState<string | null>(null);
  const request = useRef(0);

  useEffect(() => {
    void commands.authListUsers().then((res) => res.status === "ok" && setUsers(res.data));
  }, []);

  useEffect(() => {
    const mine = ++request.current;
    void commands.auditList(filter, null, 50).then((res) => {
      if (mine !== request.current) return;
      if (res.status !== "ok") return setError(res.error);
      setError(null);
      setRows(res.data.rows);
      setNext(res.data.next);
    });
  }, [filter]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || sale) return;
      e.preventDefault();
      onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sale, onBack]);

  if (sale) return <SaleDetail saleId={sale} onBack={() => setSale(null)} onReturn={onReturn} />;

  const more = () => {
    if (!next) return;
    void commands.auditList(filter, next, 50).then((res) => {
      if (res.status !== "ok") return setError(res.error);
      setRows((r) => [...r, ...res.data.rows]);
      setNext(res.data.next);
    });
  };
  const set = (change: Partial<AuditFilterDto>) => setFilter((f) => ({ ...f, ...change }));

  return (
    <main className="sales">
      <header className="returns-header">
        <h1>Audit log</h1>
        <button type="button" className="secondary" onClick={onBack}>
          Back <kbd>Esc</kbd>
        </button>
      </header>
      <form className="card sales-filters" role="search" aria-label="Filter the audit log" onSubmit={(e) => e.preventDefault()}>
        <label>
          What
          <select value={filter.action ?? ""} onChange={(e) => set({ action: e.target.value || null })}>
            {GROUPS.map((g) => (
              <option key={g.value} value={g.value}>
                {g.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          Who
          <select value={filter.userId ?? ""} onChange={(e) => set({ userId: e.target.value || null })}>
            <option value="">Anyone</option>
            {users.map((u) => (
              <option key={u.userId} value={u.userId}>
                {u.displayName}
              </option>
            ))}
          </select>
        </label>
        <label>
          From
          <input type="date" value={filter.from ?? ""} onChange={(e) => set({ from: e.target.value || null })} />
        </label>
        <label>
          To
          <input type="date" value={filter.to ?? ""} onChange={(e) => set({ to: e.target.value || null })} />
        </label>
      </form>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <section className="card" aria-label="Audit entries">
        <table className="sales-table">
          <thead>
            <tr>
              <th scope="col">When</th>
              <th scope="col">What</th>
              <th scope="col">Who</th>
              <th scope="col">On</th>
              <th scope="col">Details</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.auditId} data-testid="audit-row">
                <td>{r.localTime}</td>
                <td>{ACTION[r.action] ?? r.action}</td>
                <td>
                  {r.actorName ?? "System"}
                  {r.approverName && <span className="small"> · approved by {r.approverName}</span>}
                </td>
                <td>
                  {r.entityType === "sale" ? (
                    <button type="button" className="link mono" onClick={() => setSale(r.entityId)}>
                      {r.entityLabel ?? "sale"}
                    </button>
                  ) : r.entityType === "product" ? (
                    <button type="button" className="link" onClick={() => onOpenProduct(r.entityId)}>
                      {r.entityLabel ?? "product"}
                    </button>
                  ) : (
                    <span className="mono">{r.entityLabel ?? r.entityType}</span>
                  )}
                </td>
                <td className="small">{details(r.metadata)}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {rows.length === 0 && !error && <p className="small">Nothing recorded for these filters.</p>}
        {next && (
          <div className="actions">
            <button type="button" className="secondary" onClick={more}>
              Show more
            </button>
          </div>
        )}
      </section>
    </main>
  );
}
