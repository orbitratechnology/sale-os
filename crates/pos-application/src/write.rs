//! The single write path (ADR-008).
//!
//! Every business mutation goes through one shape:
//! use case → domain decisions → [`WriteSet`] → [`LocalStore::apply`], which runs one
//! `BEGIN IMMEDIATE` transaction. A `WriteSet` can only be obtained from
//! [`WriteSetBuilder::build`], which rejects sets that break the invariants:
//!
//! - at least one audit entry;
//! - every syncable record has an outbox event for the same aggregate, carrying the right
//!   `base_version` (ADR-009, ADR-011);
//! - record versions are bumped by exactly one;
//! - event IDs and number slots are unique, and audit links point at events in the set.
//!
//! The store fills in envelope fields (business/location/register, `local_sequence`,
//! `occurred_at`) and allocates register-local numbers inside the transaction.

use crate::audit::AuditEntry;
use crate::records::{AggregateRef, BusinessRecord, LocationRecord, RegisterRecord, SettingRecord};
use pos_domain::{
    BusinessId, ErrorCode, EventId, HasErrorCode, IdGenerator, LocationId, RegisterId, Timestamp, UserId,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Who, where and when, for every row in the set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteContext {
    pub business_id: BusinessId,
    pub location_id: LocationId,
    pub register_id: RegisterId,
    /// `None` only for system actions (setup before the owner exists, sync, migrations).
    pub actor: Option<UserId>,
    pub occurred_at: Timestamp,
}

/// Register-local human-readable number sequences (docs/backlog/04, `register_counters`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Counter {
    Receipt,
    Return,
    InventoryDocument,
}

impl Counter {
    pub const fn column_name(self) -> &'static str {
        match self {
            Counter::Receipt => "receipt_seq",
            Counter::Return => "return_seq",
            Counter::InventoryDocument => "inventory_doc_seq",
        }
    }
}

/// Names a number allocated inside the transaction, so records in the same set can use it
/// and the caller can read it from [`AppliedWriteSet::numbers`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NumberSlot(pub &'static str);

/// A record write. Upserts carry the version the caller based its edit on
/// (`None` = the record must not exist yet), checked inside the transaction (ADR-011).
#[derive(Clone, Debug, PartialEq)]
pub enum WriteOp {
    PutBusiness {
        record: BusinessRecord,
        expected_version: Option<u64>,
    },
    PutLocation {
        record: LocationRecord,
        expected_version: Option<u64>,
    },
    PutRegister {
        record: RegisterRecord,
        expected_version: Option<u64>,
    },
    PutSetting {
        record: SettingRecord,
        expected_version: Option<u64>,
    },
    AllocateNumber {
        counter: Counter,
        slot: NumberSlot,
    },
}

impl WriteOp {
    /// The syncable aggregate this op writes, with the version it produces and expects.
    fn syncable(&self) -> Option<(AggregateRef, u64, Option<u64>)> {
        match self {
            WriteOp::PutBusiness {
                record,
                expected_version,
            } => Some((record.aggregate(), record.version, *expected_version)),
            WriteOp::PutLocation {
                record,
                expected_version,
            } => Some((record.aggregate(), record.version, *expected_version)),
            WriteOp::PutRegister {
                record,
                expected_version,
            } => Some((record.aggregate(), record.version, *expected_version)),
            WriteOp::PutSetting {
                record,
                expected_version,
            } => Some((record.aggregate(), record.version, *expected_version)),
            WriteOp::AllocateNumber { .. } => None,
        }
    }
}

/// An outbox event before the store assigns its envelope (docs/backlog/07 §4).
#[derive(Clone, Debug, PartialEq)]
pub struct OutboxDraft {
    pub event_id: EventId,
    pub event_type: &'static str,
    pub schema_version: u32,
    pub aggregate: AggregateRef,
    /// Master data only (ADR-011): the version the edit was based on; 0 for a create.
    pub base_version: Option<u64>,
    pub payload: serde_json::Value,
}

impl OutboxDraft {
    pub fn new(
        ids: &dyn IdGenerator,
        event_type: &'static str,
        schema_version: u32,
        aggregate: AggregateRef,
        base_version: Option<u64>,
        payload: &impl Serialize,
    ) -> Result<Self, WriteSetError> {
        let payload = serde_json::to_value(payload).map_err(|e| WriteSetError::Payload(e.to_string()))?;
        Ok(Self {
            event_id: EventId::generate(ids),
            event_type,
            schema_version,
            aggregate,
            base_version,
            payload,
        })
    }

    fn master(
        ids: &dyn IdGenerator,
        event_type: &'static str,
        aggregate: AggregateRef,
        version: u64,
        payload: &impl Serialize,
    ) -> Result<Self, WriteSetError> {
        Self::new(
            ids,
            event_type,
            1,
            aggregate,
            Some(version.saturating_sub(1)),
            payload,
        )
    }

    pub fn business_updated(ids: &dyn IdGenerator, r: &BusinessRecord) -> Result<Self, WriteSetError> {
        Self::master(ids, "business.updated", r.aggregate(), r.version, r)
    }
    pub fn location_upserted(ids: &dyn IdGenerator, r: &LocationRecord) -> Result<Self, WriteSetError> {
        Self::master(ids, "location.upserted", r.aggregate(), r.version, r)
    }
    pub fn register_upserted(ids: &dyn IdGenerator, r: &RegisterRecord) -> Result<Self, WriteSetError> {
        Self::master(ids, "register.upserted", r.aggregate(), r.version, r)
    }
    pub fn setting_updated(ids: &dyn IdGenerator, r: &SettingRecord) -> Result<Self, WriteSetError> {
        Self::master(ids, "setting.updated", r.aggregate(), r.version, r)
    }
}

/// Why a set of writes is not acceptable. These are programming errors in a use case, not
/// operator errors, so they map to `INTERNAL`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteSetError {
    #[error("a write set must contain at least one audit entry")]
    MissingAudit,
    #[error("syncable record {0:?} has no outbox event in the same write set")]
    MissingOutbox(AggregateRef),
    #[error("outbox event for {aggregate:?} has base_version {actual:?}, expected {expected:?}")]
    BaseVersionMismatch {
        aggregate: AggregateRef,
        expected: Option<u64>,
        actual: Option<u64>,
    },
    #[error("record {aggregate:?} must be written at version {expected}, got {actual}")]
    VersionNotIncremented {
        aggregate: AggregateRef,
        expected: u64,
        actual: u64,
    },
    #[error("duplicate event id {0}")]
    DuplicateEventId(EventId),
    #[error("duplicate number slot {0:?}")]
    DuplicateSlot(NumberSlot),
    #[error("audit entry links to event {0}, which is not in this write set")]
    DanglingAuditLink(EventId),
    #[error("write set touches business {found}, but the context is business {expected}")]
    ForeignBusiness { expected: BusinessId, found: BusinessId },
    #[error("payload could not be serialised: {0}")]
    Payload(String),
}

impl HasErrorCode for WriteSetError {
    fn error_code(&self) -> ErrorCode {
        ErrorCode::Internal
    }
}

/// A validated set of writes. Obtainable only through [`WriteSetBuilder::build`].
#[derive(Clone, Debug, PartialEq)]
pub struct WriteSet {
    context: WriteContext,
    ops: Vec<WriteOp>,
    outbox: Vec<OutboxDraft>,
    audit: Vec<AuditEntry>,
}

impl WriteSet {
    pub fn builder(context: WriteContext) -> WriteSetBuilder {
        WriteSetBuilder {
            context,
            ops: Vec::new(),
            outbox: Vec::new(),
            audit: Vec::new(),
        }
    }
    pub fn context(&self) -> &WriteContext {
        &self.context
    }
    pub fn ops(&self) -> &[WriteOp] {
        &self.ops
    }
    pub fn outbox(&self) -> &[OutboxDraft] {
        &self.outbox
    }
    pub fn audit(&self) -> &[AuditEntry] {
        &self.audit
    }
}

#[derive(Clone, Debug)]
pub struct WriteSetBuilder {
    context: WriteContext,
    ops: Vec<WriteOp>,
    outbox: Vec<OutboxDraft>,
    audit: Vec<AuditEntry>,
}

impl WriteSetBuilder {
    pub fn put_business(mut self, record: BusinessRecord, expected_version: Option<u64>) -> Self {
        self.ops.push(WriteOp::PutBusiness {
            record,
            expected_version,
        });
        self
    }
    pub fn put_location(mut self, record: LocationRecord, expected_version: Option<u64>) -> Self {
        self.ops.push(WriteOp::PutLocation {
            record,
            expected_version,
        });
        self
    }
    pub fn put_register(mut self, record: RegisterRecord, expected_version: Option<u64>) -> Self {
        self.ops.push(WriteOp::PutRegister {
            record,
            expected_version,
        });
        self
    }
    pub fn put_setting(mut self, record: SettingRecord, expected_version: Option<u64>) -> Self {
        self.ops.push(WriteOp::PutSetting {
            record,
            expected_version,
        });
        self
    }
    pub fn allocate_number(mut self, counter: Counter, slot: NumberSlot) -> Self {
        self.ops.push(WriteOp::AllocateNumber { counter, slot });
        self
    }
    pub fn outbox(mut self, draft: OutboxDraft) -> Self {
        self.outbox.push(draft);
        self
    }
    pub fn audit(mut self, entry: AuditEntry) -> Self {
        self.audit.push(entry);
        self
    }

    /// Validates the ADR-008 invariants and returns the immutable set.
    pub fn build(self) -> Result<WriteSet, WriteSetError> {
        if self.audit.is_empty() {
            return Err(WriteSetError::MissingAudit);
        }

        let mut event_ids = BTreeSet::new();
        for e in &self.outbox {
            if !event_ids.insert(e.event_id) {
                return Err(WriteSetError::DuplicateEventId(e.event_id));
            }
        }
        for a in &self.audit {
            if let Some(link) = a.outbox_event_id
                && !event_ids.contains(&link)
            {
                return Err(WriteSetError::DanglingAuditLink(link));
            }
        }

        let mut slots = BTreeSet::new();
        let by_aggregate: BTreeMap<&AggregateRef, &OutboxDraft> =
            self.outbox.iter().map(|e| (&e.aggregate, e)).collect();
        for op in &self.ops {
            if let WriteOp::AllocateNumber { slot, .. } = op
                && !slots.insert(*slot)
            {
                return Err(WriteSetError::DuplicateSlot(*slot));
            }
            self.check_business(op)?;
            let Some((aggregate, version, expected)) = op.syncable() else {
                continue;
            };
            let required = expected.unwrap_or(0) + 1;
            if version != required {
                return Err(WriteSetError::VersionNotIncremented {
                    aggregate,
                    expected: required,
                    actual: version,
                });
            }
            let Some(event) = by_aggregate.get(&aggregate) else {
                return Err(WriteSetError::MissingOutbox(aggregate));
            };
            let base = Some(expected.unwrap_or(0));
            if event.base_version != base {
                return Err(WriteSetError::BaseVersionMismatch {
                    aggregate,
                    expected: base,
                    actual: event.base_version,
                });
            }
        }

        Ok(WriteSet {
            context: self.context,
            ops: self.ops,
            outbox: self.outbox,
            audit: self.audit,
        })
    }

    fn check_business(&self, op: &WriteOp) -> Result<(), WriteSetError> {
        let found = match op {
            WriteOp::PutBusiness { record, .. } => record.business_id,
            WriteOp::PutLocation { record, .. } => record.business_id,
            WriteOp::PutRegister { record, .. } => record.business_id,
            WriteOp::PutSetting { record, .. } => record.business_id,
            WriteOp::AllocateNumber { .. } => return Ok(()),
        };
        if found != self.context.business_id {
            return Err(WriteSetError::ForeignBusiness {
                expected: self.context.business_id,
                found,
            });
        }
        Ok(())
    }
}

/// What `apply` allocated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppliedWriteSet {
    /// Register-local numbers allocated for each requested slot.
    pub numbers: BTreeMap<NumberSlot, u64>,
    /// `(event_id, local_sequence)` for every outbox event, in insertion order.
    pub outbox: Vec<(EventId, u64)>,
}

/// Why `apply` failed. Nothing was written in every case.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    #[error("{aggregate:?} is at version {actual:?}, the edit was based on {expected:?}")]
    VersionConflict {
        aggregate: AggregateRef,
        expected: Option<u64>,
        actual: Option<u64>,
    },
    #[error("unique constraint violated: {constraint}")]
    UniqueViolation { constraint: String },
    #[error("database is busy")]
    Busy,
    #[error("database integrity problem: {0}")]
    Corrupt(String),
    #[error("database error: {0}")]
    Io(String),
    #[error("fault injected before statement {statement}")]
    FaultInjected { statement: usize },
}

impl HasErrorCode for WriteError {
    fn error_code(&self) -> ErrorCode {
        match self {
            WriteError::VersionConflict { .. } => ErrorCode::ConflictVersion,
            WriteError::UniqueViolation { .. } => ErrorCode::ValidationFailed,
            WriteError::Busy => ErrorCode::StoreBusy,
            WriteError::Corrupt(_) => ErrorCode::StoreCorrupt,
            WriteError::Io(_) | WriteError::FaultInjected { .. } => ErrorCode::StoreIo,
        }
    }
}

/// Published after every successful commit (consumed by the sync worker and the UI).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeNotice {
    pub aggregates: Vec<AggregateRef>,
    pub outbox_appended: bool,
}

/// The write port (implemented by `pos-local-db`).
#[async_trait::async_trait]
pub trait LocalStore: Send + Sync {
    /// Applies the whole set in one transaction, or nothing. The caller must not report
    /// success to the operator before this returns `Ok`.
    async fn apply(&self, write_set: WriteSet) -> Result<AppliedWriteSet, WriteError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{self, AuditEntry};
    use pos_domain::{Currency, LocationId, RegisterId};
    use pos_testkit::SeqIdGenerator;

    fn ctx(ids: &SeqIdGenerator) -> WriteContext {
        WriteContext {
            business_id: BusinessId::generate(ids),
            location_id: LocationId::generate(ids),
            register_id: RegisterId::generate(ids),
            actor: None,
            occurred_at: Timestamp::from_millis(1_000),
        }
    }

    fn setting(ctx: &WriteContext, key: &str, version: u64) -> SettingRecord {
        SettingRecord {
            business_id: ctx.business_id,
            key: key.into(),
            value: serde_json::json!(true),
            version,
            updated_at: ctx.occurred_at,
        }
    }

    fn audit(ids: &SeqIdGenerator) -> AuditEntry {
        AuditEntry::new(ids, audit::SETTINGS_UPDATED, "setting", "k")
    }

    #[test]
    fn audit_is_mandatory() {
        let ids = SeqIdGenerator::default();
        let err = WriteSet::builder(ctx(&ids)).build().unwrap_err();
        assert_eq!(err, WriteSetError::MissingAudit);
    }

    #[test]
    fn syncable_record_requires_matching_outbox_event() {
        let ids = SeqIdGenerator::default();
        let c = ctx(&ids);
        let s = setting(&c, "inventory.allow_negative", 1);
        let err = WriteSet::builder(c.clone())
            .put_setting(s.clone(), None)
            .audit(audit(&ids))
            .build()
            .unwrap_err();
        assert_eq!(err, WriteSetError::MissingOutbox(s.aggregate()));

        // An event for a different aggregate does not count.
        let other = OutboxDraft::setting_updated(&ids, &setting(&c, "other", 1)).unwrap();
        let err = WriteSet::builder(c.clone())
            .put_setting(s.clone(), None)
            .outbox(other)
            .audit(audit(&ids))
            .build()
            .unwrap_err();
        assert!(matches!(err, WriteSetError::MissingOutbox(_)));

        let ok = WriteSet::builder(c)
            .outbox(OutboxDraft::setting_updated(&ids, &s).unwrap())
            .put_setting(s, None)
            .audit(audit(&ids))
            .build();
        assert!(ok.is_ok());
    }

    #[test]
    fn versions_must_increment_by_one_and_match_base_version() {
        let ids = SeqIdGenerator::default();
        let c = ctx(&ids);
        // Editing version 3 must write version 4.
        let s = setting(&c, "k", 3);
        let err = WriteSet::builder(c.clone())
            .outbox(OutboxDraft::setting_updated(&ids, &s).unwrap())
            .put_setting(s, Some(3))
            .audit(audit(&ids))
            .build()
            .unwrap_err();
        assert!(matches!(
            err,
            WriteSetError::VersionNotIncremented {
                expected: 4,
                actual: 3,
                ..
            }
        ));

        let s = setting(&c, "k", 4);
        let mut event = OutboxDraft::setting_updated(&ids, &s).unwrap();
        event.base_version = Some(2);
        let err = WriteSet::builder(c.clone())
            .outbox(event)
            .put_setting(s.clone(), Some(3))
            .audit(audit(&ids))
            .build();
        assert!(matches!(err, Err(WriteSetError::BaseVersionMismatch { .. })));

        let ok = WriteSet::builder(c)
            .outbox(OutboxDraft::setting_updated(&ids, &s).unwrap())
            .put_setting(s, Some(3))
            .audit(audit(&ids))
            .build();
        assert!(ok.is_ok());
    }

    #[test]
    fn duplicate_ids_slots_and_dangling_links_are_rejected() {
        let ids = SeqIdGenerator::default();
        let c = ctx(&ids);
        let s = setting(&c, "k", 1);
        let e = OutboxDraft::setting_updated(&ids, &s).unwrap();
        let err = WriteSet::builder(c.clone())
            .outbox(e.clone())
            .outbox(e)
            .audit(audit(&ids))
            .build()
            .unwrap_err();
        assert!(matches!(err, WriteSetError::DuplicateEventId(_)));

        let err = WriteSet::builder(c.clone())
            .allocate_number(Counter::Receipt, NumberSlot("receipt"))
            .allocate_number(Counter::Receipt, NumberSlot("receipt"))
            .audit(audit(&ids))
            .build()
            .unwrap_err();
        assert_eq!(err, WriteSetError::DuplicateSlot(NumberSlot("receipt")));

        let missing = EventId::generate(&ids);
        let err = WriteSet::builder(c)
            .audit(audit(&ids).linked_to(missing))
            .build()
            .unwrap_err();
        assert_eq!(err, WriteSetError::DanglingAuditLink(missing));
    }

    #[test]
    fn records_from_another_business_are_rejected() {
        let ids = SeqIdGenerator::default();
        let c = ctx(&ids);
        let mut s = setting(&c, "k", 1);
        s.business_id = BusinessId::generate(&ids);
        let err = WriteSet::builder(c)
            .outbox(OutboxDraft::setting_updated(&ids, &s).unwrap())
            .put_setting(s, None)
            .audit(audit(&ids))
            .build()
            .unwrap_err();
        assert!(matches!(err, WriteSetError::ForeignBusiness { .. }));
    }

    #[test]
    fn master_events_use_catalogue_names_and_base_versions() {
        let ids = SeqIdGenerator::default();
        let c = ctx(&ids);
        let r = BusinessRecord {
            business_id: c.business_id,
            name: "Shop".into(),
            legal_name: None,
            tax_registration_no: None,
            address: serde_json::json!({}),
            phone: None,
            email: None,
            logo_path: None,
            country_code: "GB".into(),
            timezone: "Europe/London".into(),
            currency: Currency::from_iso("GBP").unwrap(),
            prices_include_tax: true,
            version: 1,
            created_at: c.occurred_at,
            updated_at: c.occurred_at,
        };
        let e = OutboxDraft::business_updated(&ids, &r).unwrap();
        assert_eq!(
            (e.event_type, e.schema_version, e.base_version),
            ("business.updated", 1, Some(0))
        );
        assert_eq!(e.payload["name"], "Shop");
        assert_eq!(e.payload["currency"]["code"], "GBP");
    }
}
