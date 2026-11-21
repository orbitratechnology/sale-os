import type { SyncStatusDto } from "../../ipc/bindings";
import { currentLanguage, translate, type T } from "../../i18n";

/** The status bar's sync words (E17-T09, PRD §5.1). */
export function indicatorText(s: SyncStatusDto, t: T = (e, v) => translate(currentLanguage(), e, v)): string {
  const pending = s.pending;
  switch (s.indicator) {
    case "synced":
      return s.lastPushOkAt
        ? t("Synced (last at {time})", {
            time: new Date(s.lastPushOkAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" }),
          })
        : t("Synced");
    case "pending":
      return t("Sync pending ({n})", { n: pending });
    case "offline":
      return t("Offline ({n} pending)", { n: pending });
    case "attention": {
      const parts = [];
      if (s.conflicts > 0) parts.push(s.conflicts === 1 ? t("1 conflict") : t("{n} conflicts", { n: s.conflicts }));
      if (s.rejected > 0) parts.push(t("{n} refused", { n: s.rejected }));
      return parts.length ? t("Attention needed ({what})", { what: parts.join(", ") }) : t("Attention needed");
    }
    case "revoked":
      return t("Device revoked · saved on this computer");
    case "update_required":
      return t("Update required · saved on this computer");
    default:
      return t("Not linked · saved on this computer");
  }
}

/** A sale's mark (AT-13). */
export function saleSyncText(state: string | null | undefined, t: T = (e, v) => translate(currentLanguage(), e, v)): string {
  switch (state) {
    case "synced":
      return t("Synced");
    case "pending":
      return t("Saved on this device · Sync pending");
    case "attention":
      return t("Saved on this device · The cloud refused it: see Settings → Cloud");
    default:
      return t("Saved on this device · Not linked to the cloud");
  }
}

/** Why the cloud refused an event, in plain words (the code is shown beside it). */
export function refusalText(code: string | null | undefined, t: T = (e, v) => translate(currentLanguage(), e, v)): string {
  switch (code) {
    case "SNAPSHOT_MISMATCH":
      return t("The cloud checked the totals of this record and they did not add up, so it was not counted there. It is kept here unchanged.");
    case "EVENT_INVALID":
      return t("The cloud could not accept this record as it is. It is kept here; send the support file to support.");
    case "EVENT_VERSION_UNSUPPORTED":
      return t("The cloud does not understand this kind of record yet, so it holds up what came after it. It is sent again automatically once the cloud is updated.");
    case "EVENT_ID_REUSED":
      return t("This record has the same identity as a different one already in the cloud. Contact support.");
    default:
      return t("The cloud refused this record. It is kept here; send the support file to support.");
  }
}
