# ADR-018: Frontend state

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- **Rust is the authority** for carts, pricing, permissions and every persisted state.
- React uses:
  - **TanStack Query** for IPC reads, invalidated by Rust-emitted events
    (`cart://changed`, `sync://status`, …)
  - **Zustand** only for ephemeral UI state (focused panel, open dialog)
- The UI never computes totals. It displays the `PricedCart` that Rust returns.

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
