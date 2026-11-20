import { useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { PaymentAttemptDto } from "../../ipc/bindings";
import type { ApprovalScope } from "../../ipc";
import { scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { ApprovalDialog } from "../auth/ApprovalDialog";
import { describeError } from "../shell/errors";

/** How often to ask the terminal while the customer pays, and for how long at most. */
const POLL_MS = 1000;
const POLL_LIMIT = 150;

/**
 * A card payment on the integrated terminal (E22-T02, PAY-009/010). The amount goes to the
 * terminal; the till then follows the attempt until the terminal answers. Only an approval
 * from the terminal becomes a tender ("Approved by terminal"); a timeout or unknown outcome
 * is never treated as declined: the cashier checks the terminal and a manager settles it.
 */
export function TerminalPayment({
  cartId,
  remainingMinor,
  exponent,
  currencyCode,
  requesterId,
  resume,
  onApproved,
  onBack,
}: {
  cartId: string;
  remainingMinor: bigint;
  exponent: number;
  currencyCode: string;
  requesterId: string;
  /** An attempt already in progress (after reopening the panel or a restart). */
  resume: PaymentAttemptDto | null;
  onApproved: (a: PaymentAttemptDto) => void;
  onBack: () => void;
}) {
  const [amount, setAmount] = useState("");
  const [attempt, setAttempt] = useState<PaymentAttemptDto | null>(resume);
  const [error, setError] = useState<IpcError | string | null>(null);
  const [busy, setBusy] = useState(false);
  const [settle, setSettle] = useState<{ outcome: "approved" | "declined"; reference: string; reason: string } | null>(null);
  const [approval, setApproval] = useState<{ scope: ApprovalScope; retry: (token: string) => void } | null>(null);
  const polls = useRef(0);
  const money = (m: bigint | string) => formatMoney(m.toString(), { exponent });

  // Follow a pending attempt until the terminal answers.
  useEffect(() => {
    if (!attempt || attempt.status !== "pending") return;
    if (polls.current >= POLL_LIMIT) return;
    const t = setTimeout(() => {
      polls.current += 1;
      void commands.paymentPoll(attempt.attemptId).then((res) => {
        if (res.status === "ok") setAttempt(res.data);
        else setError(res.error);
      });
    }, POLL_MS);
    return () => clearTimeout(t);
  }, [attempt]);

  useEffect(() => {
    if (attempt?.usable) onApproved(attempt);
  }, [attempt, onApproved]);

  async function start(e: FormEvent) {
    e.preventDefault();
    const minor = amount.trim() ? scaleDecimal(amount, exponent) : remainingMinor.toString();
    if (minor === null || minor === "0") return setError("Enter the amount to charge.");
    if (BigInt(minor) > remainingMinor) return setError("A card payment cannot be more than the amount due.");
    setBusy(true);
    setError(null);
    polls.current = 0;
    const res = await commands.paymentStart(cartId, minor);
    setBusy(false);
    if (res.status === "ok") setAttempt(res.data);
    else setError(res.error);
  }

  async function cancel() {
    if (!attempt) return;
    setBusy(true);
    const res = await commands.paymentCancel(attempt.attemptId);
    setBusy(false);
    if (res.status === "ok") setAttempt(res.data);
    else setError(res.error);
  }

  async function resolve(token: string | null) {
    if (!attempt || !settle) return;
    setBusy(true);
    const res = await commands.paymentResolve(
      attempt.attemptId,
      { outcome: settle.outcome, reference: settle.reference.trim() || null },
      settle.reason,
      token,
    );
    setBusy(false);
    if (res.status === "ok") {
      setSettle(null);
      setAttempt(res.data);
    } else if (res.error.code === "APPROVAL_REQUIRED" && res.error.details?.scope) {
      setApproval({ scope: res.error.details.scope, retry: (t) => void resolve(t) });
    } else setError(res.error);
  }

  const status = attempt?.status;
  return (
    <div className="external-form" aria-label="Card on terminal">
      {!attempt || status === "declined" || status === "cancelled" ? (
        <form onSubmit={start}>
          {attempt && (
            <p role="alert" className="field-error">
              {status === "declined" ? `Declined: ${attempt.reason ?? "by the card issuer"}` : "Cancelled. Nothing was charged."}
            </p>
          )}
          <label>
            Amount to charge ({currencyCode}) · empty = the rest, {money(remainingMinor)}
            <input autoFocus inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} disabled={busy} />
          </label>
          <div className="actions">
            <button type="button" className="secondary" onClick={onBack} disabled={busy}>
              Back
            </button>
            <button type="submit" disabled={busy}>
              {busy ? "Sending…" : "Send to terminal"}
            </button>
          </div>
        </form>
      ) : status === "pending" ? (
        <div role="status" aria-live="polite">
          <p>
            <strong>{money(attempt.amountMinor)}</strong> sent to the terminal. Waiting for the customer…
          </p>
          {polls.current >= POLL_LIMIT && <p className="small">No answer yet. Check the terminal; the outcome will be settled, never guessed.</p>}
          <div className="actions">
            <button type="button" className="secondary" onClick={() => void cancel()} disabled={busy}>
              Cancel on terminal
            </button>
          </div>
        </div>
      ) : status === "reconciliation_required" ? (
        <div role="alert">
          <p>
            <strong>Outcome unknown.</strong> {attempt.reason ?? "The terminal did not confirm."} The card may have been charged:
            look at the terminal or its slip.
          </p>
          {!settle ? (
            <div className="actions">
              <button type="button" className="secondary" onClick={() => void commands.paymentPoll(attempt.attemptId).then((r) => r.status === "ok" && setAttempt(r.data))}>
                Ask the terminal again
              </button>
              <button type="button" onClick={() => setSettle({ outcome: "approved", reference: "", reason: "" })}>
                It was approved
              </button>
              <button type="button" className="secondary" onClick={() => setSettle({ outcome: "declined", reference: "", reason: "" })}>
                It was not charged
              </button>
            </div>
          ) : (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void resolve(null);
              }}
            >
              {settle.outcome === "approved" && (
                <label>
                  Reference from the slip (optional, never the card number)
                  <input value={settle.reference} maxLength={64} autoComplete="off" onChange={(e) => setSettle({ ...settle, reference: e.target.value })} />
                </label>
              )}
              <label>
                Reason (kept in the audit log)
                <input required value={settle.reason} maxLength={200} onChange={(e) => setSettle({ ...settle, reason: e.target.value })} />
              </label>
              <div className="actions">
                <button type="button" className="secondary" onClick={() => setSettle(null)}>
                  Back
                </button>
                <button type="submit" disabled={busy || !settle.reason.trim()}>
                  {settle.outcome === "approved" ? "Record as approved" : "Record as not charged"}
                </button>
              </div>
            </form>
          )}
        </div>
      ) : (
        <p role="status">{status === "approved" ? "Approved." : `The payment is ${status?.replace("_", " ")}.`}</p>
      )}
      {error && (
        <p role="alert" className="field-error">
          {typeof error === "string" ? error : describeError(error)}
        </p>
      )}
      {approval && (
        <ApprovalDialog
          scope={approval.scope}
          requesterId={requesterId}
          onCancel={() => setApproval(null)}
          onApproved={(token) => {
            const a = approval;
            setApproval(null);
            a.retry(token);
          }}
        />
      )}
    </div>
  );
}
