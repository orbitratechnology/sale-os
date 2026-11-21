import { TabList, tabPanelProps } from "../../ui/Tabs";
import type { LucideIcon } from "lucide-react";
import {
  ArrowLeft,
  BadgePercent,
  Banknote,
  Boxes,
  Building,
  ClipboardList,
  Cloud,
  CornerDownLeft,
  CreditCard,
  Database,
  Gauge,
  Package,
  PackageMinus,
  PanelBottom,
  PanelTop,
  Percent,
  Printer,
  ReceiptText,
  Ruler,
  ScanBarcode,
  Settings,
  ShieldCheck,
  Timer,
  Undo2,
  Users,
  Volume2,
  Wallet,
  Weight,
} from "../../ui/icons";
import { PanelHeader } from "../../ui/PanelHeader";
import { useCallback, useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { SettingDto } from "../../ipc/bindings";
import { percentToPpm, ppmToPercent } from "../../lib/decimal";
import { describeError } from "../shell/errors";
import { BackupsPanel } from "./BackupsPanel";
import { BusinessPanel } from "./BusinessPanel";
import { CloudPanel } from "./CloudPanel";
import { StaffPanel } from "./StaffPanel";
import { DevicesPanel } from "./DevicesPanel";
import { TaxesPanel } from "./TaxesPanel";

/** The business and tax pages sit beside the registry groups; they are not registry settings. */
const BUSINESS = "Business";
const TAXES = "Taxes";
const CLOUD = "Cloud";
const BACKUPS = "Backups";
const STAFF = "Staff";
const DEVICES = "Devices";

/**
 * Settings (E04-T03, owner only). Every setting comes from Rust's registry with its type,
 * so this screen only draws the right control and sends the typed value back with the
 * version it showed. Each change is saved on its own and audited.
 */
export function SettingsScreen({
  initialGroup,
  onBack,
  onChanged,
  onRelinked,
}: {
  /** The page to open on (e.g. "Staff" from Getting started); otherwise the first. */
  initialGroup?: string | undefined;
  onBack: () => void;
  onChanged: () => void;
  /** The till joined another business: its data and staff were replaced. */
  onRelinked: () => void;
}) {
  const [settings, setSettings] = useState<SettingDto[] | null>(null);
  const [group, setGroup] = useState<string | null>(initialGroup ?? null);
  const [error, setError] = useState<IpcError | null>(null);
  /** A manager: only the Devices page (the rest is the owner's). */
  const [limited, setLimited] = useState(false);

  const load = useCallback(() => {
    void commands.settingsGet().then((res) => {
      if (res.status !== "ok") {
        if (res.error.code === "PERMISSION_DENIED") {
          setLimited(true);
          setGroup(DEVICES);
          return;
        }
        return setError(res.error);
      }
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

  const groups = limited ? [DEVICES] : [...new Set((settings ?? []).map((s) => s.group)), BUSINESS, STAFF, TAXES, DEVICES, CLOUD, BACKUPS];
  return (
    <main className="sales">
      <header className="returns-header">
        <h1 className="page-title">
          <Settings />
          Settings
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
      <div className="settings-layout">
      <TabList
        label="Settings pages"
        idBase="settings"
        className="side-nav"
        vertical
        tabs={groups.map((g) => ({ value: g, label: g, icon: GROUP_ICONS[g] ?? Settings }))}
        selected={group}
        onSelect={setGroup}
      />
      {group && (
        <section className="card settings-list" {...tabPanelProps("settings", group)}>
          <PanelHeader icon={GROUP_ICONS[group] ?? Settings} title={group}>
            {GROUP_HELP[group]}
          </PanelHeader>
          {group === BUSINESS ? (
            <BusinessPanel onChanged={onChanged} />
          ) : group === STAFF ? (
            <StaffPanel />
          ) : group === TAXES ? (
            <TaxesPanel />
          ) : group === DEVICES ? (
            <DevicesPanel />
          ) : group === CLOUD ? (
            <CloudPanel onRelinked={onRelinked} />
          ) : group === BACKUPS ? (
            <BackupsPanel />
          ) : (
            (settings ?? [])
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
              ))
          )}
        </section>
      )}
      </div>
    </main>
  );
}

/** Plain words for choice values that are codes (units); others show as stored. */
const CHOICE_LABEL: Record<string, string> = {
  '"ea"': "Each (ea)",
  '"kg"': "Kilogram (kg)",
  '"g"': "Gram (g)",
  '"l"': "Litre (l)",
  '"ml"': "Millilitre (ml)",
  '"m"': "Metre (m)",
  '"cm"': "Centimetre (cm)",
  '"box"': "Box",
  '"pack"': "Pack",
  "58": "58 mm (narrow)",
  "80": "80 mm (standard)",
  '"Enter"': "Enter",
  '"Tab"': "Tab",
  '"none"': "Nothing (a short pause ends the code)",
};

/** One plain sentence under each page's name, saying what the page is for. */
const GROUP_HELP: Record<string, string> = {
  Inventory: "What the till does when stock runs low or out.",
  Products: "Starting values for the products you add.",
  "Cash drawer": "When the cash drawer must be open, and when it opens by itself.",
  Security: "What cashiers may do on their own, and when the till locks itself.",
  Receipts: "Paper size and the extra lines printed on every receipt.",
  Scanner: "How the till recognises your barcode scanner and scale labels.",
  Business: "Your shop's details, printed on receipts.",
  Staff: "Who can sign in to this till, and what each person may do.",
  Taxes: "Tax rates, and the tax categories products belong to.",
  Devices: "The receipt printer, cash drawer, serial scanner and card terminal.",
  Cloud: "Link this till to your online account so sales and products sync.",
  Backups: "Automatic copies of this till's data, and how to bring one back.",
};

/** An icon for each setting, so a long page is easy to scan; new ones use their page's icon. */
const SETTING_ICONS: Record<string, LucideIcon> = {
  "inventory.allow_negative": PackageMinus,
  "catalog.default_unit": Ruler,
  "catalog.default_track_stock": ClipboardList,
  "cash.require_session_for_sale": Wallet,
  "drawer.open_on_cash_sale": Banknote,
  "discount.cashier_max_ppm": BadgePercent,
  "returns.cashier_requires_approval": Undo2,
  "payments.cashier_external_tender": CreditCard,
  "security.lock_timeout_secs": Timer,
  "receipt.paper_width_mm": Ruler,
  "receipt.auto_print": Printer,
  "receipt.header_lines": PanelTop,
  "receipt.footer_lines": PanelBottom,
  "scanner.variable_measure_rules": Weight,
  "scanner.suffix": CornerDownLeft,
  "scanner.max_key_gap_ms": Gauge,
  "scanner.error_sound": Volume2,
};

/** The unit shown after a number field, so nobody has to guess what the number means. */
const UNITS: Record<string, string> = {
  "security.lock_timeout_secs": "seconds",
  "scanner.max_key_gap_ms": "ms",
};

/** An icon for each settings page; pages added later fall back to the gear. */
const GROUP_ICONS: Record<string, LucideIcon> = {
  Inventory: Boxes,
  Products: Package,
  "Cash drawer": Banknote,
  Security: ShieldCheck,
  Receipts: ReceiptText,
  Scanner: ScanBarcode,
  Business: Building,
  Staff: Users,
  Taxes: Percent,
  Cloud: Cloud,
  Backups: Database,
  Devices: Printer,
};

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
        <span className="switch-field">
          <span aria-hidden="true">{current === true ? "On" : "Off"}</span>
          <input
            id={id}
            type="checkbox"
            className="switch"
            checked={current === true}
            onChange={(e) => void save(e.target.checked ? "true" : "false")}
          />
        </span>
      );
    }
    if (k.type === "choice") {
      return (
        <select id={id} value={setting.valueJson} onChange={(e) => void save(e.target.value)}>
          {k.options.map((o) => (
            <option key={o} value={o}>
              {CHOICE_LABEL[o] ?? o.replace(/^"|"$/g, "")}
            </option>
          ))}
        </select>
      );
    }
    const input =
      k.type === "lines" ? (
        <textarea id={id} rows={Math.min(k.maxLines, 5)} value={draft} onChange={(e) => setDraft(e.target.value)} maxLength={k.maxLines * (k.maxChars + 1)} />
      ) : (
        <input id={id} inputMode="decimal" value={draft} onChange={(e) => setDraft(e.target.value)} />
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
        {k.type === "ppm" && <span className="unit">%</span>}
        {UNITS[setting.key] && <span className="unit">{UNITS[setting.key]}</span>}
        <button type="submit" className="secondary">
          Save
        </button>
      </form>
    );
  })();

  const Icon = SETTING_ICONS[setting.key] ?? GROUP_ICONS[setting.group] ?? Settings;
  return (
    <div className="setting-row has-icon" data-testid="setting-row">
      <span className="icon-tile sm" aria-hidden="true">
        <Icon />
      </span>
      <label htmlFor={id}>
        <strong>{setting.label}</strong>
        <span className="small">{setting.help}</span>
      </label>
      <div className="setting-control">
        {control}
        <span aria-live="polite" className="small setting-status">
          {state.kind === "saved" && <span className="saved-note">Saved.</span>}
          {state.kind === "error" && (
            <span role="alert" className="field-error">
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
