import { useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { Column, ImportCommitDto, ImportPreviewDto, ImportSummaryDto, MappingEntryDto, OnDuplicateDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";

const COLUMNS: { column: Column; label: string; help?: string; required?: boolean }[] = [
  { column: "name", label: "Product name", required: true },
  { column: "price", label: "Price", required: true },
  { column: "variant", label: "Variant", help: "Rows with the same name (or parent SKU) become one product; this names each variant." },
  { column: "parent_sku", label: "Parent SKU", help: "Groups rows into one product when their names differ." },
  { column: "sku", label: "SKU" },
  { column: "barcode", label: "Barcode", help: "Several barcodes may be separated by | or ;" },
  { column: "cost", label: "Cost" },
  { column: "unit", label: "Unit", help: "ea, kg, g, l, ml, m, cm, box or pack. Blank: ea." },
  { column: "tax_category", label: "Tax category", help: "By name. Blank: the default." },
  { column: "category", label: "Category", help: "By name; the category must exist." },
  { column: "track_stock", label: "Track stock", help: "yes or no. Blank: yes." },
  { column: "low_stock_threshold", label: "Low-stock alert" },
  { column: "scan_quantity", label: "Units per scan" },
  { column: "status", label: "Status", help: "Rows marked archived are skipped." },
];

const POLICIES: { value: OnDuplicateDto; label: string; help: string }[] = [
  { value: "skip", label: "Skip them", help: "Products that already exist are left as they are." },
  { value: "update", label: "Update them", help: "Only the columns in the file change; other variants are kept." },
  { value: "error", label: "Report them as errors", help: "Nothing that already exists is touched; the rows are listed." },
];

type Step =
  | { kind: "choose" }
  | { kind: "map"; preview: ImportPreviewDto }
  | { kind: "summary"; preview: ImportPreviewDto; summary: ImportSummaryDto }
  | { kind: "committing"; preview: ImportPreviewDto; summary: ImportSummaryDto }
  | { kind: "done"; preview: ImportPreviewDto; summary: ImportSummaryDto; result: ImportCommitDto };

/**
 * Import products from a CSV file (E06-T08, CAT-008): choose the file (the system dialog,
 * opened by Rust), check the preview and the columns (matched by their headings), choose
 * what happens to products that already exist, see what will be imported and what will not,
 * then import. Nothing changes in the catalogue before "Import"; cancelling before then
 * leaves it as it was.
 */
export function ImportWizard({ onClose }: { onClose: (imported: boolean) => void }) {
  const [step, setStep] = useState<Step>({ kind: "choose" });
  const [mapping, setMapping] = useState<Partial<Record<Column, number>>>({});
  const [policy, setPolicy] = useState<OnDuplicateDto>("skip");
  const [error, setError] = useState<IpcError | null>(null);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);

  const jobId = step.kind === "choose" ? null : step.preview.jobId;
  const committed = step.kind === "done";

  async function close() {
    if (jobId && (step.kind === "map" || step.kind === "summary")) await commands.importProductsCancel(jobId);
    onClose(committed);
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || step.kind === "committing") return;
      e.preventDefault();
      void close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  async function choose() {
    setBusy(true);
    setError(null);
    const res = await commands.importProductsOpen();
    setBusy(false);
    if (res.status === "error") return setError(res.error);
    if (!res.data) return;
    const m: Partial<Record<Column, number>> = {};
    for (const e of res.data.mapping) m[e.column] = e.index;
    setMapping(m);
    setStep({ kind: "map", preview: res.data });
  }

  async function validate(preview: ImportPreviewDto) {
    const entries: MappingEntryDto[] = Object.entries(mapping)
      .filter((e): e is [Column, number] => typeof e[1] === "number")
      .map(([column, index]) => ({ column, index }));
    setBusy(true);
    setError(null);
    const res = await commands.importProductsValidate(preview.jobId, entries, policy);
    setBusy(false);
    if (res.status === "error") return setError(res.error);
    setStep({ kind: "summary", preview, summary: res.data });
  }

  async function commit(preview: ImportPreviewDto, summary: ImportSummaryDto) {
    setStep({ kind: "committing", preview, summary });
    setError(null);
    const res = await commands.importProductsCommit(preview.jobId);
    if (res.status === "error") {
      // Whole chunks may already be in; importing again finishes the rest without duplicates.
      setError(res.error);
      return setStep({ kind: "summary", preview, summary });
    }
    setStep({ kind: "done", preview, summary, result: res.data });
  }

  async function report(id: string) {
    const res = await commands.importProductsErrorReport(id);
    if (res.status === "error") return setError(res.error);
    if (res.data) setSaved(res.data.fileName);
  }

  const missing = COLUMNS.filter((c) => c.required && mapping[c.column] === undefined);
  const notImported = (s: ImportSummaryDto) => s.errorRows + s.duplicateRows + s.skippedRows;

  return (
    <main className="sales import-wizard">
      <header className="returns-header">
        <h1>Import products</h1>
        <button type="button" className="secondary" disabled={step.kind === "committing"} onClick={() => void close()}>
          {committed ? "Back to products" : "Cancel"} <kbd>Esc</kbd>
        </button>
      </header>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}

      {step.kind === "choose" && (
        <section className="card" aria-label="Choose a file">
          <p>
            Choose a CSV file with a heading row. The products export is in this format; most spreadsheets can save as CSV
            (comma, semicolon or tab separated; UTF-8 or Windows encoding).
          </p>
          <button type="button" disabled={busy} onClick={() => void choose()}>
            Choose file…
          </button>
        </section>
      )}

      {step.kind === "map" && (
        <>
          <section className="card" aria-label="Preview">
            <h2>
              {step.preview.fileName} · {step.preview.totalRows} rows
            </h2>
            <div className="import-preview">
              <table className="sales-table">
                <thead>
                  <tr>
                    {step.preview.header.map((h, i) => (
                      <th key={i} scope="col">
                        {h || `Column ${i + 1}`}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {step.preview.sample.slice(0, 5).map((r, i) => (
                    <tr key={i}>
                      {step.preview.header.map((_, j) => (
                        <td key={j}>{r[j] ?? ""}</td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>
          <form
            className="card"
            aria-label="Columns"
            onSubmit={(e) => {
              e.preventDefault();
              void validate(step.preview);
            }}
          >
            <h2>Columns</h2>
            <p className="small">Matched by their headings. Choose a column for anything not matched, or leave it out.</p>
            {COLUMNS.map((c) => (
              <div className="setting-row" key={c.column}>
                <label htmlFor={`map-${c.column}`}>
                  <strong>
                    {c.label}
                    {c.required ? " (required)" : ""}
                  </strong>
                  {c.help && <span className="small">{c.help}</span>}
                </label>
                <select
                  id={`map-${c.column}`}
                  value={mapping[c.column] ?? ""}
                  onChange={(e) => setMapping({ ...mapping, [c.column]: e.target.value === "" ? undefined : parseInt(e.target.value, 10) })}
                >
                  <option value="">(not in the file)</option>
                  {step.preview.header.map((h, i) => (
                    <option key={i} value={i}>
                      {h || `Column ${i + 1}`}
                    </option>
                  ))}
                </select>
              </div>
            ))}
            <fieldset>
              <legend>Products that already exist (same name, barcode or SKU)</legend>
              {POLICIES.map((p) => (
                <label key={p.value} className="check">
                  <input type="radio" name="policy" value={p.value} checked={policy === p.value} onChange={() => setPolicy(p.value)} />{" "}
                  <strong>{p.label}</strong> <span className="small">{p.help}</span>
                </label>
              ))}
            </fieldset>
            {missing.length > 0 && <p className="small">Choose the column for {missing.map((c) => c.label.toLowerCase()).join(" and ")}.</p>}
            <button type="submit" disabled={busy || missing.length > 0}>
              {busy ? "Checking…" : "Check the file"}
            </button>
          </form>
        </>
      )}

      {(step.kind === "summary" || step.kind === "committing") && (
        <section className="card" aria-label="Check results">
          <h2>What will happen</h2>
          <dl className="import-counts" data-testid="import-counts">
            <dt>New products</dt>
            <dd>{step.summary.productsToCreate}</dd>
            <dt>Products updated</dt>
            <dd>{step.summary.productsToUpdate}</dd>
            <dt>Rows ready</dt>
            <dd>{step.summary.validRows}</dd>
            <dt>Rows with errors</dt>
            <dd>{step.summary.errorRows}</dd>
            <dt>Duplicates</dt>
            <dd>{step.summary.duplicateRows}</dd>
            <dt>Skipped</dt>
            <dd>{step.summary.skippedRows}</dd>
          </dl>
          {step.summary.problems.length > 0 && (
            <>
              <h3>Rows that will not be imported</h3>
              <table className="sales-table" aria-label="Problems">
                <thead>
                  <tr>
                    <th scope="col">Row</th>
                    <th scope="col">Problem</th>
                  </tr>
                </thead>
                <tbody>
                  {step.summary.problems.map((p) => (
                    <tr key={p.line} data-testid="import-problem">
                      <td>{p.line}</td>
                      <td>{p.messages.join("; ")}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {notImported(step.summary) > step.summary.problems.length && (
                <p className="small">The first {step.summary.problems.length} are shown; the error report lists every row.</p>
              )}
            </>
          )}
          <p>
            <button
              type="button"
              disabled={step.kind === "committing" || step.summary.validRows === 0}
              onClick={() => void commit(step.preview, step.summary)}
            >
              {step.kind === "committing" ? "Importing…" : `Import ${step.summary.productsToCreate + step.summary.productsToUpdate} products`}
            </button>{" "}
            <button type="button" className="secondary" disabled={step.kind === "committing"} onClick={() => setStep({ kind: "map", preview: step.preview })}>
              Change columns
            </button>{" "}
            {notImported(step.summary) > 0 && (
              <button type="button" className="secondary" disabled={step.kind === "committing"} onClick={() => void report(step.preview.jobId)}>
                Save error report
              </button>
            )}
          </p>
          {step.kind === "committing" && (
            <p role="status" className="small">
              Importing {step.summary.productsToCreate + step.summary.productsToUpdate} products, 500 at a time. Each batch is saved
              whole; if the app is closed, importing again finishes the rest.
            </p>
          )}
        </section>
      )}

      {step.kind === "done" && (
        <section className="card" aria-label="Imported">
          <div role="status">
            <h2>
              Imported: {step.result.productsCreated} new, {step.result.productsUpdated} updated.
            </h2>
          </div>
          {notImported(step.summary) > 0 && (
            <p>
              {notImported(step.summary)} rows were not imported.{" "}
              <button type="button" className="secondary" onClick={() => void report(step.preview.jobId)}>
                Save error report
              </button>
            </p>
          )}
        </section>
      )}
      {saved && <p className="small">Saved {saved}.</p>}
    </main>
  );
}
