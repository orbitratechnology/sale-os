import { Modal } from "../../ui/Modal";
import { useCallback, useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { ParkedCartDto } from "../../ipc/bindings";
import { formatMoney } from "../../lib/money";
import { describeError } from "../shell/errors";
import type { PendingApproval } from "./useCart";
import { useT } from "../../i18n";

function parkedTime(iso: string): string {
  const d = new Date(iso);
  const sameDay = d.toDateString() === new Date().toDateString();
  return d.toLocaleString([], sameDay ? { timeStyle: "short", hourCycle: "h23" } : { dateStyle: "short", timeStyle: "short", hourCycle: "h23" });
}

/**
 * The parked carts on this till (POS-012), oldest first: resume one (when the current cart is
 * empty) or discard one with a reason (a cashier needs a manager's approval, as for a void).
 * `↑`/`↓` choose, `Enter` resumes, `Esc` closes.
 */
export function ParkedDialog({
  exponent,
  canResume,
  onResume,
  onClose,
  onApproval,
  onCount,
  paused,
}: {
  exponent: number;
  /** The current cart is empty. */
  canResume: boolean;
  onResume: (cartId: string) => void;
  onClose: () => void;
  onApproval: (pending: PendingApproval) => void;
  onCount: (n: number) => void;
  /** Another dialog (the approval PIN pad) owns the keyboard. */
  paused: boolean;
}) {
  const t = useT();
  const [list, setList] = useState<ParkedCartDto[] | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [selected, setSelected] = useState(0);
  const [discarding, setDiscarding] = useState<ParkedCartDto | null>(null);
  const [reason, setReason] = useState("");

  const show = useCallback(
    (rows: ParkedCartDto[]) => {
      setList(rows);
      onCount(rows.length);
      setSelected((s) => Math.min(s, Math.max(rows.length - 1, 0)));
    },
    [onCount],
  );

  useEffect(() => {
    void commands.cartParkedList().then((res) => (res.status === "ok" ? show(res.data) : setError(res.error)));
  }, [show]);

  const discard = async (cart: ParkedCartDto, why: string, token: string | null): Promise<boolean> => {
    const res = await commands.cartDiscardParked(cart.cartId, why, token);
    if (res.status === "ok") {
      setDiscarding(null);
      setReason("");
      setError(null);
      show(res.data);
      return true;
    }
    const scope = res.error.details?.scope;
    if (res.error.code === "APPROVAL_REQUIRED" && scope) {
      onApproval({
        scope: { permission: scope.permission, entityId: scope.entityId, maxPpm: scope.maxPpm ?? null },
        retry: (token) => discard(cart, why, token),
      });
      return false;
    }
    setError(res.error);
    return false;
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (paused) return;
      if (discarding) {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          setDiscarding(null);
        }
        return;
      }
      const rows = list ?? [];
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelected((s) => Math.min(s + 1, rows.length - 1));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelected((s) => Math.max(s - 1, 0));
      } else if (e.key === "Enter" && canResume && rows[selected] && !isControl(e.target)) {
        // Enter on a focused button (Discard…, Close) does what that button says; only Enter
        // on the list itself resumes the selected cart.
        e.preventDefault();
        onResume(rows[selected].cartId);
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [list, selected, canResume, discarding, paused, onClose, onResume]);

  return (
    <Modal label={t("Parked carts")} className="wide" focusSelf>
        <h2>{t("Parked carts")}</h2>
        {error && (
          <p role="alert" className="field-error">
            {describeError(error)}
          </p>
        )}
        {!canResume && <p className="small">{t("Park or finish the current sale to resume one of these.")}</p>}
        {list === null && !error && <p aria-busy="true">{t("Loading…")}</p>}
        {list?.length === 0 && <p>{t("No carts are parked on this till.")}</p>}
        {list && list.length > 0 && (
          <table className="sales-table" aria-label={t("Parked carts")}>
            <thead>
              <tr>
                <th scope="col">{t("Cart")}</th>
                <th scope="col">{t("Parked")}</th>
                <th scope="col">{t("By")}</th>
                <th scope="col" className="num">
                  {t("Items")}
                </th>
                <th scope="col" className="num">
                  {t("Total now")}
                </th>
                <th scope="col">
                  <span className="visually-hidden">{t("Actions")}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {list.map((p, i) => (
                <tr key={p.cartId} data-testid="parked-row" aria-selected={i === selected} className={i === selected ? "selected" : undefined} onClick={() => setSelected(i)}>
                  <th scope="row">{p.label ?? t("Cart {n}", { n: i + 1 })}</th>
                  <td>{parkedTime(p.parkedAt)}</td>
                  <td>{p.parkedByName}</td>
                  <td className="num">{p.lines}</td>
                  <td className="num">{formatMoney(p.totalMinor, { exponent })}</td>
                  <td>
                    <button type="button" disabled={!canResume} onClick={() => onResume(p.cartId)}>
                      {t("Resume")}
                    </button>{" "}
                    <button
                      type="button"
                      className="secondary"
                      onClick={() => {
                        setReason("");
                        setDiscarding(p);
                      }}
                    >
                      {t("Discard…")}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {discarding && (
          <form
            role="group"
            aria-label={t("Discard a parked cart")}
            onSubmit={(e) => {
              e.preventDefault();
              if (reason.trim()) void discard(discarding, reason.trim(), null);
            }}
          >
            <p>
              {t("Discard {cart} ({items})? It is voided and kept for the audit trail.", {
                cart: discarding.label ?? t("this cart"),
                items: discarding.lines === 1 ? t("1 item") : t("{n} items", { n: discarding.lines }),
              })}
            </p>
            <label>
              {t("Reason")}
              <input autoFocus value={reason} onChange={(e) => setReason(e.target.value)} />
            </label>
            <div className="actions">
              <button type="button" className="secondary" onClick={() => setDiscarding(null)}>
                {t("Keep it")}
              </button>
              <button type="submit" className="danger" disabled={!reason.trim()}>
                {t("Discard cart")}
              </button>
            </div>
          </form>
        )}
        <div className="actions">
          <button type="button" className="secondary" onClick={onClose}>
            {t("Close")}
          </button>
        </div>
    </Modal>
  );
}

/** A control that handles Enter itself: the dialog's own keys leave it alone. */
function isControl(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest("button, input, select, textarea, a[href]") !== null;
}
