import { useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { ReceiptDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";

/**
 * Whose receipt: a sale's (`id: null` is the last sale on this till), a return's, a closed
 * cash session's Z summary (`id: null` is the last one), or a sale's tax invoice (D-02).
 */
export type ReceiptSource =
  | { kind: "sale"; id: string | null }
  | { kind: "return"; id: string }
  | { kind: "z"; id: string | null }
  | { kind: "taxInvoice"; id: string };

/** The stored document rendered for a width; `data: null` only for a sale without a tax invoice. */
export function getReceipt(source: ReceiptSource, width: 58 | 80 | null) {
  if (source.kind === "sale") return commands.receiptGet(source.id, width);
  if (source.kind === "return") return commands.returnReceiptGet(source.id, width);
  if (source.kind === "taxInvoice") return commands.taxInvoiceGet(source.id, width);
  return commands.cashSessionZGet(source.id, width);
}

export function printReceipt(source: ReceiptSource) {
  if (source.kind === "sale") return commands.receiptPrint(source.id);
  if (source.kind === "return") return commands.returnReceiptPrint(source.id);
  if (source.kind === "taxInvoice") return commands.taxInvoicePrint(source.id);
  return commands.cashSessionZPrint(source.id);
}

/**
 * A receipt exactly as it prints (E13-T03): Rust renders the stored document into lines for
 * 32 or 48 columns, and this shows them on a monospace grid. Paper width can be switched.
 */
export function ReceiptPreview({ source }: { source: ReceiptSource }) {
  const [width, setWidth] = useState<58 | 80 | null>(null);
  const [receipt, setReceipt] = useState<ReceiptDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  /** A sale without a tax invoice (the only source that can be absent). */
  const [absent, setAbsent] = useState(false);

  // Callers pass a new object each render: depend on what it says, not on its identity.
  const { kind, id } = source;
  useEffect(() => {
    let live = true;
    void getReceipt({ kind, id } as ReceiptSource, width).then((res) => {
      if (!live) return;
      if (res.status === "ok") {
        setReceipt(res.data);
        setAbsent(res.data === null);
        setError(null);
      } else setError(res.error);
    });
    return () => {
      live = false;
    };
  }, [kind, id, width]);

  if (error) return <p role="alert">{describeError(error)}</p>;
  if (absent) return <p className="small">No tax invoice was issued for this sale.</p>;
  if (!receipt) return <p className="small">Loading receipt…</p>;
  return (
    <figure className="receipt" aria-label={`${kind === "taxInvoice" ? "Tax invoice" : "Receipt"} ${receipt.receiptNumber}`}>
      <div className="receipt-widths" role="group" aria-label="Paper width">
        {([58, 80] as const).map((w) => (
          <button key={w} type="button" className={receipt.columns === (w === 58 ? 32 : 48) ? "" : "secondary"} onClick={() => setWidth(w)}>
            {w} mm
          </button>
        ))}
      </div>
      <pre className="receipt-paper" style={{ width: `${receipt.columns + 2}ch` }} data-testid="receipt-text">
        {receipt.lines.map((l, i) => (
          <span key={i} className={l.bold ? "receipt-bold" : undefined}>
            {l.text}
            {"\n"}
          </span>
        ))}
      </pre>
    </figure>
  );
}
