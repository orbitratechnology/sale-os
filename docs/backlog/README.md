# Universal Retail POS — Technical Implementation Backlog

This backlog turns the [PRD](../PRD.md) into a concrete build sequence: epics, modules,
database migrations, Rust interfaces, API/IPC/event contracts, development tasks, and
testable acceptance criteria.

It is meant to be **worked from**, not just read. Every task has an ID, a size, dependencies,
PRD traceability, and acceptance criteria that a reviewer can check.

## How the documents fit together

| # | Document | What it answers |
|---|---|---|
| 00 | [Roadmap & build sequence](00-roadmap.md) | Stages, milestones, critical path, exit gates, staffing and rough sizing |
| 01 | [Epics & traceability](01-epics-and-traceability.md) | The 21 epics, and which tasks satisfy each PRD requirement ID |
| 02 | [Architecture & modules](02-architecture-and-modules.md) | Crates, packages, dependency rules, the single write path |
| 03 | [Decisions (ADRs) & open questions](03-decisions.md) | Fixed engineering decisions and recommended defaults for PRD §19 open questions |
| 04 | [Local database (SQLite)](04-local-database.md) | Ordered migrations with DDL, invariants, indexes, durability settings |
| 05 | [Cloud database (PostgreSQL)](05-cloud-database.md) | Tenancy, inbox, change log, projections, RLS |
| 06 | [Rust interfaces](06-rust-interfaces.md) | Domain types, pricing/tax/return rules, application services, ports |
| 07 | [Contracts: IPC, REST, events](07-contracts.md) | Tauri commands, error codes, REST API, sync event catalogue |
| 08 | [Test strategy & acceptance mapping](08-test-strategy.md) | Test layers, fixtures, and how PRD §16.4 acceptance tests are automated |
| — | [Stage 0 tasks](tasks/stage-0-foundations.md) | Architecture & technical spikes |
| — | [Stage 1 tasks](tasks/stage-1-catalogue-checkout.md) | Catalogue & checkout foundation |
| — | [Stage 2 tasks](tasks/stage-2-sales-stock-receipts.md) | Sales, stock & receipts |
| — | [Stage 3 tasks](tasks/stage-3-cloud-sync.md) | Offline cloud synchronization |
| — | [Stage 4 tasks](tasks/stage-4-pilot-hardening.md) | Pilot hardening |
| — | [Stage 5 tasks](tasks/stage-5-hardware.md) | Physical hardware validation |
| — | [Post-MVP (P1/P2) backlog](tasks/post-mvp.md) | Parked requirements, ordered |

## Conventions

### Task IDs

`E<epic>-T<nn>`, for example `E09-T08`. IDs are stable: when a task is split, the parts get
suffixes (`E06-T06a`, `E06-T06b`), and a dropped task is marked **Dropped**, never reused.

### Sizes

| Size | Meaning |
|---|---|
| **S** | ≤ 2 engineer-days |
| **M** | 3–5 engineer-days |
| **L** | 6–10 engineer-days. **Must be split before it enters a sprint.** |

Sizes are planning estimates for a mid-level engineer familiar with the stack. They exist to
sequence work and spot overload, not to set delivery promises.

### Requirement references

- `CAT-003`, `SYN-006`, etc. refer to PRD §6 functional requirement IDs.
- `NFR-*` refers to PRD §14 rows (e.g. `NFR-lookup` = local barcode lookup p95 < 50 ms).
- `AT-01` … `AT-13` refer to the PRD §16.4 MVP acceptance tests, in order.
- `DER-*` marks a **derived requirement**: something the PRD implies but does not list as an
  FR (for example, cash sessions are needed for the Stage 2 "reconciled operating day" exit
  gate). Each one is listed in [01](01-epics-and-traceability.md#derived-requirements) so
  product can veto it.
- `ADR-*` and `D-*` refer to [decisions](03-decisions.md).

### Definition of Done (applies to every task)

Taken from PRD §20 and made checkable. A task is **not done** until:

1. Domain validation exists in Rust (not only in the UI).
2. State is persisted through the single write path (`LocalStore::apply`), where it applies.
3. Permission checks run in the application layer, with a test that calls the command
   directly as an under-privileged actor (where it applies).
4. Errors use a catalogued error code (see [07 §2](07-contracts.md#2-error-model)), and the UI
   shows what happened and what to do next.
5. Offline behaviour matches the PRD §11.6 matrix, with a test (where it applies).
6. Every state-changing operation writes an audit event and an outbox event in the same
   transaction.
7. Automated tests cover the acceptance criteria. CI is green on Windows and Linux.
8. Generated IPC bindings, OpenAPI, and event JSON Schemas are regenerated and committed if a
   contract changed.
9. No money or quantity is handled as a binary float anywhere, including TypeScript.

### Working agreements

- **Contracts first.** Changes to an IPC command, REST endpoint, event payload, or migration
  are reviewed as contract changes before the implementation is reviewed.
- **Migrations are append-only.** Once a migration has merged to `main`, it is never edited. A
  fix is a new migration.
- **Simulators are first-class.** The virtual scanner, printer, payment provider, and network
  fault injector ship in the debug build and are used by end-to-end tests. They are not
  throwaway test code.
