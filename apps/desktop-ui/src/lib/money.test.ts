import { describe, expect, it } from "vitest";
import { formatMoney } from "./money";

describe("formatMoney", () => {
  it("formats minor units using the currency exponent", () => {
    expect(formatMoney("1999", { exponent: 2 })).toBe("19.99");
    expect(formatMoney("123456", { exponent: 2 })).toBe("1,234.56");
    expect(formatMoney("5", { exponent: 2 })).toBe("0.05");
    expect(formatMoney("0", { exponent: 2 })).toBe("0.00");
    expect(formatMoney("1000000", { exponent: 0 })).toBe("1,000,000");
    expect(formatMoney("1500", { exponent: 3 })).toBe("1.500");
  });

  it("handles negatives and leading zeros", () => {
    expect(formatMoney("-5", { exponent: 2 })).toBe("-0.05");
    expect(formatMoney("-123456", { exponent: 2 })).toBe("-1,234.56");
    expect(formatMoney("-0", { exponent: 2 })).toBe("0.00");
    expect(formatMoney("000123", { exponent: 2 })).toBe("1.23");
  });

  it("supports locale separators", () => {
    expect(formatMoney("123456", { exponent: 2, decimal: ",", group: "." })).toBe("1.234,56");
    expect(formatMoney("123456", { exponent: 2, group: null })).toBe("1234.56");
  });

  it("stays exact beyond 2^53 (no JS number conversion)", () => {
    // i64::MAX and i64::MIN in minor units.
    expect(formatMoney("9223372036854775807", { exponent: 2, group: null })).toBe("92233720368547758.07");
    expect(formatMoney("-9223372036854775808", { exponent: 2 })).toBe("-92,233,720,368,547,758.08");
  });

  it("matches the Rust formatter for the shared examples", () => {
    // Same cases as pos-domain money::tests::format_examples.
    expect(formatMoney("123456", { exponent: 2 })).toBe("1,234.56");
    expect(formatMoney("123456", { exponent: 2, decimal: ",", group: "." })).toBe("1.234,56");
    expect(formatMoney("1000000", { exponent: 0 })).toBe("1,000,000");
  });

  it("rejects non-integer input", () => {
    for (const bad of ["1.5", "", "abc", "1e5", " 1", "+1"]) {
      expect(() => formatMoney(bad, { exponent: 2 })).toThrow(TypeError);
    }
    expect(() => formatMoney("1", { exponent: 5 })).toThrow(RangeError);
  });
});
