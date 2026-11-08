import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { TaxConfigDto, TaxRateConfigDto } from "../../ipc/bindings";
import { percentToPpm, ppmToPercent } from "../../lib/decimal";
import { describeError } from "../shell/errors";

function message(e: IpcError): string {
  const d = e.details;
  if (e.code === "VALIDATION_FAILED") {
    if (d?.field === "categories") return `Still used by ${d.value ?? "a tax category"}: take it out of those first.`;
    if (d?.field === "name" && d.value) return `Another active rate called "${d.value}" covers some of the same days.`;
    if (d?.field === "name") return "Enter a name.";
    if (d?.field === "period") return "The end date must be after the start date.";
    if (d?.field === "default") return "One tax category must stay the default: make another one the default instead.";
  }
  if (e.code === "CONFLICT_VERSION") return "It was changed meanwhile; the current values are shown.";
  return describeError(e);
}

const blank = {
  id: null as string | null,
  version: null as number | null,
  name: "",
  percent: "",
  from: "",
  to: "",
};

/**
 * Taxes (E06-T05, owner): rates with the dates they apply, and tax categories (the sets of
 * rates products use), exactly one of them the default. A rate change on a date is a new
 * rate starting that day and the old one ending that day.
 */
export function TaxesPanel() {
  const [cfg, setCfg] = useState<TaxConfigDto | null>(null);
  const [rate, setRate] = useState(blank);
  const [error, setError] = useState<string | null>(null);
  const [newCategory, setNewCategory] = useState("");

  const load = useCallback(() => {
    void commands.taxConfigGet().then((res) => (res.status === "ok" ? setCfg(res.data) : setError(describeError(res.error))));
  }, []);
  useEffect(load, [load]);

  const done = (res: { status: "ok" } | { status: "error"; error: IpcError }) => {
    if (res.status === "ok") setError(null);
    else setError(message(res.error));
    load();
    return res.status === "ok";
  };

  async function saveRate(e: FormEvent) {
    e.preventDefault();
    const ppm = percentToPpm(rate.percent);
    if (ppm === null) return setError("Enter the rate as a percentage, e.g. 20 or 7.5.");
    const ok = done(await commands.taxRateSave(rate.id, rate.name, ppm, rate.from || null, rate.to || null, rate.version));
    if (ok) setRate(blank);
  }

  const edit = (r: TaxRateConfigDto) =>
    setRate({
      id: r.taxRateId,
      version: r.version,
      name: r.name,
      percent: ppmToPercent(r.ratePpm),
      from: r.from ?? "",
      to: r.to ?? "",
    });

  if (!cfg) return <p className="small">Loading…</p>;
  const activeRates = cfg.rates.filter((r) => !r.archived);
  return (
    <div className="taxes">
      {error && (
        <p role="alert" className="field-error">
          {error}
        </p>
      )}
      <h2>Rates</h2>
      <table className="sales-table" aria-label="Tax rates">
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col" className="num">
              Rate
            </th>
            <th scope="col">From</th>
            <th scope="col">Until (not including)</th>
            <th scope="col">
              <span className="visually-hidden">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {activeRates.map((r) => (
            <tr key={r.taxRateId} data-testid="rate-row">
              <th scope="row">{r.name}</th>
              <td className="num">{ppmToPercent(r.ratePpm)}%</td>
              <td>{r.from ?? "always"}</td>
              <td>{r.to ?? "no end"}</td>
              <td>
                <button type="button" className="link" onClick={() => edit(r)}>
                  Edit
                </button>{" "}
                <button
                  type="button"
                  className="link"
                  aria-label={`Archive ${r.name}`}
                  onClick={() => void commands.taxRateArchive(r.taxRateId, r.version).then(done)}
                >
                  Archive
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <form className="card sales-filters" aria-label={rate.id ? "Change a rate" : "Add a rate"} onSubmit={(e) => void saveRate(e)}>
        <label>
          Name
          <input value={rate.name} onChange={(e) => setRate({ ...rate, name: e.target.value })} placeholder="e.g. VAT 20%" />
        </label>
        <label>
          Rate (%)
          <input inputMode="decimal" value={rate.percent} onChange={(e) => setRate({ ...rate, percent: e.target.value })} />
        </label>
        <label>
          From
          <input type="date" value={rate.from} onChange={(e) => setRate({ ...rate, from: e.target.value })} />
        </label>
        <label>
          Until (not including)
          <input type="date" value={rate.to} onChange={(e) => setRate({ ...rate, to: e.target.value })} />
        </label>
        <span>
          <button type="submit">{rate.id ? "Save rate" : "Add rate"}</button>{" "}
          {rate.id && (
            <button type="button" className="secondary" onClick={() => setRate(blank)}>
              Cancel
            </button>
          )}
        </span>
      </form>

      <h2>Tax categories</h2>
      <p className="small">Each product has one tax category; its rates are added up on the receipt.</p>
      <ul className="plain-list" aria-label="Tax categories">
        {cfg.categories
          .filter((c) => !c.archived)
          .map((c) => (
            <li key={c.taxCategoryId} data-testid="taxcat-row">
              <strong>{c.name}</strong>
              {c.isDefault ? (
                <span className="tag"> default</span>
              ) : (
                <>
                  {" "}
                  <button
                    type="button"
                    className="link"
                    onClick={() => void commands.taxCategorySave(c.taxCategoryId, c.name, c.rateIds, true, c.version).then(done)}
                  >
                    Make default
                  </button>
                </>
              )}
              <div role="group" aria-label={`Rates in ${c.name}`}>
                {activeRates.map((r) => (
                  <label key={r.taxRateId} className="check">
                    <input
                      type="checkbox"
                      checked={c.rateIds.includes(r.taxRateId)}
                      onChange={(e) => {
                        const ids = e.target.checked ? [...c.rateIds, r.taxRateId] : c.rateIds.filter((x) => x !== r.taxRateId);
                        void commands.taxCategorySave(c.taxCategoryId, c.name, ids, c.isDefault, c.version).then(done);
                      }}
                    />{" "}
                    {r.name}
                    {r.from || r.to ? ` (${r.from ?? "…"} – ${r.to ?? "…"})` : ""}
                  </label>
                ))}
              </div>
            </li>
          ))}
      </ul>
      <form
        className="setting-edit"
        aria-label="Add a tax category"
        onSubmit={(e) => {
          e.preventDefault();
          void commands.taxCategorySave(null, newCategory, [], false, null).then((res) => {
            if (done(res)) setNewCategory("");
          });
        }}
      >
        <input aria-label="New tax category" value={newCategory} onChange={(e) => setNewCategory(e.target.value)} placeholder="e.g. Zero-rated" />
        <button type="submit" className="secondary" disabled={!newCategory.trim()}>
          Add tax category
        </button>
      </form>
    </div>
  );
}
