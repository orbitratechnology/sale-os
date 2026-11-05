# ADR-011: Master-data versioning (optimistic local versions)

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- Each mutable aggregate has a `version` column: business profile, location, register,
  settings, staff user, category, tax rate, tax category, and the product aggregate (product +
  variants + barcodes).
- A local edit sets `version = version + 1` and emits an event carrying
  `base_version = old version`.
- The server accepts the edit if `server.version == base_version`, and stores
  `version = base_version + 1`. Because one register's events are applied in order, the local
  and server versions stay in step.
- If they do not match, the server answers `conflict` and returns its current snapshot. The
  client records a `sync_conflicts` row and marks the local aggregate `sync_status = conflict`.
  Later local edits to that aggregate are allowed but are also conflicted, until the conflict
  is resolved.
- Resolving a conflict emits a new event with `base_version = server_version`.
- **Facts are never versioned.** Sales, returns, refunds, inventory documents, stock
  movements, cash sessions and audit events are immutable. "Last write wins" is never applied
  to them (SYN-009).

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
