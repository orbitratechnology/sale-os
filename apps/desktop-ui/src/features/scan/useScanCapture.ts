import { useEffect, useRef } from "react";
import { DEFAULT_SCAN_CONFIG, ScanDetector, type DetectedScan, type ScanDetectorConfig } from "./scanDetector";

export interface CapturedScan extends DetectedScan {
  /** Unique per physical scan; Rust de-duplicates repeated *delivery* by this id (BAR-007). */
  scanEventId: string;
  source: "wedge";
  /** True when the scan was typed into a `[data-scan-field]` input (its owner clears it). */
  inScanField: boolean;
}

function isEditable(el: Element | null): boolean {
  if (!el) return false;
  if (el instanceof HTMLInputElement) return !["button", "checkbox", "radio", "submit", "reset"].includes(el.type);
  return el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement || (el as HTMLElement).isContentEditable;
}

/**
 * Captures keyboard-wedge scans (E07-T01).
 *
 * - **Scanner-ready mode**: focus is not in a text field. Scan characters are swallowed and
 *   delivered as a scan.
 * - **Scan field**: an input marked `data-scan-field`. Characters appear in the field as
 *   typed; a detected scan is delivered with `inScanField: true` and its terminator swallowed.
 * - **Any other text field** (search, forms) is left alone: human typing is never a scan.
 */
export function useScanCapture(onScan: (scan: CapturedScan) => void, config: Partial<ScanDetectorConfig> = {}): void {
  const onScanRef = useRef(onScan);
  useEffect(() => {
    onScanRef.current = onScan;
  });
  const merged: ScanDetectorConfig = { ...DEFAULT_SCAN_CONFIG, ...config };
  // A stable key so the listener is rebuilt only when a setting actually changes.
  const configKey = JSON.stringify(merged);

  useEffect(() => {
    const cfg = JSON.parse(configKey) as ScanDetectorConfig;
    let inScanField = false;
    const detector = new ScanDetector(cfg, (scan) =>
      onScanRef.current({ ...scan, scanEventId: crypto.randomUUID(), source: "wedge", inScanField }),
    );
    let idleTimer: ReturnType<typeof setTimeout> | undefined;

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey || e.isComposing) return;
      const target = e.target instanceof Element ? e.target : null;
      const field = target?.closest("[data-scan-field]") ?? null;
      const editable = isEditable(target);
      if (editable && !field) return;
      inScanField = field !== null;

      const verdict = detector.key(e.key, e.timeStamp);
      if (verdict === "scan" || (verdict === "buffered" && !editable)) {
        e.preventDefault();
        if (verdict === "scan") e.stopPropagation();
      }
      if (cfg.suffix === "none") {
        clearTimeout(idleTimer);
        idleTimer = setTimeout(() => detector.idle(performance.now()), cfg.idleTimeoutMs);
      }
    };

    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () => {
      window.removeEventListener("keydown", onKeyDown, { capture: true });
      clearTimeout(idleTimer);
    };
  }, [configKey]);
}
