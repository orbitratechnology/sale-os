import { useEffect, useRef, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { PrintJobDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import { printReceipt, type ReceiptSource } from "./ReceiptPreview";

const FAILURE: Record<string, string> = {
  paper_out: "The printer is out of paper. Load paper, then try again.",
  cover_open: "The printer cover is open. Close it, then try again.",
  disconnected: "The printer is not connected. Check the cable or power, then try again.",
  rejected: "The printer refused the receipt. Check it, then try again.",
};

/**
 * Printing a receipt after the sale (E13-T06). A refused job (paper out) can be retried
 * safely. A job with no answer may have printed: the cashier says whether it did, and
 * "not printed" prints a copy marked REPRINT. Nothing here can affect the sale.
 */
export function PrintControls({ source, autoPrint }: { source: ReceiptSource; autoPrint: boolean }) {
  const [job, setJob] = useState<PrintJobDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<IpcError | null>(null);
  const started = useRef(false);

  async function run(call: () => Promise<{ status: "ok"; data: PrintJobDto } | { status: "error"; error: IpcError }>) {
    setBusy(true);
    setError(null);
    const res = await call();
    setBusy(false);
    if (res.status === "ok") setJob(res.data);
    else setError(res.error);
  }

  const print = () => printReceipt(source);

  useEffect(() => {
    // Auto-print once, after the sale has been committed (RCP-006).
    if (autoPrint && !started.current) {
      started.current = true;
      void run(print);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once per mount
  }, [autoPrint]);

  const status = (() => {
    if (busy) return <span>Printing…</span>;
    if (error) return <span role="alert">{describeError(error)}</span>;
    if (!job) return null;
    if (job.status === "completed") return <span>{job.reprint ? "Reprint printed." : "Printed."}</span>;
    if (job.status === "failed")
      return (
        <span role="alert">
          {FAILURE[job.errorCode ?? "rejected"] ?? FAILURE.rejected}{" "}
          <button type="button" className="secondary" onClick={() => void run(() => commands.printJobRetry(job.jobId))}>
            Try again
          </button>
        </span>
      );
    if (job.status === "unknown")
      return (
        <span role="alert">
          Print status unknown — did it print?{" "}
          <button type="button" className="secondary" onClick={() => void run(() => commands.printJobResolve(job.jobId, true))}>
            Printed
          </button>{" "}
          <button type="button" className="secondary" onClick={() => void run(() => commands.printJobResolve(job.jobId, false))}>
            Not printed — reprint
          </button>
        </span>
      );
    return <span>{job.status}</span>;
  })();

  return (
    <div className="print-controls" aria-live="polite">
      <button type="button" className="secondary" disabled={busy} onClick={() => void run(print)}>
        {job ? "Print another copy" : "Print receipt"}
      </button>
      <span className="small" data-testid="print-status">
        {status}
      </span>
    </div>
  );
}
