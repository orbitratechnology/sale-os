import { useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { ReceiptDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";

/** Whose receipt: a sale's (`id: null` is the last sale on this till) or a return's. */
export type ReceiptSource = { kind: "sale"; id: string | null } | { kind: "return"; id: string };

/**
 * A receipt exactly as it prints (E13-T03): Rust renders the stored document into lines for
 * 32 or 48 columns, and this shows them on a monospace grid. Paper width can be switched.
 */
export function ReceiptPreview({ source }: { source: ReceiptSource }) {
  const [width, setWidth] = useState<58 | 80 | null>(null);
  const [receipt, setReceipt] = useState<ReceiptDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  useEffect(() => {
    let live = true;
    const get = source.kind === "sale" ? commands.receiptGet(source.id, width) : commands.returnReceiptGet(source.id, width);
    void get.then((res) => {
      if (!live) return;
      if (res.status === "ok") {
        setReceipt(res.data);
        setError(null);
      } else setError(res.error);
    });
    return () => {
      live = false;
    };
  }, [source.kind, source.id, width]);

  if (error) return <p role="alert">{describeError(error)}</p>;
  if (!receipt) return <p className="small">Loading receipt…</p>;
  return (
    <figure className="receipt" aria-label={`Receipt ${receipt.receiptNumber}`}>
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
