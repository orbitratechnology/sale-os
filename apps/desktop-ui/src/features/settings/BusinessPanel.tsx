import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { BusinessProfileDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import type { LucideIcon } from "lucide-react";
import { Hash, Image, Landmark, Lock, Mail, MapPin, Monitor, Phone, Store } from "../../ui/icons";

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

const ICONS: Record<keyof Draft, LucideIcon> = {
  name: Store,
  legalName: Landmark,
  taxRegistrationNo: Hash,
  addressLines: MapPin,
  phone: Phone,
  email: Mail,
  registerName: Monitor,
};

const HELP: Partial<Record<keyof Draft, string>> = {
  legalName: "The registered company name, if it is different.",
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
      "aria-describedby": [HELP[key] ? `${id}-help` : "", invalid ? `${id}-error` : ""].filter(Boolean).join(" ") || undefined,
      onChange: (e: { target: { value: string } }) => {
        setSaved(false);
        setDraft({ ...draft, [key]: e.target.value });
      },
    };
    const Icon = ICONS[key];
    return (
      <div className="setting-row has-icon" key={key}>
        <span className="icon-tile sm" aria-hidden="true">
          <Icon />
        </span>
        <div className="setting-text">
          <label htmlFor={id}>{LABELS[key]}</label>
          {HELP[key] && (
            <span id={`${id}-help`} className="small">
              {HELP[key]}
            </span>
          )}
        </div>
        <div className="setting-control">
          {key === "addressLines" ? <textarea rows={4} {...common} /> : <input type={key === "email" ? "email" : "text"} {...common} />}
          {invalid && (
            <span id={`${id}-error`} role="alert" className="field-error">
              {problem.text}
            </span>
          )}
        </div>
      </div>
    );
  };

  return (
    <>
      <LogoRow
        profile={profile}
        onChanged={(p) => {
          setProfile(p);
          onChanged();
        }}
        onConflict={load}
      />
      <form aria-label="Business details" noValidate onSubmit={(e) => void save(e)}>
      {(Object.keys(LABELS) as (keyof Draft)[]).map(input)}
      <div className="setting-row has-icon">
        <span className="icon-tile sm" aria-hidden="true">
          <Lock />
        </span>
        <span>
          <strong>Fixed at setup</strong>
          <span className="small">Chosen when the till was set up. They cannot change, because past sales and prices were made under them.</span>
        </span>
        <div className="setting-control">
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
      </div>
      <div className="actions" aria-live="polite">
        {saved && <span className="saved-note">Saved. New receipts use these details.</span>}
        {problem && !problem.field && (
          <span role="alert" className="field-error">
            {problem.text}
          </span>
        )}
        <button type="submit">Save details</button>
      </div>
      </form>
    </>
  );
}

const LOGO_PROBLEMS: Record<string, string> = {
  tooLarge: "That file is larger than 10 MB. Choose a smaller image.",
  notAnImage: "That file is not a PNG or JPEG image.",
  unreadable: "That image could not be read. Try saving it again as PNG.",
};

/**
 * The business logo (SET-001): chosen through the system dialog (Rust opens it), kept as a
 * PNG of at most 512 px, and printed at the top of receipts saved as PDF.
 */
function LogoRow({
  profile,
  onChanged,
  onConflict,
}: {
  profile: BusinessProfileDto;
  onChanged: (p: BusinessProfileDto) => void;
  onConflict: () => void;
}) {
  const [fetched, setFetched] = useState<{ version: number; url: string | null } | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!profile.hasLogo) return;
    let live = true;
    const version = profile.businessVersion;
    void commands.businessLogoGet().then((res) => {
      if (live && res.status === "ok") setFetched({ version, url: res.data });
    });
    return () => {
      live = false;
    };
  }, [profile.hasLogo, profile.businessVersion]);
  const url = profile.hasLogo && fetched?.version === profile.businessVersion ? fetched.url : null;

  const failed = (e: IpcError) => {
    if (e.code === "CONFLICT_VERSION") {
      setProblem("Someone changed the business details meanwhile; try again.");
      onConflict();
    } else setProblem(e.details?.field === "logo" ? (LOGO_PROBLEMS[e.details.value ?? ""] ?? describeError(e)) : describeError(e));
  };
  const choose = async () => {
    setBusy(true);
    const res = await commands.businessLogoChoose(profile.businessVersion);
    setBusy(false);
    if (res.status !== "ok") return failed(res.error);
    setProblem(null);
    if (res.data) onChanged(res.data);
  };
  const remove = async () => {
    const res = await commands.businessLogoClear(profile.businessVersion);
    if (res.status !== "ok") return failed(res.error);
    setProblem(null);
    onChanged(res.data);
  };

  return (
    <section className="setting-row has-icon" aria-label="Logo">
      <span className="icon-tile sm" aria-hidden="true">
        <Image />
      </span>
      <span>
        <strong>Logo</strong>
        <span className="small">A PNG or JPEG picture, shown at the top of receipts saved as PDF.</span>
      </span>
      <div className="setting-control">
        {url ? <img src={url} alt="The business logo" className="logo-preview" /> : <p className="small">{profile.hasLogo ? "Loading…" : "No logo."}</p>}
        <p className="button-row">
          <button type="button" className="secondary" disabled={busy} onClick={() => void choose()}>
            {profile.hasLogo ? "Change logo…" : "Choose a logo…"}
          </button>
          {profile.hasLogo && (
            <button type="button" className="secondary" onClick={() => void remove()}>
              Remove logo
            </button>
          )}
        </p>
        {problem && (
          <p role="alert" className="field-error">
            {problem}
          </p>
        )}
      </div>
    </section>
  );
}
