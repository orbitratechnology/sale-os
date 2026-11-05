# Backlog Status

Updated at the end of each working session. "Done" means every acceptance criterion was
checked. Anything that could not be checked yet is listed under **Open**, with the reason.

## Sprint 1 (started 2026-11-05)

| Task | Status | Evidence | Open |
|---|---|---|---|
| E00-T01 Monorepo scaffold | **Done** | Cargo and pnpm workspaces with the full crate layout; `cargo xtask check-deps` enforces the layering rules (`xtask/src/deps.rs` tests inject forbidden edges); README | A first build on a clean Windows checkout happens in CI |
| E00-T02 CI pipeline | **Written, not yet run** | `.github/workflows/ci.yml`: fmt, clippy, tests on Windows and Linux; check-deps; generated-contract drift; traceability; Postgres-backed cloud tests; cargo-deny; frontend; Windows NSIS/MSI artefact; `deny.toml` | Confirm on the first PR run: a warning blocks the PR, the GPL licence check fails, wall time is under 15 min |
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

### Next up (Sprint 2 per [00-roadmap §4](00-roadmap.md#4-parallel-tracks-team-of-6))

E02-T03 (`LocalStore::apply` + WriteSet), E02-T04 (crash harness), E03-T04 (error catalogue
docs), E13-T01 (ESC/POS spike), E00-T05 (contracts skeleton), E01-T03 (capability lockdown),
E01-T04 (logging + redaction), E07-T01 (keyboard-wedge spike), E00-T06 (Stage 0 vertical slice).
