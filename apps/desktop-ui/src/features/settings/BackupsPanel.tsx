import { Database, Download, HardDrive, History, KeyRound, RefreshCw } from "../../ui/icons";
import { SectionTitle } from "../../ui/PanelHeader";
import { Dialog } from "../checkout/Dialog";
import { useCallback, useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { BackupCheckDto, BackupDto, BackupStatusDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";

const KIND: Record<string, string> = {
  hourly: "Hourly",
  daily: "Daily",
  shutdown: "When the app closed",
  pre_migration: "Before an update",
  manual: "Made by hand",
  pre_restore: "Before a restore",
};

function when(iso: string): string {
  return new Date(iso).toLocaleString([], { dateStyle: "medium", timeStyle: "short", hourCycle: "h23" });
}

function size(kb: number): string {
  return kb >= 1024 ? `${(kb / 1024).toFixed(1)} MB` : `${kb} KB`;
}

/**
 * Settings → Backups (E19-T03, owner only). The app backs up on its own: daily, hourly when
 * something changed, and when it closes. The owner can back up now, choose a second folder
 * (a USB stick or external drive) for copies, check a backup, and restore one. A restore
 * keeps the replaced database as a backup, then restarts the app. The owner can also export all
 * the data as CSV in a ZIP (E19-T04).
 */
export function BackupsPanel() {
  const [status, setStatus] = useState<BackupStatusDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [busy, setBusy] = useState(false);
  const [checks, setChecks] = useState<Record<string, BackupCheckDto>>({});
  const [restoring, setRestoring] = useState<BackupDto | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [recovery, setRecovery] = useState<string | null>(null);

  const load = useCallback(() => {
    void commands.backupStatus().then((res) => {
      if (res.status === "ok") setStatus(res.data);
      else setError(res.error);
    });
  }, []);
  useEffect(load, [load]);

  async function run<T>(f: () => Promise<{ status: "ok"; data: T } | { status: "error"; error: IpcError }>): Promise<T | null> {
    setBusy(true);
    setError(null);
    const res = await f();
    setBusy(false);
    if (res.status === "error") {
      setError(res.error);
      return null;
    }
    return res.data;
  }

  async function backUpNow() {
    const made = await run(() => commands.backupNow());
    if (made) {
      setDone(`Backed up: ${made.sales} ${made.sales === 1 ? "sale" : "sales"}, ${size(made.sizeKb)}.`);
      load();
    }
  }

  async function check(b: BackupDto) {
    const c = await run(() => commands.backupVerify(b.id));
    if (c) setChecks((all) => ({ ...all, [b.id]: c }));
  }

  async function showRecoveryKey() {
    const k = await run(() => commands.securityRecoveryKey());
    if (k) setRecovery(k);
  }

  async function confirmRecoveryKey() {
    const s = await run(() => commands.securityRecoveryKeyConfirmed());
    if (s) {
      setStatus(s);
      setRecovery(null);
    }
  }

  async function exportAll() {
    const res = await run(() => commands.dataExportFull());
    if (res) setDone(`Exported ${res.rows} rows from ${res.tables} tables to ${res.path}.`);
  }

  async function folder(clear: boolean) {
    const s = await run(() => (clear ? commands.backupClearFolder() : commands.backupChooseFolder()));
    if (s) setStatus(s);
  }

  async function restore() {
    if (!restoring) return;
    setBusy(true);
    setError(null);
    const res = await commands.backupRestore(restoring.id, confirmed);
    setBusy(false);
    if (res.status === "ok") setDone("Restored. Sale OS is restarting…");
    else setError(res.error);
    setRestoring(null);
    setConfirmed(false);
  }

  if (!status) return error ? <p role="alert">{describeError(error)}</p> : <p aria-busy="true">Loading…</p>;
  return (
    <div className="panel-sections">
      <p>
        Sale OS backs up this till by itself: once a day, every hour something changed, and when it closes. Copies go to a
        second folder too if you choose one, such as a USB stick.
      </p>
      {done && <p role="status">{done}</p>}
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <p className="button-row">
        <button type="button" disabled={busy} onClick={() => void backUpNow()}>
          <RefreshCw />
          Back up now
        </button>
      </p>
      <section aria-label="Encryption">
        <SectionTitle icon={KeyRound}>Encryption</SectionTitle>
        {status.encrypted ? (
          <>
            <p>
              This till's data and its backups are encrypted. They open only on this computer, or with the{" "}
              <strong>recovery key</strong>.
            </p>
            {!status.recoveryKeyConfirmed && (
              <p role="alert">
                Write the recovery key down and keep it somewhere safe, away from this computer. Without it, the backups
                cannot be opened if this computer fails.
              </p>
            )}
            {recovery ? (
              <div className="recovery-key">
                <p className="mono" data-testid="recovery-key">
                  {recovery}
                </p>
                <p className="small">Anyone with this key and a backup can read the shop's data. Do not photograph it or email it.</p>
                <p className="button-row">
                  <button type="button" disabled={busy} onClick={() => void confirmRecoveryKey()}>
                    I have written it down
                  </button>
                  <button type="button" className="secondary" onClick={() => setRecovery(null)}>
                    Hide
                  </button>
                </p>
              </div>
            ) : (
              <button type="button" className="secondary" disabled={busy} onClick={() => void showRecoveryKey()}>
                Show recovery key
              </button>
            )}
          </>
        ) : (
          <p role="alert">
            This till's data is not encrypted{status.encryptionProblem ? `: ${status.encryptionProblem}` : "."} Use
            BitLocker on this computer and on backup sticks.
          </p>
        )}
      </section>
      <section aria-label="Second folder">
        <SectionTitle icon={HardDrive}>Second folder</SectionTitle>
        {status.secondFolder ? (
          <>
            <p>
              Copies also go to <span className="mono">{status.secondFolder}</span>.
            </p>
            <p className="button-row">
              <button type="button" className="secondary" disabled={busy} onClick={() => void folder(false)}>
                Change…
              </button>
              <button type="button" className="secondary" disabled={busy} onClick={() => void folder(true)}>
                Stop copying
              </button>
            </p>
          </>
        ) : (
          <>
            <p>No second folder: if this computer's disk fails, the backups go with it. A USB stick or a network folder keeps a safe copy.</p>
            <p className="button-row">
              <button type="button" className="secondary" disabled={busy} onClick={() => void folder(false)}>
                Choose a folder…
              </button>
            </p>
          </>
        )}
        {status.secondFolderProblem && (
          <p role="alert">The last copy to the second folder failed: {status.secondFolderProblem}</p>
        )}
      </section>
      <section aria-label="Saved backups">
      <SectionTitle icon={History}>Saved backups</SectionTitle>
      <table className="sales-table" aria-label="Backups">
        <thead>
          <tr>
            <th scope="col">Made</th>
            <th scope="col">Why</th>
            <th scope="col" className="num">
              Sales
            </th>
            <th scope="col" className="num">
              Size
            </th>
            <th scope="col">Check</th>
            <th scope="col" className="row-actions">
              <span className="visually-hidden">Restore</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {status.backups.map((b) => {
            const c = checks[b.id];
            return (
              <tr key={b.id} data-testid="backup-row">
                <th scope="row">
                  {when(b.createdAt)}
                  {b.secondFolderOnly && <span className="small"> · second folder only</span>}
                </th>
                <td>{KIND[b.kind] ?? b.kind}</td>
                <td className="num">{b.sales}</td>
                <td className="num">{size(b.sizeKb)}</td>
                <td>
                  {c ? (
                    c.restorable ? (
                      "Whole"
                    ) : (
                      <span role="alert">{c.tooNew ? "From a newer version" : "Damaged"}</span>
                    )
                  ) : (
                    <button type="button" className="secondary" disabled={busy} onClick={() => void check(b)}>
                      Check
                    </button>
                  )}
                </td>
                <td className="row-actions">
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() => {
                      setConfirmed(false);
                      setRestoring(b);
                    }}
                  >
                    Restore…
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {status.backups.length === 0 && (
        <div className="empty-state">
          <span className="empty-icon">
            <Database />
          </span>
          <p>No backups yet. The first is made a minute after start-up.</p>
        </div>
      )}
      </section>
      <section aria-label="Export all data">
        <SectionTitle icon={Download}>Export all data</SectionTitle>
        <p>
          Everything this till holds — sales, returns, stock, products, staff, settings and the audit log — as spreadsheet
          files (CSV) in one ZIP, for your records or another system. PINs are not included.
        </p>
        <p className="button-row">
          <button type="button" className="secondary" disabled={busy} onClick={() => void exportAll()}>
            <Download />
            Export all data…
          </button>
        </p>
      </section>
      {status.linked && (
        <p className="small">
          This till is linked to the cloud: after a restore, its sales since the backup come back from the cloud. Restore
          only while it is online.
        </p>
      )}
      {restoring && (
        <Dialog
          title="Restore a backup"
          alert
          safeFocus
          confirmLabel="Restore and restart"
          confirmDisabled={busy || (!confirmed && !status.linked)}
          onConfirm={() => void restore()}
          onCancel={() => setRestoring(null)}
        >
          <p>
            Restore the backup made <strong>{when(restoring.createdAt)}</strong> ({restoring.sales}{" "}
            {restoring.sales === 1 ? "sale" : "sales"})? The till goes back to that moment and Sale OS restarts. What it holds
            now is kept as a backup, so this can be undone.
          </p>
          {status.linked ? (
            <p>
              Sales, returns and stock changes made on this till since then come back from the cloud after the restart.
              Until they have, sales cannot be completed.
            </p>
          ) : (
            <label className="check">
              <input type="checkbox" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} /> I understand
              that sales, returns and stock changes made on this till after {when(restoring.createdAt)} will be lost.
            </label>
          )}
        </Dialog>
      )}
    </div>
  );
}
