# Restore drill (E19-T03, PRD §13)

How to bring a till back from a backup, and how to rehearse it. Run the drill once before a
shop goes live and again after any change to backups or the database.

## Where backups are

| | Where | Kept |
|---|---|---|
| Daily | `<app data>/backups/pos.daily.<time>.db` | 14 |
| Hourly, when anything changed | `pos.hourly.<time>.db` | 24 |
| When the app closes | `pos.shutdown.<time>.db` | 5 |
| Before a database upgrade | `pos.pre-migrate-v<from>-v<to>-<time>.db` | 10 |
| Made by hand ("Back up now") | `pos.manual.<time>.db` | 20 |
| The database a restore replaced | `pos.pre_restore.<time>.db` | 5 |

`<app data>` is `%APPDATA%\dev.saleos.pos` on Windows. Each backup has a `.json`
description beside it (when, why, schema version, last outbox sequence, number of sales).
When the owner has chosen a **second folder** (Settings → Backups), every new backup is also
copied there, such as a USB stick that goes home at night. A backup is written under a temporary
name and renamed when complete, so a file named like a backup is always whole.

## Restoring (the owner, in the app)

1. Settings → **Backups**. Pick the backup from just before the problem; **Check** it (a full
   `integrity_check`). "Damaged" or "From a newer version" backups cannot be restored.
2. **Restore…**, read the warning, tick "I understand that sales … after <time> will be lost",
   **Restore and restart**.
3. The app keeps the database it replaces as a "Before a restore" backup, puts the backup in
   place, records `backup.restored` (who, which backup, what was kept), and restarts. Sign in
   again.
4. Check: the sales history ends at the backup's time; stock and the drawer match it.

**A till linked to the cloud cannot restore yet.** Its sales after the backup are in the cloud
and must come back first (E19-T03 part 2). Until then: if the database is damaged, contact
support. Do not copy files by hand.

**Undoing a restore:** restore the "Before a restore" backup the same way.

## If the app does not start

The app's own restore screen needs the app to open. If the database itself will not open:

1. Close Sale OS.
2. In `<app data>`, move `pos.db`, `pos.db-wal` and `pos.db-shm` into a new folder named
   `damaged-<date>` (keep them for support; never delete them).
3. Copy the chosen backup to `<app data>\pos.db` (from `backups\` or the second folder).
4. Start Sale OS. An older backup is upgraded on start-up, after its own pre-upgrade backup.

## The drill

Automated, run on every CI build:

- `crates/pos-local-db/tests/backup.rs`
  - Back up after 2 sales, sell 2 more, restore: 2 sales remain and the database passes
    `integrity_check`.
  - The replaced database is kept as a pre-restore backup with all 4; restoring it brings
    them back.
  - A damaged backup and one from a newer app are refused without touching the live
    database.
  - Retention keeps 24 hourly backups; copying to a second folder gives a whole, checkable
    copy.
- `apps/desktop-tauri/tests/permissions.rs`
  (`a_restore_puts_the_till_back_to_the_backup_and_records_it`): the same over the app's
  commands, as the owner. It is refused until the loss is confirmed; afterwards the product
  made after the backup is gone, `backup.restored` is recorded and the replaced database is
  kept.
- Browser tests (`e2e/cloud.e2e.ts`, "backups"): the owner's page, with "Back up now", the
  second folder, Check, the confirmation, and restore unavailable on a linked till.

By hand, on the pilot machine before go-live (record the date and the result below):

1. Make three sales. **Back up now**. Make two more sales.
2. Choose a second folder on a USB stick; check a backup appears there.
3. Restore the manual backup. After the restart, sales history shows three sales.
4. Restore the "Before a restore" backup. Five sales again.
5. Close the app, remove `pos.db`, copy the latest backup from the USB stick to `pos.db`
   ("If the app does not start"), start: the till opens with the backup's sales.

| Date | Machine | Who | Result |
|---|---|---|---|
| 2026-11-18 | Linux CI container (automated steps) | CI | Passed |
| — | Pilot Windows machine | — | Not yet run |
