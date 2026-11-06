import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";

/**
 * A modal confirmation (E09-T03: destructive actions confirm). Focus starts on the input or
 * the confirm button; Esc cancels. Nothing behind it can be clicked while it is open.
 */
export function Dialog({
  title,
  children,
  confirmLabel,
  danger,
  input,
  onConfirm,
  onCancel,
}: {
  title: string;
  children?: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  /** Ask for a value; confirm stays disabled while it is empty. */
  input?: { label: string; initial?: string; inputMode?: "decimal" | "text" };
  onConfirm: (value: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(input?.initial ?? "");
  const first = useRef<HTMLInputElement | HTMLButtonElement | null>(null);
  useEffect(() => {
    first.current?.focus();
  }, []);
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

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (input && !value.trim()) return;
    onConfirm(value);
  };

  return (
    <div className="backdrop">
      <form role="dialog" aria-modal="true" aria-label={title} className="dialog card" onSubmit={submit}>
        <h2>{title}</h2>
        {children}
        {input && (
          <label>
            {input.label}
            <input
              ref={(el) => {
                first.current = el;
              }}
              inputMode={input.inputMode ?? "text"}
              value={value}
              onChange={(e) => setValue(e.target.value)}
            />
          </label>
        )}
        <div className="actions">
          <button type="button" className="secondary" onClick={onCancel}>
            Cancel
          </button>
          <button
            type="submit"
            className={danger ? "danger" : undefined}
            disabled={!!input && !value.trim()}
            ref={
              input
                ? undefined
                : (el) => {
                    first.current = el;
                  }
            }
          >
            {confirmLabel}
          </button>
        </div>
      </form>
    </div>
  );
}
