import { Modal } from "../../ui/Modal";
import { FileText } from "../../ui/icons";
import { useEffect, useId, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { ReceiptDto } from "../../ipc/bindings";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { describeError } from "../shell/errors";

/**
 * A sale's VAT tax invoice (D-02, Sri Lanka Gazette 2481/22). A VAT-registered buyer asks
 * for one; it states their TIN, name and address. One per sale; once issued it is printed or
 * saved again from here. Rust checks the TINs and builds the invoice from what the sale
 * recorded.
 */
export function TaxInvoicePanel({ saleId, hasVat }: { saleId: string; hasVat: boolean }) {
  const [invoice, setInvoice] = useState<ReceiptDto | null | undefined>(undefined);
  const [asking, setAsking] = useState(false);
  const [show, setShow] = useState(false);

  useEffect(() => {
    let live = true;
    void commands.taxInvoiceGet(saleId, null).then((res) => {
      if (live) setInvoice(res.status === "ok" ? res.data : null);
    });
    return () => {
      live = false;
    };
  }, [saleId]);

  if (invoice === undefined) return null;
  return (
    <section aria-label="Tax invoice" className="tax-invoice">
      <h2>Tax invoice</h2>
      {invoice ? (
        <>
          <p>
            Issued <strong className="mono" data-testid="tax-invoice-number">{invoice.receiptNumber}</strong>.
          </p>
          <PrintControls source={{ kind: "taxInvoice", id: saleId }} autoPrint={false} />
          <div className="actions">
            <button type="button" className="secondary" onClick={() => setShow((s) => !s)}>
              {show ? "Hide tax invoice" : "Show tax invoice"}
            </button>
          </div>
          {show && <ReceiptPreview source={{ kind: "taxInvoice", id: saleId }} />}
        </>
      ) : hasVat ? (
        <div className="actions">
          <button type="button" className="secondary" onClick={() => setAsking(true)}>
            <FileText />
            Issue tax invoice
          </button>
        </div>
      ) : (
        <p className="small">No VAT on this sale, so it has no tax invoice.</p>
      )}
      {asking && (
        <IssueDialog
          saleId={saleId}
          onCancel={() => setAsking(false)}
          onIssued={(r) => {
            setAsking(false);
            setInvoice(r);
            setShow(true);
          }}
        />
      )}
    </section>
  );
}

function IssueDialog({ saleId, onCancel, onIssued }: { saleId: string; onCancel: () => void; onIssued: (r: ReceiptDto) => void }) {
  const [tin, setTin] = useState("");
  const [name, setName] = useState("");
  const [address, setAddress] = useState("");
  const [error, setError] = useState<IpcError | null>(null);
  const [busy, setBusy] = useState(false);
  const titleId = useId();

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    const lines = address
      .split("\n")
      .map((l) => l.trim())
      .filter(Boolean);
    const res = await commands.taxInvoiceIssue(saleId, tin.trim(), name.trim(), lines);
    setBusy(false);
    if (res.status === "ok") onIssued(res.data);
    else setError(res.error);
  }

  return (
    <Modal labelledBy={titleId} onDismiss={onCancel} onSubmit={(e) => void submit(e)}>
      <h2 id={titleId}>Issue a tax invoice</h2>
      <p className="small">As on the buyer's VAT registration certificate. A sale has one tax invoice; it cannot be changed once issued.</p>
      <label>
        Buyer's TIN (9 digits)
        <input value={tin} onChange={(e) => setTin(e.target.value)} inputMode="numeric" autoComplete="off" maxLength={15} data-autofocus required />
      </label>
      <label>
        Buyer's name
        <input value={name} onChange={(e) => setName(e.target.value)} maxLength={120} autoComplete="off" required />
      </label>
      <label>
        Buyer's address (up to 4 lines)
        <textarea value={address} onChange={(e) => setAddress(e.target.value)} rows={3} maxLength={330} required />
      </label>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <div className="actions">
        <button type="button" className="secondary" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" disabled={busy}>
          {busy ? "Issuing…" : "Issue tax invoice"}
        </button>
      </div>
    </Modal>
  );
}
