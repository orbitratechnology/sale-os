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
 * **Slow scanners** (older cordless and Bluetooth units at 40–120 ms per character, E21-T03)
 * are recognised by *regular* timing rather than speed: a scanner's keys arrive at a near
 * constant rate, a person's do not. This needs a suffix (the suffix proves the burst ended
 * on purpose) and at least `slowMinLength` characters.
 *
 * **Short codes** (1–3 character internal codes) count as scans when a suffix ends them at
 * scanner speed; without a suffix, `minLength` still applies because silence alone cannot
 * tell a short code from a stray key press.
 *
 * **GS (FNC1)** inside GS1 codes arrives as Ctrl+] from most wedge scanners; the hook passes
 * it in as the character `\x1d`, which is kept in the scan for GS1 decoding in Rust.
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
  /** Shortest code accepted as a scan when no suffix ends it (silence or a gap does). */
  minLength: number;
  /** Shortest code accepted when the configured suffix ends it at scanner speed. */
  minLengthWithSuffix: number;
  /**
   * Largest average gap for a *regular-timing* scan (slow scanners). 0 turns slow-scanner
   * recognition off. Only used with a suffix.
   */
  slowMaxInterKeyMs: number;
  /** Shortest code a slow scan may have: short bursts are too few keys to judge rhythm. */
  slowMinLength: number;
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
  minLengthWithSuffix: 2,
  slowMaxInterKeyMs: 120,
  slowMinLength: 6,
  suffix: "Enter",
  idleTimeoutMs: 200,
};

export interface DetectedScan {
  raw: string;
  /** How the end of the scan was recognised. */
  terminatedBy: "Enter" | "Tab" | "gap" | "idle";
  durationMs: number;
  /** "slow": recognised by its regular rhythm, not its speed (a slow scanner). */
  speed: "fast" | "slow";
}

/**
 * Something that looked like a scanner but was not delivered as a scan, so the till can
 * explain it instead of failing silently.
 */
export type ScanDiagnostic =
  /** A long burst at scanner speed ended without the configured suffix. */
  | "missing_suffix";

/** What the caller should do with the key event. */
export type KeyVerdict =
  /** Part of a possible scan; in scanner-ready mode the caller should swallow it. */
  | "buffered"
  /** A scan terminator; the caller must swallow it (no form submit, no focus move). */
  | "scan"
  /** Not scan input; let it through. */
  | "ignored";

const MODIFIERS = new Set(["Shift", "CapsLock", "AltGraph"]);

/** A short code must start this long after any discarded burst (see `discardedAt`). */
const SHORT_CODE_CLEAN_MS = 300;

export class ScanDetector {
  private buffer = "";
  private firstAt = 0;
  private lastAt = 0;
  private stalls = 0;
  /** Gaps between consecutive characters of the current burst. */
  private gaps: number[] = [];
  /**
   * When a non-empty burst was last thrown away. A short code right after that is probably
   * the *remainder* of an interrupted scan, which could match a different product.
   */
  private discardedAt = Number.NEGATIVE_INFINITY;

  constructor(
    private readonly config: ScanDetectorConfig,
    private readonly onScan: (scan: DetectedScan) => void,
    private readonly onDiagnostic: (d: ScanDiagnostic) => void = () => {},
  ) {}

  /** Feed one keydown: `key` is `KeyboardEvent.key` (or `"\x1d"` for GS), `at` its time in ms. */
  key(key: string, at: number): KeyVerdict {
    if (key.length === 1) {
      if (this.buffer) {
        const gap = at - this.lastAt;
        const absorbStall =
          gap <= this.config.maxStallMs && this.stalls < this.config.maxStalls && this.isFastSoFar();
        if (gap > this.config.maxInterKeyMs && absorbStall) {
          this.stalls += 1;
          this.gaps.push(gap);
        } else if (gap > this.config.maxInterKeyMs && this.mayBeSlowScan(gap)) {
          this.gaps.push(gap);
        } else if (gap > this.config.maxInterKeyMs) {
          // The burst has ended. Without a suffix the gap is the terminator; with one, an
          // unterminated burst is human typing or a partial read: drop it.
          let emitted = false;
          if (this.config.suffix === "none") emitted = this.emitIfScan("gap", this.lastAt);
          else this.diagnoseUnterminated();
          this.reset(!emitted);
        } else {
          this.gaps.push(gap);
        }
      }
      if (!this.buffer) this.firstAt = at;
      this.buffer += key;
      this.lastAt = at;
      return "buffered";
    }
    if (MODIFIERS.has(key)) return "ignored";
    if (this.config.suffix !== "none" && key === this.config.suffix) {
      const gap = at - this.lastAt;
      const inTime =
        gap <= this.config.maxStallMs || (this.config.slowMaxInterKeyMs > 0 && gap <= this.config.slowMaxInterKeyMs);
      const emitted = this.buffer !== "" && inTime && this.emitIfScan(key, at);
      this.reset(!emitted);
      return emitted ? "scan" : "ignored";
    }
    // Any other key (arrows, F-keys, Backspace...) means a person is at the keyboard.
    this.reset();
    return "ignored";
  }

  /** True while characters of a possible scan are buffered. */
  pending(): boolean {
    return this.buffer !== "";
  }

  /** With suffix "none", call after `idleTimeoutMs` of silence. */
  idle(at: number): void {
    if (this.config.suffix === "none" && this.buffer && at - this.lastAt >= this.config.idleTimeoutMs) {
      this.reset(!this.emitIfScan("idle", this.lastAt));
    }
  }

  /** At least two keys, all arriving at scanner speed on average. */
  private isFastSoFar(): boolean {
    return this.buffer.length >= 2 && (this.lastAt - this.firstAt) / (this.buffer.length - 1) <= this.config.maxInterKeyMs;
  }

  /** A slow gap may continue a burst that could still be a slow scanner's (suffix needed). */
  private mayBeSlowScan(gap: number): boolean {
    const slow = this.config.slowMaxInterKeyMs;
    if (this.config.suffix === "none" || slow <= 0 || gap > slow) return false;
    return this.buffer.length === 1 || (this.lastAt - this.firstAt) / (this.buffer.length - 1) <= slow;
  }

  /** Scanners type at a near-constant rate: most gaps sit close to the median. */
  private isRegular(gaps: number[]): boolean {
    if (gaps.length < 3) return false;
    const sorted = [...gaps].sort((a, b) => a - b);
    const median = sorted[Math.floor(sorted.length / 2)] ?? 0;
    if (median <= 0) return false;
    const close = gaps.filter((g) => g >= median * 0.5 && g <= median * 1.8).length;
    return close / gaps.length >= 0.85;
  }

  private diagnoseUnterminated(): void {
    if (this.buffer.length >= 6 && this.isFastSoFar()) this.onDiagnostic("missing_suffix");
  }

  /** `endAt` is the terminator's time (suffix) or the last character's (gap/idle). */
  private emitIfScan(terminatedBy: DetectedScan["terminatedBy"], endAt: number): boolean {
    const bySuffix = terminatedBy === "Enter" || terminatedBy === "Tab";
    const keys = this.buffer.length + (endAt > this.lastAt ? 1 : 0);
    const cleanStart = this.firstAt - this.discardedAt > SHORT_CODE_CLEAN_MS;
    const minLength = bySuffix && cleanStart ? this.config.minLengthWithSuffix : this.config.minLength;
    if (this.buffer.length < minLength || keys < 2) return false;
    const averageGap = (endAt - this.firstAt) / (keys - 1);
    let speed: DetectedScan["speed"] = "fast";
    if (averageGap > this.config.maxInterKeyMs) {
      const gaps = endAt > this.lastAt ? [...this.gaps, endAt - this.lastAt] : this.gaps;
      const slowOk =
        bySuffix &&
        this.config.slowMaxInterKeyMs > 0 &&
        this.buffer.length >= this.config.slowMinLength &&
        averageGap <= this.config.slowMaxInterKeyMs &&
        this.stalls === 0 &&
        this.isRegular(gaps);
      if (!slowOk) return false;
      speed = "slow";
    }
    this.onScan({ raw: this.buffer, terminatedBy, durationMs: this.lastAt - this.firstAt, speed });
    return true;
  }

  /** `discarded`: the burst was thrown away rather than delivered as a scan. */
  private reset(discarded = true): void {
    if (discarded && this.buffer) this.discardedAt = this.lastAt;
    this.buffer = "";
    this.stalls = 0;
    this.gaps = [];
  }
}
