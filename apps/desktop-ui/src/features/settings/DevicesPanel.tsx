import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { DeviceDto, DiscoveryDto, PrinterProfileDto, PrinterStatusDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";

type Connection = "network" | "serial" | "deviceFile" | "spooler";

const CONNECTIONS: { value: Connection; label: string; help: string }[] = [
  { value: "network", label: "Network (Ethernet / Wi-Fi)", help: "Raw TCP, usually port 9100. Works with Epson, Star, Bixolon and most network printers." },
  { value: "spooler", label: "Installed printer (USB, Bluetooth, any driver)", help: "Sends receipts raw through the system printer queue. Use this for USB printers with a Windows driver, including “Generic / Text Only”." },
  { value: "serial", label: "Serial / COM port (RS-232, USB-COM, Bluetooth)", help: "For older serial printers and USB or Bluetooth printers that appear as a COM port." },
  { value: "deviceFile", label: "Device file (Linux USB /dev/usb/lp0, parallel LPT)", help: "Writes straight to the device: /dev/usb/lp0 on Linux, LPT1 for parallel printers." },
];
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

/**
 * This till's devices (E20-T11, DEV-001…009): the receipt printer (any connection, any
 * model), a serial barcode scanner, and the card terminal. Keyboard-wedge scanners need no
 * set-up here: they work as soon as they are plugged in (Settings → Scanner for timing).
 */
export function DevicesPanel() {
  const [devices, setDevices] = useState<DeviceDto[] | null>(null);
  const [profiles, setProfiles] = useState<PrinterProfileDto[]>([]);
  const [status, setStatus] = useState<PrinterStatusDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  /** The printer section's last outcome; kept here because saving remounts the section. */
  const [printerMessage, setPrinterMessage] = useState<string | null>(null);

  const load = useCallback(() => {
    void commands.devicesList().then((r) => (r.status === "ok" ? setDevices(r.data) : setError(r.error)));
    void commands.printerStatus().then((r) => r.status === "ok" && setStatus(r.data));
  }, []);
  useEffect(() => {
    load();
    void commands.printerProfiles().then((r) => r.status === "ok" && setProfiles(r.data));
  }, [load]);

  const byKind = (k: string) => devices?.find((d) => d.kind === k && d.role === "main") ?? null;
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
      setResult("Saved. The next receipt prints here.");
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
    <section aria-label="Receipt printer" className="device-section">
      <h2>Receipt printer</h2>
      {status && (
        <p data-testid="device-printer-status" className="small">
          {status.configured ? (
            <>
              {status.adapter} · {status.target} ·{" "}
              <strong>
                {status.state === "ready" ? "Ready" : status.state === "unknown" ? "Connected one way (no status)" : (status.message ?? status.state)}
              </strong>
              {status.drawer && " · cash drawer on the kick port"}
            </>
          ) : (
            "No printer: receipts can be previewed and saved as PDF."
          )}
        </p>
      )}
      {device && !editing && (
        <div className="actions">
          <button type="button" onClick={() => void test()} disabled={busy}>
            Test print
          </button>
          <button type="button" className="secondary" onClick={() => setEditing(true)}>
            Change
          </button>
          <button type="button" className="secondary" onClick={() => void remove()} disabled={busy}>
            Remove
          </button>
        </div>
      )}
      {editing && (
        <form onSubmit={(e) => void save(e)} className="device-form" aria-label="Printer settings">
          <label>
            Name
            <input value={draft.name} maxLength={80} onChange={(e) => set({ name: e.target.value })} />
          </label>
          <label>
            Connection
            <select value={draft.connection} onChange={(e) => set({ connection: e.target.value as Connection })}>
              {CONNECTIONS.map((c) => (
                <option key={c.value} value={c.value}>
                  {c.label}
                </option>
              ))}
            </select>
          </label>
          <p className="small">{CONNECTIONS.find((c) => c.value === draft.connection)?.help}</p>
          {draft.connection === "network" && (
            <div className="row">
              <label>
                IP address or name {errorFor("host")}
                <input value={draft.host} placeholder="192.168.1.50" onChange={(e) => set({ host: e.target.value })} />
              </label>
              <label>
                Port {errorFor("port")}
                <input inputMode="numeric" value={draft.port} onChange={(e) => set({ port: e.target.value })} />
              </label>
            </div>
          )}
          {draft.connection === "serial" && (
            <div className="row">
              <label>
                Port {errorFor("port")}
                <input list="serial-ports" value={draft.serialPort} placeholder="COM3 or /dev/ttyUSB0" onChange={(e) => set({ serialPort: e.target.value })} />
              </label>
              <label>
                Speed (baud) {errorFor("baud")}
                <select value={draft.baud} onChange={(e) => set({ baud: e.target.value })}>
                  {BAUDS.map((b) => (
                    <option key={b}>{b}</option>
                  ))}
                </select>
              </label>
              <label>
                Flow control
                <select value={draft.flow} onChange={(e) => set({ flow: e.target.value as PrinterDraft["flow"] })}>
                  <option value="none">None</option>
                  <option value="rtsCts">Hardware (RTS/CTS)</option>
                  <option value="xonXoff">Software (XON/XOFF)</option>
                </select>
              </label>
            </div>
          )}
          {draft.connection === "deviceFile" && (
            <label>
              Device {errorFor("path")}
              <input list="device-files" value={draft.path} onChange={(e) => set({ path: e.target.value })} />
            </label>
          )}
          {draft.connection === "spooler" && (
            <label>
              Printer {errorFor("queue")}
              <input list="queues" value={draft.queue} placeholder="EPSON TM-T20III Receipt" onChange={(e) => set({ queue: e.target.value })} />
            </label>
          )}
          <label>
            Model {errorFor("profileId")}
            <select value={draft.profileId} onChange={(e) => set({ profileId: e.target.value })}>
              {vendors.map((v) => (
                <optgroup key={v} label={v}>
                  {profiles
                    .filter((p) => p.vendor === v)
                    .map((p) => (
                      <option key={p.id} value={p.id} disabled={!p.textSupported}>
                        {p.name} · {p.columns} columns{p.verified ? "" : " · not yet verified"}
                        {p.textSupported ? "" : " · image-only, not supported"}
                      </option>
                    ))}
                </optgroup>
              ))}
            </select>
          </label>
          {profile && (
            <p className="small">
              {profile.dialect === "StarLine" ? "Star Line Mode. " : profile.dialect === "PlainText" ? "Plain text only. " : "ESC/POS. "}
              {profile.columns} characters per line{profile.columnsFontB ? ` (${profile.columnsFontB} in the small font)` : ""}
              {profile.cutter ? ", cutter" : ", no cutter"}
              {profile.drawer ? ", cash drawer port" : ""}
              {profile.barcode ? ", barcodes" : ""}. {profile.notes}
            </p>
          )}
          <div className="row">
            <label>
              Characters per line (empty = the model's) {errorFor("columnsOverride")}
              <input inputMode="numeric" value={draft.columns} placeholder={profile ? String(profile.columns) : ""} onChange={(e) => set({ columns: e.target.value })} />
            </label>
            <label>
              Cut
              <select value={draft.cut} onChange={(e) => set({ cut: e.target.value as PrinterDraft["cut"] })}>
                <option value="">As the model</option>
                <option value="partial">Partial</option>
                <option value="full">Full</option>
                <option value="none">No cutter (tear off)</option>
              </select>
            </label>
            <label>
              Cash drawer
              <select value={draft.drawerPin} onChange={(e) => set({ drawerPin: e.target.value as PrinterDraft["drawerPin"] })}>
                <option value="pin2">Kick port pin 2 (most drawers)</option>
                <option value="pin5">Kick port pin 5</option>
              </select>
            </label>
            <label>
              Receipt number barcode
              <select value={draft.barcode} onChange={(e) => set({ barcode: e.target.value as PrinterDraft["barcode"] })}>
                <option value="">As the model</option>
                <option value="yes">Print it</option>
                <option value="no">Text only</option>
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
                Cancel
              </button>
            )}
            <button type="submit" disabled={busy}>
              {busy ? "Checking…" : "Save printer"}
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
          <summary>Find printers and ports</summary>
          <div className="row">
            <label>
              Also search the network (e.g. 192.168.1.0/24, optional)
              <input value={subnet} onChange={(e) => setSubnet(e.target.value)} placeholder="192.168.1.0/24" />
            </label>
            <button type="button" className="secondary" disabled={busy} onClick={() => void discover()}>
              {busy ? "Searching…" : "Search"}
            </button>
          </div>
          {found && (
            <ul aria-label="Found devices" className="small">
              {found.network.map((n) => (
                <li key={`${n.host}:${n.port}`}>
                  {n.host}:{n.port} {n.identified ? `· ${n.identified.maker ?? ""} ${n.identified.model ?? ""}` : ""}{" "}
                  <button
                    type="button"
                    className="link"
                    onClick={() => set({ connection: "network", host: n.host, port: String(n.port), ...(n.profileId ? { profileId: n.profileId } : {}) })}
                  >
                    Use
                  </button>
                </li>
              ))}
              {found.serialPorts.map((p) => (
                <li key={p.port}>
                  {p.label}{" "}
                  <button type="button" className="link" onClick={() => set({ connection: "serial", serialPort: p.port })}>
                    Use
                  </button>
                </li>
              ))}
              {found.queues.map((q) => (
                <li key={q}>
                  Printer queue: {q}{" "}
                  <button type="button" className="link" onClick={() => set({ connection: "spooler", queue: q })}>
                    Use
                  </button>
                </li>
              ))}
              {found.deviceFiles.map((f) => (
                <li key={f}>
                  Device: {f}{" "}
                  <button type="button" className="link" onClick={() => set({ connection: "deviceFile", path: f })}>
                    Use
                  </button>
                </li>
              ))}
              {found.networkProblem && <li role="alert">{found.networkProblem}</li>}
              {found.network.length + found.serialPorts.length + found.queues.length + found.deviceFiles.length === 0 && <li>Nothing found.</li>}
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
    setMessage(res.status === "ok" ? "Saved. Scans from this port go to the till." : describeError(res.error));
    if (res.status === "ok") onChanged();
  }

  return (
    <section aria-label="Serial scanner" className="device-section">
      <h2>Serial barcode scanner (optional)</h2>
      <p className="small">
        USB scanners in keyboard mode need nothing here. Use this only for scanners on a COM port (RS-232, USB-COM or Bluetooth SPP).
        The till reconnects by itself after the scanner is unplugged.
      </p>
      <form onSubmit={(e) => void save(e)} className="row" aria-label="Scanner settings">
        <label>
          Port
          <input value={port} placeholder="COM4 or /dev/ttyACM0" onChange={(e) => setPort(e.target.value)} />
        </label>
        <label>
          Speed (baud)
          <select value={baud} onChange={(e) => setBaud(e.target.value)}>
            {BAUDS.map((b) => (
              <option key={b}>{b}</option>
            ))}
          </select>
        </label>
        <button type="submit" disabled={!port.trim()}>
          Save scanner
        </button>
        {device && (
          <button
            type="button"
            className="secondary"
            onClick={() => void commands.deviceRemove(device.deviceId).then(() => onChanged())}
          >
            Remove
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

function TerminalSection({ device, onChanged }: { device: DeviceDto | null; onChanged: () => void }) {
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
    setMessage(res.status === "ok" ? "Saved. “Card (terminal)” appears at payment." : describeError(res.error));
    if (res.status === "ok") onChanged();
  }
  return (
    <section aria-label="Card terminal" className="device-section">
      <h2>Card terminal</h2>
      <p className="small">
        A separate (standalone) terminal needs nothing here: record its payments as “Card (separate terminal)”. An integrated terminal is
        connected through its provider once the agreement and test credentials exist. Until then the sandbox terminal lets you practise:
        amounts ending in .01 decline, .02 time out (to practise settling), .03 take a few seconds.
      </p>
      {device ? (
        <div className="actions">
          <span>{device.name}</span>
          <button type="button" className="secondary" onClick={() => void commands.deviceRemove(device.deviceId).then(() => onChanged())}>
            Remove
          </button>
        </div>
      ) : (
        <button type="button" className="secondary" onClick={() => void save()}>
          Use the sandbox terminal (practice mode only)
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
