import { useId, useState, type FormEvent, type ReactNode } from "react";
import { commands, type IpcError } from "../../ipc";
import { percentToPpm } from "../../lib/decimal";
import { describeError } from "../shell/errors";
import { localeDefaults } from "./localeDefaults";
import { useT } from "../../i18n";

/**
 * First run (E04-T02): business → region → tax → owner → review. Each step is a form, so
 * Enter moves on and every control is reachable by Tab. Nothing is written until the last
 * step; then Rust creates everything in one transaction and the owner is signed in with
 * the PIN they just chose.
 */
interface Rate {
  name: string;
  percent: string;
}

interface Draft {
  businessName: string;
  addressLines: string;
  phone: string;
  email: string;
  countryCode: string;
  timezone: string;
  currencyCode: string;
  pricesIncludeTax: boolean;
  rates: Rate[];
  ownerName: string;
  pin: string;
  pinAgain: string;
}

const STEPS = ["Business", "Region", "Tax", "Owner", "Review"] as const;
const MAX_RATES = 3;

/** The steps' names as shown, translated at render (`STEPS` stays the code's own names). */
const STEP_COPY: Record<(typeof STEPS)[number], string> = {
  Business: "Business",
  Region: "Region",
  Tax: "Tax",
  Owner: "Owner",
  Review: "Review",
};

export function SetupWizard({ onDone }: { onDone: () => void }) {
  const t = useT();
  // The notice names the mode in bold: the sentence is translated whole and split where it goes.
  const [noticeBefore, noticeAfter = ""] = t(
    "Not linked to the cloud: everything stays on this computer until you link it. The till starts in {mode} so you can practise; switch to live when you are ready.",
  ).split("{mode}");
  const defaults = localeDefaults();
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState<Draft>({
    businessName: "",
    addressLines: "",
    phone: "",
    email: "",
    countryCode: defaults.countryCode,
    timezone: defaults.timezone,
    currencyCode: defaults.currencyCode,
    pricesIncludeTax: true,
    rates: [{ name: "Standard", percent: "" }],
    ownerName: "",
    pin: "",
    pinAgain: "",
  });
  const [problem, setProblem] = useState<string | null>(null);
  const [failure, setFailure] = useState<IpcError | null>(null);
  const [busy, setBusy] = useState(false);

  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((d) => ({ ...d, [key]: value }));

  function check(): string | null {
    switch (STEPS[step]) {
      case "Business":
        return draft.businessName.trim() ? null : t("Enter the business name.");
      case "Region":
        if (!/^[A-Za-z]{2}$/.test(draft.countryCode.trim())) return t("Enter a two-letter country code, e.g. GB.");
        if (!draft.timezone.trim()) return t("Enter a time zone, e.g. Europe/London.");
        return /^[A-Za-z]{3}$/.test(draft.currencyCode.trim()) ? null : t("Enter a three-letter currency code, e.g. GBP.");
      case "Tax": {
        const used = draft.rates.filter((r) => r.name.trim() || r.percent.trim());
        for (const r of used) {
          if (!r.name.trim()) return t("Give every tax rate a name.");
          if (percentToPpm(r.percent) === null) return t("Enter the rate for {name} as a percentage, e.g. 20.", { name: r.name.trim() });
        }
        const names = used.map((r) => r.name.trim().toLowerCase());
        return new Set(names).size === names.length ? null : t("Tax rate names must be different.");
      }
      case "Owner":
        if (!draft.ownerName.trim()) return t("Enter the owner's name.");
        if (!/^\d{4,8}$/.test(draft.pin)) return t("The PIN must be 4 to 8 digits.");
        return draft.pin === draft.pinAgain ? null : t("The two PINs do not match.");
      default:
        return null;
    }
  }

  async function next(e: FormEvent) {
    e.preventDefault();
    const issue = check();
    setProblem(issue);
    if (issue) return;
    if (STEPS[step] !== "Review") {
      setStep((s) => s + 1);
      return;
    }
    setBusy(true);
    setFailure(null);
    const rates = draft.rates
      .filter((r) => r.name.trim())
      .map((r) => ({ name: r.name.trim(), ratePpm: percentToPpm(r.percent) ?? 0 }));
    const created = await commands.setupCreateWorkspace({
      businessName: draft.businessName.trim(),
      legalName: null,
      addressLines: draft.addressLines
        .split("\n")
        .map((l) => l.trim())
        .filter(Boolean),
      phone: draft.phone.trim() || null,
      email: draft.email.trim() || null,
      countryCode: draft.countryCode.trim().toUpperCase(),
      timezone: draft.timezone.trim(),
      currencyCode: draft.currencyCode.trim().toUpperCase(),
      pricesIncludeTax: draft.pricesIncludeTax,
      locationName: "Main",
      registerName: "Till 1",
      ownerName: draft.ownerName.trim(),
      ownerPin: draft.pin,
      taxRates: rates,
    });
    if (created.status === "error") {
      setBusy(false);
      setFailure(created.error);
      return;
    }
    // Sign the owner in: a real, audited sign-in. The PIN is dropped from memory afterwards.
    await commands.authLogin(created.data.ownerUserId, draft.pin);
    setDraft((d) => ({ ...d, pin: "", pinAgain: "" }));
    onDone();
  }

  return (
    <main className="setup">
      <header className="setup-head">
        <h1>{t("Set up Sale OS")}</h1>
        <ol className="steps" aria-label={t("Setup steps")}>
          {STEPS.map((s, i) => (
            <li key={s} aria-current={i === step ? "step" : undefined} className={i < step ? "done" : undefined}>
              {t(STEP_COPY[s])}
            </li>
          ))}
        </ol>
      </header>
      <p className="notice">
        {noticeBefore}
        <strong>{t("sandbox mode")}</strong>
        {noticeAfter}
      </p>

      <form className="card setup-form" onSubmit={(e) => void next(e)} noValidate>
        <h2>{t(STEP_COPY[STEPS[step] ?? "Business"])}</h2>
        {STEPS[step] === "Business" && (
          <>
            <Field label={t("Business name")}>
              {(id) => <input id={id} autoFocus value={draft.businessName} onChange={(e) => set("businessName", e.target.value)} />}
            </Field>
            <Field label={t("Address (printed on receipts, one line each)")}>
              {(id) => <textarea id={id} rows={3} value={draft.addressLines} onChange={(e) => set("addressLines", e.target.value)} />}
            </Field>
            <div className="row">
              <Field label={t("Phone (optional)")}>
                {(id) => <input id={id} type="tel" value={draft.phone} onChange={(e) => set("phone", e.target.value)} />}
              </Field>
              <Field label={t("Email (optional)")}>
                {(id) => <input id={id} type="email" value={draft.email} onChange={(e) => set("email", e.target.value)} />}
              </Field>
            </div>
          </>
        )}
        {STEPS[step] === "Region" && (
          <div className="row">
            <Field label={t("Country code")}>
              {(id) => <input id={id} autoFocus maxLength={2} value={draft.countryCode} onChange={(e) => set("countryCode", e.target.value)} />}
            </Field>
            <Field label={t("Time zone")}>
              {(id) => <input id={id} value={draft.timezone} onChange={(e) => set("timezone", e.target.value)} />}
            </Field>
            <Field label={t("Currency")}>
              {(id) => <input id={id} maxLength={3} value={draft.currencyCode} onChange={(e) => set("currencyCode", e.target.value)} />}
            </Field>
          </div>
        )}
        {STEPS[step] === "Tax" && (
          <>
            <fieldset className="choice">
              <legend>{t("Shelf prices")}</legend>
              <label>
                <input type="radio" name="taxmode" autoFocus={draft.pricesIncludeTax} checked={draft.pricesIncludeTax} onChange={() => set("pricesIncludeTax", true)} />
                {t("Include tax (common for VAT and GST)")}
              </label>
              <label>
                <input type="radio" name="taxmode" autoFocus={!draft.pricesIncludeTax} checked={!draft.pricesIncludeTax} onChange={() => set("pricesIncludeTax", false)} />
                {t("Exclude tax (tax is added at the till)")}
              </label>
            </fieldset>
            <p className="small">
              {t("Up to {n} rates now; more in settings. Not registered for VAT? Leave the rates empty: sales then carry no tax. Change it later in Settings → Taxes.", { n: MAX_RATES })}
            </p>
            {draft.rates.map((r, i) => (
              <div className="row" key={i}>
                <Field label={t("Rate {n} name", { n: i + 1 })}>
                  {(id) => (
                    <input id={id} value={r.name} onChange={(e) => set("rates", draft.rates.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))} />
                  )}
                </Field>
                <Field label={t("Rate {n} (%)", { n: i + 1 })}>
                  {(id) => (
                    <input id={id} inputMode="decimal" value={r.percent} onChange={(e) => set("rates", draft.rates.map((x, j) => (j === i ? { ...x, percent: e.target.value } : x)))} />
                  )}
                </Field>
              </div>
            ))}
            {draft.rates.length < MAX_RATES && (
              <button type="button" className="secondary" onClick={() => set("rates", [...draft.rates, { name: "", percent: "" }])}>
                {t("Add another rate")}
              </button>
            )}
          </>
        )}
        {STEPS[step] === "Owner" && (
          <>
            <Field label={t("Owner's name")}>
              {(id) => <input id={id} autoFocus autoComplete="off" value={draft.ownerName} onChange={(e) => set("ownerName", e.target.value)} />}
            </Field>
            <div className="row">
              <Field label={t("PIN (4 to 8 digits)")}>
                {(id) => <input id={id} type="password" inputMode="numeric" autoComplete="new-password" value={draft.pin} onChange={(e) => set("pin", e.target.value)} />}
              </Field>
              <Field label={t("PIN again")}>
                {(id) => <input id={id} type="password" inputMode="numeric" autoComplete="new-password" value={draft.pinAgain} onChange={(e) => set("pinAgain", e.target.value)} />}
              </Field>
            </div>
            <p className="small">{t("The owner can add cashiers and managers after setup.")}</p>
          </>
        )}
        {STEPS[step] === "Review" && (
          <dl>
            <dt>{t("Business")}</dt>
            <dd>{draft.businessName}</dd>
            <dt>{t("Region")}</dt>
            <dd>
              {draft.countryCode.toUpperCase()} · {draft.timezone} · {draft.currencyCode.toUpperCase()}
            </dd>
            <dt>{t("Tax")}</dt>
            <dd>
              {draft.pricesIncludeTax ? t("Prices include tax") : t("Tax added at the till")}
              {draft.rates.filter((r) => r.name.trim()).map((r) => ` · ${r.name.trim()} ${r.percent.trim()}%`)}
            </dd>
            <dt>{t("Owner")}</dt>
            <dd>{draft.ownerName}</dd>
          </dl>
        )}

        {problem && (
          <p role="alert" className="field-error">
            {problem}
          </p>
        )}
        {failure && (
          <p role="alert" className="field-error">
            {describeError(failure)} <span className="mono small">{t("ref")} {failure.correlationId}</span>
          </p>
        )}
        <div className="actions">
          {step > 0 && (
            <button type="button" className="secondary" onClick={() => setStep((s) => s - 1)} disabled={busy}>
              {t("Back")}
            </button>
          )}
          {/* Keyed so it remounts on Review: autoFocus only applies when an element mounts. */}
          <button type="submit" key={STEPS[step] === "Review" ? "create" : "next"} disabled={busy} autoFocus={STEPS[step] === "Review"}>
            {busy ? t("Creating…") : STEPS[step] === "Review" ? t("Create and sign in") : t("Next")}
          </button>
        </div>
      </form>
    </main>
  );
}

function Field({ label, children }: { label: string; children: (id: string) => ReactNode }) {
  const id = useId();
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      {children(id)}
    </div>
  );
}
