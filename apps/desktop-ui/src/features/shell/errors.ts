import type { IpcError } from "../../ipc";

/**
 * What happened and what to do, per error code (07-contracts §2, PRD §5.1). `message` from
 * Rust is for logs only and is never shown.
 */
const COPY: Partial<Record<IpcError["code"], string>> = {
  SESSION_LOCKED: "You are signed out. Sign in to continue.",
  PERMISSION_DENIED: "Your role cannot do this. Ask a manager.",
  APPROVAL_REQUIRED: "This needs a manager's approval.",
  APPROVAL_INVALID: "That approval cannot be used: it was already used, has expired, or the approver cannot approve this.",
  VALIDATION_FAILED: "That value is not valid. Check it and try again.",
  CARD_DATA_REJECTED:
    "That looks like a card number, so it was not saved. Never type card numbers into Sale OS: take card payments on the card terminal.",
  CATALOG_NOT_FOUND: "That product no longer exists.",
  CATALOG_DUPLICATE_BARCODE: "Another product already uses that barcode.",
  CATALOG_DUPLICATE_SKU: "Another product already uses that SKU.",
  SCAN_INVALID: "That code could not be read. Scan it again.",
  CART_REVISION_MISMATCH: "The cart changed. Check it and try again.",
  PRICING_DISCOUNT_EXCEEDS: "The discount is larger than the amount.",
  RETURN_QTY_EXCEEDS: "That is more than is left to return on this sale (some may already have been returned). Check the quantities.",
  CASH_SESSION_REQUIRED: "The cash drawer is not open. Open it with a float first (F10).",
  CASH_SESSION_ALREADY_OPEN: "The drawer is already open on this till.",
  RETURN_REFUND_MISMATCH: "The refund must add up to exactly the amount being returned.",
  STORE_BUSY: "The till is busy. Try again.",
  STORE_IO: "Sale OS cannot read or write its database. Check disk space and that the data folder is on this computer.",
  STORE_DISK_FULL: "Cannot save — disk full. Nothing was saved and the cart is kept. Free space on this computer, then try again.",
  STORE_CORRUPT: "The local database looks damaged. Do not delete it; contact support to restore from a backup.",
  AUTH_CREDENTIALS_INVALID: "That email or password is not right for the cloud account.",
  AUTH_RATE_LIMITED: "Too many sign-in attempts. Wait a few minutes and try again.",
  AUTH_REQUIRED: "Sign in to the cloud account first.",
  SYNC_REPLAY_PENDING:
    "This till is getting its sales back from the cloud after a restore. The cart is kept; finish the sale once that is done (it needs the internet).",
  SYNC_OFFLINE: "The cloud could not be reached. Check the internet connection and try again; sales continue on this till.",
  SYNC_JOIN_NOT_ALLOWED: "This till already has live sales or products, so it cannot join another business. Create the cloud business from it instead.",
  SYNC_BUSINESS_ID_TAKEN: "Another cloud account already has this business. Sign in with that account, or contact support.",
  SYNC_REGISTER_IN_USE: "Another device is linked to this till in the cloud.",
  SYNC_DEVICE_REVOKED: "This register was disconnected by the owner. Sales continue on this till.",
  SYNC_UPGRADE_REQUIRED: "The cloud needs a newer version of Sale OS. Update the app; sales continue meanwhile.",
  STORE_SCHEMA_TOO_NEW: "This database was created by a newer version of Sale OS. Update the app to continue.",
};

export function describeError(e: IpcError): string {
  if (e.code === "AUTH_PIN_INVALID") {
    const left = e.details?.remainingAttempts;
    return left === undefined || left === null
      ? "Wrong PIN."
      : `Wrong PIN. ${left} ${left === 1 ? "try" : "tries"} left before a temporary lock.`;
  }
  return COPY[e.code] ?? "Something went wrong. If it happens again, note the reference and contact support.";
}
