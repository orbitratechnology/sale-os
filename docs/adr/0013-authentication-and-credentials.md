# ADR-013: Authentication and credentials

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- **Staff PIN:** 4–8 digits, hashed with Argon2id (`m=64 MiB, t=3, p=1`) with a per-user salt. The originally proposed `m=19 MiB, t=2` measured only about 18 ms per verification on current hardware (E05-T02), below the 50 ms floor, so the cost was raised: about 135 ms measured. Stored hashes record their parameters, so the cost can be raised again later without breaking existing PINs. A 4–8 digit PIN cannot resist an offline attack on a stolen database whatever the cost; database encryption is evaluated in E19-T05.
  After 5 failures the account is locked for 30 s. The lockout doubles on each further failure,
  up to 15 min. Throttle state is device-local.
- **Staff PIN hashes sync** to the other registers of the same business, so a cashier can sign
  in on any register while it is offline. This is accepted because the hashes are Argon2id.
  Revisit if the D-02 compliance profile objects.
- **Cloud owner account:** email and password (Argon2id), behind an `IdentityProvider` trait so
  a managed OIDC provider can replace it (see D-09).
- **Device credentials:** a per-register refresh token (opaque, 256-bit, stored hashed on the
  server) and a short-lived access JWT (EdDSA, 15 min) carrying `business_id`, `register_id`
  and `device_id` claims. Refresh tokens rotate on every use, and reusing an old one revokes
  the device.
- The refresh token is stored in the **OS credential store** (Windows Credential Manager, via
  the `keyring` crate). It is never stored in SQLite, config files or the WebView.

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
