//! The register's HTTP side of sync (E17-T08, ADR-013): [`CloudClient`] speaks the cloud API
//! (07 §3), [`DeviceSession`] is the sync transport (it keeps the 15-minute device token in
//! memory and rotates the refresh token, one refresh at a time), and the credential stores
//! keep the refresh token: the OS keychain in the app, memory in tests.
//!
//! Nothing here logs a token, a password or an email.

pub mod client;
pub mod credentials;
pub mod session;

pub use client::CloudClient;
pub use credentials::{KeychainCredentials, MemoryCredentials};
pub use session::DeviceSession;
