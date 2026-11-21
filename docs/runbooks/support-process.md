# Pilot support process (E19-T11)

> **To fill in before the pilot:** names, phone numbers and the rota below. Nothing else in
> this file depends on them.

## Levels

| Level | Who | Handles | Answers within |
|---|---|---|---|
| 1 | Shop owner or manager | Pilot runbook §4–§9: daily close, conflicts, revoked device, locked database (with the recovery key), diagnostics | — |
| 2 | Pilot support (on call) | Anything the runbook does not fix; reads the diagnostics file; can revoke devices and look at the cloud | 1 hour in shop hours |
| 3 | Engineering | Suspected bugs, data damage, a refused event that should be accepted, cloud outages, security incidents | 4 hours; at once for data loss or security |

## When to escalate straight to level 3

- **"The local database looks damaged"** (`STORE_CORRUPT`), or the app does not start.
  Do not delete or move anything: see restore-drill.md, "If the app does not start".
- A suspected **card number in Sale OS** (typed somewhere it should not be), or any other
  security concern.
- **Sales missing** from history or from the cloud.
- **Stock drift** reported in diagnostics that a rebuild does not clear.
- **The cloud is down** for every till (status "Offline" everywhere while the internet works).

## What to send

1. The diagnostics ZIP (status bar → Save diagnostics…).
2. The till code (status bar).
3. The time it happened.
4. What the person did and what they saw. A photo of the screen is fine.

Never send:
- PINs or the owner's cloud password;
- the recovery key (support never needs it: the owner types it into the till);
- full data exports (unless the owner chooses to, for a specific reason).

## On-call rota (pilot period)

| Week of | Level 2 (name, phone) | Level 3 (name, phone) |
|---|---|---|
| _to fill in_ | | |

## After an incident

Write down, within two working days:
- what happened and the time line;
- the cause;
- what was done, and what changes so it does not happen again (a test, a fix, a runbook
  line).

Link the diagnostics file in the record, not its contents.
