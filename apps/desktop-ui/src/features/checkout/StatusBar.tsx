import {
  Banknote,
  Cloud,
  CloudOff,
  HardDrive,
  ScanLine,
  Store,
  User,
} from "../../ui/icons";
import { useEffect, useState } from "react";
import { commands, type Session, type Workspace } from "../../ipc";
import type { CashStateDto_Serialize as CashState, PrinterStatusDto, SyncStatusDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import { indicatorText } from "../sync/syncText";

/** How often the status bar asks Rust where sync stands. */
const SYNC_POLL_MS = 10_000;
/** How often it asks how the receipt printer is (DEV-006). */
const PRINTER_POLL_MS = 15_000;

/**
 * Always visible, never modal (E09-T06): who is signed in, which till, sandbox or live,
 * scanner and sync state (E17-T09: Not linked, Synced, Sync pending, Offline, Attention
 * needed, Device revoked, Update required).
 */
export function StatusBar({
  workspace,
  session,
  scannerReady,
  drawer,
  onConflicts = null,
}: {
  workspace: Workspace;
  session: Session;
  scannerReady: boolean;
  /** The cash drawer (E15-T02); `null` while loading. */
  drawer: CashState | null;
  /** Opens the conflicts screen (managers and owners) when there are conflicts. */
  onConflicts?: (() => void) | null;
}) {
  const [sync, setSync] = useState<SyncStatusDto | null>(null);
  const [diag, setDiag] = useState<string | null>(null);
  async function saveDiagnostics() {
    setDiag("Saving diagnostics…");
    const res = await commands.diagnosticsExport();
    if (res.status === "error") setDiag(describeError(res.error));
    else setDiag(res.data ? `Diagnostics saved to ${res.data.path}` : null);
  }
  const [printer, setPrinter] = useState<PrinterStatusDto | null>(null);
  useEffect(() => {
    let live = true;
    const load = () =>
      void commands.printerStatus().then((res) => {
        if (live && res.status === "ok") setPrinter(res.data);
      });
    load();
    const timer = window.setInterval(load, PRINTER_POLL_MS);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, []);
  useEffect(() => {
    let live = true;
    const load = () =>
      void commands.syncStatus().then((res) => {
        if (live && res.status === "ok") setSync(res.data);
      });
    load();
    const timer = window.setInterval(load, SYNC_POLL_MS);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, []);
  return (
    <footer className="statusbar" aria-label="Status">
      <span>
        <User />
        {session.displayName} <span className="role-badge">{session.role}</span>
      </span>
      <span>
        <Store />
        {workspace.registerName} · <span className="mono">{workspace.registerCode}</span>
      </span>
      {workspace.mode === "sandbox" ? <span className="sandbox-tag">SANDBOX</span> : <span className="live-tag">LIVE</span>}
      <span>
        <ScanLine />
        {scannerReady ? "Scanner ready" : "Typing in a field"}
      </span>
      {drawer && (
        <span data-testid="drawer-status">
          <Banknote />
          {drawer.session ? (
            <>
              Drawer open · <span className="mono">{drawer.session.number}</span>
            </>
          ) : (
            "Drawer closed"
          )}
        </span>
      )}
      {printer?.configured && (
        <span data-testid="printer-status" data-state={printer.state} title={printer.target ?? undefined}>
          {printer.state === "ready" && "Printer ready"}
          {printer.state === "unknown" && "Printer set up"}
          {printer.state === "warning" && `Printer: ${printer.message ?? "check it"}`}
          {printer.state === "offline" && <strong>Printer: {printer.message ?? "offline"}</strong>}
        </span>
      )}
      <span data-testid="sync-status" data-state={sync?.indicator ?? "not_linked"}>
        {!sync || sync.indicator === "not_linked" ? <HardDrive /> : sync.indicator === "offline" ? <CloudOff /> : <Cloud />}
        {sync ? indicatorText(sync) : "Not linked · saved on this computer"}
      </span>
      <button
        type="button"
        className="link"
        title="A file for support: the till's state and its logs, without business records or secrets"
        onClick={() => void saveDiagnostics()}
      >
        Save diagnostics…
      </button>
      {diag && (
        <span role="status" className="small">
          {diag}
        </span>
      )}
      {sync && sync.conflicts > 0 && onConflicts && (
        <button type="button" className="secondary small" onClick={onConflicts}>
          Resolve conflicts
        </button>
      )}
    </footer>
  );
}
