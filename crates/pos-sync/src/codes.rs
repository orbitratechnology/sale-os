//! Codes carried in an [`EventResult`](crate::EventResult)'s `code` for `rejected`,
//! `transient` and `conflict` results. The client's repair queue and conflict screens key off
//! these strings, so they are part of the contract: add, never rename.

/// rejected: the event id was already used for a different event. Integrity alert.
pub const EVENT_ID_REUSED: &str = "EVENT_ID_REUSED";
/// rejected: an event type, or a schema version, this server does not know. The client keeps
/// the event and asks for an update (07 §4).
pub const EVENT_VERSION_UNSUPPORTED: &str = "EVENT_VERSION_UNSUPPORTED";
/// rejected: the payload does not match its schema, or fails the applier's checks.
pub const EVENT_INVALID: &str = "EVENT_INVALID";
/// rejected: a fact's totals do not add up (`verify_snapshot`).
pub const SNAPSHOT_MISMATCH: &str = "SNAPSHOT_MISMATCH";
/// transient: a return arrived before its original sale.
pub const ORIGINAL_SALE_PENDING: &str = "ORIGINAL_SALE_PENDING";
/// transient: the server knows the event type but cannot apply it yet (rolling deploys).
pub const APPLIER_UNAVAILABLE: &str = "APPLIER_UNAVAILABLE";
/// transient: a server-side failure; retry with backoff.
pub const SERVER_BUSY: &str = "SERVER_BUSY";
/// conflict: the edit was based on an older version.
pub const VERSION: &str = "VERSION";
/// conflict: the barcode belongs to another product.
pub const BARCODE_TAKEN: &str = "BARCODE_TAKEN";
/// conflict: the SKU belongs to another product.
pub const SKU_TAKEN: &str = "SKU_TAKEN";
