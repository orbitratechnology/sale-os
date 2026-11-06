import { useEffect } from "react";

const KEYS = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "clear", "0", "ok"] as const;
export const MAX_PIN = 8;

/**
 * A PIN pad for mouse, touch and keyboard (E05-T06). Digits, Backspace and Enter work from
 * the keyboard while it is shown. Shows dots only; the PIN never appears on screen.
 */
export function PinPad({
  value,
  onChange,
  onSubmit,
  disabled,
  label,
}: {
  value: string;
  onChange: (pin: string) => void;
  onSubmit: () => void;
  disabled: boolean;
  label: string;
}) {
  useEffect(() => {
    if (disabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey) return;
      if (/^\d$/.test(e.key)) {
        e.preventDefault();
        if (value.length < MAX_PIN) onChange(value + e.key);
      } else if (e.key === "Backspace") {
        e.preventDefault();
        onChange(value.slice(0, -1));
      } else if (e.key === "Enter" && value.length >= 4) {
        e.preventDefault();
        onSubmit();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [value, onChange, onSubmit, disabled]);

  return (
    <div className="pinpad" role="group" aria-label={label}>
      <output className="pin-dots" aria-label={`${value.length} digits entered`}>
        {Array.from({ length: Math.max(4, value.length) }, (_, i) => (
          <span key={i} className={i < value.length ? "dot filled" : "dot"} />
        ))}
      </output>
      <div className="pin-keys">
        {KEYS.map((k) => (
          <button
            key={k}
            type="button"
            className={k === "ok" ? "pin-key ok" : "pin-key"}
            disabled={disabled || (k === "ok" && value.length < 4)}
            aria-label={k === "clear" ? "Delete last digit" : k === "ok" ? "Sign in" : k}
            onClick={() => {
              if (k === "clear") onChange(value.slice(0, -1));
              else if (k === "ok") onSubmit();
              else if (value.length < MAX_PIN) onChange(value + k);
            }}
          >
            {k === "clear" ? "⌫" : k === "ok" ? "OK" : k}
          </button>
        ))}
      </div>
    </div>
  );
}
