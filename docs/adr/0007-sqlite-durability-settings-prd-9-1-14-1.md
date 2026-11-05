# ADR-007: SQLite durability settings (PRD §9.1, §14.1)

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

| Setting | Value | Why |
|---|---|---|
| `journal_mode` | `WAL` | Reads (search, reports) do not block the writer |
| `synchronous` | `FULL` | In WAL mode, `NORMAL` can lose the last commits on power loss. A completed sale must survive. |
| `foreign_keys` | `ON` | Integrity |
| `busy_timeout` | `5000` ms | Avoids spurious `SQLITE_BUSY` |
| Write transactions | `BEGIN IMMEDIATE` | Takes the write lock at the start. No lock upgrades, no deadlocks. |
| Writer concurrency | One writer connection (a serialized write actor) plus N reader connections | |
| Checkpoint | `wal_autocheckpoint` default, plus `PRAGMA wal_checkpoint(TRUNCATE)` when idle for 60 s and on clean shutdown | |
| Backups | `VACUUM INTO` before every migration, hourly when there are changes (keep 24), daily (keep 14), and on clean shutdown | |
| Integrity | `PRAGMA quick_check` on start-up. Full `integrity_check` in the diagnostics bundle. | |
| Database location | Per-user application data directory. **Never a network share** (start-up refuses UNC paths). | |

These settings are confirmed by the crash harness in task `E02-T04`, and again by the
campaign in `E19-T02`.

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
