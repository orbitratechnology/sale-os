import { useLayoutEffect, useRef, type FormEvent, type ReactNode } from "react";

/**
 * A modal dialog on the native `<dialog>` element, opened with `showModal()`.
 *
 * The browser does what custom overlays have to fake: everything outside the dialog becomes
 * inert (Tab cannot leave it, nothing behind can be clicked or read by a screen reader), it
 * sits in the top layer over a `::backdrop`, and when it closes focus goes back to whatever
 * had it before (in the till usually the page itself, so the scanner stays ready).
 *
 * - Opening and closing follow mounting: render it to open it, stop rendering it to close it.
 * - Initial focus: an element marked `data-autofocus`, else the browser's choice (the first
 *   focusable element). React's `autoFocus` runs before the dialog is open, so it is not used.
 * - Escape: a dialog that handles Escape itself (a keydown listener that calls
 *   `preventDefault`) keeps doing so. An Escape it does not handle becomes the native
 *   `cancel` event, which calls `onDismiss`; without `onDismiss` the dialog stays open.
 */
export function Modal({
  label,
  labelledBy,
  className,
  onDismiss,
  onSubmit,
  focusSelf,
  alert,
  children,
}: {
  /** Accessible name when there is no visible title to point at. */
  label?: string;
  /** Id of the visible title (preferred over `label`). */
  labelledBy?: string;
  className?: string;
  onDismiss?: () => void;
  /** Wraps the content in a form submitted by Enter in a field or a submit button. */
  onSubmit?: (e: FormEvent<HTMLFormElement>) => void;
  /**
   * Focus the dialog itself rather than a control inside it: for dialogs driven by their own
   * keys (arrows pick a row, Enter acts on it). Screen readers announce its name; Tab then
   * moves into its controls.
   */
  focusSelf?: boolean;
  /** An urgent confirmation (restore, join): exposed as an alert dialog. */
  alert?: boolean;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement | null>(null);
  const dismiss = useRef(onDismiss);
  useLayoutEffect(() => {
    dismiss.current = onDismiss;
  });

  useLayoutEffect(() => {
    const d = ref.current;
    if (!d) return;
    let mounted = true;
    if (!d.open) d.showModal();
    if (focusSelf) d.focus();
    else d.querySelector<HTMLElement>("[data-autofocus]")?.focus();
    const onCancel = (e: Event) => {
      // React decides when the dialog closes: ask the owner, never close behind its back.
      e.preventDefault();
      dismiss.current?.();
    };
    const onClose = () => {
      // The browser may close a modal on repeated Escape presses despite preventDefault.
      // Keep the app's state and the screen in step: dismiss, or reopen if not dismissible.
      // `close` is queued, so it can arrive after the dialog was reopened (React's StrictMode
      // mounts effects twice in development): act only if it really is closed now.
      if (!mounted || d.open) return;
      if (dismiss.current) dismiss.current();
      else d.showModal();
    };
    d.addEventListener("cancel", onCancel);
    d.addEventListener("close", onClose);
    return () => {
      mounted = false;
      d.removeEventListener("cancel", onCancel);
      d.removeEventListener("close", onClose);
      // Closing (rather than just removing the element) returns focus to where it was.
      if (d.open) d.close();
    };
    // Opened once, on mount: later prop changes must not reopen or refocus it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <dialog ref={ref} role={alert ? "alertdialog" : undefined} tabIndex={focusSelf ? -1 : undefined} aria-label={labelledBy ? undefined : label} aria-labelledby={labelledBy} className={`dialog card ${className ?? ""}`.trim()}>
      {onSubmit ? (
        <form className="dialog-form" onSubmit={onSubmit}>
          {children}
        </form>
      ) : (
        children
      )}
    </dialog>
  );
}
