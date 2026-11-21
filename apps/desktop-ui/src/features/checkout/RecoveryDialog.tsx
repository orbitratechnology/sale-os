import { Modal } from "../../ui/Modal";
import { useEffect, useRef } from "react";
import type { RecoveredCartDto } from "../../ipc/bindings";
import { formatMoney } from "../../lib/money";
import { useT } from "../../i18n";

/**
 * "Recovered cart" (E09-T02, POS-011): the till was closed with a sale in progress. Resume
 * keeps it, priced as items sell now, with changed prices marked; Discard empties it (audited).
 * Resume has the focus, so `Enter` resumes; `Esc` also resumes, as it loses nothing. A
 * scanner's Enter, with the focus elsewhere, does not answer the prompt.
 */
export function RecoveryDialog({
  recovered,
  exponent,
  onResume,
  onDiscard,
}: {
  recovered: RecoveredCartDto;
  exponent: number;
  onResume: () => void;
  onDiscard: () => void;
}) {
  const t = useT();
  const resume = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    resume.current?.focus();
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onResume();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [onResume]);

  const items = recovered.lines === 1 ? t("1 item") : t("{n} items", { n: recovered.lines });
  return (
    <Modal label={t("Recovered cart")}>
        <h2>{t("Recovered cart ({items}, {total})", { items, total: formatMoney(recovered.totalMinor, { exponent }) })}</h2>
        <p>{t("The till was closed during a sale. Carry on with it, or start again with an empty cart.")}</p>
        {recovered.repricedLines > 0 && (
          <p data-testid="recovery-repriced">
            {recovered.repricedLines === 1
              ? t("1 price has changed since; the total uses today's prices and the changed line is marked.")
              : t("{n} prices have changed since; the total uses today's prices and the changed lines are marked.", {
                  n: recovered.repricedLines,
                })}
          </p>
        )}
        <div className="actions">
          <button type="button" className="danger" onClick={onDiscard}>
            {t("Discard")}
          </button>
          <button type="button" ref={resume} onClick={onResume}>
            {t("Resume")}
          </button>
        </div>
    </Modal>
  );
}
