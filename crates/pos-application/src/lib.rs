//! Use-case services, `CommandContext`, permission enforcement, the `WriteSet` builder and
//! the port traits implemented by `pos-local-db` and device adapters
//! (docs/backlog/06-rust-interfaces.md §9).

pub mod audit;
pub mod auth;
pub mod context;
pub mod queries;
pub mod records;
pub mod spike;
pub mod staff;
pub mod write;

pub use write::{
    AppliedWriteSet, ChangeNotice, Counter, LocalStore, NumberSlot, OutboxDraft, WriteContext, WriteError,
    WriteOp, WriteSet, WriteSetBuilder, WriteSetError,
};
