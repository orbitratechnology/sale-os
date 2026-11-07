import type { TenderKind } from "../../ipc/bindings";

/** Short names for tender kinds, for lists and detail. */
export const TENDER_LABEL: Record<TenderKind, string> = {
  cash: "Cash",
  card_external: "Card",
  bank_transfer_external: "Bank transfer",
  qr_external: "QR",
  other_external: "Other",
};

export const REASON_LABEL: Record<string, string> = {
  changed_mind: "Changed mind",
  defective: "Faulty",
  wrong_item: "Wrong item",
  damaged_in_transit: "Damaged",
  other: "Other",
};
