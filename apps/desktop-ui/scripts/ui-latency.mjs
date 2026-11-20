#!/usr/bin/env node
/**
 * UI latency, cold start and memory of Sale OS, measured from real key events to the next
 * painted frame (pilot runbook §11).
 *
 * Three ways to point it at the app:
 *
 *   --launch "C:\Program Files\Sale OS\Sale OS.exe"   Start the real app (Windows): measures
 *       cold start, waits for someone to sign in, measures, clears its test cart and closes the
 *       app again. The WebView2 DevTools port is turned on for this one process only, through
 *       WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS in the child's environment; nothing is changed on
 *       the PC, and the app is closed at the end so the port does not stay open.
 *   --cdp http://127.0.0.1:9222   Attach to an app already started with that port open.
 *   --url http://localhost:5173/app-harness.html?preset=checkout   The browser harness with the
 *       fake backend (development; add --cpu 4 to slow the CPU down 4 times).
 *
 * On the real app it scans --codes (barcodes that exist in its catalogue) and searches for
 * --search (the first letters of a product name). It refuses to run
 * unless the till is in SANDBOX mode with an empty cart, so no real sale is touched.
 *
 * Other options: --n 25 (samples per measure), --port 9222, --json results.json.
 */
import { spawn, execFileSync } from "node:child_process";
import os from "node:os";
import { writeFileSync } from "node:fs";

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith("--") ? [...acc, [a.slice(2), all[i + 1]?.startsWith("--") ? "1" : (all[i + 1] ?? "1")]] : acc), []),
);
const N = parseInt(args.n ?? "25", 10);
const port = parseInt(args.port ?? "9222", 10);
const cpu = parseInt(args.cpu ?? "1", 10);
if (!args.launch && !args.cdp && !args.url) {
  console.error("Usage: ui-latency.mjs (--launch <Sale OS.exe> | --cdp <endpoint> | --url <harness url>) [--codes a,b,c] [--n 25] [--json out.json]");
  process.exit(2);
}
const harness = Boolean(args.url);
const codes = (args.codes ?? (harness ? "4006381333931,5000159407236,7622210449283" : "")).split(",").filter(Boolean);
const searchText = args.search ?? (harness ? "co" : "");
if (codes.length === 0 || !searchText) {
  console.error("Give --codes (two or three barcodes) and --search (the first letters of a product name) from this till's catalogue.");
  process.exit(2);
}

// Playwright inside the repository; playwright-core in the pilot-tools folder.
const { chromium } = await import("playwright").catch(() => import("playwright-core"));

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const pct = (a, p) => [...a].sort((x, y) => x - y)[Math.min(a.length - 1, Math.floor(p * a.length))];
const summary = (all) => {
  const a = all.filter((x) => Number.isFinite(x));
  return a.length ? { p50: +pct(a, 0.5).toFixed(1), p95: +pct(a, 0.95).toFixed(1), max: +Math.max(...a).toFixed(1), n: a.length, missed: all.length - a.length } : { n: 0, missed: all.length };
};

/** The process tree of `pid` (the app and its WebView2 processes), summed: Windows only. */
function memoryMb(pid) {
  if (process.platform !== "win32" || !pid) return null;
  const json = execFileSync(
    "powershell.exe",
    ["-NoProfile", "-Command", "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,WorkingSetSize,PrivatePageCount | ConvertTo-Json -Compress"],
    { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  const all = JSON.parse(json);
  const tree = new Set([pid]);
  for (let grew = true; grew; ) {
    grew = false;
    for (const p of all) {
      if (tree.has(p.ParentProcessId) && !tree.has(p.ProcessId)) {
        tree.add(p.ProcessId);
        grew = true;
      }
    }
  }
  const mine = all.filter((p) => tree.has(p.ProcessId));
  const mb = (k) => Math.round(mine.reduce((s, p) => s + Number(p[k] ?? 0), 0) / 1048576);
  return { processes: mine.length, workingSetMb: mb("WorkingSetSize"), privateMb: mb("PrivatePageCount") };
}

// Installed in the page: key timestamps, a waiter that resolves just after the paint that first
// shows a condition, slow interactions (Event Timing) and long main-thread tasks.
const INSTRUMENT = () => {
  if (window.__lat) return;
  window.__lat = { key: 0, slow: [], long: [] };
  window.addEventListener("keydown", (e) => { window.__lat.key = e.timeStamp; }, { capture: true });
  new PerformanceObserver((l) => { for (const e of l.getEntries()) if (e.duration >= 16) window.__lat.slow.push([e.name, Math.round(e.duration)]); }).observe({ type: "event", durationThreshold: 16, buffered: true });
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) window.__lat.long.push(Math.round(e.duration)); }).observe({ type: "longtask", buffered: true });
  } catch { /* not supported */ }
  window.__lat.arm = (src) => {
    const pred = new Function(`return (${src})`);
    window.__lat.result = new Promise((resolve) => {
      const done = () => requestAnimationFrame(() => setTimeout(() => resolve(performance.now() - window.__lat.key), 0));
      const check = () => { try { if (pred()) { mo.disconnect(); done(); } } catch { /* not yet */ } };
      const mo = new MutationObserver(check);
      mo.observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true });
    });
  };
};

async function measure(page, predicate, act) {
  await page.evaluate((src) => window.__lat.arm(src), predicate);
  await act();
  return page.evaluate(() => Promise.race([window.__lat.result, new Promise((r) => setTimeout(() => r(NaN), 3000))]));
}
const blur = (page) => page.evaluate(() => document.activeElement?.blur());
const markEpoch = (page, name) =>
  page.evaluate((n) => {
    const m = performance.getEntriesByName(n)[0];
    return m ? performance.timeOrigin + m.startTime : null;
  }, name);

// ---- Connect ----
let child = null;
let launchedAt = 0;
let browser;
let page;
const results = { measures: {}, startup: {}, memory: {} };
if (args.launch) {
  launchedAt = Date.now();
  child = spawn(args.launch, [], { env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` }, stdio: "ignore" });
  const endpoint = `http://127.0.0.1:${port}`;
  for (let i = 0; ; i++) {
    try {
      if ((await fetch(`${endpoint}/json/version`)).ok) break;
    } catch { /* not up yet */ }
    if (i > 600) throw new Error(`No DevTools endpoint on ${endpoint} after 60 s: the WebView2 runtime did not take the argument, or Sale OS was already running.`);
    await sleep(100);
  }
  browser = await chromium.connectOverCDP(endpoint);
} else if (args.cdp) {
  browser = await chromium.connectOverCDP(args.cdp);
} else {
  browser = await chromium.launch();
}
if (harness) {
  page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
  if (cpu > 1) await (await page.context().newCDPSession(page)).send("Emulation.setCPUThrottlingRate", { rate: cpu });
  const t = [];
  for (let i = 0; i < 5; i++) {
    await page.goto(args.url);
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    t.push(await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => setTimeout(() => r(performance.now()), 0)))));
  }
  results.startup["navigation → till ready (harness)"] = summary(t);
} else {
  for (let i = 0; !page; i++) {
    page = browser.contexts().flatMap((c) => c.pages()).find((p) => !p.url().startsWith("devtools:"));
    if (!page) {
      if (i > 300) throw new Error("The app has no page.");
      await sleep(100);
    }
  }
  if (args.launch) {
    for (let i = 0; !(await markEpoch(page, "sale-os:first-screen")); i++) {
      if (i > 600) throw new Error("The app did not show its first screen within 60 s.");
      await sleep(100);
    }
    results.startup.coldStartMs = Math.round((await markEpoch(page, "sale-os:first-screen")) - launchedAt);
    results.memory.atFirstScreen = memoryMb(child.pid);
  }
  console.log("Sign in on the till (SANDBOX mode, empty cart). Waiting…");
  for (let i = 0; !(await page.getByRole("region", { name: "Cart lines" }).isVisible().catch(() => false)); i++) {
    if (i > 6000) throw new Error("Nobody signed in within 10 minutes.");
    await sleep(100);
  }
}
await page.evaluate(INSTRUMENT);

// ---- Safety: never touch a live till or someone's sale ----
const sandbox = await page.locator(".sandbox-tag").count();
const lines = await page.locator("[data-testid=cart-line]").count();
if (!harness && (sandbox === 0 || lines > 0)) {
  console.error(sandbox === 0 ? "Refused: the till is LIVE. Measure in SANDBOX mode (before going live)." : "Refused: the cart has lines. Finish, park or clear the sale first.");
  await finish(1);
}

// ---- Measures ----
await blur(page);
{
  const t = [];
  for (let i = 0; i < N; i++) {
    const before = await page.getByTestId("total").textContent();
    await page.keyboard.type(codes[i % codes.length], { delay: 5 });
    t.push(await measure(page, `document.querySelector('[data-testid=total]').textContent !== ${JSON.stringify(before)}`, () => page.keyboard.press("Enter")));
    await sleep(80); // a cashier presents the next item
  }
  results.measures["Scan (Enter) → line and total painted"] = summary(t);
}
{
  const t = [];
  for (let i = 0; i < N; i++) {
    const before = await page.evaluate(() => document.querySelector("tr.selected")?.rowIndex);
    t.push(await measure(page, `document.querySelector('tr.selected')?.rowIndex !== ${before}`, () => page.keyboard.press(i % 2 === 0 ? "ArrowUp" : "ArrowDown")));
  }
  results.measures["Arrow key → selected line moves"] = summary(t);
}
{
  const o = [];
  const c = [];
  for (let i = 0; i < N; i++) {
    o.push(await measure(page, "document.querySelector('dialog:modal') !== null", () => page.keyboard.press("F9")));
    c.push(await measure(page, "document.querySelector('dialog') === null", () => page.keyboard.press("Escape")));
  }
  results.measures["F9 → payment dialog painted"] = summary(o);
  results.measures["Esc → dialog closed"] = summary(c);
}
{
  await page.keyboard.press("F9");
  await page.getByLabel("Cash received").waitFor();
  const t = [];
  for (let i = 0; i < N; i++) {
    if (i % 6 === 0) await page.getByLabel("Cash received").fill("");
    await page.evaluate(() => {
      window.__lat.result = new Promise((res) => document.activeElement.addEventListener("input", () => requestAnimationFrame(() => setTimeout(() => res(performance.now() - window.__lat.key), 0)), { once: true }));
    });
    await page.keyboard.press(String((i % 9) + 1));
    t.push(await page.evaluate(() => Promise.race([window.__lat.result, new Promise((r) => setTimeout(() => r(NaN), 3000))])));
  }
  results.measures["Keystroke in the cash field → character painted"] = summary(t);
  await page.getByLabel("Cash received").fill("");
  await page.keyboard.press("Escape");
}
for (const [key, heading] of [
  ["F6", "Products"],
  ["F8", "Sales"],
]) {
  const o = [];
  const c = [];
  for (let i = 0; i < Math.min(N, 15); i++) {
    await blur(page);
    o.push(await measure(page, `[...document.querySelectorAll('main h1')].some((h) => h.getClientRects().length && h.textContent.includes(${JSON.stringify(heading)}))`, () => page.keyboard.press(key)));
    await sleep(150);
    c.push(await measure(page, "!document.querySelector('.till-host').hidden", () => page.keyboard.press("Escape")));
  }
  results.measures[`${key} → ${heading} screen painted`] = summary(o);
  results.measures[`Esc → back on the till (from ${heading})`] = summary(c);
}
{
  const t = [];
  for (let i = 0; i < Math.min(N, 15); i++) {
    await page.keyboard.press("F2");
    t.push(await measure(page, "document.querySelector('[role=listbox]') !== null", () => page.keyboard.type(searchText, { delay: 0 })));
    await page.keyboard.press("Escape");
    await page.keyboard.press("Escape");
    await blur(page);
  }
  results.measures["Search typing → results painted (incl. the 80 ms typing pause)"] = summary(t);
}
results.slowInteractions = await page.evaluate(() => window.__lat.slow.filter(([name]) => name.startsWith("key")).length);
results.longTasks = await page.evaluate(() => window.__lat.long);
results.jsHeapMb = await page.evaluate(() => (performance.memory ? Math.round(performance.memory.usedJSHeapSize / 1048576) : null));
if (child) results.memory.afterMeasures = memoryMb(child.pid);
results.environment = {
  mode: args.launch ? "launch" : args.cdp ? "cdp" : `harness (cpu ${cpu}x)`,
  userAgent: await page.evaluate(() => navigator.userAgent),
  cpu: os.cpus()[0]?.model,
  cores: os.cpus().length,
  ramGb: Math.round(os.totalmem() / 1073741824),
  os: `${os.type()} ${os.release()}`,
  date: new Date().toISOString(),
  samples: N,
};

// ---- Clean up the test cart ----
if (!harness) {
  await blur(page);
  await page.getByRole("button", { name: "Clear cart" }).click();
  await page.getByRole("dialog", { name: "Clear the cart?" }).getByRole("button", { name: "Clear cart" }).click();
  await page.locator("[data-testid=cart-line]").first().waitFor({ state: "detached" });
}

// ---- Report ----
console.log(`\nSale OS UI latency — ${results.environment.mode}, ${results.environment.cpu}, ${results.environment.cores} threads, ${results.environment.ramGb} GB`);
if (results.startup.coldStartMs) console.log(`Cold start (launch → first screen): ${results.startup.coldStartMs} ms`);
for (const [k, v] of Object.entries(results.startup)) if (typeof v === "object") console.log(`${k}: p50 ${v.p50} ms, p95 ${v.p95} ms`);
for (const [k, v] of Object.entries(results.measures)) console.log(`${k.padEnd(64)} p50 ${String(v.p50).padStart(6)} ms   p95 ${String(v.p95).padStart(6)} ms   max ${String(v.max).padStart(6)} ms${v.missed ? `   (${v.missed} timed out)` : ""}`);
for (const [k, v] of Object.entries(results.memory)) if (v) console.log(`Memory ${k}: ${v.workingSetMb} MB working set, ${v.privateMb} MB private, ${v.processes} processes`);
console.log(`Key interactions slower than one frame (Event Timing): ${results.slowInteractions}; main-thread tasks over 50 ms: ${results.longTasks.length}; JS heap ${results.jsHeapMb} MB`);
if (args.json) {
  writeFileSync(args.json, JSON.stringify(results, null, 2));
  console.log(`Saved ${args.json}`);
}
await finish(0);

async function finish(code) {
  await browser?.close().catch(() => undefined);
  if (child) {
    // Close the app: it was started with the DevTools port open, which must not stay open.
    try {
      execFileSync("taskkill", ["/PID", String(child.pid), "/T"], { stdio: "ignore" });
      await sleep(5000);
      execFileSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });
    } catch { /* already closed */ }
    console.log("Sale OS was closed. Start it again normally from the Start menu.");
  }
  process.exit(code);
}
