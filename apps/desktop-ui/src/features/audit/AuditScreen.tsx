import {
  ShieldCheck,
} from "../../ui/icons";
import { useEffect, useRef, useState } from "react";
import { commands, type IpcError, type SignInUser, type Workspace } from "../../ipc";
import type { AuditFilterDto, AuditRowDto } from "../../ipc/bindings";
import { RecordView, recordOf, type RecordRef } from "../records/RecordView";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";

/** The "What" filter: an action prefix and its name, in this order (English: shown through `t(…)`). */
const GROUP_COPY: Record<string, string> = {
  "": "Everything",
  "auth.": "Sign-in and approvals",
  "staff.": "Staff",
  "sale.": "Sales",
  "tender.": "External payments",
  "cart.": "Discounts, price changes, voids",
  "return.": "Returns",
  "refund.": "Refunds",
  "inventory.": "Stock",
  "cash_session.": "Cash drawer",
  "receipt.": "Reprints",
  "print.": "Printing",
  "catalog.": "Products",
  "report.": "Exports",
  "mode.": "Practice / live",
};

/**
 * Plain words for the catalogued actions (07-contracts §5), shown through `t(…)`; unknown
 * ones show as stored.
 */
const ACTION_COPY: Record<string, string> = {
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
 * kind, person and dates. A sale, return or stock document opens in place; a product opens the
 * editor.
 */
export function AuditScreen({
  workspace,
  onBack,
  onOpenProduct,
  onReturn,
}: {
  workspace: Workspace;
  onBack: () => void;
  onOpenProduct: (productId: string) => void;
  onReturn: (receipt: string) => void;
}) {
  const t = useT();
  const [filter, setFilter] = useState<AuditFilterDto>({ action: null, userId: null, from: null, to: null });
  const [rows, setRows] = useState<AuditRowDto[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [users, setUsers] = useState<SignInUser[]>([]);
  const [error, setError] = useState<IpcError | null>(null);
  /** An entry's sale, return or stock document, open in place of the log (the log keeps its place). */
  const [record, setRecord] = useState<{ ref: RecordRef; from: string } | null>(null);
  const [focusEntry, setFocusEntry] = useState<string | null>(null);
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
      if (e.key !== "Escape" || record) return;
      e.preventDefault();
      onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [record, onBack]);

  // Back from a record: focus the link that opened it.
  useEffect(() => {
    if (record || !focusEntry) return;
    document.querySelector<HTMLElement>(`[data-entry="${CSS.escape(focusEntry)}"]`)?.focus();
  }, [record, focusEntry]);

  if (record) {
    const back = () => {
      setFocusEntry(record.from);
      setRecord(null);
    };
    return <RecordView record={record.ref} workspace={workspace} backLabel={t("Back to the audit log")} onBack={back} onReturn={onReturn} />;
  }

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
        <h1 className="page-title">
          <ShieldCheck />
          {t("Audit log")}
        </h1>
        <button type="button" className="secondary" onClick={onBack}>
          {t("Back")} <kbd>Esc</kbd>
        </button>
      </header>
      <form className="card sales-filters" role="search" aria-label={t("Filter the audit log")} onSubmit={(e) => e.preventDefault()}>
        <label>
          {t("What")}
          <select value={filter.action ?? ""} onChange={(e) => set({ action: e.target.value || null })}>
            {Object.entries(GROUP_COPY).map(([value, label]) => (
              <option key={value} value={value}>
                {t(label)}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t("Who")}
          <select value={filter.userId ?? ""} onChange={(e) => set({ userId: e.target.value || null })}>
            <option value="">{t("Anyone")}</option>
            {users.map((u) => (
              <option key={u.userId} value={u.userId}>
                {u.displayName}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t("From")}
          <input type="date" value={filter.from ?? ""} onChange={(e) => set({ from: e.target.value || null })} />
        </label>
        <label>
          {t("To")}
          <input type="date" value={filter.to ?? ""} onChange={(e) => set({ to: e.target.value || null })} />
        </label>
      </form>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <section className="card" aria-label={t("Audit entries")}>
        <table className="sales-table">
          <thead>
            <tr>
              <th scope="col">{t("When")}</th>
              <th scope="col">{t("What")}</th>
              <th scope="col">{t("Who")}</th>
              <th scope="col">{t("On")}</th>
              <th scope="col">{t("Details")}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => {
              const ref = recordOf(r.entityType, r.entityId);
              return (
                <tr key={r.auditId} data-testid="audit-row">
                  <td>{r.localTime}</td>
                  <td>{t(ACTION_COPY[r.action] ?? r.action)}</td>
                  <td>
                    {r.actorName ?? t("System")}
                    {r.approverName && <span className="small"> · {t("approved by {name}", { name: r.approverName })}</span>}
                  </td>
                  <td>
                    {ref ? (
                      <button type="button" className="link mono" data-entry={r.auditId} onClick={() => setRecord({ ref, from: r.auditId })}>
                        {r.entityLabel ?? r.entityType.replace("_", " ")}
                      </button>
                    ) : r.entityType === "product" ? (
                      <button type="button" className="link" onClick={() => onOpenProduct(r.entityId)}>
                        {r.entityLabel ?? t("product")}
                      </button>
                    ) : (
                      <span className="mono">{r.entityLabel ?? r.entityType}</span>
                    )}
                  </td>
                  <td className="small">{details(r.metadata)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {rows.length === 0 && !error && (
        <div className="empty-state">
          <span className="empty-icon">
            <ShieldCheck />
          </span>
          <p>{t("Nothing recorded for these filters.")}</p>
        </div>
      )}
        {next && (
          <div className="actions">
            <button type="button" className="secondary" onClick={more}>
              {t("Show more")}
            </button>
          </div>
        )}
      </section>
    </main>
  );
}
