import { useCallback, useEffect, useState } from "react";
import { commands, type ApprovalScope, type IpcError, type SignInUser } from "../../ipc";
import { ppmToPercent } from "../../lib/decimal";
import { describeError } from "../shell/errors";
import { PinPad } from "./PinPad";

const WHAT: Record<string, string> = {
  discount_above_threshold: "a discount above the cashier limit",
  price_override: "a price change",
  cart_void: "voiding this sale",
  return_create: "a return",
};

/** Words for what is being approved, e.g. "a 15% discount". */
export function describeScope(scope: ApprovalScope): string {
  if (scope.permission === "discount_above_threshold" && scope.maxPpm !== null) {
    return `a ${ppmToPercent(scope.maxPpm)}% discount`;
  }
  return WHAT[scope.permission] ?? "this action";
}

/**
 * Manager approval (E05-T04/T06). Reusable by any command that answers
 * `APPROVAL_REQUIRED`: a manager or owner (never the requester) enters their PIN, and the
 * caller retries the original command with the token. Esc cancels.
 */
export function ApprovalDialog({
  scope,
  requesterId,
  onApproved,
  onCancel,
}: {
  scope: ApprovalScope;
  requesterId: string;
  onApproved: (approvalTokenId: string) => void;
  onCancel: () => void;
}) {
  const [approvers, setApprovers] = useState<SignInUser[]>([]);
  const [approver, setApprover] = useState<SignInUser | null>(null);
  const [pin, setPin] = useState("");
  const [error, setError] = useState<IpcError | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void commands.authListUsers().then((res) => {
      if (res.status !== "ok") return setError(res.error);
      const list = res.data.filter((u) => u.role !== "cashier" && u.userId !== requesterId);
      setApprovers(list);
      if (list.length === 1) setApprover(list[0] ?? null);
    });
  }, [requesterId]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCancel();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [onCancel]);

  const submit = useCallback(async () => {
    if (!approver || busy || pin.length < 4) return;
    setBusy(true);
    const res = await commands.authRequestApproval(approver.userId, pin, scope);
    setBusy(false);
    setPin("");
    if (res.status === "ok") onApproved(res.data.approvalTokenId);
    else setError(res.error);
  }, [approver, busy, pin, scope, onApproved]);

  return (
    <div className="backdrop">
      <div role="dialog" aria-modal="true" aria-label="Manager approval" className="dialog card approval">
        <h2>Manager approval</h2>
        <p>This needs approval: {describeScope(scope)}.</p>
        {approvers.length === 0 && !error && <p className="small">No manager or owner is set up on this till.</p>}
        {!approver ? (
          <ul className="user-grid">
            {approvers.map((u) => (
              <li key={u.userId}>
                <button type="button" className="user-tile" onClick={() => setApprover(u)}>
                  <span className="user-name">{u.displayName}</span>
                  <span className="role-badge">{u.role}</span>
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <>
            <p>
              <strong>{approver.displayName}</strong>, enter your PIN to approve.
            </p>
            <PinPad value={pin} onChange={setPin} onSubmit={() => void submit()} disabled={busy} label="Approver PIN" />
          </>
        )}
        <div aria-live="polite" className="pin-status">
          {busy ? (
            <p className="small">Checking…</p>
          ) : (
            error && (
              <p role="alert" className="field-error">
                {describeError(error)}
              </p>
            )
          )}
        </div>
        <div className="actions">
          {approver && approvers.length > 1 && (
            <button type="button" className="secondary" onClick={() => setApprover(null)}>
              Someone else
            </button>
          )}
          <button type="button" className="secondary" onClick={onCancel}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
