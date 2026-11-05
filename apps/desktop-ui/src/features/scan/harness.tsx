/**
 * Dev-only page for the keyboard-wedge e2e test (e2e/scanCapture.e2e.ts). Not part of the
 * production build (Vite builds index.html only). `?suffix=Enter|Tab|none`.
 */
import { StrictMode, useState } from "react";
import { createRoot } from "react-dom/client";
import type { ScanSuffix } from "./scanDetector";
import { useScanCapture, type CapturedScan } from "./useScanCapture";

declare global {
  interface Window {
    __scans: CapturedScan[];
    /** Keydown timestamps, for diagnosing timing in the e2e test. */
    __keys: Array<[string, number]>;
  }
}
window.__scans = [];
window.__keys = [];
window.addEventListener("keydown", (e) => window.__keys.push([e.key, e.timeStamp]), { capture: true });

const suffix = (new URLSearchParams(location.search).get("suffix") ?? "Enter") as ScanSuffix;

function Harness() {
  const [count, setCount] = useState(0);
  const [field, setField] = useState("");
  useScanCapture(
    (scan) => {
      window.__scans.push(scan);
      if (scan.inScanField) setField("");
      setCount((c) => c + 1);
    },
    { suffix },
  );
  return (
    <main>
      <p data-testid="count">{count}</p>
      <input data-testid="search" placeholder="search" />
      <input data-testid="scan-field" data-scan-field value={field} onChange={(e) => setField(e.target.value)} />
    </main>
  );
}

const root = document.getElementById("root");
if (root)
  createRoot(root).render(
    <StrictMode>
      <Harness />
    </StrictMode>,
  );
