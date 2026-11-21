import type { CloudSalesReportDto } from "../../ipc/bindings";
import { currentLanguage, translate, type T } from "../../i18n";

const shown: T = (english, vars) => translate(currentLanguage(), english, vars);

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

function registers(n: number, t: T): string {
  return n === 1
    ? t("1 register has unsynced data")
    : t("{n} registers have unsynced data", { n });
}

/**
 * How far the consolidated figures can be trusted (E14-T05, PRD §15, §11.6):
 * "Fully synchronized"; "Last synced HH:MM — N registers have unsynced data"; or, when the
 * cloud is out of reach, the kept figures marked stale.
 */
export function cloudLabel(
  r: CloudSalesReportDto,
  now: Date = new Date(),
  t: T = shown,
): { text: string; stale: boolean } {
  const partial =
    r.unsyncedRegisters === 0
      ? t("Fully synchronized")
      : r.lastSyncedAt
        ? t("Last synced {time} — {registers}", {
            time: clock(r.lastSyncedAt, now),
            registers: registers(r.unsyncedRegisters, t),
          })
        : registers(r.unsyncedRegisters, t);
  if (r.freshness === "stale") {
    return {
      text: t("Stale: the cloud can't be reached. Figures from {time} ({partial})", {
        time: clock(r.fetchedAt, now),
        partial,
      }),
      stale: true,
    };
  }
  return { text: partial, stale: false };
}
