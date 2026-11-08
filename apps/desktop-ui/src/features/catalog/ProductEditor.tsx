import { useCallback, useEffect, useId, useState, type FormEvent, type ReactNode } from "react";
import { commands, type IpcError, type Workspace } from "../../ipc";
import type { BarcodeQualifier, CategoryDto, ProductType, TaxCategoryDto, Unit } from "../../ipc/bindings";
import { Dialog } from "../checkout/Dialog";
import { useScanCapture } from "../scan/useScanCapture";
import { describeError } from "../shell/errors";
import { flatTree } from "./categories";
import {
  blankDraft,
  fromProduct,
  isProblem,
  regrid,
  toInput,
  variantLabel,
  type Axis,
  type FieldProblem,
  type ProductDraft,
  type VariantDraft,
} from "./draft";

const UNITS: Unit[] = ["ea", "kg", "g", "l", "ml", "m", "cm", "box", "pack"];
const TYPES: { value: ProductType; label: string }[] = [
  { value: "stock", label: "Stock item (counted)" },
  { value: "non_stock", label: "Non-stock item" },
  { value: "service", label: "Service" },
];

/** Where a problem is shown: the editor input it is about. */
interface Problem extends FieldProblem {
  /** For SKU and barcode errors from Rust: the offending value, to find the input. */
  value?: string;
  ref?: string;
}

/**
 * Create or edit a product (E06-T06b). A simple product has one variant; adding option axes
 * (size, colour…) turns it into a variant grid with inline price, SKU, barcodes and
 * low-stock threshold. Scanning while the editor is open adds the barcode to the selected
 * variant. Errors from Rust are shown next to the input they are about.
 */
export function ProductEditor({
  workspace,
  productId,
  barcode,
  canEdit,
  onSaved,
  onClose,
}: {
  workspace: Workspace;
  productId: string | null;
  /** From the unknown-barcode prompt: pre-filled on a new product (E07-T04). */
  barcode?: string;
  canEdit: boolean;
  onSaved: (productId: string, created: boolean) => void;
  onClose: () => void;
}) {
  const exponent = workspace.currencyExponent;
  const [draft, setDraft] = useState<ProductDraft | null>(null);
  const [taxes, setTaxes] = useState<TaxCategoryDto[]>([]);
  const [categories, setCategories] = useState<CategoryDto[]>([]);
  const [selected, setSelected] = useState(0);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [failure, setFailure] = useState<IpcError | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [confirmArchive, setConfirmArchive] = useState(false);

  useEffect(() => {
    let live = true;
    void (async () => {
      const [t, c] = await Promise.all([commands.taxList(), commands.categoriesList()]);
      if (!live) return;
      if (t.status === "error") return setFailure(t.error);
      setTaxes(t.data);
      if (c.status === "ok") setCategories(c.data);
      if (productId) {
        const p = await commands.productGet(productId);
        if (!live) return;
        if (p.status === "error") return setFailure(p.error);
        setDraft(fromProduct(p.data, exponent));
      } else {
        const def = t.data.find((c) => c.isDefault) ?? t.data[0];
        setDraft(blankDraft(def?.taxCategoryId ?? "", barcode));
      }
    })();
    return () => {
      live = false;
    };
  }, [productId, barcode, exponent]);

  const update = useCallback((change: (d: ProductDraft) => ProductDraft) => {
    setSaved(false);
    setDraft((d) => (d ? change(d) : d));
  }, []);

  const setVariant = (i: number, change: Partial<VariantDraft>) =>
    update((d) => ({ ...d, variants: d.variants.map((v, j) => (j === i ? { ...v, ...change } : v)) }));

  const setAxes = (axes: Axis[]) => update((d) => ({ ...d, axes, variants: regrid(axes, d.variants) }));

  const addBarcode = useCallback(
    (i: number, raw: string, qualifier: BarcodeQualifier = "alias") => {
      const code = raw.trim();
      if (!code) return;
      update((d) => ({
        ...d,
        variants: d.variants.map((v, j) =>
          j === i && !v.barcodes.some((b) => b.raw === code)
            ? { ...v, barcodes: [...v.barcodes, { raw: code, qualifier: v.barcodes.length ? qualifier : "primary", packQty: "1" }] }
            : v,
        ),
      }));
    },
    [update],
  );

  // A scan while the editor is open adds a barcode to the selected variant (E06-T06b).
  useScanCapture((scan) => addBarcode(selected, scan.raw), {}, canEdit && draft !== null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented && !confirmArchive) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, confirmArchive]);

  async function save(e: FormEvent) {
    e.preventDefault();
    if (!draft || busy) return;
    const input = toInput(draft, exponent);
    if (isProblem(input)) return setProblem(input);
    setProblem(null);
    setFailure(null);
    setBusy(true);
    const res =
      draft.productId && draft.version !== null
        ? await commands.productUpdate(draft.productId, draft.version, input)
        : await commands.productCreate(input);
    setBusy(false);
    if (res.status === "error") {
      const d = res.error.details;
      if (d?.field) {
        const at = locate(draft, d.field, d.value ?? undefined);
        setProblem({ field: d.field, message: describeField(res.error), ...at, ...(d.value ? { value: d.value } : {}) });
      } else setFailure(res.error);
      return;
    }
    const created = draft.productId === null;
    setDraft(fromProduct(res.data, exponent));
    setSaved(true);
    onSaved(res.data.productId, created);
  }

  async function archive() {
    if (!draft?.productId || draft.version === null) return;
    const res = await commands.productArchive(draft.productId, draft.version);
    if (res.status === "error") return setFailure(res.error);
    onClose();
  }

  if (!draft) {
    return (
      <main className="editor" aria-busy="true">
        {failure && <p role="alert">{describeError(failure)}</p>}
      </main>
    );
  }

  const hasAxes = draft.axes.some((a) => a.name.trim());
  const at = (field: string, variant?: number) =>
    problem && problem.field === field && (variant === undefined || problem.variant === undefined || problem.variant === variant)
      ? problem.message
      : null;
  const ro = !canEdit;

  return (
    <main className="editor">
      <form onSubmit={(e) => void save(e)} noValidate aria-label="Product editor">
        <header className="products-head">
          <button type="button" className="secondary" onClick={onClose}>
            ← Back <kbd>Esc</kbd>
          </button>
          <h1>{draft.productId ? draft.name || "Product" : "New product"}</h1>
          <span aria-live="polite" className="saved-note">
            {saved ? "Saved on this device" : ""}
          </span>
          {canEdit && (
            <button type="submit" disabled={busy}>
              {busy ? "Saving…" : "Save"}
            </button>
          )}
        </header>

        <section className="card editor-grid">
          <Field label="Name" error={at("name")}>
            {(id) => <input id={id} autoFocus readOnly={ro} value={draft.name} onChange={(e) => update((d) => ({ ...d, name: e.target.value }))} />}
          </Field>
          <Field label="Type">
            {(id) => (
              <select id={id} disabled={ro} value={draft.productType} onChange={(e) => update((d) => ({ ...d, productType: e.target.value as ProductType }))}>
                {TYPES.map((t) => (
                  <option key={t.value} value={t.value}>
                    {t.label}
                  </option>
                ))}
              </select>
            )}
          </Field>
          <Field label="Unit">
            {(id) => (
              <select id={id} disabled={ro} value={draft.unit} onChange={(e) => update((d) => ({ ...d, unit: e.target.value as Unit }))}>
                {UNITS.map((u) => (
                  <option key={u}>{u}</option>
                ))}
              </select>
            )}
          </Field>
          <Field label="Tax">
            {(id) => (
              <select id={id} disabled={ro} value={draft.taxCategoryId} onChange={(e) => update((d) => ({ ...d, taxCategoryId: e.target.value }))}>
                {taxes.map((t) => (
                  <option key={t.taxCategoryId} value={t.taxCategoryId}>
                    {t.name}
                    {t.rates.length ? ` (${t.rates.map((r) => r.name).join(", ")})` : " (no tax)"}
                  </option>
                ))}
              </select>
            )}
          </Field>
          <Field label="Category">
            {(id) => (
              <select id={id} disabled={ro} value={draft.categoryId ?? ""} onChange={(e) => update((d) => ({ ...d, categoryId: e.target.value || null }))}>
                <option value="">(none)</option>
                {draft.categoryId && !categories.some((c) => c.categoryId === draft.categoryId && !c.archived) && (
                  <option value={draft.categoryId}>{categories.find((c) => c.categoryId === draft.categoryId)?.name ?? "Archived category"}</option>
                )}
                {flatTree(categories).map(({ category: c, depth }) => (
                  <option key={c.categoryId} value={c.categoryId}>
                    {"— ".repeat(depth - 1)}
                    {c.name}
                  </option>
                ))}
              </select>
            )}
          </Field>
          <label className="check">
            <input type="checkbox" disabled={ro} checked={draft.quickSelect} onChange={(e) => update((d) => ({ ...d, quickSelect: e.target.checked }))} />
            Show on the quick-select grid
          </label>
        </section>

        <section className="card">
          <h2>Options</h2>
          <p className="small">Add options such as size or colour to sell variants. Values are separated by commas.</p>
          {draft.axes.map((a, i) => (
            <div className="row" key={i}>
              <Field label={`Option ${i + 1}`}>
                {(id) => (
                  <input id={id} readOnly={ro} placeholder="e.g. Size" value={a.name} onChange={(e) => setAxes(draft.axes.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))} />
                )}
              </Field>
              <Field label={`Option ${i + 1} values`}>
                {(id) => (
                  <input id={id} readOnly={ro} placeholder="e.g. S, M, L" value={a.values} onChange={(e) => setAxes(draft.axes.map((x, j) => (j === i ? { ...x, values: e.target.value } : x)))} />
                )}
              </Field>
              {canEdit && (
                <button type="button" className="secondary" onClick={() => setAxes(draft.axes.filter((_, j) => j !== i))}>
                  Remove option
                </button>
              )}
            </div>
          ))}
          {canEdit && draft.axes.length < 3 && (
            <button type="button" className="secondary" onClick={() => setAxes([...draft.axes, { name: "", values: "" }])}>
              Add option
            </button>
          )}
          {at("options") && <p className="field-error">{at("options")}</p>}
        </section>

        <section className="card">
          <h2>{hasAxes ? `Variants (${draft.variants.length})` : "Price and codes"}</h2>
          <p className="small">Scan a barcode now to add it to the selected {hasAxes ? "variant" : "product"}.</p>
          <table className="variant-grid" aria-label="Variants">
            <thead>
              <tr>
                {hasAxes && <th scope="col">Variant</th>}
                <th scope="col">Price ({workspace.currencyCode})</th>
                <th scope="col">SKU</th>
                <th scope="col">Barcodes</th>
                <th scope="col">Low-stock alert</th>
                <th scope="col">Units per scan</th>
              </tr>
            </thead>
            <tbody>
              {draft.variants.map((v, i) => {
                const label = variantLabel(v, draft.axes) || draft.name || "Product";
                return (
                  <tr key={i} className={i === selected ? "selected" : undefined} onFocus={() => setSelected(i)} onClick={() => setSelected(i)}>
                    {hasAxes && <th scope="row">{variantLabel(v, draft.axes)}</th>}
                    <td>
                      <input aria-label={`Price for ${label}`} readOnly={ro} inputMode="decimal" className="num" value={v.price} onChange={(e) => setVariant(i, { price: e.target.value })} />
                      <Inline msg={at("price", i)} />
                    </td>
                    <td>
                      <input aria-label={`SKU for ${label}`} readOnly={ro} value={v.sku} onChange={(e) => setVariant(i, { sku: e.target.value })} />
                      <Inline msg={problem?.field === "sku" && (problem.variant === i || problem.value === v.sku.trim()) ? problem.message : null} />
                    </td>
                    <td>
                      <ul className="chips" aria-label={`Barcodes for ${label}`}>
                        {v.barcodes.map((b) => (
                          <li key={b.raw} className={problem?.field === "barcode" && problem.value === b.raw ? "chip bad" : "chip"}>
                            <span className="mono">{b.raw}</span>
                            {b.qualifier === "pack" && <span> ×{b.packQty}</span>}
                            {canEdit && (
                              <button type="button" aria-label={`Remove barcode ${b.raw}`} onClick={() => setVariant(i, { barcodes: v.barcodes.filter((x) => x.raw !== b.raw) })}>
                                ×
                              </button>
                            )}
                          </li>
                        ))}
                      </ul>
                      {canEdit && <BarcodeAdder label={label} onAdd={(raw) => addBarcode(i, raw)} />}
                      <Inline msg={at("barcode", i)} />
                    </td>
                    <td>
                      <input aria-label={`Low-stock alert for ${label}`} readOnly={ro} inputMode="decimal" className="num" value={v.lowStock} onChange={(e) => setVariant(i, { lowStock: e.target.value })} />
                      <Inline msg={at("lowStockThreshold", i)} />
                    </td>
                    <td>
                      <input aria-label={`Units per scan for ${label}`} readOnly={ro} inputMode="decimal" className="num" value={v.scanQty} onChange={(e) => setVariant(i, { scanQty: e.target.value })} />
                      <Inline msg={at("scanQty", i)} />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <Inline msg={at("variants")} />
        </section>

        {failure && (
          <p role="alert" className="field-error">
            {describeError(failure)} <span className="mono small">ref {failure.correlationId.slice(-8)}</span>
          </p>
        )}
        {problem && (
          <p role="alert" className="field-error">
            {problem.message}
          </p>
        )}
        {canEdit && draft.productId && (
          <div className="actions">
            <button type="button" className="secondary" onClick={() => setConfirmArchive(true)}>
              Archive product
            </button>
          </div>
        )}
      </form>
      {/* Outside the editor's form: the dialog is a form of its own. */}
      {confirmArchive && (
        <Dialog
          title={`Archive ${draft.name}?`}
          confirmLabel="Archive"
          danger
          onCancel={() => setConfirmArchive(false)}
          onConfirm={() => {
            setConfirmArchive(false);
            void archive();
          }}
        >
          <p>It disappears from search and the till. Past sales keep showing it.</p>
        </Dialog>
      )}
    </main>
  );
}

/** Which variant an error from Rust is about, by the value it names. */
function locate(d: ProductDraft, field: string, value?: string): { variant?: number } {
  if (!value) return {};
  const i = d.variants.findIndex((v) =>
    field === "sku" ? v.sku.trim().toLowerCase() === value.toLowerCase() : v.barcodes.some((b) => b.raw === value || b.raw.includes(value)),
  );
  return i >= 0 ? { variant: i } : {};
}

function describeField(e: IpcError): string {
  const d = e.details;
  if (e.code === "CATALOG_DUPLICATE_BARCODE")
    return d?.ownerProductId ? `Barcode ${d.value ?? ""} already belongs to another product.` : `Barcode ${d?.value ?? ""} is used twice here.`;
  if (e.code === "CATALOG_DUPLICATE_SKU")
    return d?.ownerProductId ? `SKU ${d.value ?? ""} already belongs to another product.` : `SKU ${d?.value ?? ""} is used twice here.`;
  if (d?.field === "options") return "Two variants have the same options.";
  if (d?.field === "barcode") return "That barcode is not valid: check the digits.";
  if (d?.field === "scanQty") return "Units per scan must be a whole number above zero for this unit.";
  return describeError(e);
}

function Inline({ msg }: { msg: string | null }) {
  return msg ? <span className="field-error inline">{msg}</span> : null;
}

/**
 * Typing a barcode: Enter adds it. A scan into this field is typing followed by the
 * scanner's Enter, so it adds the barcode to this row.
 */
function BarcodeAdder({ label, onAdd }: { label: string; onAdd: (raw: string) => void }) {
  const [text, setText] = useState("");
  return (
    <input
      aria-label={`Add barcode for ${label}`}
      placeholder="Scan or type, Enter"
      value={text}
      onChange={(e) => setText(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          onAdd(text);
          setText("");
        }
      }}
    />
  );
}

function Field({ label, error, children }: { label: string; error?: string | null; children: (id: string) => ReactNode }) {
  const id = useId();
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      {children(id)}
      {error && <span className="field-error inline">{error}</span>}
    </div>
  );
}
