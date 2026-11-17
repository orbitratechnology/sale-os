# Crash-consistency harness results (E02-T04)

Run with `cargo xtask crash-harness --kills N`. Each kill sends SIGKILL (or
TerminateProcess on Windows) to a child that sells through the **real services**: it sets up a
shop once (`SetupService`), creates three products (two stock items and a service), then loops:
scan one to three items into the cart (`CartService::scan`) and complete the sale in cash
(`CheckoutService::complete_sale`). Half the kills are timed from process start, so some land
while the database is opening, migrating or being set up. After every kill the harness
reopens the database and checks:

1. `PRAGMA quick_check` is `ok`.
2. **No partial sale.** Every sale has its lines, tenders, stock movements for stock items,
   `sale.completed` event (carrying its own receipt number) and audit row; and no line,
   movement, event, audit row or completed cart exists without its sale. Lines and tenders
   add up to the sale total.
3. **No lost acknowledged sale (AT-05).** Every sale the child acknowledged (its receipt
   number written to an fsynced file only after `checkout_complete` returned, which is when
   the cashier sees "completed") is present after the restart, with its pending outbox event.
4. **Gapless sequences.** Receipt numbers and outbox `local_sequence` have no gaps, and the
   counters agree.
5. **Projections agree.** Every stock balance equals the sum of its movements; at most one
   cart is open.

**Self-test.** `CRASH_HARNESS_SABOTAGE=orphan` makes the child allocate a receipt number
outside a sale after each sale. The harness must fail, and it does on the first kill
("receipt number gap: position 2 holds 3").

**Scope.** A killed process tests transaction atomicity and SQLite WAL recovery. It does not
test fsync durability under power loss: the page cache survives a process kill. Power-off
testing is E19-T02 (a hypervisor power-off of a Windows VM).

## Results

| Date | Platform | Kills | Violations | Sales committed / acknowledged | Duration |
|---|---|---:|---:|---|---|
| 2026-11-05 | Linux x86_64 (dev container), release build, Stage 0 generic write sets | 500 | 0 | 11,271 / 11,113 | 111 s |
| 2026-11-07 | Linux x86_64 (dev container), debug build, **real checkout** (2,215 lines) | 300 | 0 | 1,148 / 1,131 | 467 s |
| — | Windows 11 | — | — | — | Pending (Windows work deferred) |

Committed sales outnumber acknowledged ones because a kill can land after a commit and before
its acknowledgement is written. That is allowed. The reverse, an acknowledged sale that is
missing, would be a violation.

**Harness bug found and fixed during the first run.** `writeln!` on an unbuffered file
issues two write calls, so a kill between them left `160` without its newline. The next
child's `161` then merged into `160161`, which looked like a missing sale. The fix: write each
acknowledgement in one call, repair an unterminated last line on child start, and count only
complete lines.

The nightly workflow (`.github/workflows/nightly.yml`) runs 100 kills plus the
10,000-apply stress test.
