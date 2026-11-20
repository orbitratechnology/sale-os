import type { ReturnLineInputDto, ReturnReason } from "../../ipc/bindings";

/**
 * An exchange on its way from the return screen to the till: what comes back, and its value.
 * Nothing is recorded yet; the till records the return and the new sale together when the
 * cashier settles (`exchange_complete`), and cancelling simply forgets this.
 */
export interface ExchangeCredit {
  saleId: string;
  receiptNumber: string;
  lines: ReturnLineInputDto[];
  reason: ReturnReason;
  note: string | null;
  /** The returned value, as the return preview planned it. */
  creditMinor: string;
  /** What comes back, for the till's banner: "Tea ×1, Mug ×2". */
  items: string;
}
