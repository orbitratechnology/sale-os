import { useCallback, useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { SettingDto } from "../../ipc/bindings";
import { percentToPpm, ppmToPercent } from "../../lib/decimal";
import { describeError } from "../shell/errors";
import { TaxesPanel } from "./TaxesPanel";

/** The tax page sits beside the registry groups; it is not a registry setting. */
const TAXES = "Taxes";

/**
 * Settings (E04-T03, owner only). Every setting comes from Rust's registry with its type,
 * so this screen only draws the right control and sends the typed value back with the
 * version it showed. Each change is saved on its own and audited.
 */
export function SettingsScreen({ onBack, onChanged }: { onBack: () => void; onChanged: () => void }) {
  const [settings, setSettings] = useState<SettingDto[] | null>(null);
  const [group, setGroup] = useState<string | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  const load = useCallback(() => {
    void commands.settingsGet().then((res) => {
      if (res.status !== "ok") return setError(res.error);
      setSettings(res.data);
      setGroup((g) => g ?? res.data[0]?.group ?? null);
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

  const groups = [...new Set((settings ?? []).map((s) => s.group)), TAXES];
  return (
    <main className="sales">
      <header className="returns-header">
        <h1>Settings</h1>
        <button type="button" className="secondary" onClick={onBack}>
          Back to till <kbd>Esc</kbd>
        </button>
      </header>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <div className="receipt-widths" role="tablist" aria-label="Settings pages">
        {groups.map((g) => (
          <button key={g} type="button" role="tab" aria-selected={g === group} className={g === group ? "" : "secondary"} onClick={() => setGroup(g)}>
            {g}
          </button>
        ))}
      </div>
      {group === TAXES ? (
        <section className="card settings-list" aria-label={TAXES}>
          <TaxesPanel />
        </section>
      ) : (
        <section className="card settings-list" aria-label={group ?? "Settings"}>
          {(settings ?? [])
            .filter((s) => s.group === group)
            .map((s) => (
              <SettingRow
                key={s.key}
                setting={s}
                onSaved={(updated) => {
                  setSettings((all) => all?.map((x) => (x.key === updated.key ? updated : x)) ?? null);
                  onChanged();
                }}
                onConflict={load}
              />
            ))}
        </section>
      )}
    </main>
  );
}

function SettingRow({ setting, onSaved, onConflict }: { setting: SettingDto; onSaved: (s: SettingDto) => void; onConflict: () => void }) {
  const current: unknown = JSON.parse(setting.valueJson);
  const [draft, setDraft] = useState(() => toText(setting, current));
  const [state, setState] = useState<{ kind: "idle" } | { kind: "saved" } | { kind: "error"; message: string }>({ kind: "idle" });
  const id = `setting-${setting.key}`;

  async function save(valueJson: string) {
    const res = await commands.settingsUpdate(setting.key, valueJson, setting.version);
    if (res.status === "ok") {
      setState({ kind: "saved" });
      onSaved(res.data);
      return;
    }
    if (res.error.code === "CONFLICT_VERSION") {
      setState({ kind: "error", message: "Someone changed this meanwhile; the current value is shown." });
      onConflict();
      return;
    }
    setState({ kind: "error", message: describeError(res.error) });
  }

  /** The typed JSON for the text in the field, or an error to show. */
  function parse(text: string): string | { error: string } {
    const k = setting.kind;
    if (k.type === "int") {
      if (!/^\d+$/.test(text.trim())) return { error: `Enter a whole number from ${k.min} to ${k.max}.` };
      return text.trim().replace(/^0+(?=\d)/, "");
    }
    if (k.type === "ppm") {
      const ppm = percentToPpm(text);
      return ppm === null ? { error: "Enter a percentage from 0 to 100." } : String(ppm);
    }
    if (k.type === "lines") {
      const lines = text.split("\n").map((l) => l.trimEnd()).filter((l) => l.length > 0);
      return JSON.stringify(lines);
    }
    return text;
  }

  const k = setting.kind;
  const control = (() => {
    if (k.type === "bool") {
      return (
        <input
          id={id}
          type="checkbox"
          checked={current === true}
          onChange={(e) => void save(e.target.checked ? "true" : "false")}
        />
      );
    }
    if (k.type === "choice") {
      return (
        <select id={id} value={setting.valueJson} onChange={(e) => void save(e.target.value)}>
          {k.options.map((o) => (
            <option key={o} value={o}>
              {o.replace(/^"|"$/g, "")}
            </option>
          ))}
        </select>
      );
    }
    const input =
      k.type === "lines" ? (
        <textarea id={id} rows={Math.min(k.maxLines, 5)} value={draft} onChange={(e) => setDraft(e.target.value)} maxLength={k.maxLines * (k.maxChars + 1)} />
      ) : (
        <input id={id} inputMode="decimal" value={draft} onChange={(e) => setDraft(e.target.value)} className="qty-input" />
      );
    return (
      <form
        className="setting-edit"
        onSubmit={(e) => {
          e.preventDefault();
          const v = parse(draft);
          if (typeof v !== "string") return setState({ kind: "error", message: v.error });
          void save(v);
        }}
      >
        {input}
        {k.type === "ppm" && <span>%</span>}
        <button type="submit" className="secondary">
          Save
        </button>
      </form>
    );
  })();

  return (
    <div className="setting-row" data-testid="setting-row">
      <label htmlFor={id}>
        <strong>{setting.label}</strong>
        <span className="small">{setting.help}</span>
      </label>
      <div>
        {control}
        <span aria-live="polite" className="small">
          {state.kind === "saved" && " Saved."}
          {state.kind === "error" && (
            <span role="alert" className="field-error">
              {" "}
              {state.message}
            </span>
          )}
        </span>
      </div>
    </div>
  );
}

function toText(s: SettingDto, v: unknown): string {
  if (s.kind.type === "ppm" && typeof v === "number") return ppmToPercent(v);
  if (s.kind.type === "lines" && Array.isArray(v)) return v.join("\n");
  return typeof v === "string" ? v : JSON.stringify(v);
}
