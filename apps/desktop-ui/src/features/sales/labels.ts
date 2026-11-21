import type { TenderKind } from "../../ipc/bindings";

/** Short names for tender kinds, for lists and detail. English: show them through `t(…)`. */
export const TENDER_LABEL_COPY: Record<TenderKind, string> = {
  cash: "Cash",
  card_external: "Card",
  bank_transfer_external: "Bank transfer",
  qr_external: "QR",
  other_external: "Other",
};

/** Return reasons, as the sale detail lists them. English: show them through `t(…)`. */
export const REASON_LABEL_COPY: Record<string, string> = {
  changed_mind: "Changed mind",
  defective: "Faulty",
  wrong_item: "Wrong item",
  damaged_in_transit: "Damaged",
  other: "Other",
};

/** The same tables under their earlier names (English; translate with `t(…)` at render). */
export const TENDER_LABEL = TENDER_LABEL_COPY;
export const REASON_LABEL = REASON_LABEL_COPY;
