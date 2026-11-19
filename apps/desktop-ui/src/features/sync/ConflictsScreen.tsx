import {
  ArrowLeft,
  CloudOff,
} from "../../ui/icons";
import { useCallback, useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { ConflictDto } from "../../ipc/bindings";
import { scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { describeError } from "../shell/errors";

type Keep = "local" | "server" | "custom";

const isMoney = (field: string) => /\/(price|cost)$/.test(field);

function show(json: string, field = "", exponent = 2): string {
  try {
    const v: unknown = JSON.parse(json);
    if (isMoney(field) && typeof v === "number") return formatMoney(String(v), { exponent });
    if (v === null) return "(none)";
    if (Array.isArray(v)) return v.length ? v.join(", ") : "(none)";
    if (typeof v === "boolean") return v ? "yes" : "no";
    if (typeof v === "object") return JSON.stringify(v);
    return String(v);
  } catch {
    return json;
  }
}

/** Fields whose custom value is typed as money, text, a number, or not at all. */
function customKind(field: string, sample: string): "money" | "text" | "number" | null {
  if (isMoney(field)) return "money";
  if (field === "pinHash" || field.endsWith("/barcodes") || /^variants\/[^/]+$/.test(field)) return null;
  try {
    const v: unknown = JSON.parse(sample);
    if (typeof v === "number") return "number";
    if (typeof v === "string" || v === null) return "text";
  } catch {
    return null;
  }
  return null;
}

/**
 * Sync conflicts (E18-T02, AT-07; managers and owners). Something was changed here and in the
 * cloud at the same time: the cloud kept its version, and this till keeps using its own until
 * someone chooses, field by field, what to keep. Nothing is overwritten silently, and selling
 * never waits for this.
 */
export function ConflictsScreen({ onBack, currencyExponent }: { onBack: () => void; currencyExponent: number }) {
  const [list, setList] = useState<ConflictDto[] | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const load = useCallback(() => {
    void commands.syncConflictsList().then((res) => {
      if (res.status !== "ok") return setError(res.error);
      setList(res.data);
    });
  }, []);
  useEffect(load, [load]);

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
    <main className="sales">
      <header className="returns-header">
        <h1 className="page-title">
          <CloudOff />
          Sync conflicts
        </h1>
        <button type="button" className="secondary" onClick={onBack}>
          <ArrowLeft />
          Back to till <kbd>Esc</kbd>
        </button>
      </header>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      {done && <p role="status">{done}</p>}
      {list && list.length === 0 && <p>No conflicts. Everything changed here agrees with the cloud.</p>}
      {list?.map((c) => (
        <Conflict
          key={c.conflictId}
          conflict={c}
          exponent={currencyExponent}
          onResolved={(what) => {
            setDone(what);
            setError(null);
            load();
          }}
          onError={setError}
        />
      ))}
    </main>
  );
}

function Conflict({
  conflict: c,
  exponent,
  onResolved,
  onError,
}: {
  conflict: ConflictDto;
  exponent: number;
  onResolved: (what: string) => void;
  onError: (e: IpcError) => void;
}) {
  const [keep, setKeep] = useState<Record<string, Keep>>(() => Object.fromEntries(c.fields.map((f) => [f.field, "local"])));
  const [custom, setCustom] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const [invalid, setInvalid] = useState<string | null>(null);

  async function submit() {
    setInvalid(null);
    const choices = [];
    for (const f of c.fields) {
      const k = keep[f.field] ?? "local";
      const kind = customKind(f.field, f.localJson);
      const raw = (custom[f.field] ?? "").trim();
      let valueJson: string | null = null;
      if (k === "custom") {
        if (kind === "money") {
          const minor = scaleDecimal(raw, exponent);
          if (minor === null) return setInvalid(`${f.label}: enter an amount like 1.50.`);
          valueJson = minor;
        } else if (kind === "number") {
          if (!/^-?\d+$/.test(raw)) return setInvalid(`${f.label}: enter a whole number.`);
          valueJson = raw;
        } else {
          valueJson = JSON.stringify(raw);
        }
      }
      choices.push({ field: f.field, keep: k, valueJson });
    }
    setBusy(true);
    const res = await commands.syncConflictResolve(c.conflictId, choices, null);
    setBusy(false);
    if (res.status !== "ok") return onError(res.error);
    onResolved(`${c.title}: resolved. It is being sent to the cloud.`);
  }

  async function identifiers(mode: "remove" | "move") {
    setBusy(true);
    const res = await commands.syncConflictResolve(c.conflictId, [], mode);
    setBusy(false);
    if (res.status !== "ok") return onError(res.error);
    onResolved(
      mode === "move"
        ? `${c.title} now has ${c.taken.join(", ")}; ${c.heldBy ?? "the other product"} no longer does.`
        : `${c.taken.join(", ")} removed from ${c.title}.`,
    );
  }

  return (
    <section className="card conflict" aria-label={`Conflict: ${c.title}`}>
      <h2>
        {c.title} <span className="small">({c.aggregateType.replace("_", " ")})</span>
      </h2>
      {c.reason === "VERSION" ? (
        <>
          <p>Changed here and in the cloud at the same time. Choose what to keep for each field.</p>
          <table>
            <thead>
              <tr>
                <th scope="col">Field</th>
                <th scope="col">Here</th>
                <th scope="col">Cloud</th>
                <th scope="col">Keep</th>
              </tr>
            </thead>
            <tbody>
              {c.fields.map((f) => {
                const kind = customKind(f.field, f.localJson);
                const name = `keep-${c.conflictId}-${f.field}`;
                return (
                  <tr key={f.field}>
                    <th scope="row">{f.label}</th>
                    <td>{show(f.localJson, f.field, exponent)}</td>
                    <td>{show(f.serverJson, f.field, exponent)}</td>
                    <td>
                      <fieldset aria-label={`Keep for ${f.label}`}>
                        {(["local", "server", ...(kind ? ["custom"] : [])] as Keep[]).map((k) => (
                          <label key={k}>
                            <input
                              type="radio"
                              name={name}
                              checked={(keep[f.field] ?? "local") === k}
                              onChange={() => setKeep((s) => ({ ...s, [f.field]: k }))}
                            />
                            {k === "local" ? "Here" : k === "server" ? "Cloud" : "Other"}
                          </label>
                        ))}
                        {keep[f.field] === "custom" && (
                          <input
                            aria-label={`New ${f.label}`}
                            inputMode={kind === "money" ? "decimal" : kind === "number" ? "numeric" : "text"}
                            value={custom[f.field] ?? ""}
                            onChange={(e) => setCustom((s) => ({ ...s, [f.field]: e.target.value }))}
                          />
                        )}
                      </fieldset>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          {invalid && (
            <p role="alert" className="field-error">
              {invalid}
            </p>
          )}
          <button type="button" disabled={busy} onClick={() => void submit()}>
            Resolve
          </button>
        </>
      ) : (
        <>
          <p>
            {c.reason === "SKU_TAKEN" ? "SKU" : "Barcode"} {c.taken.join(", ")} also belongs to{" "}
            <strong>{c.heldBy || "another product"}</strong> in the cloud.
          </p>
          <button type="button" disabled={busy} onClick={() => void identifiers("remove")}>
            Remove from {c.title}
          </button>{" "}
          <button type="button" className="secondary" disabled={busy} onClick={() => void identifiers("move")}>
            Move to {c.title}
          </button>
        </>
      )}
    </section>
  );
}
