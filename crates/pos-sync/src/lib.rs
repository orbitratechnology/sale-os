//! The sync wire contract shared by the desktop and the cloud (docs/backlog/07 §3–4,
//! E17-T01): the event envelope, every event's payload as a typed, versioned struct, the
//! push request and response, upcasters for old versions, and the schema compatibility
//! rules.
//!
//! The payload types are the *wire* format: plain ids, integers (minor units, milli-units,
//! ppm, epoch milliseconds) and enums, independent of the domain types that produce them. A
//! test replays a scripted day's outbox and requires every event to parse into these types
//! with unknown fields refused, so the two cannot drift apart unnoticed.

pub mod compat;
pub mod envelope;
pub mod events;
pub mod upcast;

pub use envelope::{EventEnvelope, EventResult, PushRequest, PushResponse, ResultStatus};
pub use events::{CATALOGUE, Payload, PayloadError, parse_payload};
