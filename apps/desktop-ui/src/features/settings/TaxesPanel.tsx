import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { TaxConfigDto, TaxRateConfigDto } from "../../ipc/bindings";
import { percentToPpm, ppmToPercent } from "../../lib/decimal";
import { describeError } from "../shell/errors";
import { useT, type T } from "../../i18n";
import { SectionTitle } from "../../ui/PanelHeader";
import { Pencil, Percent, Plus, Tag } from "../../ui/icons";

function message(e: IpcError, t: T): string {
  const d = e.details;
  if (e.code === "VALIDATION_FAILED") {
    if (d?.field === "categories") return t("Still used by {name}: take it out of those first.", { name: d.value ?? t("a tax category") });
    if (d?.field === "name" && d.value) return t("Another active rate called \"{name}\" covers some of the same days.", { name: d.value });
    if (d?.field === "name") return t("Enter a name.");
    if (d?.field === "period") return t("The end date must be after the start date.");
    if (d?.field === "default") return t("One tax category must stay the default: make another one the default instead.");
  }
  if (e.code === "CONFLICT_VERSION") return t("It was changed meanwhile; the current values are shown.");
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
export function TaxesPanel({ registered = true }: { registered?: boolean }) {
  const t = useT();
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
    else setError(message(res.error, t));
    load();
    return res.status === "ok";
  };

  async function saveRate(e: FormEvent) {
    e.preventDefault();
    const ppm = percentToPpm(rate.percent);
    if (ppm === null) return setError(t("Enter the rate as a percentage, e.g. 20 or 7.5."));
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

  if (!cfg) return <p className="small">{t("Loading…")}</p>;
  const activeRates = cfg.rates.filter((r) => !r.archived);
  return (
    <div className="taxes">
      {error && (
        <p role="alert" className="field-error">
          {error}
        </p>
      )}
      {!registered && (
        <p className="callout-warning" role="note">
          {t(
            "Not registered for VAT: sales carry no tax, receipts show no tax or tax number, and no tax invoices are issued. The rates below are kept for when you register.",
          )}
        </p>
      )}
      <SectionTitle icon={Percent}>{t("Rates")}</SectionTitle>
      <table className="sales-table" aria-label={t("Tax rates")}>
        <thead>
          <tr>
            <th scope="col">{t("Name")}</th>
            <th scope="col" className="num">
              {t("Rate")}
            </th>
            <th scope="col">{t("From")}</th>
            <th scope="col">{t("Until (not including)")}</th>
            <th scope="col" className="row-actions">
              <span className="visually-hidden">{t("Actions")}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {activeRates.map((r) => (
            <tr key={r.taxRateId} data-testid="rate-row">
              <th scope="row">{r.name}</th>
              <td className="num">{ppmToPercent(r.ratePpm)}%</td>
              <td>{r.from ?? t("always")}</td>
              <td>{r.to ?? t("no end")}</td>
              <td className="row-actions">
                <button type="button" className="link" aria-label={t("Edit {name}", { name: r.name })} onClick={() => edit(r)}>
                  {t("Edit")}
                </button>{" "}
                <button
                  type="button"
                  className="link"
                  aria-label={t("Archive {name}", { name: r.name })}
                  onClick={() => void commands.taxRateArchive(r.taxRateId, r.version).then(done)}
                >
                  {t("Archive")}
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <SectionTitle icon={rate.id ? Pencil : Plus}>{rate.id ? t("Change a rate") : t("Add a rate")}</SectionTitle>
      <form className="card sales-filters" aria-label={rate.id ? t("Change a rate") : t("Add a rate")} onSubmit={(e) => void saveRate(e)}>
        <label>
          {t("Name")}
          <input value={rate.name} onChange={(e) => setRate({ ...rate, name: e.target.value })} placeholder={t("e.g. VAT 20%")} />
        </label>
        <label>
          {t("Rate (%)")}
          <input inputMode="decimal" value={rate.percent} onChange={(e) => setRate({ ...rate, percent: e.target.value })} />
        </label>
        <label>
          {t("From")}
          <input type="date" value={rate.from} onChange={(e) => setRate({ ...rate, from: e.target.value })} />
        </label>
        <label>
          {t("Until (not including)")}
          <input type="date" value={rate.to} onChange={(e) => setRate({ ...rate, to: e.target.value })} />
        </label>
        <p className="small form-hint">{t("Leave the dates empty for a rate that always applies.")}</p>
        <span className="button-row form-hint">
          <button type="submit">{rate.id ? t("Save rate") : t("Add rate")}</button>
          {rate.id && (
            <button type="button" className="secondary" onClick={() => setRate(blank)}>
              {t("Cancel")}
            </button>
          )}
        </span>
      </form>

      <SectionTitle icon={Tag}>{t("Tax categories")}</SectionTitle>
      <p className="small">{t("Each product belongs to one tax category. Tick the rates that apply to it; they are added up on the receipt.")}</p>
      <ul className="plain-list" aria-label={t("Tax categories")}>
        {cfg.categories
          .filter((c) => !c.archived)
          .map((c) => (
            <li key={c.taxCategoryId} data-testid="taxcat-row">
              <strong>{c.name}</strong>
              {c.isDefault ? (
                <span className="tag">{t("default")}</span>
              ) : (
                <>
                  {" "}
                  <button
                    type="button"
                    className="link"
                    onClick={() => void commands.taxCategorySave(c.taxCategoryId, c.name, c.rateIds, true, c.version).then(done)}
                  >
                    {t("Make default")}
                  </button>
                </>
              )}
              <div role="group" aria-label={t("Rates in {name}", { name: c.name })} className="check-list">
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
        aria-label={t("Add a tax category")}
        onSubmit={(e) => {
          e.preventDefault();
          void commands.taxCategorySave(null, newCategory, [], false, null).then((res) => {
            if (done(res)) setNewCategory("");
          });
        }}
      >
        <input aria-label={t("New tax category")} value={newCategory} onChange={(e) => setNewCategory(e.target.value)} placeholder={t("e.g. Zero-rated")} />
        <button type="submit" className="secondary" disabled={!newCategory.trim()}>
          {t("Add tax category")}
        </button>
      </form>
    </div>
  );
}
