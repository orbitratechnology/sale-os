import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { Modal } from "../../ui/Modal";
import { scaleDecimal } from "../../lib/decimal";
import { useT, type T } from "../../i18n";
import type { WeighRequest, WeighResult } from "./useCart";

/** What each refusal means for the cashier (the reason codes from Rust's `WeighRefusal`). */
export function refusalText(reason: string, message: string, t: T): string {
  switch (reason) {
    case "unstable":
      return t("The weight did not settle. Hold the item still on the scale and weigh again.");
    case "zero":
      return t("Nothing is on the scale. Put the item on it and weigh again.");
    case "negative":
      return t("The scale reads below zero. Zero the scale (empty pan) and weigh again.");
    case "overload":
      return t("Too heavy for the scale. Weigh it in parts, or type the weight.");
    case "underload":
      return t("The scale reads below its range. Zero the scale and weigh again.");
    case "no_scale":
      return t("No scale is set up on this till. Type the weight.");
    default:
      return t("The scale could not be read: {reason}", { reason: message });
  }
}

/**
 * Weighing an item sold by weight (E20-T06): "Weighing…" while the till waits for a stable
 * weight; then the line is added (and the dialog closes), or the refusal is shown with
 * "Weigh again" and the manual fallback, typing the weight. After a refusal Esc cancels and
 * nothing is added; while weighing it waits (the answer comes within a few seconds).
 */
export function WeighDialog({
  request,
  weigh,
  onTyped,
  onClose,
}: {
  request: WeighRequest;
  weigh: (r: WeighRequest) => Promise<WeighResult>;
  /** The typed weight as thousandths of the item's unit (a kilogram is 1000, a gram is 1000). */
  onTyped: (qtyMilli: string) => void;
  onClose: () => void;
}) {
  const t = useT();
  const titleId = useId();
  const [state, setState] = useState<"weighing" | { reason: string; message: string }>("weighing");
  const [typing, setTyping] = useState(false);
  const [typed, setTyped] = useState("");
  const [invalid, setInvalid] = useState(false);
  const input = useRef<HTMLInputElement | null>(null);
  const live = useRef(true);
  // The latest props, so a re-render of the till never starts another weighing.
  const props = useRef({ request, weigh, onClose });
  useEffect(() => {
    props.current = { request, weigh, onClose };
  });

  const run = useCallback(async () => {
    setState("weighing");
    const p = props.current;
    const res = await p.weigh(p.request);
    if (!live.current) return;
    if (res.kind === "refused") setState({ reason: res.reason, message: res.message });
    else props.current.onClose();
  }, []);

  // Weighs once when the dialog opens (also under StrictMode's mount, unmount, mount: a
  // second weighing would add a second line).
  const started = useRef(false);
  useEffect(() => {
    live.current = true;
    if (!started.current) {
      started.current = true;
      void run();
    }
    return () => {
      live.current = false;
    };
  }, [run]);

  useEffect(() => {
    if (typing) input.current?.focus();
  }, [typing]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      // While weighing the answer is on its way (at most a few seconds): wait for it.
      if (state !== "weighing") onClose();
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [onClose, state]);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const milli = scaleDecimal(typed.trim(), 3);
    if (milli === null || milli === "0" || milli.startsWith("-")) {
      setInvalid(true);
      return;
    }
    onTyped(milli);
    onClose();
  };

  const unit = request.unit === "g" ? "g" : "kg";
  return (
    <Modal labelledBy={titleId} className="weigh-dialog" focusSelf={state === "weighing"}>
      <h2 id={titleId}>{t("Weigh {name}", { name: request.name })}</h2>
      {state === "weighing" ? (
        <p role="status" aria-busy="true" data-testid="weighing">
          {t("Weighing… Keep the item still on the scale.")}
        </p>
      ) : (
        <p role="alert" data-testid="weigh-refused">
          {refusalText(state.reason, state.message, t)}
        </p>
      )}
      {typing && (
        <form onSubmit={submit} className="row">
          <label>
            {unit === "kg" ? t("Weight in kg") : t("Weight in g")}
            <input
              ref={input}
              inputMode="decimal"
              value={typed}
              aria-invalid={invalid}
              onChange={(e) => {
                setTyped(e.target.value);
                setInvalid(false);
              }}
            />
          </label>
          <button type="submit">{t("Add")}</button>
          {invalid && (
            <p role="alert" className="field-error">
              {t("Enter a weight above zero, e.g. 1.25.")}
            </p>
          )}
        </form>
      )}
      {state !== "weighing" && (
        <div className="button-row">
          <button type="button" data-autofocus onClick={() => void run()}>
            {t("Weigh again")}
          </button>
          {!typing && (
            <button type="button" className="secondary" onClick={() => setTyping(true)}>
              {t("Type the weight")}
            </button>
          )}
          <button type="button" className="secondary" onClick={onClose}>
            {t("Cancel")}
          </button>
        </div>
      )}
    </Modal>
  );
}
