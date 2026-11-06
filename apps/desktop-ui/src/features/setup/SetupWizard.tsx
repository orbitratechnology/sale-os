import { useId, useState, type FormEvent, type ReactNode } from "react";
import { commands, type IpcError } from "../../ipc";
import { percentToPpm } from "../../lib/decimal";
import { describeError } from "../shell/errors";
import { localeDefaults } from "./localeDefaults";

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

export function SetupWizard({ onDone }: { onDone: () => void }) {
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
        return draft.businessName.trim() ? null : "Enter the business name.";
      case "Region":
        if (!/^[A-Za-z]{2}$/.test(draft.countryCode.trim())) return "Enter a two-letter country code, e.g. GB.";
        if (!draft.timezone.trim()) return "Enter a time zone, e.g. Europe/London.";
        return /^[A-Za-z]{3}$/.test(draft.currencyCode.trim()) ? null : "Enter a three-letter currency code, e.g. GBP.";
      case "Tax": {
        const used = draft.rates.filter((r) => r.name.trim() || r.percent.trim());
        for (const r of used) {
          if (!r.name.trim()) return "Give every tax rate a name.";
          if (percentToPpm(r.percent) === null) return `Enter the rate for ${r.name.trim()} as a percentage, e.g. 20.`;
        }
        const names = used.map((r) => r.name.trim().toLowerCase());
        return new Set(names).size === names.length ? null : "Tax rate names must be different.";
      }
      case "Owner":
        if (!draft.ownerName.trim()) return "Enter the owner's name.";
        if (!/^\d{4,8}$/.test(draft.pin)) return "The PIN must be 4 to 8 digits.";
        return draft.pin === draft.pinAgain ? null : "The two PINs do not match.";
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
        <h1>Set up Sale OS</h1>
        <ol className="steps" aria-label="Setup steps">
          {STEPS.map((s, i) => (
            <li key={s} aria-current={i === step ? "step" : undefined} className={i < step ? "done" : undefined}>
              {s}
            </li>
          ))}
        </ol>
      </header>
      <p className="notice">
        Not linked to the cloud: everything stays on this computer until you link it. The till starts in{" "}
        <strong>sandbox mode</strong> so you can practise; switch to live when you are ready.
      </p>

      <form className="card setup-form" onSubmit={(e) => void next(e)} noValidate>
        <h2>{STEPS[step]}</h2>
        {STEPS[step] === "Business" && (
          <>
            <Field label="Business name">
              {(id) => <input id={id} autoFocus value={draft.businessName} onChange={(e) => set("businessName", e.target.value)} />}
            </Field>
            <Field label="Address (printed on receipts, one line each)">
              {(id) => <textarea id={id} rows={3} value={draft.addressLines} onChange={(e) => set("addressLines", e.target.value)} />}
            </Field>
            <div className="row">
              <Field label="Phone (optional)">
                {(id) => <input id={id} type="tel" value={draft.phone} onChange={(e) => set("phone", e.target.value)} />}
              </Field>
              <Field label="Email (optional)">
                {(id) => <input id={id} type="email" value={draft.email} onChange={(e) => set("email", e.target.value)} />}
              </Field>
            </div>
          </>
        )}
        {STEPS[step] === "Region" && (
          <div className="row">
            <Field label="Country code">
              {(id) => <input id={id} autoFocus maxLength={2} value={draft.countryCode} onChange={(e) => set("countryCode", e.target.value)} />}
            </Field>
            <Field label="Time zone">
              {(id) => <input id={id} value={draft.timezone} onChange={(e) => set("timezone", e.target.value)} />}
            </Field>
            <Field label="Currency">
              {(id) => <input id={id} maxLength={3} value={draft.currencyCode} onChange={(e) => set("currencyCode", e.target.value)} />}
            </Field>
          </div>
        )}
        {STEPS[step] === "Tax" && (
          <>
            <fieldset className="choice">
              <legend>Shelf prices</legend>
              <label>
                <input type="radio" name="taxmode" autoFocus={draft.pricesIncludeTax} checked={draft.pricesIncludeTax} onChange={() => set("pricesIncludeTax", true)} />
                Include tax (common for VAT and GST)
              </label>
              <label>
                <input type="radio" name="taxmode" autoFocus={!draft.pricesIncludeTax} checked={!draft.pricesIncludeTax} onChange={() => set("pricesIncludeTax", false)} />
                Exclude tax (tax is added at the till)
              </label>
            </fieldset>
            <p className="small">Up to {MAX_RATES} rates now; more in settings. Leave empty if you do not charge tax.</p>
            {draft.rates.map((r, i) => (
              <div className="row" key={i}>
                <Field label={`Rate ${i + 1} name`}>
                  {(id) => (
                    <input id={id} value={r.name} onChange={(e) => set("rates", draft.rates.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))} />
                  )}
                </Field>
                <Field label={`Rate ${i + 1} (%)`}>
                  {(id) => (
                    <input id={id} inputMode="decimal" value={r.percent} onChange={(e) => set("rates", draft.rates.map((x, j) => (j === i ? { ...x, percent: e.target.value } : x)))} />
                  )}
                </Field>
              </div>
            ))}
            {draft.rates.length < MAX_RATES && (
              <button type="button" className="secondary" onClick={() => set("rates", [...draft.rates, { name: "", percent: "" }])}>
                Add another rate
              </button>
            )}
          </>
        )}
        {STEPS[step] === "Owner" && (
          <>
            <Field label="Owner's name">
              {(id) => <input id={id} autoFocus autoComplete="off" value={draft.ownerName} onChange={(e) => set("ownerName", e.target.value)} />}
            </Field>
            <div className="row">
              <Field label="PIN (4 to 8 digits)">
                {(id) => <input id={id} type="password" inputMode="numeric" autoComplete="new-password" value={draft.pin} onChange={(e) => set("pin", e.target.value)} />}
              </Field>
              <Field label="PIN again">
                {(id) => <input id={id} type="password" inputMode="numeric" autoComplete="new-password" value={draft.pinAgain} onChange={(e) => set("pinAgain", e.target.value)} />}
              </Field>
            </div>
            <p className="small">The owner can add cashiers and managers after setup.</p>
          </>
        )}
        {STEPS[step] === "Review" && (
          <dl>
            <dt>Business</dt>
            <dd>{draft.businessName}</dd>
            <dt>Region</dt>
            <dd>
              {draft.countryCode.toUpperCase()} · {draft.timezone} · {draft.currencyCode.toUpperCase()}
            </dd>
            <dt>Tax</dt>
            <dd>
              {draft.pricesIncludeTax ? "Prices include tax" : "Tax added at the till"}
              {draft.rates.filter((r) => r.name.trim()).map((r) => ` · ${r.name.trim()} ${r.percent.trim()}%`)}
            </dd>
            <dt>Owner</dt>
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
            {describeError(failure)} <span className="mono small">ref {failure.correlationId}</span>
          </p>
        )}
        <div className="actions">
          {step > 0 && (
            <button type="button" className="secondary" onClick={() => setStep((s) => s - 1)} disabled={busy}>
              Back
            </button>
          )}
          {/* Keyed so it remounts on Review: autoFocus only applies when an element mounts. */}
          <button type="submit" key={STEPS[step] === "Review" ? "create" : "next"} disabled={busy} autoFocus={STEPS[step] === "Review"}>
            {busy ? "Creating…" : STEPS[step] === "Review" ? "Create and sign in" : "Next"}
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
