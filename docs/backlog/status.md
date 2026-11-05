# Backlog Status

Updated at the end of each working session. "Done" means every acceptance criterion was
checked. Anything that could not be checked yet is listed under **Open**, with the reason.

## Sprint 1 (started 2026-11-05)

| Task | Status | Evidence | Open |
|---|---|---|---|
| E00-T01 Monorepo scaffold | **Done** | Cargo and pnpm workspaces with the full crate layout; `cargo xtask check-deps` enforces the layering rules (`xtask/src/deps.rs` tests inject forbidden edges); README | A first build on a clean Windows checkout happens in CI |
| E00-T02 CI pipeline | **Done** | `.github/workflows/ci.yml`. Green on PR #1 (merged): Windows and Linux tests, the Postgres job, cargo-deny, the frontend, and the Windows NSIS/MSI artefact. 12.5 min wall time. The first run failed only on an untracked `contracts/events` folder (fixed). | That a warning blocks a PR is enforced by `-D warnings` but not demonstrated with a deliberately failing PR |
| E00-T03 ADR records | **Drafted** | `docs/adr/0001–0020` (from 03-decisions), plus `0021` WebView2 distribution; template; PR template checkbox | Needs acceptance by the tech lead plus one engineer. Every status is "Proposed". |
| E03-T01 IDs, Clock, IdGenerator | **Done** | `pos-domain/src/ids.rs` and `time.rs`; `pos-testkit` fakes; DST and day-boundary tests for London, New York, Kolkata and Chatham | ID ↔ SQLite TEXT round-trip is tested where IDs are first persisted (E02-T03). By design, `pos-domain` has no sqlx dependency. |
| E03-T02 Money, Currency, Quantity, Rate | **Done** | Checked arithmetic with proptests over the full `i64` range; parse/format round-trip for exponents 0–4 in en-US, de-DE and plain formats; embedded ISO-4217 table; `clippy::float_arithmetic` denied in the domain, plus `f32`/`f64` disallowed workspace-wide | — |
| E03-T03 Rounding and allocation | **Done** | Half-away-from-zero and half-even; largest-remainder allocation with exact-sum, proportionality and sign-symmetry proptests | — |
| E02-T01 SQLite connection layer | **Done** | WAL, `synchronous=FULL`, foreign keys and 5 s busy timeout verified on writer and readers; UNC paths refused; 1,000 concurrent reads during 300 writes with no `SQLITE_BUSY` | (1) The writer is a 1-connection pool rather than an mpsc actor. Serialization is equivalent; revisit in E02-T03. (2) Network detection is path-based. A mapped drive letter (e.g. `Z:` → share) needs `GetDriveTypeW` on Windows (follow-up). |
| E02-T02 Migration runner + 0001 | **Done** | Fresh and upgrade paths; pre-migration `VACUUM INTO` backup; a failed migration restores the backup (and deletes a new file); downgrade guard leaves the file byte-identical; the full `0001_core.sql` from the spec, including the append-only triggers (tested) | Per-release upgrade fixtures start with the first pilot build |
| E01-T01 Tauri scaffold + packaging | **Partly done** | The app builds and runs. Linux smoke run under Xvfb: database created and migrated, UI rendered. `tauri build` config validated. NSIS (per-user) + MSI targets configured. WebView2 offline installer chosen (ADR-021). | Needs a Windows machine: the installer from the CI artefact on a clean Windows 11 VM, uninstall, offline install, and the cold-start measurement in `docs/perf/baseline.md` |
| E01-T02 Typed IPC | **Done** | `tauri-specta` bindings generated, with drift checked by `cargo xtask gen-bindings --check`; `IpcError { code, message, retryable, correlationId }`; uncatalogued errors map to `INTERNAL` and are logged with a correlation ID; `formatMoney` is string-only and exact beyond 2^53; an ESLint rule bans `parseFloat`/`Number(` in features and lib (verified to fire) | — |
| E16-T01 Cloud API skeleton | **Done** | Axum `/v1/health` (503 when the DB is down), `/v1/version-policy`, and a debug route proving `pos-domain` reuse; problem+json errors; `cloud-api migrate`; Dockerfile; tests against PostgreSQL 16; `check-deps` forbids tauri/sqlite in the cloud dependency tree | Uses a CI Postgres service container instead of testcontainers (no Docker daemon in all dev environments). Equivalent coverage. |

### Toolchain pins and why

| Pin | Reason | Revisit |
|---|---|---|
| `typescript@^6` | typescript-eslint does not support TypeScript 7 yet | When typescript-eslint supports TS ≥ 7 |
| `sqlx 0.8.6` | Well-known API for the persistence layer; 0.9.0 was only just released | A deliberate upgrade task after Stage 1 |
| `tauri-specta =2.0.0-rc.26`, `specta =2.0.0-rc.26`, `specta-typescript =0.0.13` | These are pre-releases, so they are pinned exactly to avoid silent breaking changes | When 2.0 is stable |

## Sprint 2 (2026-11-05). Stage 0 exit gate met on Linux; Windows deferred at the owner's request

| Task | Status | Evidence | Open |
|---|---|---|---|
| E02-T03 `LocalStore::apply` + WriteSet | **Done** | The builder enforces the ADR-008/011 invariants (8 tests). One `BEGIN IMMEDIATE` transaction runs on a detached task, so it is safe under cancellation. A fault before each of the 18 statements leaves nothing behind. 10,000 concurrent applies give a gapless outbox (nightly). p95 is about 14 ms for a 20-record set (debug build). | Preconditions such as cart revision and return quantity land with their tasks |
| E03-T04 Error catalogue | **Done** | `ErrorCode::doc()` uses an exhaustive match. `docs/errors.md` is generated and checked by a test. | — |
| E01-T04 Logging + AppServices | **Done** | `pos-observability`: JSON lines, secret-field redaction, Luhn card masking, span/global context, 7 daily files. `Secret<T>`. A test logs a struct with a PIN and a token, and neither appears. Verified in the real app. | — |
| E01-T03 Capabilities + CSP | **Done** | Commands are declared in `build.rs`, and the main window gets exactly those plus `core:event:default`. ACL tests on the mock runtime check for Tauri's "not allowed" refusal, and fail if `core:path` is granted. The CSP has no remote origins and no `unsafe-eval`. | — |
| E13-T01 ESC/POS spike | **Done** | The encoder strips control characters (no command injection). A golden test checks hand-verified bytes. The virtual printer writes `.bin` and `.txt`. | — |
| E00-T05 OpenAPI skeleton | **Done** | OpenAPI 3.1 spec, Spectral lint in CI, and a contract test that validates real responses (it catches drift). The ErrorCode enum equals the domain. | — |
| E02-T04 Crash harness | **Done (Linux)** | 500 kills, 0 violations, 11,271 sales (`docs/perf/crash-harness.md`). The sabotage self-test fails as it should. Nightly runs 100 kills. | Windows run deferred |
| E07-T01 Keyboard wedge | **Done (Chromium)** | 27 unit tests and 6 Playwright tests in Chromium, 12 consecutive runs green. **This found a truncation bug** (a 41 ms delivery stall split a scan), fixed with stall tolerance (ADR-022). | WebView2 measurement deferred with Windows |
| E00-T06 Stage 0 vertical slice | **Done (Linux)** | Set up, scan through the wedge hook, atomic cash sale (sale, line, tender, movement, audit, outbox, receipt number), then the virtual-printer capture. Tested at store level, including faults at all 11 statements and the movement insert specifically, and over IPC on the mock runtime. Clicked through in the real app under Xvfb. | Runs from the Windows artefact: deferred. The sale event is outbox #5, not #1 as the task text said, because ADR-009 sends workspace and product events first. |

### Spike to delete later

The Stage 0 slice uses throwaway pieces, all marked `STAGE 0 SPIKE`, so the real catalogue,
cart and sale tasks can replace them without a migration:

- `spike_*` tables, created at runtime rather than by a migration;
- `pos_application::spike`;
- the `WriteOp::Spike*` variants;
- the `pos_local_db::spike` module;
- the `dev_demo_*` commands;
- `Stage0Demo.tsx`.

Delete them in E09-T08 at the latest.

### Next up (Stage 1 per [00-roadmap §4](00-roadmap.md#4-parallel-tracks-team-of-6))

E04-T01 (workspace creation), E05-T01/T02 (staff and PIN auth), E06-T01 (catalogue migration),
E06-T03 (barcode normalisation), E08-T01 (tax engine).
