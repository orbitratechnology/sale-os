import {
  FileDown,
} from "../../ui/icons";
import { useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { PdfPaperDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";
import type { ReceiptSource } from "./ReceiptPreview";

/** Rust renders the stored document and asks where to save it; `data: null` is Cancel. */
function exportPdf(source: ReceiptSource, paper: PdfPaperDto) {
  switch (source.kind) {
    case "sale":
      return commands.receiptExportPdf(source.id, null, paper);
    case "return":
      return commands.receiptExportPdf(null, source.id, paper);
    case "taxInvoice":
      return commands.taxInvoiceExportPdf(source.id, paper);
    case "z":
      return commands.cashSessionZExportPdf(source.id, paper);
  }
}

/**
 * Save a receipt, a tax invoice or a Z summary as a PDF (E13-T04): no printer needed. Rust
 * renders the stored document, opens the system "Save as" dialog and writes the file;
 * cancelling the dialog does nothing.
 */
export function SavePdf({ source }: { source: ReceiptSource }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  async function save(paper: PdfPaperDto) {
    setBusy(true);
    setError(null);
    const res = await exportPdf(source, paper);
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
