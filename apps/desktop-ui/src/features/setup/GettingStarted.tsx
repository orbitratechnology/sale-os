import { useEffect, useRef } from "react";

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
    <div className="backdrop">
      <div role="dialog" aria-modal="true" aria-label="Getting started" className="dialog card wide">
        <h2>Getting started</h2>
        <p>The till is ready to sell in practice (SANDBOX) mode. When you have a moment:</p>
        <ol className="getting-started">
          <li>
            <strong>Products.</strong> Add them one by one, or import a CSV file from your supplier or old till.{" "}
            <button type="button" className="secondary" ref={first} onClick={onProducts}>
              Open Products <kbd>F6</kbd>
            </button>
          </li>
          <li>
            <strong>Staff.</strong> Give each cashier and manager their own PIN. Nobody shares the owner&apos;s.{" "}
            <button type="button" className="secondary" onClick={onStaff}>
              Add staff
            </button>
          </li>
          <li>
            <strong>Receipts.</strong> Check the details printed on receipts, and add your logo.{" "}
            <button type="button" className="secondary" onClick={onBusiness}>
              Business details
            </button>
          </li>
          <li>
            <strong>Practise</strong> a few sales, a return, and opening and closing the drawer. Practice sales are left out of
            reports.
          </li>
          <li>
            <strong>Go live</strong> when you are ready. You confirm with your PIN; practice sales stay out of reports.{" "}
            <button type="button" className="secondary" onClick={onGoLive}>
              Go live…
            </button>
          </li>
        </ol>
        <div className="actions">
          <button type="button" onClick={onClose}>
            Later
          </button>
        </div>
      </div>
    </div>
  );
}
