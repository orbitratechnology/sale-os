import { ArrowLeft, ClipboardList, Undo2 } from "../../ui/icons";
import { useEffect, useState } from "react";
import { commands, type IpcError, type Workspace } from "../../ipc";
import type { StockDocumentDto_Serialize as StockDocumentDto } from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { SaleDetail } from "../sales/SaleDetail";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";

/** What a stock movement or an audit entry points at. */
export type RecordRef = { kind: "sale" | "return" | "document"; id: string };

/** A history row's `sourceType` (or an audit entry's `entityType`) as a record, if it is one. */
export function recordOf(type: string, id: string): RecordRef | null {
  if (type === "sale") return { kind: "sale", id };
  if (type === "return") return { kind: "return", id };
  if (type === "inventory_document") return { kind: "document", id };
  return null;
}

/**
 * One sale, return or stock document, opened from the stock history or the audit log.
 * Esc or the back button returns to where it was opened from.
 */
export function RecordView({
  record,
  workspace,
  backLabel,
  onBack,
  onReturn,
}: {
  record: RecordRef;
  workspace: Workspace;
  /** The back button's text, already translated. */
  backLabel: string;
  onBack: () => void;
  /** "Return items" on a sale. */
  onReturn: (receiptNumber: string) => void;
}) {
  if (record.kind === "sale") return <SaleDetail saleId={record.id} backLabel={backLabel} onBack={onBack} onReturn={onReturn} />;
  if (record.kind === "return") return <ReturnRecord returnId={record.id} backLabel={backLabel} onBack={onBack} />;
  return <DocumentRecord documentId={record.id} workspace={workspace} backLabel={backLabel} onBack={onBack} />;
}

function useEscape(onBack: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack]);
}

function BackButton({ label, onBack }: { label: string; onBack: () => void }) {
  return (
    <button type="button" className="secondary" onClick={onBack}>
      <ArrowLeft />
      {label} <kbd>Esc</kbd>
    </button>
  );
}

/** A return as recorded: its stored refund receipt, which can be printed again. */
function ReturnRecord({ returnId, backLabel, onBack }: { returnId: string; backLabel: string; onBack: () => void }) {
  const t = useT();
  useEscape(onBack);
  return (
    <main className="sales">
      <header className="returns-header">
        <h1 className="page-title" tabIndex={-1}>
          <Undo2 />
          {t("Return")}
        </h1>
        <BackButton label={backLabel} onBack={onBack} />
      </header>
      <section className="card sale-detail" aria-label={t("Return detail")}>
        <PrintControls source={{ kind: "return", id: returnId }} autoPrint={false} />
        <ReceiptPreview source={{ kind: "return", id: returnId }} />
      </section>
    </main>
  );
}

const DOC_TYPE_COPY: Record<string, string> = {
  opening: "Opening stock",
  receipt: "Goods received",
  adjustment: "Stock adjustment",
  write_off: "Write-off",
};

const STOCK_REASON_COPY: Record<string, string> = {
  damage: "Damaged",
  loss: "Lost",
  theft: "Theft",
  count_correction: "Count correction",
  expired: "Expired",
  other: "Other",
};

/** A posted stock document: who posted it, why, and each movement it made. */
function DocumentRecord({ documentId, workspace, backLabel, onBack }: { documentId: string; workspace: Workspace; backLabel: string; onBack: () => void }) {
  const t = useT();
  const [doc, setDoc] = useState<StockDocumentDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  useEscape(onBack);
  useEffect(() => {
    let live = true;
    void commands.stockDocumentGet(documentId).then((res) => {
      if (!live) return;
      if (res.status === "ok") setDoc(res.data);
      else setError(res.error);
    });
    return () => {
      live = false;
    };
  }, [documentId]);
  const money = (minor: string) => formatMoney(minor, { exponent: workspace.currencyExponent });
  const showCost = doc?.lines.some((l) => l.unitCostMinor !== null) ?? false;

  return (
    <main className="sales" aria-busy={!doc && !error}>
      <header className="returns-header">
        <h1 className="page-title" tabIndex={-1}>
          <ClipboardList />
          {doc ? (
            <>
              {t(DOC_TYPE_COPY[doc.docType] ?? doc.docType)} <span className="mono">{doc.documentNo}</span>
            </>
          ) : (
            t("Stock document")
          )}
        </h1>
        <BackButton label={backLabel} onBack={onBack} />
      </header>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      {doc && (
        <section className="card sale-detail" aria-label={t("Stock document")}>
          <p>
            {doc.localTime} · {t("posted by {name}", { name: doc.actorName })}
            {doc.approverName ? ` · ${t("approved by {name}", { name: doc.approverName })}` : ""}
          </p>
          {(doc.reason || doc.supplierName || doc.reference || doc.note) && (
            <dl className="totals" aria-label={t("About this document")}>
              {doc.reason && <Row label={t("Reason")} value={t(STOCK_REASON_COPY[doc.reason] ?? doc.reason)} />}
              {doc.supplierName && <Row label={t("Supplier")} value={doc.supplierName} />}
              {doc.reference && <Row label={t("Reference")} value={doc.reference} />}
              {doc.note && <Row label={t("Note")} value={doc.note} />}
            </dl>
          )}
          <table className="sales-table" aria-label={t("Lines")}>
            <thead>
              <tr>
                <th scope="col">{t("Item")}</th>
                <th scope="col">{t("Stock")}</th>
                <th scope="col" className="num">
                  {t("stock quantity::Change")}
                </th>
                {showCost && (
                  <th scope="col" className="num">
                    {t("Unit cost")}
                  </th>
                )}
              </tr>
            </thead>
            <tbody>
              {doc.lines.map((l, i) => (
                <tr key={i} data-testid="document-line">
                  <th scope="row">
                    {l.name}
                    {l.sku && <span className="small"> · {l.sku}</span>}
                  </th>
                  <td>{l.bucket === "damaged" ? t("Damaged") : t("Sellable")}</td>
                  <td className="num">{l.deltaMilli.startsWith("-") ? formatMilli(l.deltaMilli) : `+${formatMilli(l.deltaMilli)}`}</td>
                  {showCost && <td className="num">{l.unitCostMinor === null ? "" : money(l.unitCostMinor)}</td>}
                </tr>
              ))}
            </tbody>
          </table>
          <p className="small">{t("A posted document cannot be changed. To correct it, post an adjustment.")}</p>
        </section>
      )}
    </main>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}
