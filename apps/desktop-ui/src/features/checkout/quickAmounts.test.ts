import { describe, expect, it } from "vitest";
import { quickAmounts } from "./PaymentPanel";

describe("quick cash amounts", () => {
  it("offers the next whole unit and the next notes, in minor units", () => {
    // 13.45: next whole 14.00, then notes 15.00, 20.00 (twice, kept once) and 50.00.
    expect(quickAmounts(1345n, 2)).toEqual([1400n, 1500n, 2000n, 5000n]);
    expect(quickAmounts(2000n, 2)).toEqual([5000n]);
    expect(quickAmounts(0n, 2)).toEqual([]);
  });

  it("scales with the currency exponent (JPY has none)", () => {
    // ¥1234 is already whole; notes of 5, 10, 20 and 50 yen round it up.
    expect(quickAmounts(1234n, 0)).toEqual([1235n, 1240n, 1250n]);
  });
});
