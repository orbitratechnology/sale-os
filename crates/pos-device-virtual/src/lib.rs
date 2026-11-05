//! Simulated devices with a scriptable scenario API (PRD §12). Stage 0: the virtual printer
//! capture (E13-T01). Scanner and payment simulators arrive with E07-T03 and E10-T02; the
//! printer gains failure scenarios and the `ReceiptOutputPort` impl with E13-T06.

pub mod printer;
