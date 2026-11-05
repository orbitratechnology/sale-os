# ADR-005: Identifiers

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- **UUIDv7**, generated on the client (time-ordered, which helps index locality).
- Stored as canonical lowercase `TEXT` in SQLite and as native `uuid` in PostgreSQL.
- Each ID kind has a Rust newtype (`SaleId`, `VariantId`, …). Mixing them up is a compile
  error.
- Human-readable numbers (receipt and return numbers) are separate. They are allocated per
  register from `register_counters` (see [04](04-local-database.md)).

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
