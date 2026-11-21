import { describe, expect, it } from "vitest";
import { DEFAULT_SCAN_CONFIG, ScanDetector, type DetectedScan, type ScanSuffix } from "./scanDetector";

/** Replays keys at explicit times: `[key, gapBeforeMs]`. */
function run(suffix: ScanSuffix, keys: Array<[string, number]>, idleAtEnd = true) {
  const scans: DetectedScan[] = [];
  const config = { ...DEFAULT_SCAN_CONFIG, suffix };
  const d = new ScanDetector(config, (s) => scans.push(s));
  let t = 1000;
  const verdicts = keys.map(([k, gap]) => {
    t += gap;
    return d.key(k, t);
  });
  if (idleAtEnd) d.idle(t + config.idleTimeoutMs);
  return { scans, verdicts };
}

/** A scanner burst: chars every `perKey` ms, then the suffix. */
function burst(code: string, perKey: number, suffix: ScanSuffix, gapBefore: number): Array<[string, number]> {
  const keys: Array<[string, number]> = [...code].map((c, i) => [c, i === 0 ? gapBefore : perKey]);
  if (suffix !== "none") keys.push([suffix, perKey]);
  return keys;
}

const CODES = Array.from({ length: 20 }, (_, i) => `50123456789${String(i).padStart(2, "0")}`);

/** Gap between consecutive scans: 50 ms with a suffix; with none, silence ends a scan. */
const gapBetween = (suffix: ScanSuffix) => (suffix === "none" ? 250 : 50);

describe.each<ScanSuffix>(["Enter", "Tab", "none"])("suffix %s", (suffix) => {
  it("delivers 20 scans at 5 ms/key, none merged or dropped", () => {
    const keys = CODES.flatMap((c) => burst(c, 5, suffix, gapBetween(suffix)));
    const { scans } = run(suffix, keys);
    expect(scans.map((s) => s.raw)).toEqual(CODES);
  });

  it("keeps identical rapid repeat scans (BAR-007: never de-duplicate by value)", () => {
    const keys = [0, 1, 2].flatMap(() => burst("012345678905", 4, suffix, gapBetween(suffix)));
    const { scans } = run(suffix, keys);
    expect(scans.map((s) => s.raw)).toEqual(["012345678905", "012345678905", "012345678905"]);
  });

  it("a delivery stall mid-scan does not truncate the code (regression: 41 ms stall)", () => {
    const keys: Array<[string, number]> = [...burst("5012345678906", 7, suffix, 50)];
    keys[5] = ["4", 41]; // "3" -> "4" delayed (index 5 is the "4" of 5012345678906)
    const { scans } = run(suffix, keys);
    expect(scans.map((s) => s.raw)).toEqual(["5012345678906"]);
  });

  it("does not treat human typing at 150 ms/key as a scan", () => {
    const keys = burst("abcdef", 150, suffix, 0);
    const { scans } = run(suffix, keys);
    expect(scans).toEqual([]);
  });
});

describe("terminators and noise", () => {
  it("swallows the suffix of a scan but lets a human Enter through", () => {
    const scan = run("Enter", burst("ABC-1234", 5, "Enter", 0));
    expect(scan.verdicts.at(-1)).toBe("scan");
    const human = run("Enter", burst("abcd", 150, "Enter", 0));
    expect(human.verdicts.at(-1)).toBe("ignored");
  });

  it("accepts short codes ended by the suffix at scanner speed, but never a single key", () => {
    expect(run("Enter", burst("A12", 5, "Enter", 0)).scans.map((s) => s.raw)).toEqual(["A12"]);
    expect(run("Enter", burst("12", 5, "Enter", 0)).scans.map((s) => s.raw)).toEqual(["12"]);
    expect(run("Enter", burst("1", 5, "Enter", 0)).scans).toEqual([]);
  });

  it("without a suffix, short bursts are still noise", () => {
    expect(run("none", burst("A12", 5, "none", 0)).scans).toEqual([]);
  });

  it("a short remainder right after a discarded burst is never a scan (truncation guard)", () => {
    // Human typing discarded, then 2 fast keys 100 ms later: could be the tail of a scan.
    const keys: Array<[string, number]> = [["x", 0], ["y", 150], ["4", 100], ["5", 5], ["Enter", 5]];
    expect(run("Enter", keys).scans).toEqual([]);
  });

  it("a long pause in the middle breaks the burst; the remainder alone is too short", () => {
    const keys: Array<[string, number]> = [["1", 0], ["2", 5], ["3", 5], ["4", 200], ["5", 5], ["Enter", 5]];
    expect(run("Enter", keys).scans).toEqual([]);
  });

  it.each<ScanSuffix>(["Enter", "Tab"])("a scan broken by a long stall is dropped, never emitted as a truncated tail (suffix %s)", (suffix) => {
    // 6 characters at scanner speed, a 200 ms stall (over maxStallMs), the other 7, the suffix:
    // the tail alone is long and fast enough to look like a scan, but it is half a code.
    const keys: Array<[string, number]> = [
      ...[..."501234"].map((c, i): [string, number] => [c, i === 0 ? 0 : 5]),
      ...[..."5678900"].map((c, i): [string, number] => [c, i === 0 ? 200 : 5]),
      [suffix, 5],
    ];
    expect(run(suffix, keys).scans).toEqual([]);
  });

  it("too many stalls drop the whole scan; the tail is not emitted", () => {
    const keys: Array<[string, number]> = [
      ...[..."5012345678900"].map((c, i): [string, number] => [c, i === 0 ? 0 : i === 3 || i === 5 || i === 7 ? 100 : 5]),
      ["Enter", 5],
    ];
    expect(run("Enter", keys).scans).toEqual([]);
  });

  it("the next clean scan after a broken one is recognised", () => {
    const keys: Array<[string, number]> = [
      ...[..."501234"].map((c, i): [string, number] => [c, i === 0 ? 0 : 5]),
      ...[..."5678900"].map((c, i): [string, number] => [c, i === 0 ? 200 : 5]),
      ["Enter", 5],
      ...burst("4006381333931", 5, "Enter", 300),
    ];
    expect(run("Enter", keys).scans.map((s) => s.raw)).toEqual(["4006381333931"]);
  });

  it("fast human digraphs separated by a pause are not a scan", () => {
    // "ab" fast, pause, "cd" fast, Enter: average gap is well above scanner speed.
    const keys: Array<[string, number]> = [["a", 0], ["b", 30], ["c", 120], ["d", 30], ["Enter", 30]];
    expect(run("Enter", keys).scans).toEqual([]);
  });

  it("more stalls than allowed break the burst", () => {
    const keys: Array<[string, number]> = [
      ...[..."123456"].map((c, i): [string, number] => [c, i === 0 ? 0 : i % 2 === 0 ? 100 : 5]),
      ["Enter", 5],
    ];
    expect(run("Enter", keys).scans).toEqual([]);
  });

  it("a scan that starts right after human typing is still recognised", () => {
    const keys: Array<[string, number]> = [["x", 0], ["y", 150], ...burst("4006381333931", 5, "Enter", 150)];
    expect(run("Enter", keys).scans.map((s) => s.raw)).toEqual(["4006381333931"]);
  });

  it("Shift between characters does not break a scan", () => {
    const keys: Array<[string, number]> = [["Shift", 0], ["A", 3], ["Shift", 2], ["B", 3], ["1", 5], ["2", 5], ["Enter", 5]];
    expect(run("Enter", keys).scans.map((s) => s.raw)).toEqual(["AB12"]);
  });

  it("navigation keys reset a pending burst", () => {
    const keys: Array<[string, number]> = [["1", 0], ["2", 5], ["ArrowDown", 5], ["3", 5], ["4", 5], ["Enter", 5]];
    expect(run("Enter", keys).scans).toEqual([]);
  });

  it("with no suffix, the last scan ends on idle", () => {
    const { scans } = run("none", burst("4006381333931", 5, "none", 0));
    expect(scans).toEqual([{ raw: "4006381333931", terminatedBy: "idle", durationMs: 60, speed: "fast" }]);
  });
});

describe("slow scanners (E21-T03)", () => {
  it.each([40, 50, 80, 110])("a scanner at a steady %i ms/char is a scan, marked slow", (perKey) => {
    const { scans, verdicts } = run("Enter", burst("4006381333931", perKey, "Enter", 0));
    expect(scans.map((s) => [s.raw, s.speed])).toEqual([["4006381333931", "slow"]]);
    expect(verdicts.at(-1)).toBe("scan");
  });

  it("small jitter on a slow scanner is fine", () => {
    const keys: Array<[string, number]> = [..."012345678905"].map((c, i) => [c, i === 0 ? 0 : 50 + ((i * 7) % 15)]);
    keys.push(["Enter", 52]);
    expect(run("Enter", keys).scans.map((s) => s.raw)).toEqual(["012345678905"]);
  });

  it("irregular human typing at a similar average speed is not a scan", () => {
    const gaps = [0, 40, 160, 30, 110, 25, 150, 35, 90];
    const keys: Array<[string, number]> = [..."abcdefghi"].map((c, i) => [c, gaps[i] ?? 0]);
    keys.push(["Enter", 60]);
    expect(run("Enter", keys).scans).toEqual([]);
  });

  it("slow bursts shorter than slowMinLength are not scans", () => {
    expect(run("Enter", burst("ABCD", 60, "Enter", 0)).scans).toEqual([]);
  });

  it("slow recognition needs a suffix", () => {
    expect(run("none", burst("4006381333931", 60, "none", 0)).scans).toEqual([]);
  });

  it("can be switched off", () => {
    const scans: DetectedScan[] = [];
    const d = new ScanDetector({ ...DEFAULT_SCAN_CONFIG, slowMaxInterKeyMs: 0 }, (s) => scans.push(s));
    let t = 0;
    for (const c of "4006381333931") d.key(c, (t += 60));
    d.key("Enter", t + 60);
    expect(scans).toEqual([]);
  });
});

describe("GS1 separators and diagnostics", () => {
  it("keeps GS (FNC1) inside the scan", () => {
    const keys = burst("0109501101530003\x1d10AB12", 5, "Enter", 0);
    expect(run("Enter", keys).scans.map((s) => s.raw)).toEqual(["0109501101530003\x1d10AB12"]);
  });

  it("reports a stalled scan as interrupted, not as a wrong suffix, once its remainder ends", () => {
    const diags: string[] = [];
    const scans: string[] = [];
    const d = new ScanDetector({ ...DEFAULT_SCAN_CONFIG, suffix: "Enter" }, (s) => scans.push(s.raw), (x) => diags.push(x));
    let t = 0;
    for (const c of "501234") d.key(c, (t += 5));
    t += 200; // a stall over maxStallMs: the burst breaks
    for (const c of "5678900") d.key(c, (t += 5));
    expect(d.key("Enter", t + 5)).toBe("dropped");
    expect(scans).toEqual([]);
    // The break itself cannot tell a stall from a missing suffix; the dropped remainder can.
    expect(diags.at(-1)).toBe("interrupted_scan");
  });

  it("reports a fast burst that ended without the configured suffix", () => {
    const diags: string[] = [];
    const d = new ScanDetector({ ...DEFAULT_SCAN_CONFIG, suffix: "Enter" }, () => {}, (x) => diags.push(x));
    let t = 0;
    for (const c of "4006381333931") d.key(c, (t += 5));
    d.key("9", t + 400); // next keypress long after: the first burst never got its Enter
    expect(diags).toEqual(["missing_suffix"]);
  });
});
