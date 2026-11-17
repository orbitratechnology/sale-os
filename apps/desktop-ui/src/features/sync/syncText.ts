import type { SyncStatusDto } from "../../ipc/bindings";

/** The status bar's sync words (E17-T09, PRD §5.1). */
export function indicatorText(s: SyncStatusDto): string {
  const pending = s.pending;
  switch (s.indicator) {
    case "synced":
      return s.lastPushOkAt
        ? `Synced (last at ${new Date(s.lastPushOkAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" })})`
        : "Synced";
    case "pending":
      return `Sync pending (${pending})`;
    case "offline":
      return `Offline (${pending} pending)`;
    case "attention": {
      const parts = [];
      if (s.conflicts > 0) parts.push(`${s.conflicts} conflict${s.conflicts === 1 ? "" : "s"}`);
      if (s.rejected > 0) parts.push(`${s.rejected} refused`);
      return `Attention needed${parts.length ? ` (${parts.join(", ")})` : ""}`;
    }
    case "revoked":
      return "Device revoked · saved on this computer";
    case "update_required":
      return "Update required · saved on this computer";
    default:
      return "Not linked · saved on this computer";
  }
}

/** A sale's mark (AT-13). */
export function saleSyncText(state: string | null | undefined): string {
  switch (state) {
    case "synced":
      return "Synced";
    case "pending":
      return "Saved on this device · Sync pending";
    case "attention":
      return "Saved on this device · The cloud refused it: see Settings → Cloud";
    default:
      return "Saved on this device · Not linked to the cloud";
  }
}

/** Why the cloud refused an event, in plain words (the code is shown beside it). */
export function refusalText(code: string | null | undefined): string {
  switch (code) {
    case "SNAPSHOT_MISMATCH":
      return "The cloud checked the totals of this record and they did not add up, so it was not counted there. It is kept here unchanged.";
    case "EVENT_INVALID":
      return "The cloud could not accept this record as it is. It is kept here; send the support file to support.";
    case "EVENT_VERSION_UNSUPPORTED":
      return "The cloud does not understand this kind of record yet, so it holds up what came after it. It is sent again automatically once the cloud is updated.";
    case "EVENT_ID_REUSED":
      return "This record has the same identity as a different one already in the cloud. Contact support.";
    default:
      return "The cloud refused this record. It is kept here; send the support file to support.";
  }
}
