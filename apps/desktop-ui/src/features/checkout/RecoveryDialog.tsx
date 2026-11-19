import { useEffect, useRef } from "react";
import type { RecoveredCartDto } from "../../ipc/bindings";
import { formatMoney } from "../../lib/money";

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

  const items = `${recovered.lines} ${recovered.lines === 1 ? "item" : "items"}`;
  return (
    <div className="backdrop">
      <div role="dialog" aria-modal="true" aria-label="Recovered cart" className="dialog card">
        <h2>
          Recovered cart ({items}, {formatMoney(recovered.totalMinor, { exponent })})
        </h2>
        <p>The till was closed during a sale. Carry on with it, or start again with an empty cart.</p>
        {recovered.repricedLines > 0 && (
          <p data-testid="recovery-repriced">
            {recovered.repricedLines === 1 ? "1 price has" : `${recovered.repricedLines} prices have`} changed since; the
            total uses today&apos;s prices and the changed {recovered.repricedLines === 1 ? "line is" : "lines are"} marked.
          </p>
        )}
        <div className="actions">
          <button type="button" className="danger" onClick={onDiscard}>
            Discard
          </button>
          <button type="button" ref={resume} onClick={onResume}>
            Resume
          </button>
        </div>
      </div>
    </div>
  );
}
