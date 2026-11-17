# Performance runs

PRD §14 targets, measured by `crates/pos-bench` (E19-T01) on a register database of 50,000
variants, 100,000 barcodes and 500,000 sale lines, through the services the app's commands
call. Each run writes `<date>.md` (the table) and `<date>.json` (the numbers).

```text
cargo run --release -p pos-bench -- --scale full --markdown docs/perf/<date>.md --json docs/perf/<date>.json
```

Without `--baseline`, a full run fails when a target is missed at p95. On Windows the same
binary is `pos-bench.exe`; the run that decides the pilot is on the D-05 reference machine
(Windows 11, 4-core i3-12100 class, 8 GB RAM, SSD).

A reduced suite (`--scale small`) runs weekly in CI (`.github/workflows/perf.yml`) and fails
when a median is more than 20% (and 0.5 ms) slower than the last good run on the same runner.

`crash-harness.md` is the AT-05 crash-consistency report (E02-T04).
