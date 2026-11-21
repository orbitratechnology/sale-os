import { useEffect, useRef } from "react";

/** A scan from a serial (RS-232, USB-COM or Bluetooth) scanner, forwarded by Rust (E21-T04). */
export interface SerialScan {
  scanEventId: string;
  raw: string;
  port: string;
}

/**
 * Delivers serial-scanner scans (`scanner://scan`) to `onScan`, while `enabled`. Outside
 * the Tauri shell (browser tests, the dev harness) there are no serial scanners, and this
 * does nothing.
 */
export function useSerialScans(onScan: (scan: SerialScan) => void, enabled = true): void {
  const ref = useRef(onScan);
  useEffect(() => {
    ref.current = onScan;
  });
  useEffect(() => {
    if (!enabled || !("__TAURI_INTERNALS__" in window)) return;
    let stop: (() => void) | null = null;
    let live = true;
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen<SerialScan>("scanner://scan", (e) => ref.current(e.payload)).then((unlisten) => {
        if (live) stop = unlisten;
        else unlisten();
      }),
    );
    return () => {
      live = false;
      stop?.();
    };
  }, [enabled]);
}
