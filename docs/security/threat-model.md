# Sale OS threat model and security review (E19-T05)

- **Date:** 2026-11-18
- **Scope:** the desktop app (Tauri 2 + React WebView), its IPC, the local SQLite database and
  backups, sync, and the cloud API (Axum + PostgreSQL with row-level security).
- **Method:** STRIDE per component, then the audits the task lists: Tauri capabilities, CSP,
  updater signing, offline PIN attack cost, dependency advisories, log redaction, card data.
- **Not done here:** an external penetration test of the cloud API (it needs a third party; see
  F-10). Everything below was checked against the code and its tests on this date.

## 1. System and trust boundaries

```text
 Cashier / owner ──▶ WebView (React) ──IPC──▶ Rust core ──▶ SQLite (pos.db, WAL, backups/)
                     untrusted UI      ▲      trusted       │
                                       │                    ├─▶ second backup folder (USB)
                         capability + CSP                   └─▶ HTTPS ──▶ cloud API ──▶ PostgreSQL (RLS)
                                                                ▲
                                     OS keychain: device refresh token
```

| Boundary | What crosses it | Control |
|---|---|---|
| WebView → Rust | IPC commands with JSON arguments | Only the app's own commands are allowed (capability `main`); the actor comes from the Rust session, never from arguments; each command checks its permission first; the card-data guard checks every argument |
| Rust → disk | SQLite, backups, exports, logs | Files in the Windows user's app-data folder; network paths refused; exports and backups only where the owner picks in the system dialog |
| Till → cloud | Events, pulls, reports, sign-in | HTTPS only (plain HTTP refused except to this computer); short-lived access tokens in memory; a rotating refresh token in the OS keychain, stored hashed in the cloud; no redirects followed |
| Cloud API → PostgreSQL | Every query | Row-level security keyed on the token's business; isolation tests (AT-12) on every secured route |

## 2. Assets

| Asset | Where | Why it matters |
|---|---|---|
| Sales, returns, cash, stock, prices | Till database, backups, exports, cloud | Business records; tax evidence; fraud if altered |
| Staff PIN hashes (Argon2id) | Till database (`staff_users`), outbox payloads, cloud | A cracked PIN lets someone act as that person at a till |
| Owner cloud password | Never stored on the till; Argon2id hash in the cloud | Owns the business in the cloud |
| Device refresh token | OS keychain on the till; SHA-256 hash in the cloud | Lets a till sync as that register |
| Audit log | Till database and cloud | Accountability for voids, refunds, overrides |
| Card data | **Nowhere**, by design (PRD §13) | PCI scope |

## 3. STRIDE

| Component | Threat | Mitigation in place | Residual |
|---|---|---|---|
| **IPC** | *Spoofing* another user | Actor taken from the unlocked session in Rust; PIN sign-in with lockout (5 failures → 30 s, doubling) | — |
| | *Elevation*: a WebView compromise calls owner commands | Each command authorizes against the session's role before anything else; the permission matrix test covers every command (E05-T03) | A compromised WebView acts with the signed-in user's rights |
| | *Tampering*: arguments shaped to bypass checks | Typed deserialization; validation in the domain; a refused argument never reaches the command | — |
| | *Information disclosure*: card numbers typed into text fields | **F-01, fixed:** IPC card-data guard | — |
| **WebView** | Script injection (XSS) | CSP `script-src 'self'`, no remote content, `connect-src` IPC only, `object-src 'none'`, `frame-ancestors 'none'`, prototype frozen; React escapes text; CSV exports defuse formulas | `style-src 'unsafe-inline'` (F-08) |
| | File-system or shell access | No `fs`, `shell`, `http` or `dialog` permission in the WebView; dialogs opened by Rust, which never takes a path from the WebView | — |
| **Local database** | *Information disclosure*: stolen PC, disk or backup stick | Windows profile ACLs; no card data; PINs hashed (Argon2id, 64 MiB) | **F-05**: the database and backups are not encrypted by the app (ADR-023) |
| | *Tampering*: someone edits `pos.db` directly | Outbox events are immutable once acknowledged, and the cloud keeps its own copy; stock verify/rebuild finds ledger drift; audit rows | A local admin can still alter unsynced data |
| | *Denial of service*: disk full, corruption | Disk-full handling, crash-safe transactions, integrity-checked backups, restore (E19-T03) | — |
| **Sync** | *Spoofing* a register | Device credentials issued at linking; refresh tokens rotate and are revocable; revoked devices stop syncing and forget them | — |
| | *Tampering* in transit | TLS (rustls, OS roots); **F-02, fixed:** HTTPS enforced; **F-03, fixed:** no redirects | — |
| | *Repudiation* | Every event carries register, actor and sequence; inbox is idempotent | — |
| | Replay of old events | Idempotent by event id; per-register sequence; restore replay re-creates only this register's own events | — |
| **Cloud API** | *Elevation* across tenants | Row-level security on every tenant table; `SET LOCAL` business per transaction; AT-12 isolation tests per route; schemathesis contract fuzzing | **F-10**: no external pen test yet |
| | Credential stuffing on sign-in | Per-email and per-IP rate limits; the same answer and timing for a wrong password and an unknown email | — |
| | *Information disclosure* in errors | Problem+JSON with codes; another business's records answer as not found | — |
| **Supply chain** | Vulnerable dependencies | `cargo deny` (advisories, licenses, sources) in CI; pnpm audit clean (**F-04, fixed**) | — |
| | Malicious update | No updater yet; E19-T06 adds signed updates | **F-09**: release blocker, tracked in E19-T06 |

## 4. Findings

| ID | Severity | Finding | Status |
|---|---|---|---|
| F-01 | High | A card number typed into any free-text field (product name, note, address…) would have been stored, synced and possibly logged. Only tender and refund references were checked. | **Fixed.** Every IPC argument is checked before the command runs (`card_guard.rs`); refused with `CARD_DATA_REJECTED`. Test: 195 text argument positions, then the database, WAL, backups and logs are scanned clean. |
| F-02 | Medium | The cloud client accepted an `http://` base URL, which would send tokens and passwords in clear text if `POS_CLOUD_URL` were mistyped. | **Fixed.** HTTPS required; plain HTTP only to loopback (development and tests). |
| F-03 | Low | The cloud client followed redirects. | **Fixed.** Redirects are not followed (the API never redirects). |
| F-04 | Low (dev tools only) | `pnpm audit`: lodash (high, code injection in `_.template`) via spectral-cli; minimatch (moderate, ReDoS) via eslint. Neither ships in the app. | **Fixed** with pnpm overrides; `pnpm audit` and `cargo deny check` clean. |
| F-05 | High | The till database, its backups and the second-folder (USB) copies were plain SQLite. A stolen PC, disk or stick exposed the business's records and the staff PIN hashes, and a 4–6 digit PIN falls to an offline attack in seconds to hours (§6). | **Fixed (2026-11-19), ADR-023.** SQLCipher 4 with a 256-bit key in the OS keychain; backups encrypted with the same key; plain databases and backups converted on first start; an owner recovery key (shown in Settings → Backups, audited); a locked-data screen to type it. Residual: the keychain is only as strong as the Windows account, the owner must keep the recovery key, and the full data export is plain by design. |
| F-06 | Medium | PIN hashes sync to the cloud and to every register (so any staff member can sign in on any till offline), so one stolen till exposes every PIN hash. | **Accepted in ADR-013.** A PIN only works physically at a till; cloud owner actions need the owner's password. Re-assessed here: unchanged, given F-05's controls. |
| F-07 | Low | `dev_money_add`, a Stage 0 demo command, is still registered in release builds. It is a pure calculation with no data access. | Open (low): remove with the next IPC cleanup. |
| F-08 | Low | CSP allows inline styles (`style-src 'unsafe-inline'`), needed by component style attributes. Inline *scripts* stay blocked. | Accepted (low). |
| F-09 | — | No auto-updater yet, so there is no signing to audit. Updates must be signed before release. | Tracked in E19-T06 (release blocker). |
| F-11 | Medium | RUSTSEC-2026-0459: the bundled SQLite (before 3.54) can return stale process memory when a database is opened beside a crafted `-wal`. No `sqlx` release takes the fixed `libsqlite3-sys` yet, and the backup check and restore opened backups in place, which could be on a stick someone else wrote. | **Mitigated (2026-11-19).** Backups from elsewhere are checked as a private copy of the single file; restore copies the file into the data folder first, then checks and describes that copy. Test: a stranger's `-wal` beside a backup is never read. Remove the `deny.toml` exception when `sqlx` moves to `libsqlite3-sys` 0.39. |
| F-10 | — | No external penetration test of the cloud API's tenancy and auth. In-house: RLS, AT-12 isolation on every route, contract fuzzing, auth tests. | **Open: needs a third party** before the cloud goes beyond the pilot. |

## 5. Audits

**Tauri capabilities** (`apps/desktop-tauri/capabilities/main.json`). One capability, for the `main`
window only. It holds `core:event:default` plus one `allow-<command>` per app command; `build.rs`
generates the list from `command_names.rs`, and the ACL test (E01-T03) refuses any command
missing from it. There is no `fs`, `shell`, `http`, `path`, `dialog` or `opener` permission.
Remote URLs get no capability.

**CSP** (`tauri.conf.json`). `default-src 'self'`; `script-src 'self'`; `connect-src ipc:
http://ipc.localhost` (the WebView cannot reach the network: the cloud is called from Rust);
`img-src 'self' data:`; `object-src 'none'`; `base-uri 'self'`; `form-action 'none'`;
`frame-ancestors 'none'`; `freezePrototype: true`. Only F-08 is a gap.

**Updater signing.** None yet (F-09).

**Log redaction** (`crates/pos-observability`):
- Fields whose names contain `pin`, `password`, `token`, `secret`, `authorization`, `cookie`,
  `api_key`, `cvv`, `pan` or `card_number` are written as `[REDACTED]`.
- Luhn-valid 14–19 digit runs in any logged text are masked to `[CARD …1234]`.
- No request or IPC bodies are logged, on the till or in the cloud.
- An IPC error logs its code and message, never its arguments. The card-data refusal logs the
  field name only.
- The sweep test confirms the logs hold no card number after 195 attempts.

**Dependencies:**
- `cargo deny check`: advisories, bans, licenses and sources all ok.
- `pnpm audit`: no known vulnerabilities, after F-04's overrides.

## 6. Offline PIN attack cost

The PIN is 4–8 digits, hashed with Argon2id at m = 64 MiB, t = 3, p = 1 with a per-user salt
(ADR-013). One verification measured about 135 ms on one core. The per-user salt means each
PIN must be attacked on its own.

| Attacker | Rate (guesses/s, estimate) | 4 digits (10⁴) | 6 digits (10⁶) | 8 digits (10⁸) |
|---|---|---|---|---|
| One core | 7 | 24 min | 40 h | 160 days |
| 16-core desktop (memory-bound) | ~100 | 100 s | 2.8 h | 12 days |
| High-end GPU (64 MiB per lane) | ~1,000 | 10 s | 17 min | 28 h |

These are worst cases (the whole space); the average is half. Conclusion: **the hash slows an
attacker but does not stop one.** A stolen database means its PINs are known within hours.
Protecting the hashes means protecting the file (F-05, ADR-023). Online guessing at a till is
bounded by the lockout.

## 7. PCI scope statement

Sale OS does not store, process or transmit cardholder data:
- Card payments are taken on a separate, standalone terminal. Sale OS records only the tender
  type, the amount and an optional terminal reference (approval code), which is refused if it
  looks like a card number (PAY-005).
- No PAN, CVV, PIN block or track data is stored or logged:
  - The IPC guard refuses card numbers and swipes in every argument.
  - The log layer masks Luhn-valid runs.
  - The sweep test scans the database, WAL, backups and logs after typing a test PAN
    everywhere.
- Staff "PINs" are Sale OS sign-in PINs, not payment PINs.

With a standalone terminal, the till is outside the cardholder data environment. That should
be confirmed with the acquirer when an integrated terminal is considered (it would change this
statement).

## 8. Re-review

Repeat when any of these changes:
- a new IPC command that takes free text (the sweep covers it automatically once it is in the
  permission table);
- the sync protocol or cloud authentication;
- an integrated payment terminal;
- the updater (E19-T06).
