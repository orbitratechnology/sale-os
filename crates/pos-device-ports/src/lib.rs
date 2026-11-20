//! Device port traits (docs/backlog/06-rust-interfaces.md §10, ADR-023): barcode input,
//! receipt output, cash drawer, payment. Implementations live in `pos-device-virtual`
//! (simulators) and `pos-device-hw` (real transports: network, serial/COM, USB, OS spooler).
//!
//! The ports describe *what* a device does, never *how* it is connected: the application
//! layer works the same with a network printer, a USB printer behind the Windows spooler or
//! the virtual printer.

pub mod drawer;
pub mod payment;
pub mod printer;
pub mod scanner;

pub use drawer::{CashDrawerPort, DrawerPin};
pub use printer::{
    CommandDialect, CutSupport, PaperLevel, PrintFailure, PrintResult, PrinterCapabilities, PrinterStatus,
    ReceiptOutputPort,
};
pub use scanner::{BarcodeInputPort, ScanEvent, ScanSource};

/// Why a device operation did not happen. Carries a short, operator-readable reason.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    /// No device of this kind is set up on this till.
    #[error("no device is set up")]
    NotConfigured,
    /// The device could not be reached (unplugged, switched off, wrong address or port).
    #[error("the device is not connected: {0}")]
    Disconnected(String),
    /// The device did not answer in time.
    #[error("the device did not answer in time")]
    Timeout,
    /// The device or its connection cannot do this (e.g. status over a one-way link).
    #[error("not supported by this device: {0}")]
    Unsupported(String),
    /// Anything else the operating system or device reported.
    #[error("device error: {0}")]
    Io(String),
}

/// What a health check found: shown in the status bar and the devices screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceHealth {
    Ready,
    /// Works, but needs attention soon (e.g. paper near end).
    Warning(String),
    /// Cannot be used right now, with the reason.
    Offline(String),
    /// The device cannot report its state (one-way connection, or not asked yet).
    Unknown,
}
