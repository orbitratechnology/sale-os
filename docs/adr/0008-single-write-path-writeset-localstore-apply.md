# ADR-008: Single write path: `WriteSet` → `LocalStore::apply`

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- Every business mutation works the same way. The use case loads state through query ports,
  calls pure domain functions, and builds a **`WriteSet`**. It then calls
  `LocalStore::apply(write_set)`, which runs **one `BEGIN IMMEDIATE` transaction**.
- Validation in `WriteSet::build()` rejects any set that has no audit event, or that writes a
  syncable record without a matching outbox event.
- `apply` allocates the outbox `local_sequence` and any receipt or return numbers **inside**
  the transaction.
- Stock balance projections are updated in the same transaction as their movements.
- The UI reports success only after `apply` returns `Ok`. This covers PRD §11.2 and AT-04.

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
