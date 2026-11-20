# ADR-023: Device adapters for any printer, scanner, drawer and payment terminal

- **Status:** Proposed
- **Date:** 2026-11-20
- **Source:** PRD §12.7 (DEV-001…009, BAR-010…013, PAY-009/010); Stage 5 (E20–E22)

## Context

The MVP ran on a virtual printer, keyboard-wedge scanners and operator-recorded card tenders.
A hardware check on 2026-11-20, against a TCP 9100 ESC/POS emulator, the escpos-tools parser
and the escpos-printer-db model database, found these gaps:

- **Printers.** There was no adapter for any real connection. Every non-ASCII character printed
  as `?`. The fixed 48 columns wrapped on 42-column printers (Epson TM-T88 II–V, many Star and
  Rongta models). The receipt barcode printed as text, and Star printers in native mode would
  misread the commands.
- **Scanners.** UPC-E, add-ons, ISBN-10, Code 39 start/stop, AIM prefixes, GS1 element
  strings, scale labels and short codes did not resolve. Slow wireless scanners lost every
  scan silently. Keyboard-layout mismatches gave `SCAN_INVALID`.
- **Payments.** No `PaymentPort` or mock provider existed (PAY-006).

## Decision

1. **Ports stay the boundary** (`pos-device-ports`): `ReceiptOutputPort` (with
   `capabilities()` and `status()`), `CashDrawerPort`, `BarcodeInputPort`, `PaymentPort`.
   `PrintResult::Uncertain` exists so an adapter can say "may be on paper". It becomes
   `unknown` and is never retried automatically.
2. **One adapter crate for real devices, `pos-device-hw`.** It splits the work in two:
   - *Transports*: raw TCP, serial (serialport, which covers RS-232, USB-COM and Bluetooth
     SPP), device files (Linux usblp/parallel/rfcomm, Windows LPT/COM), and the OS spooler in
     RAW mode (Windows winspool through the `printers` crate, CUPS `lp -o raw` elsewhere).
   - *Devices*: an ESC/POS / Star printer that is also the cash drawer, and a serial scanner.

   USB printers on Windows go through the spooler, which works with every installed driver
   including "Generic / Text Only". Direct WinUSB is not needed for the common case.
3. **Profiles, not code, describe models.** The data is curated from escpos-printer-db (with
   attribution) plus hand-added common models, each flagged unverified until E20-T01 records
   a hardware test. Per-till overrides cover columns, cut, drawer pin, barcode/QR and font.
   `GS I` identification picks a profile automatically.
4. **Encoding happens in `pos-receipts`, per dialect** (ESC/POS, Star Line Mode, plain text).
   Code pages are selected per character run from the printer's own table, with
   transliteration (deunicode) as the fallback. The application maps a printer's
   capabilities to encoder options, so `pos-receipts` never depends on device crates.
5. **Scan decoding is a pure domain function**, `decode_scan`. It returns ordered lookup
   candidates, GS1 data and an embedded measure. The catalogue lookup tries the candidates in
   order and the first hit wins, so registration normalisation does not change and existing
   barcodes keep working.
6. **Device configuration is per till and local** (a new local-only table, never synced).
   Printer addresses and COM ports belong to one machine, not to the business. Changes are
   audited. The running printer is swapped behind a switchable port without a restart.
7. **Payments.** A persisted attempt state machine sits in front of `PaymentPort`. Anything
   after `initiate` that is not a definite answer becomes `reconciliation_required`, and the
   sale stays blocked until it is resolved. Real connectors are adapters added when their
   agreements exist (PRD §17).

## Consequences

- One code path serves every printer connection, and the same print-queue rules
  (failed / unknown / reprint) hold for all of them.
- New models are mostly data changes. A new command language is a new encoder dialect.
- `unsafe` stays forbidden in our code: OS access goes through maintained crates (serialport,
  printers, tokio).
- **Still unverified on hardware:** the Star Line Mode commands, Windows spooler job-state
  tracking, and the `GS I` replies of non-Epson models. E20-T01 records each one as it is
  tested.
- **Known gaps:** USB HID-POS scanners, raster-only printers, label languages (ZPL/EPL) and
  scales are tracked as separate tasks.
