import { useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { PdfPaperDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";

/**
 * Save a receipt as a PDF (E13-T04): no printer needed. Rust renders the stored receipt,
 * opens the system "Save as" dialog and writes the file; cancelling the dialog does nothing.
 */
export function SavePdf({ saleId, returnId }: { saleId: string | null; returnId: string | null }) {
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  async function save(paper: PdfPaperDto) {
    setBusy(true);
    setError(null);
    const res = await commands.receiptExportPdf(saleId, returnId, paper);
    setBusy(false);
    if (res.status === "error") return setError(res.error);
    if (res.data) setSaved(res.data.fileName);
  }

  return (
    <div className="print-controls" aria-live="polite">
      <button type="button" className="secondary" disabled={busy} onClick={() => void save("receipt")}>
        Save as PDF
      </button>
      <button type="button" className="secondary" disabled={busy} onClick={() => void save("a4")}>
        Save A4 PDF
      </button>
      <span className="small" data-testid="pdf-status">
        {busy ? "Saving…" : error ? <span role="alert">The PDF was not saved: {describeError(error)}</span> : saved ? `Saved ${saved}.` : null}
      </span>
    </div>
  );
}
