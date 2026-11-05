# Stage 4 — Pilot Hardening

**Goal.** Make the MVP safe to put in a real shop. That means:

- validated performance on reference hardware
- crash and power-loss resilience
- backups and restore
- a security review
- signed installers and updates
- diagnostics
- usability validation

**Exit gate (PRD §17, §20).** The production pilot acceptance criteria pass on the documented
Windows hardware (D-05):

- all of AT-01…AT-13 are automated and green (`E19-T12`)
- the PRD §14 targets are met on the reference machine (`E19-T01`)
- the security review has no open high or critical findings (`E19-T05`)

**Rough size.** ≈ 35–50 engineer-days, plus lead time for the external penetration test and the code-signing certificate.

**Decisions needed at the start:** D-01, D-05, D-08.

---

## E19 Pilot hardening

### E19-T01 · Performance benchmark suite on reference hardware
- **Size** M · **Deps** E00-T04 (`bench50k`), Stage 2–3 · **PRD** §14
- **Scope:**
  - A reproducible benchmark run on the D-05 reference machine, using a dataset of 50k variants, 100k barcodes and 500k sale lines (generated).
  - Results are published to `docs/perf/<date>.md`.
- **Acceptance:** each target is met at p95 over at least 1,000 samples.

  | Metric | Target |
  |---|---|
  | Barcode lookup | < 50 ms |
  | Add item to visible cart | < 100 ms |
  | Product search | < 150 ms |
  | Cash-sale local commit | < 500 ms |
  | Cold launch | < 5 s |
  | 30-day sales report | < 1 s |
  | Sales list page | < 300 ms |

- [ ] A regression job reruns a reduced suite on CI weekly and alerts on a regression above 20%.

### E19-T02 · Crash and power-loss campaign
- **Size** M · **Deps** E02-T04, E09-T08, E12-T02, E17-T03 · **PRD** §14 (Crash recovery), §14.1, §16.3
- **Scope:**
  - Extend the crash harness to sale, return, inventory document, import chunk, migration, and outbox acknowledgement.
  - **Power-loss simulation:** a Windows VM hard reset (hypervisor power-off) during a commit loop, 50 cycles.
  - Disk-full simulation: a small VHD. The app must fail with `STORE_IO` and never corrupt the database.
- **Acceptance:**
  - [ ] Zero partial commits and zero integrity-check failures across all runs.
  - [ ] No sale reported as committed to the UI is lost after a VM power-off (validates `synchronous=FULL`).
  - [ ] When the disk is full, the sale is not saved, the cashier sees a clear "Cannot save — disk full" message, and the cart is preserved.

### E19-T03 · Local backup automation and restore
- **Size** M · **Deps** E02-T02 · **PRD** §13, §18 (user deletes or damages local data), §20
- **Scope:**
  - `BackupWorker` per ADR-007: hourly, daily, on shutdown and before migrations, with retention.
  - An optional secondary backup folder (USB/external drive).
  - A restore UI for the owner: list backups, verify with `integrity_check`, then restore. Restore keeps the current database aside as `pre-restore-*.db`.
  - **Post-restore sync safety:** after a restore, the outbox may hold events the server already has, or may be missing events the server has. The client re-reads the server `ack` and re-pulls its own register's facts from the cloud so that sales already synced are re-materialized locally. If the restored outbox sequence is behind the server's, the next sequence numbers are re-based from `ack + 1`, so no `event_id` collision is possible.
- **Acceptance:**
  - [ ] Restoring an older backup on a linked register yields a local database that includes every sale the cloud has from this register (re-pulled), with no duplicates in the cloud.
  - [ ] Restoring on an unlinked register shows a warning that sales after the backup time are lost, and the owner must confirm it.
  - [ ] A backup restore drill is documented and run (PRD §13).

### E19-T04 · Full business data export
- **Size** S · **Deps** E14-T04 · **PRD** §2.2 (7), §13, §20
- **Acceptance:**
  - [ ] An owner-only export produces a ZIP of CSV (all tables in [04](../04-local-database.md) except secrets: no `pin_hash`) plus a JSON manifest with the schema version and counts.
  - [ ] It is re-readable by a documented script (`docs/export-format.md`) and audited as `data.exported`.

### E19-T05 · Security review
- **Size** M · **Deps** Stage 3 · **PRD** §13, §18
- **Scope:**
  - A STRIDE threat model of the desktop app, the IPC, the local database, sync, and the cloud.
  - Audits of the Tauri capabilities, CSP and updater signing.
  - PIN brute-force on a stolen database (offline attack cost with the Argon2id parameters, documented); this feeds the decision on whether to evaluate SQLCipher (§13: "evaluate database encryption against the threat model").
  - `cargo audit` and `pnpm audit` clean.
  - An external API penetration test focused on tenancy and auth.
  - Log redaction review.
  - A PCI scope statement: confirm that no PAN, CVV, PIN or track data is stored or logged (grep the logs and database after the e2e suite).
- **Acceptance:**
  - [ ] There is a threat model document. All high and critical findings are fixed or formally accepted by the product owner.
  - [ ] A test runs the full e2e suite with a known test PAN typed into every free-text field, then asserts the PAN does not appear in the database, logs or outbox (the validation rejects it in reference fields, and other fields are checked for Luhn runs before logging).
  - [ ] A decision record on local database encryption exists.

### E19-T06 · Signed installers and auto-update
- **Size** M · **Deps** E01-T01, D-01 · **PRD** §13 (signed updates), §17 Stage 4
- **Scope:**
  - An Authenticode code-signing certificate (EV or OV) and signed MSI/NSIS packages.
  - The Tauri updater with an Ed25519 update-signature key held in CI secrets.
  - An update channel (`stable`, `pilot`).
  - **Updates never start during an open cart.** They are offered at session close or idle, and a migration backup is taken first.
- **Acceptance:**
  - [ ] SmartScreen shows the publisher name.
  - [ ] An update from N-1 to N on a pilot machine migrates the database and keeps all data, with the pre-migration backup present.
  - [ ] A tampered update package is rejected (signature test).
  - [ ] A rollback procedure is documented: reinstall N-1 and restore the pre-migration backup.

### E19-T07 · Diagnostics bundle
- **Size** S · **Deps** E01-T04, E11-T06 · **PRD** §14 (Observability), §17 Stage 4
- **Acceptance:**
  - [ ] `diagnostics_export` produces a ZIP with the redacted logs (7 days), app/OS/WebView2 versions, the schema version, the `integrity_check` result, projection drift, sync state and counts, print-job states, settings (non-secret), and the last 100 audit actions (IDs only, no customer data).
  - [ ] A test asserts the bundle contains no PIN hashes, tokens or full payloads.

### E19-T08 · Usability and accessibility validation in shops
- **Size** M · **Deps** Stage 2 UI · **PRD** §17 Stage 4, §5.1, §14 (Accessibility)
- **Scope:**
  - Sessions with at least 5 cashiers across at least 3 shop types (grocery, clothing, hardware).
  - Tasks: first sale, scan burst, weighed item, split tender, return, unknown barcode, lock/unlock.
  - Measure task time and errors.
- **Acceptance:**
  - [ ] A first-time cashier completes a 5-item cash sale in under 60 s after a 5-minute introduction.
  - [ ] All issues of severity ≥ "major" are fixed or ticketed before the pilot.
  - [ ] A WCAG 2.2 AA audit of the core cashier flows passes for contrast and keyboard access.

### E19-T09 · Version policy enforcement
- **Size** S · **Deps** E16-T04 · **PRD** §10.1 (app version policy)
- **Acceptance:**
  - [ ] Below `minSupported`, the server answers `426 SYNC_UPGRADE_REQUIRED`. The desktop keeps selling locally, queues events, and shows an update prompt.
  - [ ] No local data is lost while waiting for the update.

### E19-T10 · Data retention and deletion
- **Size** S · **Deps** D-02, D-06 · **PRD** §13 ("define data retention and deletion behaviour before production launch")
- **Acceptance:**
  - [ ] There is a written policy, reviewed by legal, for the D-02 country. It covers local retention of sales and logs, cloud retention, and account deletion (export, then delete, with a grace period).
  - [ ] Tooling exists: an owner "delete cloud business" request flow (soft delete, then hard delete after N days by a worker job), and local log retention is enforced.

### E19-T11 · Pilot runbook and support process
- **Size** S · **Deps** E19-T03, E19-T07
- **Acceptance:**
  - [ ] Runbooks cover install, first run, linking, the daily close, restore from backup, a replaced PC (new install joins and bootstraps; old device revoked), revoked device recovery, the "sync attention needed" playbook, and how to collect diagnostics.
  - [ ] There is an escalation path and an on-call rota for the pilot period.

### E19-T12 · MVP acceptance suite (AT-01…AT-13) automated
- **Size** M · **Deps** all · **PRD** §16.4, §20
- **Acceptance:**
  - [ ] Every AT in [08 §3](../08-test-strategy.md#3-mvp-acceptance-tests-prd-164--automation-mapping) is implemented as a named, automated test and runs green on the Windows reference machine and in CI.
  - [ ] The report is attached to the pilot go/no-go decision.
