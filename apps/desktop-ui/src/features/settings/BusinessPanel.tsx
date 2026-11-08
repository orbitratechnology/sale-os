import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { BusinessProfileDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";

interface Draft {
  name: string;
  legalName: string;
  taxRegistrationNo: string;
  addressLines: string;
  phone: string;
  email: string;
  registerName: string;
}

const LABELS: Record<keyof Draft, string> = {
  name: "Business name",
  legalName: "Legal name",
  taxRegistrationNo: "Tax registration number",
  addressLines: "Address",
  phone: "Phone",
  email: "Email",
  registerName: "This till's name",
};

const HELP: Partial<Record<keyof Draft, string>> = {
  name: "Printed at the top of every receipt.",
  taxRegistrationNo: "Printed on receipts, e.g. a VAT or GST number.",
  addressLines: "Up to 4 lines, printed under the name.",
  registerName: "Shown on receipts and reports, e.g. “Front till”.",
};

function draftOf(p: BusinessProfileDto): Draft {
  return {
    name: p.name,
    legalName: p.legalName ?? "",
    taxRegistrationNo: p.taxRegistrationNo ?? "",
    addressLines: p.addressLines.join("\n"),
    phone: p.phone ?? "",
    email: p.email ?? "",
    registerName: p.registerName,
  };
}

function message(e: IpcError): { field: keyof Draft | null; text: string } {
  const field = (e.details?.field ?? null) as keyof Draft | null;
  if (e.code === "CONFLICT_VERSION") return { field: null, text: "Someone changed these details meanwhile; the current ones are shown." };
  if (e.code === "VALIDATION_FAILED" && field && field in LABELS) {
    const label = LABELS[field];
    if (field === "email") return { field, text: "That is not an email address." };
    if (field === "addressLines" && !e.details?.value) return { field, text: "At most 4 address lines." };
    if (e.details?.value) return { field, text: `${label}: at most ${e.details.value} characters.` };
    return { field, text: `${label} is required.` };
  }
  return { field: null, text: describeError(e) };
}

/**
 * The business details printed on receipts and this till's name (E04-T03, owner). Saved
 * together with the versions shown; receipts made before keep the details they were made
 * with. Country, time zone, currency and tax mode were chosen at setup and stay fixed:
 * past sales and prices were made under them.
 */
export function BusinessPanel({ onChanged }: { onChanged: () => void }) {
  const [profile, setProfile] = useState<BusinessProfileDto | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [problem, setProblem] = useState<{ field: keyof Draft | null; text: string } | null>(null);
  const [saved, setSaved] = useState(false);

  const load = useCallback(() => {
    void commands.businessProfileGet().then((res) => {
      if (res.status !== "ok") return setProblem({ field: null, text: describeError(res.error) });
      setProfile(res.data);
      setDraft(draftOf(res.data));
    });
  }, []);
  useEffect(load, [load]);

  if (!profile || !draft) return problem ? <p role="alert" className="field-error">{problem.text}</p> : <p className="small">Loading…</p>;

  async function save(e: FormEvent) {
    e.preventDefault();
    if (!profile || !draft) return;
    const opt = (v: string) => v.trim() || null;
    const res = await commands.businessProfileUpdate(
      {
        name: draft.name,
        legalName: opt(draft.legalName),
        taxRegistrationNo: opt(draft.taxRegistrationNo),
        addressLines: draft.addressLines.split("\n").map((l) => l.trim()).filter(Boolean),
        phone: opt(draft.phone),
        email: opt(draft.email),
        registerName: draft.registerName,
      },
      profile.businessVersion,
      profile.registerVersion,
    );
    if (res.status === "ok") {
      setProfile(res.data);
      setDraft(draftOf(res.data));
      setProblem(null);
      setSaved(true);
      onChanged();
      return;
    }
    setSaved(false);
    setProblem(message(res.error));
    if (res.error.code === "CONFLICT_VERSION") load();
  }

  const input = (key: keyof Draft) => {
    const id = `business-${key}`;
    const invalid = problem?.field === key;
    const common = {
      id,
      value: draft[key],
      "aria-invalid": invalid || undefined,
      "aria-describedby": invalid ? `${id}-error` : undefined,
      onChange: (e: { target: { value: string } }) => {
        setSaved(false);
        setDraft({ ...draft, [key]: e.target.value });
      },
    };
    return (
      <div className="setting-row" key={key}>
        <label htmlFor={id}>
          <strong>{LABELS[key]}</strong>
          {HELP[key] && <span className="small">{HELP[key]}</span>}
        </label>
        <div>
          {key === "addressLines" ? <textarea rows={4} {...common} /> : <input type={key === "email" ? "email" : "text"} {...common} />}
          {invalid && (
            <span id={`${id}-error`} role="alert" className="field-error">
              {" "}
              {problem.text}
            </span>
          )}
        </div>
      </div>
    );
  };

  return (
    <form aria-label="Business details" noValidate onSubmit={(e) => void save(e)}>
      {(Object.keys(LABELS) as (keyof Draft)[]).map(input)}
      <div className="setting-row">
        <span>
          <strong>Fixed at setup</strong>
          <span className="small">Past sales and prices were made under these.</span>
        </span>
        <dl className="small" data-testid="fixed-profile">
          <dt>Country</dt>
          <dd>{profile.countryCode}</dd>
          <dt>Time zone</dt>
          <dd>{profile.timezone}</dd>
          <dt>Currency</dt>
          <dd>{profile.currencyCode}</dd>
          <dt>Prices</dt>
          <dd>{profile.pricesIncludeTax ? "include tax" : "exclude tax"}</dd>
          <dt>Till code</dt>
          <dd>{profile.registerCode}</dd>
        </dl>
      </div>
      <p aria-live="polite">
        <button type="submit">Save details</button>{" "}
        {saved && <span className="small">Saved. New receipts use these details.</span>}
        {problem && !problem.field && (
          <span role="alert" className="field-error">
            {problem.text}
          </span>
        )}
      </p>
    </form>
  );
}
