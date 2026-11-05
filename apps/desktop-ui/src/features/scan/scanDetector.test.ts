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

  it("ignores bursts shorter than minLength", () => {
    expect(run("Enter", burst("12", 5, "Enter", 0)).scans).toEqual([]);
  });

  it("a long pause in the middle breaks the burst; the remainder alone is too short", () => {
    const keys: Array<[string, number]> = [["1", 0], ["2", 5], ["3", 5], ["4", 200], ["5", 5], ["Enter", 5]];
    expect(run("Enter", keys).scans).toEqual([]);
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
    expect(scans).toEqual([{ raw: "4006381333931", terminatedBy: "idle", durationMs: 60 }]);
  });
});
