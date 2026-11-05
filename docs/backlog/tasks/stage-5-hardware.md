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
