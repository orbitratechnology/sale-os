import { useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import { describeError } from "./errors";
import { useT } from "../../i18n";

/**
 * ADR-023: the till's data is encrypted and this computer does not have the key (a new
 * Windows account, a reinstalled PC, a backup brought from another till). The owner types the
 * recovery key they wrote down; Rust checks it against the data, keeps it in the keychain and
 * restarts the app.
 */
export function UnlockScreen() {
  const t = useT();
  // The explanation names the recovery key in bold: translated whole and split where it goes.
  const [keyBefore, keyAfter = ""] = t(
    "Sale OS keeps this till's data encrypted. This computer does not have the key, so it needs the {key} the owner wrote down (Settings → Backups). Nothing has been lost or changed.",
  ).split("{key}");
  const [key, setKey] = useState("");
  const [error, setError] = useState<IpcError | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const res = await commands.dbUnlock(key);
    setBusy(false);
    if (res.status === "ok") setDone(true);
    else setError(res.error);
  }

  return (
    <section className="card" aria-labelledby="unlock-title">
      <h2 id="unlock-title">{t("This till's data is locked")}</h2>
      <p>
        {keyBefore}
        <strong>{t("recovery key")}</strong>
        {keyAfter}
      </p>
      {done ? (
        <p role="status">{t("Unlocked. Sale OS is restarting…")}</p>
      ) : (
        <form onSubmit={(e) => void submit(e)}>
          <label>
            {t("Recovery key")}
            <input
              className="mono"
              autoFocus
              autoComplete="off"
              spellCheck={false}
              placeholder="XXXXXXXX-XXXXXXXX-XXXXXXXX-XXXXXXXX-XXXXXXXX-XXXXXXXX-XXXXXXXX-XXXXXXXX"
              value={key}
              onChange={(e) => setKey(e.target.value)}
            />
          </label>
          {error && (
            <p role="alert" className="field-error">
              {describeError(error)}
            </p>
          )}
          <p>
            <button type="submit" disabled={busy || key.replace(/[\s-]/g, "").length !== 64}>
              {t("Unlock")}
            </button>
          </p>
          <p className="small">{t("Without the recovery key the data cannot be opened. Do not delete the data folder: contact support.")}</p>
        </form>
      )}
    </section>
  );
}
