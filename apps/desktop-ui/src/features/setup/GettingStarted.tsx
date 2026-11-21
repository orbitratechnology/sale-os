import { Modal } from "../../ui/Modal";
import { useEffect, useRef } from "react";
import { useT } from "../../i18n";

/**
 * "Getting started" (E04-T02 steps 5–7): what is left after the wizard, each a button to the
 * screen that does it. Shown once when the wizard finishes and offered from the SANDBOX
 * banner until the till goes live. Nothing here is required: the till already sells.
 */
export function GettingStarted({
  onProducts,
  onStaff,
  onBusiness,
  onGoLive,
  onClose,
}: {
  onProducts: () => void;
  onStaff: () => void;
  onBusiness: () => void;
  onGoLive: () => void;
  onClose: () => void;
}) {
  const t = useT();
  // Two steps start with a verb in bold: each sentence is translated whole and split where it goes.
  const [practiseBefore, practiseAfter = ""] = t(
    "{practise} a few sales, a return, and opening and closing the drawer. Practice sales are left out of reports.",
  ).split("{practise}");
  const [goLiveBefore, goLiveAfter = ""] = t(
    "{goLive} when you are ready. You confirm with your PIN; practice sales stay out of reports.",
  ).split("{goLive}");
  const first = useRef<HTMLButtonElement | null>(null);
  useEffect(() => first.current?.focus(), []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [onClose]);

  return (
    <Modal label={t("Getting started")} className="wide">
        <h2>{t("Getting started")}</h2>
        <p>{t("The till is ready to sell in practice (SANDBOX) mode. When you have a moment:")}</p>
        <ol className="getting-started">
          <li>
            <div>
              <span>
                <strong>{t("Products.")}</strong> {t("Add them one by one, or import a CSV file from your supplier or old till.")}
              </span>
              <button type="button" className="secondary" ref={first} onClick={onProducts}>
                {t("Open Products")} <kbd>F6</kbd>
              </button>
            </div>
          </li>
          <li>
            <div>
              <span>
                <strong>{t("Staff.")}</strong> {t("Give each cashier and manager their own PIN. Nobody shares the owner's.")}
              </span>
              <button type="button" className="secondary" onClick={onStaff}>
                {t("Add staff")}
              </button>
            </div>
          </li>
          <li>
            <div>
              <span>
                <strong>{t("Receipts.")}</strong> {t("Check the details printed on receipts, and add your logo.")}
              </span>
              <button type="button" className="secondary" onClick={onBusiness}>
                {t("Business details")}
              </button>
            </div>
          </li>
          <li>
            {practiseBefore}
            <strong>{t("Practise")}</strong>
            {practiseAfter}
          </li>
          <li>
            <div>
              <span>
                {goLiveBefore}
                <strong>{t("Go live")}</strong>
                {goLiveAfter}
              </span>
              <button type="button" className="secondary" onClick={onGoLive}>
                {t("Go live…")}
              </button>
            </div>
          </li>
        </ol>
        <div className="actions">
          <button type="button" onClick={onClose}>
            {t("Later")}
          </button>
        </div>
    </Modal>
  );
}
