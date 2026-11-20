# UI latency (2026-11-20)

The screen side of a till action: from the real key event to the next painted frame. Measured
by `apps/desktop-ui/scripts/ui-latency.mjs` on a production build of the browser harness
(Chromium, the engine of WebView2, with the fake backend), 1366×768, on Intel(R) Xeon(R)
Processor @ 2.10GHz, 4 threads, 16 GB, Linux. `--cpu 4` slows the CPU 4 times, roughly a
low-end till PC. The database side of the same actions is `pos-bench` (about 10 ms p95 for a
scan, 11 ms for a sale; `2026-11-19-encryption.md`); the Tauri bridge between them adds
about 1 ms per call (not measured here). A 60 Hz screen draws a frame every 16.7 ms, so about
15 ms is "the next frame".

```text
pnpm --filter @sale-os/desktop-ui exec vite build ...   # a production build of app-harness.html
node apps/desktop-ui/scripts/ui-latency.mjs --url http://localhost:<port>/app-harness.html?preset=checkout --cpu 4 --json out.json
```

| p95, ms | full speed | CPU 4× slower |
|---|---|---|
| Scan (Enter) → line and total painted | 18 | 46 |
| Arrow key → selected line moves | 14 | 16 |
| F9 → payment dialog painted | 31 | 60 |
| Esc → dialog closed | 20 | 33 |
| Keystroke in the cash field → painted | 19 | 20 |
| F6 → Products screen painted | 23 | 92 |
| Esc → back on the till (from Products) | 16 | 45 |
| F8 → Sales screen painted | 14 | 69 |
| Esc → back on the till (from Sales) | 15 | 49 |
| Search typing → results (incl. the 80 ms typing pause) | 94 | 107 |
| Navigation → till ready | 223 | 536 |

No main-thread task over 50 ms at full speed; JS heap about 10 MB. The numbers are in
`2026-11-20-ui-latency-harness-1x.json` and `-4x.json`.

**Change in this run.** Going back to the till used to rebuild it and reload its data: p95
111 ms at 4× (15 ms at full speed). The till now stays loaded, hidden, under the other screens
(`80af2f8`): 45–49 ms at 4×.

**Still to do.** These are Chromium on Linux with a fake backend. The numbers that decide are
the real app on the pilot PC (WebView2, the real database, cold start and memory across the
WebView2 processes), measured with `--launch` (pilot runbook §11).
