# ADR-014: Receipt rendering

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- There is one canonical `ReceiptDocument`: a JSON-serializable model built from a sale or
  return snapshot.
- Renderers:
  - **HTML preview:** a React component that reads the same JSON.
  - **PDF:** rendered in Rust with `printpdf` and an **embedded Noto Sans** font, so glyph
    coverage does not depend on the OS.
  - **ESC/POS:** a byte encoder with a capability profile.
- The PDF and ESC/POS renderers have separate golden test suites (PRD §12.4).

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
