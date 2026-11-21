import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import { SectionTitle } from "../../ui/PanelHeader";
import { CreditCard, Monitor, Printer, ScanBarcode, Weight } from "../../ui/icons";
import { commands, type IpcError } from "../../ipc";
import type {
  DeviceDto,
  DiscoveryDto,
  DisplayStatusDto_Serialize as DisplayStatusDto,
  HidPosDevice_Serialize as HidPosDevice,
  PrinterProfileDto,
  PrinterStatusDto,
  ScaleStatusDto,
  ScannerStatusDto_Serialize as ScannerStatusDto,
} from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";

type Connection = "network" | "serial" | "deviceFile" | "spooler";

const CONNECTIONS: Connection[] = ["network", "spooler", "serial", "deviceFile"];
/** Each connection's name in the list, translated at render. */
const CONNECTION_LABEL_COPY: Record<Connection, string> = {
  network: "Network (Ethernet / Wi-Fi)",
  spooler: "Installed printer (USB, Bluetooth, any driver)",
  serial: "Serial / COM port (RS-232, USB-COM, Bluetooth)",
  deviceFile: "Device file (Linux USB /dev/usb/lp0, parallel LPT)",
};
/** The line under the list for the chosen connection, translated at render. */
const CONNECTION_HELP_COPY: Record<Connection, string> = {
  network: "Raw TCP, usually port 9100. Works with Epson, Star, Bixolon and most network printers.",
  spooler: "Sends receipts raw through the system printer queue. Use this for USB printers with a Windows driver, including “Generic / Text Only”.",
  serial: "For older serial printers and USB or Bluetooth printers that appear as a COM port.",
  deviceFile: "Writes straight to the device: /dev/usb/lp0 on Linux, LPT1 for parallel printers.",
};
const BAUDS = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200];

interface PrinterDraft {
  name: string;
  connection: Connection;
  host: string;
  port: string;
  serialPort: string;
  baud: string;
  flow: "none" | "rtsCts" | "xonXoff";
  path: string;
  queue: string;
  profileId: string;
  columns: string;
  cut: "" | "none" | "partial" | "full";
  drawerPin: "pin2" | "pin5";
  barcode: "" | "yes" | "no";
}

const EMPTY: PrinterDraft = {
  name: "Receipt printer",
  connection: "network",
  host: "",
  port: "9100",
  serialPort: "",
  baud: "9600",
  flow: "none",
  path: "/dev/usb/lp0",
  queue: "",
  profileId: "generic-escpos-80mm-48",
  columns: "",
  cut: "",
  drawerPin: "pin2",
  barcode: "",
};

function toConfig(d: PrinterDraft): unknown {
  const connection =
    d.connection === "network"
      ? { kind: "network", host: d.host.trim(), port: parseInt(d.port, 10) || 9100 }
      : d.connection === "serial"
        ? { kind: "serial", port: d.serialPort.trim(), baud: parseInt(d.baud, 10), flow: d.flow }
        : d.connection === "deviceFile"
          ? { kind: "deviceFile", path: d.path.trim() }
          : { kind: "spooler", queue: d.queue.trim() };
  return {
    connection,
    profileId: d.profileId,
    columnsOverride: d.columns.trim() ? parseInt(d.columns, 10) : null,
    cut: d.cut || null,
    drawerPin: d.drawerPin,
    barcode: d.barcode === "" ? null : d.barcode === "yes",
  };
}

function fromDevice(dev: DeviceDto): PrinterDraft {
  const c = JSON.parse(dev.configJson) as {
    connection: { kind: Connection; host?: string; port?: number | string; baud?: number; flow?: PrinterDraft["flow"]; path?: string; queue?: string };
    profileId: string;
    columnsOverride?: number | null;
    cut?: PrinterDraft["cut"] | null;
    drawerPin?: PrinterDraft["drawerPin"];
    barcode?: boolean | null;
  };
  const k = c.connection;
  return {
    ...EMPTY,
    name: dev.name,
    connection: k.kind,
    host: k.kind === "network" ? (k.host ?? "") : "",
    port: k.kind === "network" ? String(k.port ?? 9100) : "9100",
    serialPort: k.kind === "serial" ? String(k.port ?? "") : "",
    baud: String(k.baud ?? 9600),
    flow: k.flow ?? "none",
    path: k.path ?? EMPTY.path,
    queue: k.queue ?? "",
    profileId: c.profileId,
    columns: c.columnsOverride ? String(c.columnsOverride) : "",
    cut: c.cut ?? "",
    drawerPin: c.drawerPin ?? "pin2",
    barcode: c.barcode === null || c.barcode === undefined ? "" : c.barcode ? "yes" : "no",
  };
}

type DisplayProtocol = "escPos" | "cd5220";
/** Each display command set's name in the list, translated at render. */
const PROTOCOL_LABEL_COPY: Record<DisplayProtocol, string> = {
  escPos: "Epson ESC/POS (Epson DM-D and compatibles)",
  cd5220: "CD5220 (many unbranded pole displays)",
};

/**
 * This till's devices (E20-T11, DEV-001…009): the receipt printer (any connection, any
 * model), a serial barcode scanner, a USB HID-POS scanner, the customer display, the scale, and the card terminal. Keyboard-wedge scanners need no
 * set-up here: they work as soon as they are plugged in (Settings → Scanner for timing).
 */
export function DevicesPanel() {
  const [devices, setDevices] = useState<DeviceDto[] | null>(null);
  const [profiles, setProfiles] = useState<PrinterProfileDto[]>([]);
  const [status, setStatus] = useState<PrinterStatusDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  /** The printer section's last outcome; kept here because saving remounts the section. */
  const [printerMessage, setPrinterMessage] = useState<string | null>(null);
  /** The same for the customer display. */
  const [displayMessage, setDisplayMessage] = useState<string | null>(null);
  /** The same for the USB HID-POS scanner. */
  const [hidMessage, setHidMessage] = useState<string | null>(null);
  /** The same for the scale. */
  const [scaleMessage, setScaleMessage] = useState<string | null>(null);

  const load = useCallback(() => {
    void commands.devicesList().then((r) => (r.status === "ok" ? setDevices(r.data) : setError(r.error)));
    void commands.printerStatus().then((r) => r.status === "ok" && setStatus(r.data));
  }, []);
  useEffect(() => {
    load();
    void commands.printerProfiles().then((r) => r.status === "ok" && setProfiles(r.data));
  }, [load]);

  const byKind = (k: string, role = "main") => devices?.find((d) => d.kind === k && d.role === role) ?? null;
  return (
    <div className="devices">
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <PrinterSection
        key={`printer-${byKind("receipt_printer")?.deviceId ?? "new"}-${byKind("receipt_printer")?.version ?? 0}`}
        device={byKind("receipt_printer")}
        profiles={profiles}
        status={status}
        onChanged={load}
        result={printerMessage}
        setResult={setPrinterMessage}
      />
      <ScannerSection key={`scanner-${byKind("scanner")?.deviceId ?? "new"}-${byKind("scanner")?.version ?? 0}`} device={byKind("scanner")} onChanged={load} />
      <HidScannerSection
        key={`hidpos-${byKind("scanner", HID_ROLE)?.deviceId ?? "new"}-${byKind("scanner", HID_ROLE)?.version ?? 0}`}
        device={byKind("scanner", HID_ROLE)}
        onChanged={load}
        message={hidMessage}
        setMessage={setHidMessage}
      />
      <DisplaySection
        key={`display-${byKind("customer_display")?.deviceId ?? "new"}-${byKind("customer_display")?.version ?? 0}`}
        device={byKind("customer_display")}
        onChanged={load}
        message={displayMessage}
        setMessage={setDisplayMessage}
      />
      <ScaleSection
        key={`scale-${byKind("scale")?.deviceId ?? "new"}-${byKind("scale")?.version ?? 0}`}
        device={byKind("scale")}
        onChanged={load}
        message={scaleMessage}
        setMessage={setScaleMessage}
      />
      <TerminalSection device={byKind("payment_terminal")} onChanged={load} />
    </div>
  );
}

function PrinterSection({
  device,
  profiles,
  status,
  onChanged,
  result,
  setResult,
}: {
  device: DeviceDto | null;
  profiles: PrinterProfileDto[];
  status: PrinterStatusDto | null;
  onChanged: () => void;
  result: string | null;
  setResult: (m: string | null) => void;
}) {
  const t = useT();
  const [draft, setDraft] = useState<PrinterDraft>(() => (device ? fromDevice(device) : EMPTY));
  const [editing, setEditing] = useState(device === null);
  const [busy, setBusy] = useState(false);
  const [fieldError, setFieldError] = useState<{ field: string | null; text: string } | null>(null);
  const [found, setFound] = useState<DiscoveryDto | null>(null);
  const [subnet, setSubnet] = useState("");

  const profile = profiles.find((p) => p.id === draft.profileId) ?? null;
  const vendors = useMemo(() => [...new Set(profiles.map((p) => p.vendor))].sort(), [profiles]);
  const set = (patch: Partial<PrinterDraft>) => setDraft((d) => ({ ...d, ...patch }));

  async function save(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setFieldError(null);
    setResult(null);
    const res = await commands.deviceSave({
      deviceId: device?.deviceId ?? null,
      kind: "receipt_printer",
      role: null,
      name: draft.name,
      configJson: JSON.stringify(toConfig(draft)),
      enabled: true,
      baseVersion: device?.version ?? null,
    });
    setBusy(false);
    if (res.status === "ok") {
      setEditing(false);
      setResult(t("Saved. The next receipt prints here."));
      onChanged();
    } else setFieldError({ field: res.error.details?.field ?? null, text: describeError(res.error) });
  }

  async function test() {
    setBusy(true);
    setResult(null);
    const res = await commands.printerTest();
    setBusy(false);
    setResult(res.status === "ok" ? res.data.message : describeError(res.error));
    onChanged();
  }

  async function remove() {
    if (!device) return;
    setBusy(true);
    const res = await commands.deviceRemove(device.deviceId);
    setBusy(false);
    if (res.status === "ok") onChanged();
    else setResult(describeError(res.error));
  }

  async function discover() {
    setBusy(true);
    const res = await commands.devicesDiscover(subnet.trim() || null, null);
    setBusy(false);
    if (res.status === "ok") setFound(res.data);
    else setResult(describeError(res.error));
  }

  const errorFor = (field: string) =>
    fieldError?.field?.endsWith(field) ? (
      <span role="alert" className="field-error inline">
        {fieldError.text}
      </span>
    ) : null;

  return (
    <section aria-label={t("Receipt printer")} className="device-section">
      <SectionTitle icon={Printer}>{t("Receipt printer")}</SectionTitle>
      {status && (
        <p data-testid="device-printer-status" className="small">
          {status.configured ? (
            <>
              {status.adapter} · {status.target} ·{" "}
              <strong>
                {status.state === "ready" ? t("Ready") : status.state === "unknown" ? t("Connected one way (no status)") : (status.message ?? status.state)}
              </strong>
              {status.drawer && ` · ${t("cash drawer on the kick port")}`}
            </>
          ) : (
            t("No printer: receipts can be previewed and saved as PDF.")
          )}
        </p>
      )}
      {device && !editing && (
        <div className="actions">
          <button type="button" onClick={() => void test()} disabled={busy}>
            {t("Test print")}
          </button>
          <button type="button" className="secondary" onClick={() => setEditing(true)}>
            {t("verb::Change")}
          </button>
          <button type="button" className="secondary" onClick={() => void remove()} disabled={busy}>
            {t("Remove")}
          </button>
        </div>
      )}
      {editing && (
        <form onSubmit={(e) => void save(e)} className="device-form" aria-label={t("Printer settings")}>
          <label>
            {t("Name")}
            <input value={draft.name} maxLength={80} onChange={(e) => set({ name: e.target.value })} />
          </label>
          <label>
            {t("Connection")}
            <select value={draft.connection} onChange={(e) => set({ connection: e.target.value as Connection })}>
              {CONNECTIONS.map((c) => (
                <option key={c} value={c}>
                  {t(CONNECTION_LABEL_COPY[c])}
                </option>
              ))}
            </select>
          </label>
          <p className="small">{t(CONNECTION_HELP_COPY[draft.connection])}</p>
          {draft.connection === "network" && (
            <div className="row">
              <label>
                {t("IP address or name")} {errorFor("host")}
                <input value={draft.host} placeholder="192.168.1.50" onChange={(e) => set({ host: e.target.value })} />
              </label>
              <label>
                {t("Port")} {errorFor("port")}
                <input inputMode="numeric" value={draft.port} onChange={(e) => set({ port: e.target.value })} />
              </label>
            </div>
          )}
          {draft.connection === "serial" && (
            <div className="row">
              <label>
                {t("Port")} {errorFor("port")}
                <input list="serial-ports" value={draft.serialPort} placeholder={t("COM3 or /dev/ttyUSB0")} onChange={(e) => set({ serialPort: e.target.value })} />
              </label>
              <label>
                {t("Speed (baud)")} {errorFor("baud")}
                <select value={draft.baud} onChange={(e) => set({ baud: e.target.value })}>
                  {BAUDS.map((b) => (
                    <option key={b}>{b}</option>
                  ))}
                </select>
              </label>
              <label>
                {t("Flow control")}
                <select value={draft.flow} onChange={(e) => set({ flow: e.target.value as PrinterDraft["flow"] })}>
                  <option value="none">{t("None")}</option>
                  <option value="rtsCts">{t("Hardware (RTS/CTS)")}</option>
                  <option value="xonXoff">{t("Software (XON/XOFF)")}</option>
                </select>
              </label>
            </div>
          )}
          {draft.connection === "deviceFile" && (
            <label>
              {t("Device")} {errorFor("path")}
              <input list="device-files" value={draft.path} onChange={(e) => set({ path: e.target.value })} />
            </label>
          )}
          {draft.connection === "spooler" && (
            <label>
              {t("Printer")} {errorFor("queue")}
              <input list="queues" value={draft.queue} placeholder="EPSON TM-T20III Receipt" onChange={(e) => set({ queue: e.target.value })} />
            </label>
          )}
          <label>
            {t("Model")} {errorFor("profileId")}
            <select value={draft.profileId} onChange={(e) => set({ profileId: e.target.value })}>
              {vendors.map((v) => (
                <optgroup key={v} label={v}>
                  {profiles
                    .filter((p) => p.vendor === v)
                    .map((p) => (
                      <option key={p.id} value={p.id} disabled={!p.textSupported}>
                        {p.name} · {t("{n} columns", { n: p.columns })}
                        {p.verified ? "" : ` · ${t("not yet verified")}`}
                        {p.textSupported ? "" : ` · ${t("image-only, not supported")}`}
                      </option>
                    ))}
                </optgroup>
              ))}
            </select>
          </label>
          {profile && (
            <p className="small">
              {profile.dialect === "StarLine" ? "Star Line Mode. " : profile.dialect === "PlainText" ? `${t("Plain text only.")} ` : "ESC/POS. "}
              {t("{n} characters per line", { n: profile.columns })}
              {profile.columnsFontB ? ` (${t("{n} in the small font", { n: profile.columnsFontB })})` : ""}
              {profile.cutter ? `, ${t("cutter")}` : `, ${t("no cutter")}`}
              {profile.drawer ? `, ${t("cash drawer port")}` : ""}
              {profile.barcode ? `, ${t("barcodes")}` : ""}. {profile.notes}
            </p>
          )}
          <div className="row">
            <label>
              {t("Characters per line (optional)")} {errorFor("columnsOverride")}
              <input inputMode="numeric" value={draft.columns} placeholder={profile ? String(profile.columns) : ""} onChange={(e) => set({ columns: e.target.value })} />
            </label>
            <label>
              {t("Cut")}
              <select value={draft.cut} onChange={(e) => set({ cut: e.target.value as PrinterDraft["cut"] })}>
                <option value="">{t("As the model")}</option>
                <option value="partial">{t("Partial")}</option>
                <option value="full">{t("Full")}</option>
                <option value="none">{t("No cutter (tear off)")}</option>
              </select>
            </label>
            <label>
              {t("Cash drawer")}
              <select value={draft.drawerPin} onChange={(e) => set({ drawerPin: e.target.value as PrinterDraft["drawerPin"] })}>
                <option value="pin2">{t("Pin 2 (most drawers)")}</option>
                <option value="pin5">{t("Pin 5")}</option>
              </select>
            </label>
            <label>
              {t("Receipt number barcode")}
              <select value={draft.barcode} onChange={(e) => set({ barcode: e.target.value as PrinterDraft["barcode"] })}>
                <option value="">{t("As the model")}</option>
                <option value="yes">{t("Print it")}</option>
                <option value="no">{t("Text only")}</option>
              </select>
            </label>
          </div>
          {fieldError && !fieldError.field?.match(/host|port|baud|path|queue|profileId|columnsOverride/) && (
            <p role="alert" className="field-error">
              {fieldError.text}
            </p>
          )}
          <div className="actions">
            {device && (
              <button type="button" className="secondary" onClick={() => setEditing(false)}>
                {t("Cancel")}
              </button>
            )}
            <button type="submit" disabled={busy}>
              {busy ? t("Checking…") : t("Save printer")}
            </button>
          </div>
        </form>
      )}
      {result && (
        <p role="status" className="small">
          {result}
        </p>
      )}
      {editing && (
        <details className="device-discovery">
          <summary>{t("Find printers and ports")}</summary>
          <div className="row">
            <label>
              {t("Also search the network (e.g. 192.168.1.0/24, optional)")}
              <input value={subnet} onChange={(e) => setSubnet(e.target.value)} placeholder="192.168.1.0/24" />
            </label>
            <button type="button" className="secondary" disabled={busy} onClick={() => void discover()}>
              {busy ? t("Searching…") : t("Search")}
            </button>
          </div>
          {found && (
            <ul aria-label={t("Found devices")} className="small">
              {found.network.map((n) => (
                <li key={`${n.host}:${n.port}`}>
                  {n.host}:{n.port} {n.identified ? `· ${n.identified.maker ?? ""} ${n.identified.model ?? ""}` : ""}{" "}
                  <button
                    type="button"
                    className="link"
                    onClick={() => set({ connection: "network", host: n.host, port: String(n.port), ...(n.profileId ? { profileId: n.profileId } : {}) })}
                  >
                    {t("Use")}
                  </button>
                </li>
              ))}
              {found.serialPorts.map((p) => (
                <li key={p.port}>
                  {p.label}{" "}
                  <button type="button" className="link" onClick={() => set({ connection: "serial", serialPort: p.port })}>
                    {t("Use")}
                  </button>
                </li>
              ))}
              {found.queues.map((q) => (
                <li key={q}>
                  {t("Printer queue: {queue}", { queue: q })}{" "}
                  <button type="button" className="link" onClick={() => set({ connection: "spooler", queue: q })}>
                    {t("Use")}
                  </button>
                </li>
              ))}
              {found.deviceFiles.map((f) => (
                <li key={f}>
                  {t("Device: {path}", { path: f })}{" "}
                  <button type="button" className="link" onClick={() => set({ connection: "deviceFile", path: f })}>
                    {t("Use")}
                  </button>
                </li>
              ))}
              {found.networkProblem && <li role="alert">{found.networkProblem}</li>}
              {found.network.length + found.serialPorts.length + found.queues.length + found.deviceFiles.length === 0 && <li>{t("Nothing found.")}</li>}
            </ul>
          )}
          <datalist id="serial-ports">
            {found?.serialPorts.map((p) => (
              <option key={p.port} value={p.port}>
                {p.label}
              </option>
            ))}
          </datalist>
          <datalist id="queues">{found?.queues.map((q) => <option key={q} value={q} />)}</datalist>
          <datalist id="device-files">{found?.deviceFiles.map((f) => <option key={f} value={f} />)}</datalist>
        </details>
      )}
    </section>
  );
}

function ScannerSection({ device, onChanged }: { device: DeviceDto | null; onChanged: () => void }) {
  const t = useT();
  const stored = device ? (JSON.parse(device.configJson) as { serial?: { port?: string; baud?: number } }) : null;
  const [port, setPort] = useState(stored?.serial?.port ?? "");
  const [baud, setBaud] = useState(String(stored?.serial?.baud ?? 9600));
  const [message, setMessage] = useState<string | null>(null);

  async function save(e: FormEvent) {
    e.preventDefault();
    const res = await commands.deviceSave({
      deviceId: device?.deviceId ?? null,
      kind: "scanner",
      role: null,
      name: `Scanner on ${port.trim()}`,
      configJson: JSON.stringify({ serial: { port: port.trim(), baud: parseInt(baud, 10) } }),
      enabled: true,
      baseVersion: device?.version ?? null,
    });
    setMessage(res.status === "ok" ? t("Saved. Scans from this port go to the till.") : describeError(res.error));
    if (res.status === "ok") onChanged();
  }

  return (
    <section aria-label={t("Serial scanner")} className="device-section">
      <SectionTitle icon={ScanBarcode}>{t("Serial barcode scanner (optional)")}</SectionTitle>
      <p className="small">
        {t(
          "Most USB scanners need nothing here: they type the code like a keyboard. Use this only for a scanner on a COM port (RS-232, USB-COM or Bluetooth SPP). The till reconnects by itself after the scanner is unplugged.",
        )}
      </p>
      <form onSubmit={(e) => void save(e)} className="row" aria-label={t("Scanner settings")}>
        <label>
          {t("Port")}
          <input value={port} placeholder={t("COM4 or /dev/ttyACM0")} onChange={(e) => setPort(e.target.value)} />
        </label>
        <label>
          {t("Speed (baud)")}
          <select value={baud} onChange={(e) => setBaud(e.target.value)}>
            {BAUDS.map((b) => (
              <option key={b}>{b}</option>
            ))}
          </select>
        </label>
        <button type="submit" disabled={!port.trim()}>
          {t("Save scanner")}
        </button>
        {device && (
          <button
            type="button"
            className="secondary"
            onClick={() => void commands.deviceRemove(device.deviceId).then(() => onChanged())}
          >
            {t("Remove")}
          </button>
        )}
      </form>
      {message && (
        <p role="status" className="small">
          {message}
        </p>
      )}
    </section>
  );
}

/** The role a USB HID-POS scanner is saved under, beside the serial scanner (`main`). */
const HID_ROLE = "hidpos";

/** "Honeywell Xenon 1900 (0c2e:0b61)": the same name Rust gives it. */
function hidLabel(d: HidPosDevice): string {
  const hex = (n: number) => n.toString(16).padStart(4, "0");
  const name = [d.manufacturer, d.product]
    .map((x) => (x ?? "").trim())
    .filter(Boolean)
    .join(" ");
  return `${name || "HID-POS scanner"} (${hex(d.vendorId)}:${hex(d.productId)})`;
}

/** A USB scanner in HID-POS mode (usage page 0x8C): it sends codes to the till instead of typing them (E21-T04). */
function HidScannerSection({
  device,
  onChanged,
  message,
  setMessage,
}: {
  device: DeviceDto | null;
  onChanged: () => void;
  message: string | null;
  setMessage: (m: string | null) => void;
}) {
  const t = useT();
  const stored = device ? (JSON.parse(device.configJson) as { hidPos?: { label?: string | null } }) : null;
  const [found, setFound] = useState<HidPosDevice[] | null>(null);
  const [chosen, setChosen] = useState("");
  const [status, setStatus] = useState<ScannerStatusDto | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    if (!device) return;
    void commands.scannerStatus().then((r) => r.status === "ok" && setStatus(r.data.find((x) => x.deviceId === device.deviceId) ?? null));
  }, [device]);
  useEffect(refresh, [refresh]);

  async function find() {
    setBusy(true);
    setMessage(null);
    const res = await commands.hidScannersList();
    setBusy(false);
    if (res.status === "error") return setMessage(describeError(res.error));
    setFound(res.data);
    setChosen(res.data[0]?.path ?? "");
    refresh();
  }

  async function save(e: FormEvent) {
    e.preventDefault();
    const d = found?.find((x) => x.path === chosen);
    if (!d) return;
    setBusy(true);
    const label = hidLabel(d);
    const res = await commands.deviceSave({
      deviceId: device?.deviceId ?? null,
      kind: "scanner",
      role: HID_ROLE,
      name: label,
      configJson: JSON.stringify({
        hidPos: { vendorId: d.vendorId, productId: d.productId, serialNumber: d.serialNumber, path: d.path, label },
      }),
      enabled: true,
      baseVersion: device?.version ?? null,
    });
    setBusy(false);
    setMessage(res.status === "ok" ? t("Saved. Scans from {name} go to the till.", { name: label }) : describeError(res.error));
    if (res.status === "ok") onChanged();
  }

  return (
    <section aria-label={t("USB HID-POS scanner")} className="device-section">
      <SectionTitle icon={ScanBarcode}>{t("USB HID-POS scanner (optional)")}</SectionTitle>
      <p className="small">
        {t(
          "For a USB scanner set to HID-POS mode (USB HID Point of Sale), which sends codes to the till instead of typing them. Plug it in, press “Find USB scanners” and choose it. The till finds it again after it is unplugged.",
        )}
      </p>
      {device && (
        <p data-testid="device-hidpos-status" className="small">
          {stored?.hidPos?.label ?? device.name} ·{" "}
          <strong>
            {status?.state === "ready"
              ? t("Ready")
              : status?.state === "offline"
                ? t("Not working: {reason}", { reason: status.message ?? "" })
                : t("Waiting for the scanner")}
          </strong>
        </p>
      )}
      <form onSubmit={(e) => void save(e)} className="row" aria-label={t("USB scanner settings")}>
        <button type="button" className="secondary" disabled={busy} onClick={() => void find()}>
          {t("Find USB scanners")}
        </button>
        {found && found.length > 0 && (
          <label>
            {t("Scanner")}
            <select value={chosen} onChange={(e) => setChosen(e.target.value)}>
              {found.map((d) => (
                <option key={d.path} value={d.path}>
                  {hidLabel(d)}
                </option>
              ))}
            </select>
          </label>
        )}
        <button type="submit" disabled={!chosen || busy}>
          {t("Save USB scanner")}
        </button>
        {device && (
          <button type="button" className="secondary" onClick={() => void commands.deviceRemove(device.deviceId).then(() => onChanged())}>
            {t("Remove")}
          </button>
        )}
      </form>
      {found && found.length === 0 && (
        <p role="status" className="small">
          {t("No HID-POS scanner found. Check it is plugged in and set to HID-POS mode with the set-up barcode in its manual.")}
        </p>
      )}
      {message && (
        <p role="status" className="small">
          {message}
        </p>
      )}
    </section>
  );
}

function DisplaySection({
  device,
  onChanged,
  message,
  setMessage,
}: {
  device: DeviceDto | null;
  onChanged: () => void;
  message: string | null;
  setMessage: (m: string | null) => void;
}) {
  const t = useT();
  const stored = device ? (JSON.parse(device.configJson) as { serial?: { port?: string; baud?: number }; protocol?: DisplayProtocol }) : null;
  const [port, setPort] = useState(stored?.serial?.port ?? "");
  const [baud, setBaud] = useState(String(stored?.serial?.baud ?? 9600));
  const [protocol, setProtocol] = useState<DisplayProtocol>(stored?.protocol ?? "escPos");
  const [status, setStatus] = useState<DisplayStatusDto | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    void commands.displayStatus().then((r) => r.status === "ok" && setStatus(r.data));
  }, []);
  useEffect(refresh, [refresh]);

  async function save(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    const res = await commands.deviceSave({
      deviceId: device?.deviceId ?? null,
      kind: "customer_display",
      role: null,
      name: `Customer display on ${port.trim()}`,
      configJson: JSON.stringify({ serial: { port: port.trim(), baud: parseInt(baud, 10) }, protocol }),
      enabled: true,
      baseVersion: device?.version ?? null,
    });
    setBusy(false);
    setMessage(res.status === "ok" ? t("Saved. The display greets customers and shows each item and the total.") : describeError(res.error));
    if (res.status === "ok") onChanged();
  }

  async function test() {
    setBusy(true);
    setMessage(null);
    const res = await commands.displayTest();
    setBusy(false);
    setMessage(res.status === "ok" ? (res.data.outcome === "shown" ? t("Sent. The display should show SALE OS DISPLAY TEST and a ruler that fills the second line.") : t("The display did not take it: {reason}", { reason: res.data.message })) : describeError(res.error));
    refresh();
  }

  return (
    <section aria-label={t("Customer display")} className="device-section">
      <SectionTitle icon={Monitor}>{t("Customer display (optional)")}</SectionTitle>
      <p className="small">
        {t(
          "A pole display facing the customer, with two lines of 20 characters, on a COM port (RS-232 or USB-COM). It shows each item and the running total, the total due at payment, the change, and your shop's name when the till is idle. Letters it cannot show are written in plain Latin letters.",
        )}
      </p>
      {status?.configured && (
        <p data-testid="device-display-status" className="small">
          {status.target} ·{" "}
          <strong>{status.state === "ready" ? t("Ready") : status.state === "offline" ? t("Not working: {reason}", { reason: status.message ?? "" }) : t("Nothing shown yet")}</strong>
        </p>
      )}
      <form onSubmit={(e) => void save(e)} className="row" aria-label={t("Display settings")}>
        <label>
          {t("Port")}
          <input value={port} placeholder={t("COM6 or /dev/ttyUSB1")} onChange={(e) => setPort(e.target.value)} />
        </label>
        <label>
          {t("Speed (baud)")}
          <select value={baud} onChange={(e) => setBaud(e.target.value)}>
            {BAUDS.map((b) => (
              <option key={b}>{b}</option>
            ))}
          </select>
        </label>
        <label>
          {t("Command set")}
          <select value={protocol} onChange={(e) => setProtocol(e.target.value as DisplayProtocol)}>
            {(Object.keys(PROTOCOL_LABEL_COPY) as DisplayProtocol[]).map((p) => (
              <option key={p} value={p}>
                {t(PROTOCOL_LABEL_COPY[p])}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" disabled={!port.trim() || busy}>
          {t("Save display")}
        </button>
        {device && (
          <>
            <button type="button" className="secondary" disabled={busy} onClick={() => void test()}>
              {t("Test display")}
            </button>
            <button
              type="button"
              className="secondary"
              onClick={() => void commands.deviceRemove(device.deviceId).then(() => onChanged())}
            >
              {t("Remove")}
            </button>
          </>
        )}
      </form>
      {message && (
        <p role="status" className="small">
          {message}
        </p>
      )}
    </section>
  );
}

type ScaleProtocol = "toledo8217" | "continuousAscii";
/** Each scale protocol's name in the list, translated at render. */
const SCALE_PROTOCOL_COPY: Record<ScaleProtocol, string> = {
  toledo8217: "Toledo 8217 (the till asks with W)",
  continuousAscii: "Continuous output (ST,GS,+ 1.234kg)",
};
type DataFormat = "8N1" | "7E1" | "7O1";
const DATA_FORMATS: Record<DataFormat, { dataBits: string; parity: string }> = {
  "8N1": { dataBits: "eight", parity: "none" },
  "7E1": { dataBits: "seven", parity: "even" },
  "7O1": { dataBits: "seven", parity: "odd" },
};

/** A weight in grams ("1234.5") as the cashier reads it: "1.2345 kg" from a kilogram, else "250 g". */
function gramsText(grams: string): string {
  const negative = grams.startsWith("-");
  const g = negative ? grams.slice(1) : grams;
  const [whole = "0", frac = ""] = g.split(".");
  if (whole.length <= 3) return `${grams} g`;
  const kgWhole = whole.slice(0, -3);
  const kgFrac = (whole.slice(-3) + frac).replace(/0+$/, "").padEnd(3, "0");
  return `${negative ? "-" : ""}${kgWhole}.${kgFrac} kg`;
}

/** A serial scale (E20-T06): items sold by weight take their quantity from it. */
function ScaleSection({
  device,
  onChanged,
  message,
  setMessage,
}: {
  device: DeviceDto | null;
  onChanged: () => void;
  message: string | null;
  setMessage: (m: string | null) => void;
}) {
  const t = useT();
  const stored = device
    ? (JSON.parse(device.configJson) as { serial?: { port?: string; baud?: number; dataBits?: string; parity?: string }; protocol?: ScaleProtocol; unit?: "kg" | "g" })
    : null;
  const storedFormat = (Object.keys(DATA_FORMATS) as DataFormat[]).find(
    (f) => DATA_FORMATS[f].dataBits === (stored?.serial?.dataBits ?? "eight") && DATA_FORMATS[f].parity === (stored?.serial?.parity ?? "none"),
  );
  const [port, setPort] = useState(stored?.serial?.port ?? "");
  const [baud, setBaud] = useState(String(stored?.serial?.baud ?? 9600));
  const [format, setFormat] = useState<DataFormat>(storedFormat ?? "8N1");
  const [protocol, setProtocol] = useState<ScaleProtocol>(stored?.protocol ?? "toledo8217");
  const [unit, setUnit] = useState<"kg" | "g">(stored?.unit ?? "kg");
  const [status, setStatus] = useState<ScaleStatusDto | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    void commands.scaleStatus().then((r) => r.status === "ok" && setStatus(r.data));
  }, []);
  useEffect(refresh, [refresh]);

  async function save(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    const res = await commands.deviceSave({
      deviceId: device?.deviceId ?? null,
      kind: "scale",
      role: null,
      name: `Scale on ${port.trim()}`,
      configJson: JSON.stringify({ serial: { port: port.trim(), baud: parseInt(baud, 10), ...DATA_FORMATS[format] }, protocol, unit }),
      enabled: true,
      baseVersion: device?.version ?? null,
    });
    setBusy(false);
    setMessage(res.status === "ok" ? t("Saved. Items sold by weight are weighed on this scale.") : describeError(res.error));
    if (res.status === "ok") onChanged();
  }

  async function test() {
    setBusy(true);
    setMessage(null);
    const res = await commands.scaleTest();
    setBusy(false);
    if (res.status === "error") setMessage(describeError(res.error));
    else {
      const d = res.data;
      const weight = d.weightGrams === null ? "" : gramsText(d.weightGrams);
      setMessage(
        d.outcome === "stable"
          ? t("The scale reads {weight} (stable).", { weight })
          : d.outcome === "moving"
            ? weight
              ? t("The scale reads {weight} and is still moving.", { weight })
              : t("The scale is still moving.")
            : d.outcome === "overload"
              ? t("The scale says it is overloaded.")
              : d.outcome === "underload"
                ? t("The scale reads below its range. Zero it.")
                : t("The scale did not answer: {reason}", { reason: d.message ?? "" }),
      );
    }
    refresh();
  }

  return (
    <section aria-label={t("Scale")} className="device-section">
      <SectionTitle icon={Weight}>{t("Scale (optional)")}</SectionTitle>
      <p className="small">
        {t(
          "A counter scale on a COM port (RS-232 or USB-COM). Items sold by kg or g are then weighed when they are added: only a steady weight above zero is used, and the weight can still be typed. Set the speed, data format and protocol to match the scale's settings.",
        )}
      </p>
      {status?.configured && (
        <p data-testid="device-scale-status" className="small">
          {status.target} ·{" "}
          <strong>{status.state === "ready" ? t("Ready") : status.state === "offline" ? t("Not working: {reason}", { reason: status.message ?? "" }) : t("Not read yet")}</strong>
        </p>
      )}
      <form onSubmit={(e) => void save(e)} className="row" aria-label={t("Scale settings")}>
        <label>
          {t("Port")}
          <input value={port} placeholder={t("COM3 or /dev/ttyUSB2")} onChange={(e) => setPort(e.target.value)} />
        </label>
        <label>
          {t("Speed (baud)")}
          <select value={baud} onChange={(e) => setBaud(e.target.value)}>
            {BAUDS.map((b) => (
              <option key={b}>{b}</option>
            ))}
          </select>
        </label>
        <label>
          {t("Data format")}
          <select value={format} onChange={(e) => setFormat(e.target.value as DataFormat)}>
            {(Object.keys(DATA_FORMATS) as DataFormat[]).map((f) => (
              <option key={f} value={f}>
                {f}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t("Protocol")}
          <select value={protocol} onChange={(e) => setProtocol(e.target.value as ScaleProtocol)}>
            {(Object.keys(SCALE_PROTOCOL_COPY) as ScaleProtocol[]).map((p) => (
              <option key={p} value={p}>
                {t(SCALE_PROTOCOL_COPY[p])}
              </option>
            ))}
          </select>
        </label>
        {protocol === "toledo8217" && (
          <label>
            {t("The scale weighs in")}
            <select value={unit} onChange={(e) => setUnit(e.target.value as "kg" | "g")}>
              <option value="kg">kg</option>
              <option value="g">g</option>
            </select>
          </label>
        )}
        <button type="submit" disabled={!port.trim() || busy}>
          {t("Save scale")}
        </button>
        {device && (
          <>
            <button type="button" className="secondary" disabled={busy} onClick={() => void test()}>
              {t("Test scale")}
            </button>
            <button type="button" className="secondary" onClick={() => void commands.deviceRemove(device.deviceId).then(() => onChanged())}>
              {t("Remove")}
            </button>
          </>
        )}
      </form>
      {message && (
        <p role="status" className="small" data-testid="device-scale-message">
          {message}
        </p>
      )}
    </section>
  );
}

function TerminalSection({ device, onChanged }: { device: DeviceDto | null; onChanged: () => void }) {
  const t = useT();
  const [message, setMessage] = useState<string | null>(null);
  async function save() {
    const res = await commands.deviceSave({
      deviceId: device?.deviceId ?? null,
      kind: "payment_terminal",
      role: null,
      name: "Sandbox card terminal",
      configJson: JSON.stringify({ provider: "sandbox" }),
      enabled: true,
      baseVersion: device?.version ?? null,
    });
    setMessage(res.status === "ok" ? t("Saved. “Card (terminal)” appears at payment.") : describeError(res.error));
    if (res.status === "ok") onChanged();
  }
  return (
    <section aria-label={t("Card terminal")} className="device-section">
      <SectionTitle icon={CreditCard}>{t("Card terminal")}</SectionTitle>
      <p className="small">
        {t(
          "A card machine that is not connected to the till needs nothing here: take the payment on it, then choose “Card (separate terminal)” at the till. A connected terminal is set up through its provider. To practise, use the sandbox terminal: amounts ending in .01 are declined, .02 time out, and .03 take a few seconds.",
        )}
      </p>
      {device ? (
        <div className="button-row">
          <span>{device.name}</span>
          <button type="button" className="secondary" onClick={() => void commands.deviceRemove(device.deviceId).then(() => onChanged())}>
            {t("Remove")}
          </button>
        </div>
      ) : (
        <button type="button" className="secondary" onClick={() => void save()}>
          {t("Use the sandbox terminal (practice mode only)")}
        </button>
      )}
      {message && (
        <p role="status" className="small">
          {message}
        </p>
      )}
    </section>
  );
}
