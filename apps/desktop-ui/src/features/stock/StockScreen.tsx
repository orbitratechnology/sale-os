import { TabList, tabPanelProps } from "../../ui/Tabs";
import {
  ArrowLeft,
  Boxes,
} from "../../ui/icons";
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { commands, type IpcError, type Workspace } from "../../ipc";
import type { AdjustReason, DocumentType, PostedDocumentDto } from "../../ipc/bindings";
import { formatMilli, scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { useScanCapture } from "../scan/useScanCapture";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";
import { RecordView, type RecordRef } from "../records/RecordView";
import { INITIAL_OVERVIEW, StockOverview, type OverviewState } from "./StockOverview";

/** The radio choices, in order; English, translated at render with `t`. */
const KIND_COPY: Record<DocumentType, { label: string; post: string }> = {
  receipt: { label: "Receive goods", post: "Post goods received" },
  opening: { label: "Opening stock", post: "Post opening stock" },
  adjustment: { label: "Adjust stock", post: "Post adjustment" },
  write_off: { label: "Write off damaged", post: "Post write-off" },
};
const KINDS = Object.keys(KIND_COPY) as DocumentType[];

/** The adjustment reasons, in order; English, translated at render with `t`. */
const REASON_COPY: Record<AdjustReason, { label: string; hint: string }> = {
  damage: { label: "Damaged", hint: "Moves the quantity from sellable to damaged stock." },
  loss: { label: "Lost", hint: "Removes the quantity from sellable stock." },
  theft: { label: "Theft", hint: "Removes the quantity from sellable stock." },
  expired: { label: "Expired", hint: "Removes the quantity from sellable stock." },
  count_correction: { label: "Count correction", hint: "Use − to remove (counted fewer) or + to add." },
  other: { label: "Other (note required)", hint: "Use − to remove or + to add; say why in the note." },
};
const REASONS = Object.keys(REASON_COPY) as AdjustReason[];

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
export function StockScreen({
  workspace,
  canPost,
  onBack,
  onReturn,
}: {
  workspace: Workspace;
  canPost: boolean;
  onBack: () => void;
  /** "Return items" on a sale opened from the history. */
  onReturn: (receiptNumber: string) => void;
}) {
  // Managers and owners land on posting (receiving is the common job); cashiers only see stock.
  const t = useT();
  const [view, setView] = useState<"post" | "onHand">(canPost ? "post" : "onHand");
  const [overview, setOverview] = useState<OverviewState>(INITIAL_OVERVIEW);
  const [record, setRecord] = useState<RecordRef | null>(null);
  if (record) {
    return <RecordView record={record} workspace={workspace} backLabel={t("Back to stock")} onBack={() => setRecord(null)} onReturn={onReturn} />;
  }
  const overviewPanel = <StockOverview state={overview} onState={setOverview} onRecord={setRecord} />;
  if (view === "onHand" || !canPost) {
    return (
      <main className="sales">
        <StockHeader canPost={canPost} view="onHand" onView={setView} onBack={onBack} />
        {canPost ? (
          <div className="stack" {...tabPanelProps("stock", "onHand")}>
            {overviewPanel}
          </div>
        ) : (
          overviewPanel
        )}
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
  const t = useT();
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
        <h1 className="page-title">
          <Boxes />
          {t("Stock")}
        </h1>
        <button type="button" className="secondary" onClick={onBack}>
          <ArrowLeft />
          {t("Back to till")} <kbd>Esc</kbd>
        </button>
      </header>
      {canPost && (
        <TabList
          label={t("Stock")}
          idBase="stock"
          className="receipt-widths"
          tabs={[
            { value: "post", label: t("Receive and adjust") },
            { value: "onHand", label: t("On hand") },
          ]}
          selected={view}
          onSelect={onView}
        />
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
  const t = useT();
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
  const postLabel = t(KIND_COPY[kind].post);

  // Ready for the first scan, unless the user arrived here by arrowing along the view tabs:
  // then focus stays on the tab, so the arrow keys keep working.
  useEffect(() => {
    if (document.activeElement?.getAttribute("role") !== "tab") codeInput.current?.focus();
  }, []);

  const add = useCallback(async (raw: string) => {
    const text = raw.trim();
    if (!text) return;
    const res = await commands.catalogLookupCode(text);
    if (res.status !== "ok") return setError(res.error);
    const hit = res.data;
    if (!hit) return setError(t("No item has the code {code}.", { code: text }));
    if (!hit.trackStock) return setError(t("{name} does not track stock.", { name: hit.name }));
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
  }, [t]);

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
      if (milli === null || milli === "0") return setError(t("Check the quantity for {name}.", { name: l.name }));
      const cost = costs && l.cost.trim() ? scaleDecimal(l.cost, workspace.currencyExponent) : null;
      if (costs && l.cost.trim() && cost === null) return setError(t("Check the unit cost for {name}.", { name: l.name }));
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
  const [postedBefore, postedAfter = ""] = t("Posted {number}. Stock is updated.").split("{number}");

  return (
    <main className="sales">
      <StockHeader canPost view="post" onView={onView} onBack={onBack} />
      <div className="stack" {...tabPanelProps("stock", "post")}>
      <section className="card return-form" aria-label={t("Stock document")}>
        {/* Native radios: one Tab stop, arrow keys move between them, announced as a group. */}
        <fieldset className="segmented">
          <legend className="visually-hidden">{t("What are you doing?")}</legend>
          {KINDS.map((k) => (
            <label key={k}>
              <input
                type="radio"
                name="stock-kind"
                value={k}
                checked={kind === k}
                onChange={() => {
                  setKind(k);
                  setPosted(null);
                  setError(null);
                }}
                onClick={(e) => {
                  // A pointer click readies the code field for the next scan; arrow keys
                  // (which also fire click, with detail 0) keep focus in the group.
                  if (e.detail > 0) codeInput.current?.focus();
                }}
              />
              {t(KIND_COPY[k].label)}
            </label>
          ))}
        </fieldset>
        {kind === "adjustment" && (
          <label>
            {t("Reason")}
            <select value={reason} onChange={(e) => setReason(e.target.value as AdjustReason)}>
              {REASONS.map((r) => (
                <option key={r} value={r}>
                  {t(REASON_COPY[r].label)}
                </option>
              ))}
            </select>
            <span className="small">{t(REASON_COPY[reason].hint)}</span>
          </label>
        )}
        {kind === "write_off" && <p className="small">{t("Takes the quantity out of damaged stock (it cannot be sold).")}</p>}

        <form onSubmit={enterCode} className="stock-code">
          <label>
            {t("Scan or type a code")}
            <input
              ref={codeInput}
              data-scan-field
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder={t("Barcode or SKU, then Enter")}
              autoComplete="off"
              spellCheck={false}
            />
          </label>
        </form>

        <table className="sales-table" aria-label={t("Lines")}>
          <thead>
            <tr>
              <th scope="col">{t("Item")}</th>
              <th scope="col">{t("Quantity")}</th>
              {costs && <th scope="col">{t("Unit cost (optional)")}</th>}
              <th scope="col">
                <span className="visually-hidden">{t("Remove")}</span>
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
                    aria-label={t("Quantity: {name}", { name: l.name })}
                    inputMode="decimal"
                    value={l.qty}
                    onChange={(e) => setLines((ls) => ls.map((x, j) => (j === i ? { ...x, qty: e.target.value } : x)))}
                  />
                </td>
                {costs && (
                  <td>
                    <input
                      className="qty-input"
                      aria-label={t("Unit cost: {name}", { name: l.name })}
                      inputMode="decimal"
                      value={l.cost}
                      placeholder={formatMoney("0", { exponent: workspace.currencyExponent })}
                      onChange={(e) => setLines((ls) => ls.map((x, j) => (j === i ? { ...x, cost: e.target.value } : x)))}
                    />
                  </td>
                )}
                <td>
                  <button type="button" className="secondary" aria-label={t("Remove {name}", { name: l.name })} onClick={() => setLines((ls) => ls.filter((_, j) => j !== i))}>
                    {t("Remove")}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {lines.length === 0 && <p className="small">{t("Scan an item to add it.")}</p>}

        <div className="return-options">
          {costs && (
            <>
              <label>
                {t("Supplier (optional)")}
                <input value={supplier} onChange={(e) => setSupplier(e.target.value)} maxLength={200} />
              </label>
              <label>
                {t("Reference (optional)")}
                <input value={reference} onChange={(e) => setReference(e.target.value)} maxLength={200} placeholder={t("Delivery note or invoice")} />
              </label>
            </>
          )}
          <label>
            {kind === "adjustment" && reason === "other" ? t("Note") : t("Note (optional)")}
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
              {postedBefore}
              <strong className="mono">{posted.documentNo}</strong>
              {postedAfter}
            </p>
          )}
        </div>
        <div className="actions">
          <button type="button" className="pay" disabled={busy || lines.length === 0} onClick={() => void post()}>
            {postLabel}
          </button>
        </div>
      </section>
      </div>
    </main>
  );
}
