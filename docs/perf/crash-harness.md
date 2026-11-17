# Crash-consistency harness results (E02-T04, E19-T02)

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

## E19-T02: every kind of write, upgrades, and a full disk

**More than sales.** The child now cycles through everything a till writes:
- sales;
- returns of one unit of a recent sale (`ReturnService`, cash refund, restocked);
- goods receipts (`StockService`);
- the cloud acknowledging the outbox (the real `SyncWorker`, against an in-process cloud that
  accepts every event, so the kill can land inside `record_push`);
- now and then a 1,200-product CSV import (`ImportService`, three chunks of 500).

About one kill in four waits up to 3 s, so imports get to commit and some are killed
mid-commit. The next child carries on an interrupted import, as the app does. Each kind is
acknowledged (fsynced) only after its command returned, and the verifier adds:
- a return with its lines, refunds that add up, `return.completed` event (with its number),
  audit row and restock movements; nothing of a return without the return; never more
  returned than sold;
- a stock document with its movements, event and audit row;
- return and document numbers gapless, agreeing with their counters;
- imports in whole chunks: every committed row's product exists, every imported product's
  row is marked committed, every product has its `product.upserted` event, and a job marked
  committed has nothing left;
- acknowledgements: no event at or below the acknowledged sequence still pending, none above
  it marked applied, and the acknowledged sequence never goes back;
- every acknowledged return, document and import present.

**Upgrades** (`crash-harness migrate --kills N`). A populated database is built at head (3,600
imported products and a few seconds of trading), then taken back to schema 13 by undoing
migrations 0014 and 0015 by hand. 0014 includes a data migration that rebuilds the search
index rows. The harness times one upgrade from process start, then repeatedly copies the
template, starts the app on it and kills it at a random moment across that time. Each time
the app must open again, finish the upgrade and keep every count (sales, lines, returns,
documents, products, variants, outbox, audit, search rows, stock), with every search row
mapped, and pass all the checks above.

**A full disk** (`crash-harness disk-full --dir D`, D on a small file system). The till trades
for a moment; the disk is then filled except 4 MB, and the till trades until it stops. It
must stop with "the disk is full" (`STORE_DISK_FULL`), not anything else. With space
freed, every check above must pass and the same till must trade again.

**Found and fixed: one full disk stopped every later save.** On a full disk SQLite rolls the
transaction back by itself. sqlx's own `ROLLBACK` then fails, and sqlx goes on counting a
transaction that no longer exists. The database has one writing connection, so every later
write failed ("begin_with at non-zero transaction depth") until the app restarted, even after
space was freed. The writer pool now closes a connection returned in that state and opens a
fresh one (`after_release` in `LocalDb::open`). `tests/disk_full.rs` reproduces it on any OS
(SQLite's `max_page_count` gives the same SQLITE_FULL): the refused sale leaves nothing, the
cart is kept, the database is sound, and the same cart then completes. A full disk was also
reported as the generic `STORE_IO`; it is now `STORE_DISK_FULL`, shown as "Cannot save — disk
full. Nothing was saved and the cart is kept…" (browser test).

| Date | Campaign | Platform | Result |
|---|---|---|---|
| 2026-11-17 | `run`, 300 kills (sales, returns, documents, imports, acknowledgements) | Linux x86_64, release | 0 violations; 2,416 sales (2,399 acknowledged), 348 returns, 383 stock documents, 26 imports of 1,200 products, 33,166 events acknowledged by the cloud; 31 min |
| 2026-11-17 | `migrate`, 50 kills during an upgrade v13 → v15 (6,003 products) | Linux x86_64, release | 0 violations; at the kill, 43 databases still at v13 (before or inside the upgrade's transaction) and 7 at v15; none landed between the two migrations |
| 2026-11-17 | `disk-full` on a 24 MB tmpfs, 5 runs | Linux x86_64, debug | Stopped with "the disk is full" every time (after 0 to 5 more sales: what filled the disk varies, a sale or an import); nothing partial; 13 to 21 sales once space was freed |
| — | Power-off of a Windows VM (hypervisor reset during a commit loop, 50 cycles) | Windows 11 | **Not run**: needs a Windows VM and a hypervisor, which this environment does not have |
| — | Disk full on a small VHD | Windows 11 | **Not run** (the same `disk-full` mode, on a mounted VHD) |

**What a process kill does not test:** fsync durability under power loss, because the page
cache survives a kill. `synchronous = FULL` (ADR-007) is what makes an acknowledged sale
survive a power cut; only the VM power-off campaign verifies it.
