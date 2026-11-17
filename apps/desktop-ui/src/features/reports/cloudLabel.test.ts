import { describe, expect, it } from "vitest";
import type { CloudSalesReportDto } from "../../ipc/bindings";
import { cloudLabel } from "./cloudLabel";

const now = new Date(2026, 10, 17, 10, 0);
const at = (h: number, m: number, day = 17) =>
  new Date(2026, 10, day, h, m).toISOString();

function report(over: Partial<CloudSalesReportDto>): CloudSalesReportDto {
  const zero = {
    key: "total",
    label: "Total",
    salesCount: 0,
    salesMinor: "0",
    taxMinor: "0",
    discountMinor: "0",
    returnsCount: 0,
    refundsMinor: "0",
    netMinor: "0",
  };
  return {
    groupBy: "day",
    from: "2026-11-17",
    to: "2026-11-17",
    currencyExponent: 2,
    rows: [],
    total: zero,
    freshness: "fully_synchronized",
    unsyncedRegisters: 0,
    lastSyncedAt: null,
    staleReason: null,
    asOf: at(10, 0),
    fetchedAt: at(10, 0),
    registers: [],
    ...over,
  };
}

describe("cloudLabel", () => {
  it("says fully synchronized when every register is current", () => {
    expect(cloudLabel(report({}), now)).toEqual({
      text: "Fully synchronized",
      stale: false,
    });
  });

  it("says when it last synced and how many registers are behind", () => {
    expect(
      cloudLabel(
        report({
          freshness: "partial",
          unsyncedRegisters: 2,
          lastSyncedAt: at(9, 5),
        }),
        now,
      ).text,
    ).toBe("Last synced 09:05 — 2 registers have unsynced data");
    expect(
      cloudLabel(
        report({
          freshness: "partial",
          unsyncedRegisters: 1,
          lastSyncedAt: at(9, 5),
        }),
        now,
      ).text,
    ).toBe("Last synced 09:05 — 1 register has unsynced data");
    // A register that never synced: no time to give.
    expect(
      cloudLabel(report({ freshness: "partial", unsyncedRegisters: 1 }), now)
        .text,
    ).toBe("1 register has unsynced data");
  });

  it("marks kept figures stale, with when they were fetched", () => {
    const l = cloudLabel(
      report({
        freshness: "stale",
        staleReason: "SYNC_OFFLINE",
        fetchedAt: at(8, 30),
      }),
      now,
    );
    expect(l.stale).toBe(true);
    expect(l.text).toBe(
      "Stale: the cloud can't be reached. Figures from 08:30 (Fully synchronized)",
    );
    const older = cloudLabel(
      report({ freshness: "stale", fetchedAt: at(8, 30, 16) }),
      now,
    );
    expect(older.text).toContain(
      `${new Date(2026, 10, 16).toLocaleDateString()} 08:30`,
    );
  });
});
