import {
  ArrowLeft,
  Upload,
} from "../../ui/icons";
import { useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { Column, ImportCommitDto, ImportPreviewDto, ImportSummaryDto, MappingEntryDto, OnDuplicateDto } from "../../ipc/bindings";
import { useT } from "../../i18n";
import { describeError } from "../shell/errors";

/** How often the progress bar asks how far the commit has got. */
const PROGRESS_POLL_MS = 250;

/** The importer's columns in the order shown; `column` is the code sent to Rust, never translated. */
const COLUMNS: { column: Column; required?: boolean }[] = [
  { column: "name", required: true },
  { column: "price", required: true },
  { column: "variant" },
  { column: "parent_sku" },
  { column: "sku" },
  { column: "barcode" },
  { column: "cost" },
  { column: "unit" },
  { column: "tax_category" },
  { column: "category" },
  { column: "track_stock" },
  { column: "low_stock_threshold" },
  { column: "scan_quantity" },
  { column: "status" },
];

/** Each column's name on screen (translated at render). */
const COLUMN_COPY: Record<Column, string> = {
  name: "Product name",
  price: "Price",
  variant: "Variant",
  parent_sku: "Parent SKU",
  sku: "SKU",
  barcode: "Barcode",
  cost: "Cost",
  unit: "Unit",
  tax_category: "Tax category",
  category: "Category",
  track_stock: "Track stock",
  low_stock_threshold: "Low-stock alert",
  scan_quantity: "Units per scan",
  status: "Status",
};

/**
 * Help under a column's name (translated at render). The unit codes, "yes"/"no" and
 * "archived" are values the file holds, so translations keep them as they are.
 */
const COLUMN_HELP_COPY: Partial<Record<Column, string>> = {
  variant: "Rows with the same name (or parent SKU) become one product; this names each variant.",
  parent_sku: "Groups rows into one product when their names differ.",
  barcode: "Several barcodes may be separated by | or ;",
  unit: "ea, kg, g, l, ml, m, cm, box or pack. Blank: ea.",
  tax_category: "By name. Blank: the default.",
  category: "By name; the category must exist.",
  track_stock: "yes or no. Blank: yes.",
  status: "Rows marked archived are skipped.",
};

const POLICIES: OnDuplicateDto[] = ["skip", "update", "error"];
/** What to do with products that already exist (translated at render). */
const POLICY_COPY: Record<OnDuplicateDto, string> = {
  skip: "Skip them",
  update: "Update them",
  error: "Report them as errors",
};
const POLICY_HELP_COPY: Record<OnDuplicateDto, string> = {
  skip: "Products that already exist are left as they are.",
  update: "Only the columns in the file change; other variants are kept.",
  error: "Nothing that already exists is touched; the rows are listed.",
};

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
  const t = useT();
  const [step, setStep] = useState<Step>({ kind: "choose" });
  const [mapping, setMapping] = useState<Partial<Record<Column, number>>>({});
  const [policy, setPolicy] = useState<OnDuplicateDto>("skip");
  const [error, setError] = useState<IpcError | null>(null);
  const [busy, setBusy] = useState(false);
  /** Rows written of rows to write, while a commit runs. */
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const committingJob = step.kind === "committing" ? step.preview.jobId : null;
  useEffect(() => {
    if (!committingJob) return;
    let live = true;
    const poll = () =>
      void commands.importProductsProgress(committingJob).then((res) => {
        if (live && res.status === "ok") setProgress({ done: res.data.rowsDone, total: res.data.rowsTotal });
      });
    poll();
    const timer = setInterval(poll, PROGRESS_POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
      setProgress(null);
    };
  }, [committingJob]);
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
  /** The missing columns' names in lower case, joined with "and". */
  const missingNames = missing
    .map((c) => t(COLUMN_COPY[c.column]).toLowerCase())
    .reduce((a, b) => (a ? t("{a} and {b}", { a, b }) : b), "");
  const notImported = (s: ImportSummaryDto) => s.errorRows + s.duplicateRows + s.skippedRows;

  return (
    <main className="sales import-wizard">
      <header className="returns-header">
        <h1 className="page-title">
          <Upload />
          {t("Import products")}
        </h1>
        <button type="button" className="secondary" disabled={step.kind === "committing"} onClick={() => void close()}>
          <ArrowLeft />
          {committed ? t("Back to products") : t("Cancel")} <kbd>Esc</kbd>
        </button>
      </header>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}

      {step.kind === "choose" && (
        <section className="card" aria-label={t("Choose a file")}>
          <p>
            {t(
              "Choose a spreadsheet saved as a CSV file, with the column names in the first row. Excel, Google Sheets and most other spreadsheets can save as CSV.",
            )}
          </p>
          <p className="small">{t("Tip: export your products first (Reports → Products) to see the columns the till expects.")}</p>
          <button type="button" disabled={busy} onClick={() => void choose()}>
            {t("Choose file…")}
          </button>
        </section>
      )}

      {step.kind === "map" && (
        <>
          <section className="card" aria-label={t("Preview")}>
            <h2>
              {step.preview.fileName} · {t("{n} rows", { n: step.preview.totalRows })}
            </h2>
            <div className="import-preview">
              <table className="sales-table">
                <thead>
                  <tr>
                    {step.preview.header.map((h, i) => (
                      <th key={i} scope="col">
                        {h || t("Column {n}", { n: i + 1 })}
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
            aria-label={t("Columns")}
            onSubmit={(e) => {
              e.preventDefault();
              void validate(step.preview);
            }}
          >
            <h2>{t("Columns")}</h2>
            <p className="small">{t("Matched by their headings. Choose a column for anything not matched, or leave it out.")}</p>
            {COLUMNS.map((c) => (
              <div className="setting-row" key={c.column}>
                <label htmlFor={`map-${c.column}`}>
                  <strong>
                    {t(COLUMN_COPY[c.column])}
                    {c.required ? ` ${t("(required)")}` : ""}
                  </strong>
                  {COLUMN_HELP_COPY[c.column] && <span className="small">{t(COLUMN_HELP_COPY[c.column] ?? "")}</span>}
                </label>
                <select
                  id={`map-${c.column}`}
                  value={mapping[c.column] ?? ""}
                  onChange={(e) => setMapping({ ...mapping, [c.column]: e.target.value === "" ? undefined : parseInt(e.target.value, 10) })}
                >
                  <option value="">{t("(not in the file)")}</option>
                  {step.preview.header.map((h, i) => (
                    <option key={i} value={i}>
                      {h || t("Column {n}", { n: i + 1 })}
                    </option>
                  ))}
                </select>
              </div>
            ))}
            <fieldset>
              <legend>{t("Products that already exist (same name, barcode or SKU)")}</legend>
              {POLICIES.map((p) => (
                <label key={p} className="check">
                  <input type="radio" name="policy" value={p} checked={policy === p} onChange={() => setPolicy(p)} />{" "}
                  <strong>{t(POLICY_COPY[p])}</strong> <span className="small">{t(POLICY_HELP_COPY[p])}</span>
                </label>
              ))}
            </fieldset>
            {missing.length > 0 && <p className="small">{t("Choose the column for {columns}.", { columns: missingNames })}</p>}
            <button type="submit" disabled={busy || missing.length > 0}>
              {busy ? t("Checking…") : t("Check the file")}
            </button>
          </form>
        </>
      )}

      {(step.kind === "summary" || step.kind === "committing") && (
        <section className="card" aria-label={t("Check results")}>
          <h2>{t("What will happen")}</h2>
          <dl className="import-counts" data-testid="import-counts">
            <dt>{t("New products")}</dt>
            <dd>{step.summary.productsToCreate}</dd>
            <dt>{t("Products updated")}</dt>
            <dd>{step.summary.productsToUpdate}</dd>
            <dt>{t("Rows ready")}</dt>
            <dd>{step.summary.validRows}</dd>
            <dt>{t("Rows with errors")}</dt>
            <dd>{step.summary.errorRows}</dd>
            <dt>{t("Duplicates")}</dt>
            <dd>{step.summary.duplicateRows}</dd>
            <dt>{t("Skipped")}</dt>
            <dd>{step.summary.skippedRows}</dd>
          </dl>
          {step.summary.problems.length > 0 && (
            <>
              <h3>{t("Rows that will not be imported")}</h3>
              <table className="sales-table" aria-label={t("Problems")}>
                <thead>
                  <tr>
                    <th scope="col">{t("Row")}</th>
                    <th scope="col">{t("Problem")}</th>
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
                <p className="small">{t("The first {n} are shown; the error report lists every row.", { n: step.summary.problems.length })}</p>
              )}
            </>
          )}
          <p>
            <button
              type="button"
              disabled={step.kind === "committing" || step.summary.validRows === 0}
              onClick={() => void commit(step.preview, step.summary)}
            >
              {step.kind === "committing" ? t("Importing…") : t("Import {n} products", { n: step.summary.productsToCreate + step.summary.productsToUpdate })}
            </button>{" "}
            <button type="button" className="secondary" disabled={step.kind === "committing"} onClick={() => setStep({ kind: "map", preview: step.preview })}>
              {t("Change columns")}
            </button>{" "}
            {notImported(step.summary) > 0 && (
              <button type="button" className="secondary" disabled={step.kind === "committing"} onClick={() => void report(step.preview.jobId)}>
                {t("Save error report")}
              </button>
            )}
          </p>
          {step.kind === "committing" && (
            <div className="import-progress">
              <progress
                aria-label={t("Import progress")}
                max={progress?.total || 1}
                value={progress?.done ?? 0}
                data-testid="import-progress"
              />
              <p className="small" data-testid="import-progress-text">
                {progress && progress.total > 0
                  ? t("{done} of {total} rows ({pct}%)", {
                      done: progress.done.toLocaleString(),
                      total: progress.total.toLocaleString(),
                      pct: Math.floor((progress.done * 100) / progress.total),
                    })
                  : t("Starting…")}
              </p>
              <p role="status" className="small">
                {t("Importing {n} products, 500 at a time. Each batch is saved whole; if the app is closed, importing again finishes the rest.", {
                  n: step.summary.productsToCreate + step.summary.productsToUpdate,
                })}
              </p>
            </div>
          )}
        </section>
      )}

      {step.kind === "done" && (
        <section className="card" aria-label={t("Imported")}>
          <div role="status">
            <h2>{t("Imported: {created} new, {updated} updated.", { created: step.result.productsCreated, updated: step.result.productsUpdated })}</h2>
          </div>
          {notImported(step.summary) > 0 && (
            <p>
              {t("{n} rows were not imported.", { n: notImported(step.summary) })}{" "}
              <button type="button" className="secondary" onClick={() => void report(step.preview.jobId)}>
                {t("Save error report")}
              </button>
            </p>
          )}
        </section>
      )}
      {saved && <p className="small">{t("Saved {file}.", { file: saved })}</p>}
    </main>
  );
}
