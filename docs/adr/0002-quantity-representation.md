# ADR-002: Quantity representation

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- A quantity is an **`i64` count of milli-units**: a fixed scale of 3, so 1.250 kg is `1250`.
- Whether a unit allows fractions is a property of the unit (`units.allows_fraction`). For
  example, `ea` allows integers only, so the value must be a multiple of 1000.
- Quantities are also decimal strings in IPC.

## Consequences

- Cost of change: high. If a pilot needs finer precision (for example grams of gold), add a
  new unit with a scale override instead of changing the global scale.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
