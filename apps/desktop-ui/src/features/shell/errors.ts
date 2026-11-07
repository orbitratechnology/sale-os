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
  CATALOG_NOT_FOUND: "That product no longer exists.",
  CATALOG_DUPLICATE_BARCODE: "Another product already uses that barcode.",
  CATALOG_DUPLICATE_SKU: "Another product already uses that SKU.",
  SCAN_INVALID: "That code could not be read. Scan it again.",
  CART_REVISION_MISMATCH: "The cart changed. Check it and try again.",
  PRICING_DISCOUNT_EXCEEDS: "The discount is larger than the amount.",
  RETURN_QTY_EXCEEDS: "That is more than is left to return on this sale (some may already have been returned). Check the quantities.",
  RETURN_REFUND_MISMATCH: "The refund must add up to exactly the amount being returned.",
  STORE_BUSY: "The till is busy. Try again.",
  STORE_IO: "Sale OS cannot read or write its database. Check disk space and that the data folder is on this computer.",
  STORE_CORRUPT: "The local database looks damaged. Do not delete it; contact support to restore from a backup.",
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
