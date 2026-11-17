import { describe, expect, it } from "vitest";
import { formatMilli, percentToPpm, ppmToPercent, scaleDecimal, stepMilli } from "./decimal";

describe("scaleDecimal", () => {
  it.each([
    ["1.5", 3, "1500"],
    ["1,5", 3, "1500"],
    ["0.333", 3, "333"],
    ["12", 3, "12000"],
    [".5", 3, "500"],
    ["  2 ", 0, "2"],
    ["000", 3, "0"],
    ["9007199254740993", 2, "900719925474099300"],
  ])("%s at scale %d is %s", (input, scale, out) => {
    expect(scaleDecimal(input, scale)).toBe(out);
  });

  it.each(["", "-1", "1.2345", "1e3", "abc", "1.2.3", "+1"])("refuses %j", (input) => {
    expect(scaleDecimal(input, 3)).toBeNull();
  });
});

describe("percent and ppm", () => {
  it("converts both ways without floats", () => {
    expect(percentToPpm("12.5")).toBe(125_000);
    expect(percentToPpm("100")).toBe(1_000_000);
    expect(percentToPpm("0.0001")).toBe(1);
    expect(percentToPpm("100.01")).toBeNull();
    expect(percentToPpm("1234567")).toBeNull();
    expect(ppmToPercent(125_000)).toBe("12.5");
    expect(ppmToPercent(200_000)).toBe("20");
    expect(ppmToPercent(1)).toBe("0.0001");
  });
});

describe("milli-units", () => {
  it("formats", () => {
    expect(formatMilli("1500")).toBe("1.5");
    expect(formatMilli("3000")).toBe("3");
    expect(formatMilli("333")).toBe("0.333");
    expect(formatMilli("0")).toBe("0");
    expect(formatMilli("-250")).toBe("-0.25");
  });

  it("steps by whole units, never below one", () => {
    expect(stepMilli("1000", 1)).toBe("2000");
    expect(stepMilli("2000", -1)).toBe("1000");
    expect(stepMilli("1000", -1)).toBe("1000");
  });
});
