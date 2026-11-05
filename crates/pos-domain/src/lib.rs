//! Pure retail POS domain (see `docs/backlog/02-architecture-and-modules.md`).
//!
//! Rules for this crate:
//! - no IO, no async, no database or UI dependencies;
//! - no binary floating point for money, quantities or rates (ADR-001..003);
//! - every fallible operation returns an error that maps to a catalogued [`ErrorCode`].
#![deny(clippy::float_arithmetic)]

pub mod access;
pub mod catalog;
pub mod error;
pub mod ids;
pub mod money;
pub mod numbering;
pub mod pricing;
pub mod quantity;
pub mod rate;
pub mod rounding;
pub mod secret;
pub mod staff;
pub mod time;

pub use access::{Permission, Role, role_has};
pub use error::{ArithmeticError, ErrorCode, HasErrorCode};
pub use ids::*;
pub use money::{Currency, CurrencyCode, Money, NumberFormat};
pub use numbering::receipt_number;
pub use quantity::Quantity;
pub use rate::RatePpm;
pub use rounding::{RoundingMode, allocate, round_to_minor};
pub use secret::Secret;
pub use time::{BusinessDate, Clock, IdGenerator, SystemClock, Timestamp, UuidV7Generator, business_date};
