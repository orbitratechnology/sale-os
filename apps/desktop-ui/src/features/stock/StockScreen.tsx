import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type IpcError, type Workspace } from "../../ipc";
import type { AdjustReason, DocumentType, PostedDocumentDto } from "../../ipc/bindings";
import { formatMilli, scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { useScanCapture } from "../scan/useScanCapture";
import { describeError } from "../shell/errors";
import { StockOverview } from "./StockOverview";

const KINDS: { value: DocumentType; label: string; post: string }[] = [
  { value: "receipt", label: "Receive goods", post: "Post goods received" },
  { value: "opening", label: "Opening stock", post: "Post opening stock" },
  { value: "adjustment", label: "Adjust stock", post: "Post adjustment" },
  { value: "write_off", label: "Write off damaged", post: "Post write-off" },
];

const REASONS: { value: AdjustReason; label: string; hint: string }[] = [
  { value: "damage", label: "Damaged", hint: "Moves the quantity from sellable to damaged stock." },
  { value: "loss", label: "Lost", hint: "Removes the quantity from sellable stock." },
  { value: "theft", label: "Theft", hint: "Removes the quantity from sellable stock." },
  { value: "expired", label: "Expired", hint: "Removes the quantity from sellable stock." },
  { value: "count_correction", label: "Count correction", hint: "Use − to remove (counted fewer) or + to add." },
  { value: "other", label: "Other (note required)", hint: "Use − to remove or + to add; say why in the note." },
];

interface Line {
  variantId: string;
  name: string;
  qty: string;
  cost: string;
}

/** "1.5" → "1500", "-2" → "-2000"; null if not a number. */
function signedMilli(input: string, allowSign: boolean): string | null {
  const t = input.trim();
  const negative = t.startsWith("-");
  if (negative && !allowSign) return null;
  const milli = scaleDecimal(negative || t.startsWith("+") ? t.slice(1) : t, 3);
  return milli === null ? null : negative && milli !== "0" ? `-${milli}` : milli;
}

/**
 * Stock (E11-T02/T03): receive goods, enter opening stock, adjust with a reason, write off
 * damaged stock. Scan items (or type a code) to add lines; a second scan adds to the line.
 * Posting is one transaction in Rust; a posted document cannot be edited, only corrected by
 * a new one.
 */
export function StockScreen({ workspace, canPost, onBack }: { workspace: Workspace; canPost: boolean; onBack: () => void }) {
  // Managers and owners land on posting (receiving is the common job); cashiers only see stock.
  const [view, setView] = useState<"post" | "onHand">(canPost ? "post" : "onHand");
  if (view === "onHand" || !canPost) {
    return (
      <main className="sales">
        <StockHeader canPost={canPost} view="onHand" onView={setView} onBack={onBack} />
        <StockOverview />
      </main>
    );
  }
  return <PostDocument workspace={workspace} onBack={onBack} onView={setView} />;
}

function StockHeader({
  canPost,
  view,
  onView,
  onBack,
}: {
  canPost: boolean;
  view: "post" | "onHand";
  onView: (v: "post" | "onHand") => void;
  onBack: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack]);
  return (
    <>
      <header className="returns-header">
        <h1>Stock</h1>
        <button type="button" className="secondary" onClick={onBack}>
          Back to till <kbd>Esc</kbd>
        </button>
      </header>
      {canPost && (
        <div className="receipt-widths" role="tablist" aria-label="Stock">
          <button type="button" role="tab" aria-selected={view === "post"} className={view === "post" ? "" : "secondary"} onClick={() => onView("post")}>
            Receive and adjust
          </button>
          <button type="button" role="tab" aria-selected={view === "onHand"} className={view === "onHand" ? "" : "secondary"} onClick={() => onView("onHand")}>
            On hand
          </button>
        </div>
      )}
    </>
  );
}

function PostDocument({
  workspace,
  onBack,
  onView,
}: {
  workspace: Workspace;
  onBack: () => void;
  onView: (v: "post" | "onHand") => void;
}) {
  const [kind, setKind] = useState<DocumentType>("receipt");
  const [reason, setReason] = useState<AdjustReason>("damage");
  const [lines, setLines] = useState<Line[]>([]);
  const [code, setCode] = useState("");
  const [supplier, setSupplier] = useState("");
  const [reference, setReference] = useState("");
  const [note, setNote] = useState("");
  const [error, setError] = useState<IpcError | string | null>(null);
  const [posted, setPosted] = useState<PostedDocumentDto | null>(null);
  const [busy, setBusy] = useState(false);
  const codeInput = useRef<HTMLInputElement | null>(null);
  const costs = kind === "receipt" || kind === "opening";
  const signed = kind === "adjustment" && (reason === "count_correction" || reason === "other");
  const postLabel = KINDS.find((k) => k.value === kind)?.post ?? "Post";

  useEffect(() => codeInput.current?.focus(), []);

  const add = useCallback(async (raw: string) => {
    const text = raw.trim();
    if (!text) return;
    const res = await commands.catalogLookupCode(text);
    if (res.status !== "ok") return setError(res.error);
    const hit = res.data;
    if (!hit) return setError(`No item has the code ${text}.`);
    if (!hit.trackStock) return setError(`${hit.name} does not track stock.`);
    setError(null);
    setPosted(null);
    const by = BigInt(hit.scanQtyMilli);
    setLines((ls) => {
      const at = ls.findIndex((l) => l.variantId === hit.variantId);
      const name = hit.label ? `${hit.name} ${hit.label}` : hit.name;
      if (at < 0) {
        return [...ls, { variantId: hit.variantId, name, qty: formatMilli(by.toString()), cost: "" }];
      }
      // Scanning it again adds to the line.
      return ls.map((l, i) => {
        if (i !== at) return l;
        const current = signedMilli(l.qty, true);
        return current === null ? l : { ...l, qty: formatMilli((BigInt(current) + by).toString()) };
      });
    });
  }, []);

  useScanCapture((scan) => {
    if (scan.inScanField) setCode("");
    void add(scan.raw);
  });

  function enterCode(e: FormEvent) {
    e.preventDefault();
    void add(code);
    setCode("");
  }

  async function post() {
    if (busy) return;
    const out = [];
    for (const l of lines) {
      const milli = signedMilli(l.qty, signed);
      if (milli === null || milli === "0") return setError(`Check the quantity for ${l.name}.`);
      const cost = costs && l.cost.trim() ? scaleDecimal(l.cost, workspace.currencyExponent) : null;
      if (costs && l.cost.trim() && cost === null) return setError(`Check the unit cost for ${l.name}.`);
      out.push({ variantId: l.variantId, quantityMilli: milli, unitCostMinor: cost });
    }
    setBusy(true);
    const res = await commands.inventoryPostDocument({
      docType: kind,
      reason: kind === "adjustment" ? reason : null,
      supplierName: costs ? supplier.trim() || null : null,
      reference: costs ? reference.trim() || null : null,
      note: note.trim() || null,
      lines: out,
    });
    setBusy(false);
    if (res.status !== "ok") return setError(res.error);
    setPosted(res.data);
    setError(null);
    setLines([]);
    setSupplier("");
    setReference("");
    setNote("");
    codeInput.current?.focus();
  }

  const errorText = error === null ? null : typeof error === "string" ? error : describeError(error);

  return (
    <main className="sales">
      <StockHeader canPost view="post" onView={onView} onBack={onBack} />
      <section className="card return-form" aria-label="Stock document">
        <div className="receipt-widths" role="radiogroup" aria-label="What are you doing?">
          {KINDS.map((k) => (
            <button
              key={k.value}
              type="button"
              role="radio"
              aria-checked={kind === k.value}
              className={kind === k.value ? "" : "secondary"}
              onClick={() => {
                setKind(k.value);
                setPosted(null);
                setError(null);
                codeInput.current?.focus(); // ready for the next scan
              }}
            >
              {k.label}
            </button>
          ))}
        </div>
        {kind === "adjustment" && (
          <label>
            Reason
            <select value={reason} onChange={(e) => setReason(e.target.value as AdjustReason)}>
              {REASONS.map((r) => (
                <option key={r.value} value={r.value}>
                  {r.label}
                </option>
              ))}
            </select>
            <span className="small">{REASONS.find((r) => r.value === reason)?.hint}</span>
          </label>
        )}
        {kind === "write_off" && <p className="small">Takes the quantity out of damaged stock (it cannot be sold).</p>}

        <form onSubmit={enterCode} className="stock-code">
          <label>
            Scan or type a code
            <input
              ref={codeInput}
              data-scan-field
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="Barcode or SKU, then Enter"
              autoComplete="off"
              spellCheck={false}
            />
          </label>
        </form>

        <table className="sales-table" aria-label="Lines">
          <thead>
            <tr>
              <th scope="col">Item</th>
              <th scope="col">Quantity</th>
              {costs && <th scope="col">Unit cost (optional)</th>}
              <th scope="col">
                <span className="visually-hidden">Remove</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {lines.map((l, i) => (
              <tr key={l.variantId} data-testid="stock-line">
                <th scope="row">{l.name}</th>
                <td>
                  <input
                    className="qty-input"
                    aria-label={`Quantity: ${l.name}`}
                    inputMode="decimal"
                    value={l.qty}
                    onChange={(e) => setLines((ls) => ls.map((x, j) => (j === i ? { ...x, qty: e.target.value } : x)))}
                  />
                </td>
                {costs && (
                  <td>
                    <input
                      className="qty-input"
                      aria-label={`Unit cost: ${l.name}`}
                      inputMode="decimal"
                      value={l.cost}
                      placeholder={formatMoney("0", { exponent: workspace.currencyExponent })}
                      onChange={(e) => setLines((ls) => ls.map((x, j) => (j === i ? { ...x, cost: e.target.value } : x)))}
                    />
                  </td>
                )}
                <td>
                  <button type="button" className="secondary" aria-label={`Remove ${l.name}`} onClick={() => setLines((ls) => ls.filter((_, j) => j !== i))}>
                    Remove
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {lines.length === 0 && <p className="small">Scan an item to add it.</p>}

        <div className="return-options">
          {costs && (
            <>
              <label>
                Supplier (optional)
                <input value={supplier} onChange={(e) => setSupplier(e.target.value)} maxLength={200} />
              </label>
              <label>
                Reference (optional)
                <input value={reference} onChange={(e) => setReference(e.target.value)} maxLength={200} placeholder="Delivery note or invoice" />
              </label>
            </>
          )}
          <label>
            Note{kind === "adjustment" && reason === "other" ? "" : " (optional)"}
            <input value={note} onChange={(e) => setNote(e.target.value)} maxLength={200} />
          </label>
        </div>

        <div aria-live="polite">
          {errorText && (
            <p role="alert" className="field-error">
              {errorText}
            </p>
          )}
          {posted && (
            <p role="status" data-testid="posted">
              Posted <strong className="mono">{posted.documentNo}</strong>. Stock is updated.
            </p>
          )}
        </div>
        <div className="actions">
          <button type="button" className="pay" disabled={busy || lines.length === 0} onClick={() => void post()}>
            {postLabel}
          </button>
        </div>
      </section>
    </main>
  );
}
