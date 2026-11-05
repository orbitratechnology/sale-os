# ADR-016: Cloud tenancy

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- `business_id` is on every tenant row. Foreign keys are composite, `(business_id, id)`.
- PostgreSQL RLS is enabled on every tenant table. Its policy reads
  `current_setting('app.business_id')`, which is set by `SET LOCAL` at the start of each
  request transaction. The value comes from the authenticated principal, never from the
  request body.
- The application database role is **not** the table owner and does **not** have
  `BYPASSRLS`. Migrations run as a separate owner role.

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
