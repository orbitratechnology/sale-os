import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Modal } from "../../ui/Modal";

/**
 * A modal confirmation (E09-T03: destructive actions confirm). Focus starts on the input or
 * the confirm button; Esc cancels. A native modal dialog: nothing behind it can be reached,
 * by mouse or by Tab, while it is open, and focus returns to where it was when it closes.
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
  /** Ask for a value; confirm stays disabled while it is empty unless `optional`. */
  input?: { label: string; initial?: string; inputMode?: "decimal" | "text"; optional?: boolean; maxLength?: number };
  onConfirm: (value: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(input?.initial ?? "");
  const titleId = useId();
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
    if (input && !input.optional && !value.trim()) return;
    onConfirm(value);
  };

  return (
    <Modal labelledBy={titleId} onSubmit={submit} onDismiss={onCancel}>
        <h2 id={titleId}>{title}</h2>
        {children}
        {input && (
          <label>
            {input.label}
            <input
              ref={(el) => {
                first.current = el;
              }}
              inputMode={input.inputMode ?? "text"}
              maxLength={input.maxLength}
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
            disabled={!!input && !input.optional && !value.trim()}
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
    </Modal>
  );
}
