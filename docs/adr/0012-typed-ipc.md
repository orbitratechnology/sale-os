# ADR-012: Typed IPC

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- Tauri commands are thin adapters. Each one does three things: deserialize the input, call an
  application service, and map the error.
- Bindings are generated with **`tauri-specta`** into `apps/desktop-ui/src/ipc/bindings.ts`.
  CI fails if the generated file differs from what is committed.
- Every command returns `Result<T, IpcError>`. The `IpcError` shape is defined in
  [07 §2](07-contracts.md#2-error-model).

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
