# 00 — Roadmap and Build Sequence

## 1. Stages at a glance

| Stage | Theme | Exit gate (from PRD §17) | Proven by | Rough effort (eng-days) |
|---|---|---|---|---|
| **0** | Architecture and spikes | Desktop shell creates the database, stores a product, simulates a scan, and commits a demo sale atomically | E00-T06 automated on Windows CI | 45–60 |
| **1** | Catalogue and checkout foundation | Fast local checkout in demo mode, with no cloud and no hardware | AT-02, AT-03 e2e; perf budgets in E06-T04 and E09-T07 | 85–105 |
| **2** | Sales, stock and receipts | A complete local operating day, run and reconciled on one desktop | E15-T03 scripted day; AT-01, AT-04, AT-05, AT-08, AT-09, AT-10, AT-11 | 85–105 |
| **3** | Offline cloud sync | Disconnected sales replay after reconnection with no duplicate effects | E17-T10 fault suite; AT-06, AT-07, AT-12, AT-13 | 70–85 |
| **4** | Pilot hardening | Pilot acceptance passes on the documented Windows hardware | E19-T12 (all ATs), E19-T01 perf, E19-T05 security | 35–50 + external lead times |
| **5** | Hardware validation | Matrix devices pass the compatibility suites | E20 registry entries | 15–25 + procurement |

**Path to pilot (Stages 0–4):** about 320–400 engineer-days. With a team of 6 at a realistic
70% focus factor, that is **about 12 two-week sprints (≈ 24 weeks)**. Stage 5 can run alongside
the pilot.

These are planning ranges. Re-estimate at the end of Stage 0, when the spikes have retired the
biggest unknowns.

## 2. Milestones

| Milestone | What a stakeholder can see |
|---|---|
| **M0 Walking skeleton** (end of S0) | An installer on a Windows 11 VM. One product, a virtual scan, an atomic sale, and ESC/POS bytes in a file. |
| **M1 Demo checkout** (end of S1) | The first-run wizard works offline, 20 products are imported, cashiers log in, and they scan, search, discount and see correct tax. |
| **M2 Local shop day** (end of S2) | Open the drawer, sell, receive stock, return, print/PDF, report, and close with zero variance against the fixture. |
| **M3 Sync beta** (end of S3) | Two registers, a week offline, reconnection, exactly-once cloud totals, a conflict surfaced and resolved |
| **M4 Pilot go** (end of S4) | Signed installer, updates, backups, security sign-off, all 13 acceptance tests green |
| **M5 Hardware certified** (S5) | Registry entries for the scanners, printers and drawer in the test matrix |

## 3. Critical path

```text
E00-T01 → E03-T02 → E02-T03 ─┬→ E04-T01 → E05-T02 → E05-T03 ─┐
                             └→ E06-T01 → E06-T02 → E06-T04 ─┴→ E09-T01 → E08-T01 → E08-T02
   → E10-T01 + E11-T01 + E13-T02 → E09-T08 (complete sale) → E12-T02 → E15-T03 (Stage 2 exit)
   → E17-T03/T04/T05/T06/T07 → E17-T08 → E17-T10 (Stage 3 exit) → E19-T12 (Pilot)
```

The most important item on the path is **E09-T08 (complete sale)**. Every later stage builds on
its WriteSet, snapshot and outbox shape. Give it the strongest reviewers and do not cut its
fault-injection tests.

## 4. Parallel tracks (team of 6)

Suggested team:

- 2 Rust core engineers, one of them the tech lead
- 1 Rust cloud/sync engineer
- 2 frontend engineers (React/TS)
- 1 QA/automation engineer, from Stage 1 onwards
- product and design, part-time

| Sprint | Core Rust (2) | Cloud/sync (1) | Frontend (2) | QA (1) |
|---|---|---|---|---|
| 1 | E00-T01..T03, E03-T01..T03, E02-T01..T02 | E00-T02 CI, E16-T01 spike | E01-T01, E01-T02 | — |
| 2 | E02-T03, E02-T04, E03-T04, E13-T01 | E00-T05, E01-T03/T04 | E07-T01 wedge spike, E00-T06 UI | — |
| 3 | E04-T01, E05-T01/T02, E06-T01 | E06-T03, E08-T01 | E05-T06, E09-T03 | Test harness, e2e on Tauri (Playwright + WebDriver), fixtures |
| 4 | E06-T02, E06-T04, E05-T03/T04/T05 | E08-T02, E08-T03, E06-T05 | E04-T02, E06-T06a/b | E07-T03 virtual scanner suite |
| 5 | E09-T01/T02, E07-T02, E06-T07 | E04-T03, E00-T04 seed/bench | E09-T04/T05/T06, E07-T04, E06-T08 | AT-02, AT-03, perf (E09-T07) |
| 6 | E11-T01, E10-T01, E13-T02 | **E16-T02, E16-T03** (pulled forward) | E10-T03, E13-T03 | Pricing and tender scenario tests |
| 7 | E09-T08a/b, E11-T02..T04 | **E16-T04, E16-T05, E17-T01** | E11-T05, E12-T03 (list/detail) | AT-04/05 fault injection |
| 8 | E12-T01/T02/T04, E13-T04..T06, E15-T01/T02 | E16-T07, E16-T08, E17-T02 | E13-T07, E14-T03, E12-T03 (return wizard) | E15-T03 scripted day |
| 9 | E14-T01/T02/T04, E11-T06, E10-T02, E15-T03 | E17-T03, E17-T04 | E17-T09 | Cross-tenant suite (E16-T05) |
| 10 | E17-T05, E17-T06 | E17-T07, E17-T08 | E18-T01/T02, E14-T05 UI | E17-T10 fault suite |
| 11 | E19-T02, E19-T03, E19-T04 | E16-T06, E16-T09, E19-T09 | E19-T08 usability fixes | E19-T01 perf on reference hardware |
| 12 | E19-T05 fixes, E19-T07 | E19-T06 signing/updates, E19-T10 | Accessibility fixes | E19-T12 acceptance suite, E19-T11 runbooks |

Notes:

- **The cloud track starts in Sprint 6, not after Stage 2 ends.** The cloud and ingestion
  foundations depend only on `pos-domain` and `pos-sync`, so they can be built early. This
  shortens the timeline by about one sprint.
- Frontend work runs against **real IPC from day one**. There is no mock backend, because the
  Rust core is the authority and mocking it hides contract bugs.
- If the team is smaller, keep the order and stretch the timeline. **Do not reorder to start
  sync before E09-T08 is stable.**

## 5. Sprint 1 commitment (start immediately)

These tasks have no open product decisions blocking them:

1. `E00-T01` Monorepo scaffold
2. `E00-T02` CI pipeline (Windows and Linux)
3. `E00-T03` ADRs 001–020 accepted
4. `E03-T01..T03` IDs, Clock, Money, Quantity, Rate, rounding, allocation
5. `E02-T01..T02` SQLite connection layer and migration runner
6. `E01-T01..T02` Tauri scaffold, Windows installer spike, typed IPC
7. `E16-T01` Axum skeleton reusing `pos-domain`

**Sprint 1 demo:** CI-built Windows installer → the app opens → a debug command creates the
database → the generated TS binding calls a Rust command that adds two `Money` values and
returns the formatted result, with the same domain crate running inside the Axum health
check.

## 6. Decision calendar

Each decision has a recommended default in [03](03-decisions.md#part-2--open-product-decisions-prd-19-with-recommended-defaults).

| By | Decide |
|---|---|
| Start of Stage 1 (sprint 3) | D-04 (link later). Confirm DER-001…DER-010. |
| End of Stage 1 (sprint 5) | **D-02 pilot country and tax/invoice profile.** This changes the pricing goldens, the receipt's mandatory fields and retention. |
| Start of Stage 2 (sprint 6) | D-11 cash sessions |
| Start of Stage 3 (sprint 6–7, because the cloud track starts early) | D-03, D-06, D-09, D-10 |
| Start of Stage 4 (sprint 11) | D-01 name and signing identity (**order the certificate around sprint 8** for lead time), D-05 reference hardware (**buy the machine around sprint 8**), D-08 |
| Start of Stage 5 | D-07 devices (order around sprint 10 so they arrive for the pilot) |

## 7. Risks to the build sequence (supplements PRD §18)

| Risk | Signal | Response |
|---|---|---|
| WebView2 key events are too slow or unreliable for wedge scanning | E07-T01 misses scans at a 5 ms key interval | Fall back to a Rust-side low-level keyboard hook for the wedge (Windows `RawInput`) behind the same `BarcodeInputPort`. Decide by the end of sprint 2. |
| `synchronous=FULL` commit latency is too high on cheap SSDs or HDDs | E02-T03 or E19-T01 commit p95 > 300 ms | Batch nothing (a sale must be durable). Profile the fsync count instead, keep the database on local SSD, and document the minimum hardware. |
| Pricing rules differ in the pilot country | D-02 is late | The goldens make any change visible. The `TaxRoundingMode` extension point is already designed. |
| change_log visibility gaps under concurrent ingestion | E17-T07 property test fails | Use the per-business advisory-lock appender (option (a)) |
| Scope creep from pilot shop types (serials, lots, wholesale price lists) | Requests during S4 usability | Point to the [post-MVP list](tasks/post-mvp.md) and the PRD §4.3 scope guardrail |
