import type { CloudSalesReportDto } from "../../ipc/bindings";

/** HH:MM on this computer's clock; the date too when it is not today. */
export function clock(iso: string, now: Date = new Date()): string {
  const at = new Date(iso);
  const time = at.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });
  return at.toDateString() === now.toDateString()
    ? time
    : `${at.toLocaleDateString()} ${time}`;
}

function registers(n: number): string {
  return n === 1
    ? "1 register has unsynced data"
    : `${n} registers have unsynced data`;
}

/**
 * How far the consolidated figures can be trusted (E14-T05, PRD §15, §11.6):
 * "Fully synchronized"; "Last synced HH:MM — N registers have unsynced data"; or, when the
 * cloud is out of reach, the kept figures marked stale.
 */
export function cloudLabel(
  r: CloudSalesReportDto,
  now: Date = new Date(),
): { text: string; stale: boolean } {
  const partial =
    r.unsyncedRegisters === 0
      ? "Fully synchronized"
      : r.lastSyncedAt
        ? `Last synced ${clock(r.lastSyncedAt, now)} — ${registers(r.unsyncedRegisters)}`
        : registers(r.unsyncedRegisters);
  if (r.freshness === "stale") {
    return {
      text: `Stale: the cloud can't be reached. Figures from ${clock(r.fetchedAt, now)} (${partial})`,
      stale: true,
    };
  }
  return { text: partial, stale: false };
}
