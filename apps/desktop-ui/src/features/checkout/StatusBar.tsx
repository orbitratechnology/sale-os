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
import { useT } from "../../i18n";
import { roleName } from "../auth/SignIn";

/** How often the status bar asks Rust where sync stands. */
const SYNC_POLL_MS = 10_000;
/** How often it asks how the receipt printer is (DEV-006), unless the printer pushes it. */
const PRINTER_POLL_MS = 15_000;
/**
 * A printer that pushes its status (automatic status back) sends every change as
 * `printer://status`; asking is then only a slow check.
 */
const PRINTER_PUSH_POLL_MS = 60_000;

/**
 * Status the receipt printer pushes (`printer://status`), as it arrives. Outside the Tauri
 * shell (browser tests, the dev harness) nothing is pushed.
 */
function usePrinterPushes(onStatus: (s: PrinterStatusDto) => void): void {
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let stop: (() => void) | null = null;
    let live = true;
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen<PrinterStatusDto>("printer://status", (e) => onStatus(e.payload)).then((unlisten) => {
        if (live) stop = unlisten;
        else unlisten();
      }),
    );
    return () => {
      live = false;
      stop?.();
    };
  }, [onStatus]);
}

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
  const t = useT();
  const [sync, setSync] = useState<SyncStatusDto | null>(null);
  const [diag, setDiag] = useState<string | null>(null);
  async function saveDiagnostics() {
    setDiag(t("Saving diagnostics…"));
    const res = await commands.diagnosticsExport();
    if (res.status === "error") setDiag(describeError(res.error));
    else setDiag(res.data ? t("Diagnostics saved to {path}", { path: res.data.path }) : null);
  }
  const [printer, setPrinter] = useState<PrinterStatusDto | null>(null);
  const push = printer?.push ?? false;
  useEffect(() => {
    let live = true;
    const load = () =>
      void commands.printerStatus().then((res) => {
        if (live && res.status === "ok") setPrinter(res.data);
      });
    load();
    const timer = window.setInterval(load, push ? PRINTER_PUSH_POLL_MS : PRINTER_POLL_MS);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [push]);
  usePrinterPushes(setPrinter);
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
    <footer className="statusbar" aria-label={t("Status")}>
      <span>
        <User />
        {session.displayName} <span className="role-badge">{roleName(t, session.role)}</span>
      </span>
      <span>
        <Store />
        {workspace.registerName} · <span className="mono">{workspace.registerCode}</span>
      </span>
      {workspace.mode === "sandbox" ? <span className="sandbox-tag">SANDBOX</span> : <span className="live-tag">LIVE</span>}
      <span>
        <ScanLine />
        {scannerReady ? t("Scanner ready") : t("Typing in a field")}
      </span>
      {drawer && (
        <span data-testid="drawer-status">
          <Banknote />
          {drawer.session ? (
            <>
              {t("Drawer open")} · <span className="mono">{drawer.session.number}</span>
            </>
          ) : (
            t("Drawer closed")
          )}
        </span>
      )}
      {printer?.configured && (
        <span data-testid="printer-status" data-state={printer.state} title={printer.target ?? undefined}>
          {printer.state === "ready" && t("Printer ready")}
          {printer.state === "unknown" && t("Printer set up")}
          {printer.state === "warning" && t("Printer: {state}", { state: printer.message ?? t("check it") })}
          {printer.state === "offline" && <strong>{t("Printer: {state}", { state: printer.message ?? t("offline") })}</strong>}
        </span>
      )}
      <span data-testid="sync-status" data-state={sync?.indicator ?? "not_linked"}>
        {!sync || sync.indicator === "not_linked" ? <HardDrive /> : sync.indicator === "offline" ? <CloudOff /> : <Cloud />}
        {sync ? indicatorText(sync, t) : t("Not linked · saved on this computer")}
      </span>
      <button
        type="button"
        className="link"
        title={t("A file for support: the till's state and its logs, without business records or secrets")}
        onClick={() => void saveDiagnostics()}
      >
        {t("Save diagnostics…")}
      </button>
      {diag && (
        <span role="status" className="small">
          {diag}
        </span>
      )}
      {sync && sync.conflicts > 0 && onConflicts && (
        <button type="button" className="secondary small" onClick={onConflicts}>
          {t("Resolve conflicts")}
        </button>
      )}
    </footer>
  );
}
