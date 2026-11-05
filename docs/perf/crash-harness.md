# Crash-consistency harness results (E02-T04)

Run with `cargo xtask crash-harness --kills N`. Each kill sends SIGKILL (or
TerminateProcess on Windows) to a child that is committing multi-record "sales" through
`LocalStore::apply`. Half the kills are timed from process start, so some land while the
database is opening or migrating. After every kill the harness reopens the database and checks:

1. `PRAGMA quick_check` is `ok`.
2. **No partial sale.** Every sale has all 5 line records, all 5 outbox events and its audit
   row, or none of them.
3. **No lost acknowledged sale.** Every sale the child acknowledged (written to an fsynced
   file only after `apply` returned) is present.
4. **Gapless sequences.** Sales, receipt numbers and outbox `local_sequence` have no gaps,
   and the counters agree.

**Self-test.** `CRASH_HARNESS_SABOTAGE=split` makes the child commit each sale in two
transactions. The harness must fail, and it does on the first kill
("partial sale: 10 sales but 55 line records ...").

**Scope.** A killed process tests transaction atomicity and SQLite WAL recovery. It does not
test fsync durability under power loss: the page cache survives a process kill. Power-off
testing is E19-T02 (a hypervisor power-off of a Windows VM).

## Results

| Date | Platform | Kills | Violations | Sales committed / acknowledged | Duration |
|---|---|---:|---:|---|---|
| 2026-11-05 | Linux x86_64 (dev container), release build | 500 | 0 | 11,271 / 11,113 | 111 s |
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
