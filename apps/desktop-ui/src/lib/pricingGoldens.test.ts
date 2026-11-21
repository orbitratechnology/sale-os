import { describe, expect, it } from "vitest";
import { formatMoney } from "./money";

/**
 * E08-T03: the UI side of the pricing goldens (`tests/fixtures/pricing/*.json`).
 *
 * Rust computes every amount (ADR-018); the Rust runner checks those. The UI only formats
 * what it receives, so this test feeds the same expected minor-unit strings to the UI's
 * formatter and checks it prints the fixture's `display` strings, which the Rust formatter
 * prints too. No amount is computed here.
 */

interface Amounts {
  subtotal?: string;
  lineDiscountTotal?: string;
  orderDiscountTotal?: string;
  taxTotal?: string;
  total?: string;
  lines?: { total: string }[];
  settlement?: Record<string, string>;
  returns?: { refund: string }[];
  display: Record<string, string | string[]>;
}

interface Scenario {
  id: string;
  currency: { code: string; exponent: number };
  expectError?: string;
  expect?: Amounts;
}

interface FixtureFile {
  rule: string;
  scenarios: Scenario[];
}

const files = import.meta.glob<FixtureFile>("../../../../tests/fixtures/pricing/*.json", {
  eager: true,
  import: "default",
});

const scenarios = Object.values(files).flatMap((f) => f.scenarios);

describe("pricing goldens: the UI prints what Rust computed", () => {
  it("finds the shared fixtures", () => {
    expect(Object.keys(files).length).toBeGreaterThan(0);
    expect(scenarios.length).toBeGreaterThanOrEqual(40);
  });

  it.each(scenarios.filter((s) => s.expect).map((s) => [s.id, s] as const))("%s", (_id, s) => {
    const exp = s.expect as Amounts;
    const fmt = (minor: string) => formatMoney(minor, { exponent: s.currency.exponent });
    const shown: Record<string, string | string[]> = {};
    for (const k of ["subtotal", "lineDiscountTotal", "orderDiscountTotal", "taxTotal", "total"] as const) {
      const v = exp[k];
      if (v !== undefined) shown[k] = fmt(v);
    }
    if (exp.lines) shown.lineTotals = exp.lines.map((l) => fmt(l.total));
    if (exp.settlement) for (const [k, v] of Object.entries(exp.settlement)) shown[k] = fmt(v);
    if (exp.returns) shown.refunds = exp.returns.map((r) => fmt(r.refund));
    expect(shown).toEqual(exp.display);
  });
});
