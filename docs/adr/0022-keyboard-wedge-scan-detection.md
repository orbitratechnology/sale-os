# ADR-022: Keyboard-wedge scan detection in the WebView

- **Status:** Proposed
- **Date:** 2026-11-05
- **Source:** Task E07-T01 (keyboard-wedge capture spike); BAR-002, BAR-007

## Context

USB HID scanners in keyboard mode type the barcode as key events, normally a few milliseconds
apart, followed by a configurable suffix (Enter, Tab or none). The WebView has to tell these
bursts apart from human typing without dropping or merging rapid scans (AT-03). The roadmap
named WebView key-event latency as a risk, with a fallback of a Rust-side Windows Raw Input
hook.

## Decision

- Detection runs in the UI (`features/scan/scanDetector.ts`, a pure state machine), wired by
  `useScanCapture`, which uses a capture-phase `keydown` listener on `window`.
- A burst is a scan when:
  - it has at least 4 characters;
  - its **average** inter-key gap is at most 35 ms;
  - it ends with the configured suffix (or with silence when there is no suffix).
- **Stall tolerance:** up to 2 single delays of at most 150 ms are absorbed inside a burst
  whose characters so far arrived at scanner speed. This is required, not cosmetic. In the
  spike, Chromium under load delivered one key 41 ms late. The original split-on-any-slow-gap
  rule then dropped the burst's prefix and emitted the fast remainder as a *truncated
  barcode*, which can match a different product.
- **Focus rules:**
  - With no text field focused (scanner-ready mode), scan characters are swallowed.
  - In an input marked `data-scan-field`, characters appear as typed and the scan is
    delivered with `inScanField`.
  - Any other text field is never intercepted.
- Each scan gets a `scanEventId`. Repeated *delivery* is de-duplicated in Rust by that ID
  (E07-T02). Identical scans are never de-duplicated by value or time.

## Consequences

- **No-suffix mode** ends a scan on silence (200 ms), so consecutive scans must be more than
  150 ms apart. This always holds physically, because presenting the next item takes at least
  about 250 ms, but rapid *automated* re-sends would merge. The setup guide should recommend
  configuring scanners with an Enter suffix.
- **Evidence:**
  - 27 unit tests, including a regression test for the 41 ms stall.
  - Playwright tests drive the real hook in Chromium at 5 ms per key: 20 scans per suffix
    mode, human typing in a field and in scanner-ready mode, and the scan field. They ran
    12 times in a row with no failures after the fix.
- **WebView2 itself has not been measured yet** (Windows work is deferred). Chromium is the
  same engine, so these results are a strong proxy. The Raw Input fallback stays on the
  roadmap risk list until E20-T02 measures real scanners on Windows.
