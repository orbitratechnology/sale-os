# Stage 5 — Physical Hardware Validation (after the core MVP)

**Goal.** Swap virtual adapters for real ones without touching the domain or application
layers. Every adapter implements the existing ports from
[06 §10](../06-rust-interfaces.md#10-device-ports--pos-device-ports-e07-e13-e10).

**Exit gate.** Each device in the D-07 test matrix passes its compatibility suite. Results
are recorded in the hardware compatibility registry.

**Rough size.** ≈ 15–25 engineer-days for E20-T01..T05, plus hardware procurement lead time. E20-T06 and E20-T07 are conditional and not included.

**Decision needed at the start:** D-07 (devices to buy).

> Rule for this stage: if an adapter needs a change in `pos-domain` or `pos-application`,
> that change is a design review item. The ports were meant to make that unnecessary.

---

## E20 Hardware

### E20-T01 · Hardware compatibility registry
- **Size** S · **Deps** — · **PRD** §12.6
- **Scope:** `docs/hardware/registry.yaml` with these fields:
  - vendor, model, connection, OS build, adapter and version, firmware
  - capabilities tested (cut, drawer, QR, code pages, paper widths)
  - result, test date, tester, notes

  Also a rendered page in the docs, and a JSON copy embedded in the app so the settings page can show "verified" badges.
- **Acceptance:**
  - [ ] The schema is validated in CI.
  - [ ] Every later E20 task adds a registry entry.

### E20-T02 · USB HID keyboard-wedge scanner validation
- **Size** M · **Deps** E07-T01, D-07 · **PRD** BAR-008, §12.3, §12.6 (1)
- **Scope:**
  - Validate the wedge path with real scanners, using the configured suffix (Enter/Tab).
  - Check keyboard-layout issues: a scanner set to US layout while Windows uses another layout (for example AZERTY) mangles digits. Detect this pattern and show a setup guide.
  - A scanner configuration barcode sheet in the docs.
- **Acceptance:**
  - [ ] For each matrix scanner: 500 mixed EAN-13, UPC-A and Code128 scans at full speed are all recognized. Rapid repeats of the same item are all counted (AT-03 on hardware).
  - [ ] Under a mismatched keyboard layout, the app shows a "Scanner keyboard layout mismatch" hint instead of silent not-found errors.

### E20-T03 · Windows OS printer / PDF path
- **Size** M · **Deps** E13-T04 · **PRD** RCP-008, §12.6 (2)
- **Scope:** An `OsSpoolerAdapter` that prints the PDF or a rendered bitmap through the Windows print spooler (the Win32 print API through the `windows` crate), mapping job status to `PrintStatus`.
- **Acceptance:**
  - [ ] Prints to the "Microsoft Print to PDF" printer and to a real A4 printer.
  - [ ] Spooler errors map to `failed`. A job the spooler accepted but whose final state is unobservable maps to `unknown`, never auto-retried.

### E20-T04 · Raw ESC/POS over USB and Ethernet
- **Size** M · **Deps** E13-T05, E13-T06 · **PRD** RCP-008, §12.6 (3)
- **Scope:**
  - `EscPosNetAdapter` (TCP 9100, with DLE EOT real-time status where supported).
  - `EscPosUsbAdapter` (WinUSB or raw spooler passthrough, decided in a spike).
  - A capability profile per model in the registry.
- **Acceptance:**
  - [ ] For each matrix printer: the golden receipts print legibly at 58 and 80 mm, the cut works, the code page renders the accented and currency characters in the D-02 fixture, and the QR/barcode scans back with the E20-T02 scanner.
  - [ ] Paper-out and cover-open are detected as `failed` with reasons where the model reports status. Otherwise they become `unknown`.
  - [ ] Unplugging the printer mid-job gives `unknown`, an operator prompt, and no duplicate print (RCP-007 on hardware).

### E20-T05 · Cash drawer through the printer kick port
- **Size** S · **Deps** E20-T04 · **PRD** §12.6 (4)
- **Scope:**
  - A `CashDrawerPort` implementation that sends the ESC/POS pulse.
  - The drawer opens automatically on cash sale completion (setting) and through a "No sale / open drawer" command (needs a manager by default, and is audited as `cash_drawer.opened`).
- **Acceptance:**
  - [ ] The drawer opens on a cash sale only, never on external-tender-only sales.
  - [ ] An open without a sale is audited with the user and reason.

### E20-T06 · Scale integration (conditional)
- **Size** M · **Deps** D-07, real device · **PRD** §12.6 (5), CAT-007
- **Scope:** A `ScalePort` (new port, reviewed): serial/USB protocol per the vendor; weighed items read their quantity from the scale with a stable-weight check.
- **Gate:** start only when a target scale and its protocol docs are in hand.

### E20-T07 · Payment terminal or provider connector (conditional)
- **Size** L (split when started) · **Deps** D-08, commercial agreement, sandbox credentials · **PRD** PAY-007, PAY-008, §12.6 (6)
- **Scope:** An implementation of `PaymentPort` against the chosen provider, built on the E10-T02 state machine and reconciliation.
- **Gate:** start only when the agreements, SDK and test credentials exist (PRD §17 Stage 5).

---

## Universal device support (added 2026-11-20, PRD §12.7, ADR-023)

E20-T01…T07 validate one device of each kind. The tasks below make the till work with the
hardware a shop already has, however it is connected. Each one lands behind the existing
ports and is tested without hardware (emulators, PTY null-modems, an in-test TCP printer).
Each still needs a registry entry (E20-T01) once someone tests it on real equipment.

### E20-T08 · Printer transports: network, serial, device file, OS spooler
- **Size** L · **Deps** — · **PRD** DEV-001, RCP-008
- **Scope:** the `pos-device-hw` crate.
  - Transports: raw TCP (9100), serial (RS-232, USB-COM, Bluetooth SPP), device files (`/dev/usb/lp*`, `/dev/lp*`, `/dev/rfcomm*`, `LPT1`), and the OS spooler in RAW mode (winspool on Windows, `lp -o raw` on CUPS).
  - Bounded timeouts, a connection per job, and jobs serialised per printer.
- **Acceptance:**
  - [ ] Against an in-test printer: printed, paper out and cover open (failed, no job bytes sent), unreachable (failed `disconnected`), hung or dropped mid-job (`unknown`, never `printed`), paper out after the job (`unknown`).
  - [ ] Serial transport tested over a PTY pair. The spooler command construction is unit-tested.

### E20-T09 · Model profiles and identification
- **Size** M · **Deps** E20-T08 · **PRD** DEV-002, DEV-004
- **Scope:** profiles from escpos-printer-db (with attribution) plus common models and generic ESC/POS, Star Line and plain-text profiles. `GS I` identification; per-till overrides (columns, font, cut, drawer pin, barcode/QR).
- **Acceptance:**
  - [ ] TM-T88V gives 42 columns, TM-T20III gives 48, generic 58 mm gives 32.
  - [ ] An identified printer picks its profile.
  - [ ] Hand-added profiles are flagged unverified.

### E20-T10 · Encoder dialects, code pages, barcodes
- **Size** L · **Deps** E13-T05 · **PRD** DEV-003, DEV-005, RCP-009 (Star part)
- **Scope:**
  - ESC/POS, Star Line Mode and plain-text encoders.
  - Code pages generated from reference tables; per-run code-page selection with transliteration fallback.
  - CODE128/QR for the receipt number; cut modes; drawer kick.
- **Acceptance:**
  - [ ] The existing byte goldens are unchanged under default options.
  - [ ] `Café crème — Größe L £ € Ż` prints without `?` on a printer with CP858/CP852.
  - [ ] No encoded line is wider than its column count.
  - [ ] Control-character injection is blocked in every dialect.

### E20-T11 · Device configuration, live status, test print, drawer
- **Size** L · **Deps** E20-T08…T10 · **PRD** DEV-006, DEV-007, DEV-009, E20-T05
- **Scope:**
  - Migration `0017_devices.sql`: a local-only `devices` table, and `print_jobs.adapter` widened to every transport.
  - `DeviceService`: list, save (validated per field, audited), remove, test print, status, discover, open drawer ("no sale", audited with a reason; needs a manager by default).
  - A switchable printer behind the print queue, swapped without a restart. Printing and preview use the printer's columns.
  - Drawer kick on cash sales (setting). IPC commands and the Settings → Devices page.
- **Acceptance:**
  - [ ] A printer saved in settings is used by the next print with no restart.
  - [ ] A test print reaches the in-test printer.
  - [ ] Status shows paper out.
  - [ ] The drawer opens on a cash sale only.
  - [ ] "No sale" is audited with user and reason.
  - [ ] The permission harness covers every new command.

### E20-T12 · Device discovery
- **Size** M · **Deps** E20-T08 · **PRD** DEV-008
- **Scope:** serial ports with USB vendor labels, spooler queues, device files, a bounded /24 scan for port 9100 with identification.
- **Acceptance:**
  - [ ] Ranges larger than /24 are refused.
  - [ ] Discovery never blocks the UI for more than its timeout.
  - [ ] A listener found on the network is identified.

### E21-T01 · Scan decoding engine
- **Size** L · **Deps** E06-T03 · **PRD** BAR-010, BAR-011, BAR-009
- **Scope:**
  - `pos_domain::catalog::decode_scan`: ordered lookup candidates (AIM ids, UPC-E↔UPC-A, add-ons, ISBN-10/ISSN, Code 39 start/stop/check, Codabar start/stop, GS1 AIs, GS1 Digital Link, keyboard-layout remap).
  - Configurable variable-measure rules.
  - Catalogue lookup tries the candidates in order.
- **Acceptance:**
  - [ ] Every case in the 2026-11-20 symbology matrix that was "not found" now resolves to the shop's registered form.
  - [ ] The exact form is always tried first.
  - [ ] Proptests: never panics, candidates are fixed points of `normalize_barcode`.

### E21-T02 · Variable-measure items in the cart
- **Size** M · **Deps** E21-T01, E09-T01 · **PRD** BAR-011
- **Scope:**
  - Rules in a setting (`scanner.variable_measure_rules`).
  - A weight label sets the quantity of a weighed item.
  - A price label sets the line to the printed price (audited as `barcode_price`, without the manager approval a manual override needs, because the label comes from the shop's own scale).
- **Acceptance:**
  - [ ] Weight and price labels add the right quantity/price.
  - [ ] An unknown item code is "not found" as before.
  - [ ] A malformed rule is refused by settings validation.

### E21-T03 · Keyboard-wedge robustness
- **Size** M · **Deps** E07-T01 · **PRD** BAR-012, DER-008
- **Scope:**
  - GS (Ctrl+]) kept in the scan.
  - Codes of 1–3 characters accepted when a suffix ends them at scanner speed.
  - Slow scanners recognised by regular timing.
  - A slow-scanner hint and a keyboard-layout hint in the till.
- **Acceptance:**
  - [ ] Chromium e2e at 5, 20 and 50 ms/char, with no silent loss.
  - [ ] Human typing in fields is still never captured.

### E21-T04 · Serial scanners
- **Size** M · **Deps** E20-T08 · **PRD** BAR-013
- **Scope:** `SerialScanner` (terminators, prefix strip, GS kept, auto-reconnect with backoff, health), started from device configuration. Scans reach the cart through the same scan command as the wedge.
- **Acceptance:**
  - [ ] PTY-based tests for splitting and reconnect.
  - [ ] A scan from a configured serial scanner adds to the cart.

### E22-T01 · Payment port, mock provider, attempt state machine
- **Size** L · **Deps** E10-T01 · **PRD** PAY-006, PAY-009, PAY-010
- **Scope:**
  - `PaymentPort`, `MockPaymentProvider` (E10-T02's scenarios), and `payment_attempts` with an append-only history.
  - `PaymentService` (start, poll, cancel, resolve, reverse).
  - Checkout accepts an approved attempt as a provider-confirmed tender.
- **Acceptance:** E10-T02's acceptance list, plus:
  - [ ] An attempt can't be used for another cart or amount.
  - [ ] A sale is refused while its cart has an unresolved attempt.

### E22-T02 · Terminal payment in the till (sandbox provider)
- **Size** M · **Deps** E22-T01 · **PRD** PAY-009
- **Scope:** IPC commands and a "Card (terminal)" option in the payment panel that drives the attempt (waiting, approved, declined, reconcile), available with the mock provider in sandbox mode and with any configured provider adapter.
- **Acceptance:**
  - [ ] Chromium e2e for approve, decline and timeout-then-reconcile.

### E22-T03 · Provider connectors (conditional, one task per provider)
- **Size** L each · **Deps** E22-T01, D-08, agreements and sandbox credentials · **PRD** PAY-007, PAY-008
- **Scope:** an adapter per provider family (Adyen Terminal API/nexo, Stripe Terminal, SumUp, Square Terminal, ZVT, OPI, PAX POSLink, Verifone, Ingenico), each with its provider's test environment.
- **Gate:** as E20-T07.
