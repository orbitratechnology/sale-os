# ADR-010: Sync delivery semantics

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- Delivery is **at-least-once**. The server deduplicates on `(business_id, event_id)` in the
  `event_inbox` table.
- The server applies each register's events **in `local_sequence` order**, one transaction per
  event. Each transaction both applies the effect and records the inbox result (PRD §11.3).
- Each event gets a per-event result:
  - `applied`
  - `duplicate` (the original result is returned)
  - `conflict` (master data whose `base_version` does not match)
  - `rejected` (a permanent validation failure)
- `conflict` and `rejected` still count as **handled**. They are acknowledged so later events
  are not blocked, and they are surfaced on the client: conflicts go to the conflict queue
  (SYN-010) and rejections go to the repair queue (PRD §11.3).
- Transient failures (5xx, timeouts, a lock timeout) stop the batch. The client retries with
  exponential backoff and full jitter, starting at 2 s and capped at 5 min.
- `ack_sequence` is the highest **contiguous** handled `local_sequence` for the register.
- If an event arrives with a gap (sequence N+2 before N+1), the server answers
  `out_of_order` and the client resends from `ack_sequence + 1`.

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
