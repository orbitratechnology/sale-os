import { Modal } from "../../ui/Modal";
import { useCallback, useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import { PinPad } from "../auth/PinPad";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";

/**
 * Going live (PRD §7.1 step 9): the owner confirms with their PIN. Practice sales stay
 * marked as practice and out of reports; there is no way back to practice mode.
 */
export function GoLiveDialog({ onDone, onCancel }: { onDone: () => void; onCancel: () => void }) {
  const t = useT();
  const [pin, setPin] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<IpcError | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCancel();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [onCancel]);

  const submit = useCallback(async () => {
    if (busy || pin.length < 4) return;
    setBusy(true);
    const res = await commands.registerGoLive(pin);
    setBusy(false);
    setPin("");
    if (res.status === "ok") onDone();
    else setError(res.error);
  }, [busy, pin, onDone]);

  const message = (e: IpcError) =>
    e.code === "VALIDATION_FAILED" ? t("Finish or clear the cart and close the practice drawer session first.") : describeError(e);

  return (
    <Modal label={t("Go live")} className="approval">
        <h2>{t("Go live")}</h2>
        <p>{t("From now on, sales on this till are real: they count in reports and need an open cash drawer.")}</p>
        <ul className="small">
          <li>{t("Practice sales stay marked as practice and are never counted.")}</li>
          <li>{t("This cannot be undone: the till will not go back to practice mode.")}</li>
        </ul>
        <p>{t("Enter your owner PIN to confirm.")}</p>
        <PinPad value={pin} onChange={setPin} onSubmit={() => void submit()} disabled={busy} label={t("Owner PIN")} />
        <div aria-live="polite" className="pin-status">
          {error && (
            <p role="alert" className="field-error">
              {message(error)}
            </p>
          )}
        </div>
        <div className="actions">
          <button type="button" className="secondary" onClick={onCancel}>
            {t("Stay in practice mode")}
          </button>
        </div>
    </Modal>
  );
}
