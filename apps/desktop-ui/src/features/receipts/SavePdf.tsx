import {
  FileDown,
} from "../../ui/icons";
import { useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { PdfPaperDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";

/**
 * Save a receipt as a PDF (E13-T04): no printer needed. Rust renders the stored receipt,
 * opens the system "Save as" dialog and writes the file; cancelling the dialog does nothing.
 */
export function SavePdf({
  saleId,
  returnId,
  taxInvoiceOf,
}: {
  saleId: string | null;
  returnId: string | null;
  /** The sale whose tax invoice to save (D-02), instead of a receipt. */
  taxInvoiceOf?: string;
}) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  async function save(paper: PdfPaperDto) {
    setBusy(true);
    setError(null);
    const res = taxInvoiceOf ? await commands.taxInvoiceExportPdf(taxInvoiceOf, paper) : await commands.receiptExportPdf(saleId, returnId, paper);
    setBusy(false);
    if (res.status === "error") return setError(res.error);
    if (res.data) setSaved(res.data.fileName);
  }

  return (
    <div className="print-controls" aria-live="polite">
      <button type="button" className="secondary" disabled={busy} onClick={() => void save("receipt")}>
        <FileDown />
        {t("Save as PDF")}
      </button>
      <button type="button" className="secondary" disabled={busy} onClick={() => void save("a4")}>
        <FileDown />
        {t("Save A4 PDF")}
      </button>
      <span className="small" data-testid="pdf-status">
        {busy ? (
          t("Saving…")
        ) : error ? (
          <span role="alert">{t("The PDF was not saved: {why}", { why: describeError(error) })}</span>
        ) : saved ? (
          t("Saved {file}.", { file: saved })
        ) : null}
      </span>
    </div>
  );
}
