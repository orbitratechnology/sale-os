/**
 * Keyboard-wedge scan detection (E07-T01, BAR-002, BAR-007).
 *
 * USB HID scanners in keyboard mode "type" a barcode far faster than a person, a few
 * milliseconds per key, usually followed by a configured suffix (Enter or Tab). The detector
 * tells such bursts apart from human typing by timing alone. It never de-duplicates by value
 * or time: two identical scans 50 ms apart are two scans (BAR-007). Duplicate *delivery* of
 * the same scan is handled in Rust by `scanEventId`.
 *
 * **Stalls.** Key events can be delayed by tens of milliseconds when the machine is busy
 * (GC, rendering, a loaded CI runner: 41 ms was observed). Splitting a burst at such a stall
 * is dangerous: the fast remainder would be emitted as a *truncated* barcode, which may
 * match a different product. So a short stall inside a burst that has so far arrived at
 * scanner speed is absorbed. The whole burst must still average scanner speed to count as
 * a scan.
 *
 * Pure and timer-free, so it can be unit-tested with explicit timestamps. The React hook
 * (`useScanCapture`) wires it to DOM events.
 */

export type ScanSuffix = "Enter" | "Tab" | "none";

export interface ScanDetectorConfig {
  /** Largest *average* gap between keys of a scan. Human typing is far slower. */
  maxInterKeyMs: number;
  /** Longest single delay absorbed inside a fast burst (event-delivery jitter). */
  maxStallMs: number;
  /** How many stalls one scan may contain. */
  maxStalls: number;
  /** Shortest code accepted as a scan; shorter bursts are treated as noise. */
  minLength: number;
  /** Key the scanner sends after the code, or "none" (end detected by silence). */
  suffix: ScanSuffix;
  /**
   * With suffix "none": silence that ends a scan. Must exceed `maxStallMs`, so consecutive
   * scans need more than this gap between them. Physically they always do: presenting the
   * next item takes at least ~250 ms.
   */
  idleTimeoutMs: number;
}

export const DEFAULT_SCAN_CONFIG: ScanDetectorConfig = {
  maxInterKeyMs: 35,
  maxStallMs: 150,
  maxStalls: 2,
  minLength: 4,
  suffix: "Enter",
  idleTimeoutMs: 200,
};

export interface DetectedScan {
  raw: string;
  /** How the end of the scan was recognised. */
  terminatedBy: "Enter" | "Tab" | "gap" | "idle";
  durationMs: number;
}

/** What the caller should do with the key event. */
export type KeyVerdict =
  /** Part of a possible scan; in scanner-ready mode the caller should swallow it. */
  | "buffered"
  /** A scan terminator; the caller must swallow it (no form submit, no focus move). */
  | "scan"
  /** Not scan input; let it through. */
  | "ignored";

const MODIFIERS = new Set(["Shift", "CapsLock", "AltGraph"]);

export class ScanDetector {
  private buffer = "";
  private firstAt = 0;
  private lastAt = 0;
  private stalls = 0;

  constructor(
    private readonly config: ScanDetectorConfig,
    private readonly onScan: (scan: DetectedScan) => void,
  ) {}

  /** Feed one keydown: `key` is `KeyboardEvent.key`, `at` its timestamp in ms. */
  key(key: string, at: number): KeyVerdict {
    if (key.length === 1) {
      if (this.buffer) {
        const gap = at - this.lastAt;
        const absorbStall =
          gap <= this.config.maxStallMs && this.stalls < this.config.maxStalls && this.isFastSoFar();
        if (gap > this.config.maxInterKeyMs && absorbStall) {
          this.stalls += 1;
        } else if (gap > this.config.maxInterKeyMs) {
          // The burst has ended. Without a suffix the gap is the terminator; with one, an
          // unterminated burst is human typing or a partial read: drop it.
          if (this.config.suffix === "none") this.emitIfScan("gap", this.lastAt);
          this.reset();
        }
      }
      if (!this.buffer) this.firstAt = at;
      this.buffer += key;
      this.lastAt = at;
      return "buffered";
    }
    if (MODIFIERS.has(key)) return "ignored";
    if (this.config.suffix !== "none" && key === this.config.suffix) {
      const emitted = at - this.lastAt <= this.config.maxStallMs && this.emitIfScan(key, at);
      this.reset();
      return emitted ? "scan" : "ignored";
    }
    // Any other key (arrows, F-keys, Backspace...) means a person is at the keyboard.
    this.reset();
    return "ignored";
  }

  /** With suffix "none", call after `idleTimeoutMs` of silence. */
  idle(at: number): void {
    if (this.config.suffix === "none" && this.buffer && at - this.lastAt >= this.config.idleTimeoutMs) {
      this.emitIfScan("idle", this.lastAt);
      this.reset();
    }
  }

  /** At least two keys, all arriving at scanner speed on average. */
  private isFastSoFar(): boolean {
    return this.buffer.length >= 2 && (this.lastAt - this.firstAt) / (this.buffer.length - 1) <= this.config.maxInterKeyMs;
  }

  /** `endAt` is the terminator's time (suffix) or the last character's (gap/idle). */
  private emitIfScan(terminatedBy: DetectedScan["terminatedBy"], endAt: number): boolean {
    const keys = this.buffer.length + (endAt > this.lastAt ? 1 : 0);
    if (this.buffer.length < this.config.minLength || keys < 2) return false;
    const averageGap = (endAt - this.firstAt) / (keys - 1);
    if (averageGap > this.config.maxInterKeyMs) return false;
    this.onScan({ raw: this.buffer, terminatedBy, durationMs: this.lastAt - this.firstAt });
    return true;
  }

  private reset(): void {
    this.buffer = "";
    this.stalls = 0;
  }
}
