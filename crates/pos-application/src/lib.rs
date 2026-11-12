//! Use-case services, `CommandContext`, permission enforcement, the `WriteSet` builder and
//! the port traits implemented by `pos-local-db` and device adapters
//! (docs/backlog/06-rust-interfaces.md §9).

pub mod approval;
pub mod audit;
pub mod audit_log;
pub mod auth;
pub mod cart;
pub mod cash;
pub mod catalog;
pub mod checkout;
pub mod config;
pub mod context;
pub mod export;
pub mod import;
pub mod mode;
pub mod printing;
pub mod queries;
pub mod receipts;
pub mod records;
pub mod reports;
pub mod returns;
pub mod sales;
pub mod scan;
pub mod settings;
pub mod setup;
pub mod staff;
pub mod stock;
pub mod sync;
pub mod write;

pub use write::{
    AppliedWriteSet, ChangeNotice, Counter, LocalStore, NumberSlot, OutboxDraft, WriteContext, WriteError,
    WriteOp, WriteSet, WriteSetBuilder, WriteSetError,
};
