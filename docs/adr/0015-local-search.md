# ADR-015: Local search

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- Barcode and SKU lookups use exact `UNIQUE` indexes on normalized values. The target is
  p95 < 50 ms at 100k aliases.
- Name search uses **SQLite FTS5**:
  - `unicode61 remove_diacritics 2`
  - prefix indexes 2 and 3
- The FTS index is maintained by the store inside the same transaction as the catalogue write.

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
