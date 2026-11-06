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

## Stage 1, backend first (2026-11-05)

Domain and storage are built and tested before the IPC commands and screens, so each screen
lands on a finished, tested service.

| Task | Status | Evidence | Open |
|---|---|---|---|
| E06-T03 Barcode normalisation | **Done** | UPC-A, UPC-E and GTIN-14 normalise to EAN-13; GS1 check digits; uniqueness per normalised code is enforced in the store | — |
| E08-T01 Tax engine | **Done** | `price_cart` for inclusive and exclusive modes; multiple rates per category; effective dates by `business_date`; tax breakdown. The six golden scenarios and the inclusive-total proptest pass. | The 40-scenario JSON suite is E08-T03 |
| E05-T01/T02 Staff and PIN sign-in | **Done** | Argon2id (64 MiB, t=3; p95 141 ms, ADR-013 updated); lockout 5 attempts then 30 s doubling to 15 min; sessions; display-name rules; owner-only PIN reset | Login UI is E05-T06 |
| E06-T01 Catalogue migration | **Done** | `0003_catalogue.sql` from the spec, with FTS5 search; schema tests | — |
| E04-T01 Workspace creation | **Done** | `SetupService::create_workspace` writes business, location, register, owner, tax rates and settings in one transaction, and binds the device to the register | Wizard UI is E04-T02 |
| E06-T02 Product aggregate | **Done** | Create, update (optimistic version) and archive; barcode and SKU uniqueness; outbox events with `base_version` | — |
| E09-T01 Cart backend | **Done** | `0004_carts.sql`; `CartService`: add, set quantity, remove, discounts, override, clear and void. 0 and fractional `ea` quantities refused; stale `clear` gives `CART_REVISION_MISMATCH`; one open cart per register; carts never reach the outbox. **Found and fixed a lost-update race**: 20 concurrent scans lost items. Mutations are now serialised, and the store compares the revision. | Discount limits and approvals are E08-T02/E05-T04 |
| E07-T02 Scan pipeline | **Backend done** | `CartService::scan`: the scan event is recorded in the same transaction as the cart change, so a repeated delivery counts once; 10 scans = 10 items (AT-03); pack + unit = 13; a discounted line gets a new line; unknown codes change nothing; malformed codes give `SCAN_INVALID` | `SESSION_LOCKED` needs E05-T05. The p95 < 30 ms with `shop5k` needs the E06-T04 fixture. |
| Stage 1 IPC commands | **Done (backend)** | 28 commands for bootstrap, setup, sign-in, staff, catalogue, scan and cart; bindings regenerated; one ACL permission per command. The session lives in Rust, and the actor is never an argument. **Found and fixed a sign-in bug**: a mistyped PIN on top of a session signed the current user out. | `cart://changed` and `session://locked` events land with the checkout screen and E05-T05 |
| E05-T03 Permission harness | **Done for existing commands** | `tests/permissions.rs`: an unclassified command fails the build. Every command is called over IPC as owner, manager and cashier and checked against the matrix; after logout, everything needing a session gives `SESSION_LOCKED`. Verified it fails when a check is weakened. | AT-11 names `inventory_post_document`, `settings_update` and `return_create`, which do not exist yet; the harness covers each automatically when it is added. Approval cells now give `APPROVAL_REQUIRED` (E05-T04). |
| E09-T02 Cart recovery | **Partly done** | The open cart is reloaded from storage and repriced against the current catalogue after a restart (tested with a new service instance) | The Resume/Discard prompt, and highlighting changed prices (needs a price-at-add snapshot) |

### Screens (2026-11-06)

Tested in Chromium against a recording fake backend (`e2e/screens.e2e.ts`); the Rust behind
each command is covered by the Rust and IPC tests. Also clicked through the real app under
Xvfb (real Rust, WebKitGTK): setup → checkout → unknown scan → lock.

| Task | Status | Evidence | Open |
|---|---|---|---|
| E04-T02 First-run wizard | **Partly done** | Business, region (OS-locale defaults), tax (mode + up to 3 rates), owner PIN, review; completed by keyboard only in an e2e test; sandbox banner; "not linked" explainer | Logo, the optional cashiers step, "add products now / import", the receipt preview, and the Live switch with owner PIN and `mode.changed` audit |
| E05-T06 Sign-in UI | **Partly done** | PIN pad by mouse, touch and keyboard; tries left and a lockout countdown from typed `details`; forced PIN change after a reset | Staff management screens; `ApprovalDialog` comes with E05-T04 |
| E09-T03 Checkout layout | **Done (Chromium)** | 40 lines at 1366×768: table, totals and actions visible, no page scroll; total 56 px at ≥ 7:1; inline qty/discount/remove; highlight ≤ 150 ms with no transition; clear and void confirm | WebView2 check with the Windows work |
| E09-T04 Search and quick-select | **Partly done** | Search as you type (80 ms), ↑/↓/Enter adds, price and SKU shown | Quick-select grid, variant picker, on-hand stock (Stage 2) |
| E09-T05 Key map and accessibility | **Mostly done** | `docs/keyboard.md`; keyboard-only e2e for scan 3 / qty / discount / remove; axe-core has no serious or critical issues; 150% text at 1920×1080 | "Open pay" waits for E10. **Deviation:** `+`/`−` are the numpad keys, because main-row `-` can be scanner input (see the doc) |
| E09-T06 Status bar | **Mostly done** | User and role, register code, SANDBOX/LIVE, "Scanner ready" vs typing, "Not linked" | `scanner://status` for serial scanners (Stage 5) |

**Bugs found while building the screens:** newer Chromium returns a Promise from
`scrollIntoView`, which broke a brace-less React effect; `autoFocus` on a reused button meant
the wizard could not finish from the keyboard; debug builds took seconds per PIN hash (argon2
is now optimised in the dev profile).

### Approvals and discount limits (2026-11-06)

| Task | Status | Evidence | Open |
|---|---|---|---|
| E05-T04 Manager approval tokens | **Done** | Single use (taken from the registry before the command; put back only if the command fails for another reason); expires after 120 s on a fake clock; scoped to permission + cart + rate, so another cart or a larger discount gives `APPROVAL_INVALID`; the approver must hold the permission and is never the requester; the approver PIN has the sign-in lockout; recorded as `authorized_by` and `approver_user_id`, and in `auth.approval_granted` | Returns (`ReturnCreate`) use it when E12 lands |
| E08-T02 Discount engine and permissions | **Done** | 10.00 across 3.33/3.33/3.34 allocates exactly; over-large discounts give `PRICING_DISCOUNT_EXCEEDS`; a cashier at 15% with a 10% limit gets `APPROVAL_REQUIRED` and succeeds with a manager token; an approved free item has zero tax; `cart.discount_applied` records before and after. The limit applies to the effective rate, so fixed amounts are covered. A reason is required above the limit and for price overrides. | — |
| E05-T06 `ApprovalDialog` | **Done** | Reusable: any `APPROVAL_REQUIRED` opens it, a manager or owner enters their PIN, and the original command is retried with the token (browser test) | Staff management screens |

**Bug found:** checkout's scan capture kept listening while the approval PIN pad was open, so a
fast-typed PIN was submitted as a barcode. Scan capture now pauses while a dialog owns the
keyboard.

### Next up

Product editor and list (E06-T05/T06) so products can be created in the app; then E10
payment and E09-T08 checkout, which also retires the Stage 0 spike.

**Build note:** a debug build with full debug info fills the session disk (28 GB). Use
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` (2 GB) for local test runs.
