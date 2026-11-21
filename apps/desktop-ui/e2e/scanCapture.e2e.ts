/**
 * E07-T01 acceptance in a real browser engine: Chromium (the engine behind WebView2 on
 * Windows) receives synthetic key events at scanner speed through the real React hook.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { chromium, type Browser, type Page } from "playwright";
import { createServer, type ViteDevServer } from "vite";
import { fileURLToPath } from "node:url";

let server: ViteDevServer;
let browser: Browser;
let base: string;

beforeAll(async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  server = await createServer({ root, server: { port: 0, strictPort: false }, logLevel: "error" });
  await server.listen();
  const address = server.httpServer?.address();
  if (!address || typeof address === "string") throw new Error("no server address");
  base = `http://localhost:${address.port}`;
  browser = await chromium.launch();
}, 60_000);

afterAll(async () => {
  await browser?.close();
  await server?.close();
});

const CODES = Array.from({ length: 20 }, (_, i) => `50123456789${String(i).padStart(2, "0")}`);

async function open(suffix: string): Promise<Page> {
  const page = await browser.newPage();
  await page.goto(`${base}/scan-harness.html?suffix=${suffix}`);
  await page.waitForSelector("[data-testid=count]");
  // Scanner-ready mode: keyboard focus on the page, not in a text field.
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  return page;
}

async function scans(page: Page): Promise<string[]> {
  return page.evaluate(() => window.__scans.map((s) => s.raw));
}

describe.each(["Enter", "Tab", "none"])("keyboard wedge, suffix %s", (suffix) => {
  it("delivers 20 scans typed at 5 ms/key: no merges, no drops, no truncation", async () => {
    const page = await open(suffix);
    for (const code of CODES) {
      await page.keyboard.type(code, { delay: 5 });
      if (suffix !== "none") await page.keyboard.press(suffix);
      // 50 ms between scans; with no suffix, silence ends a scan, so leave more than maxStallMs.
      await page.waitForTimeout(suffix === "none" ? 250 : 50);
    }
    await page.waitForTimeout(300); // let the final idle timer fire for suffix "none"
    const got = await scans(page);
    let why = "";
    if (JSON.stringify(got) !== JSON.stringify(CODES)) {
      // Timing diagnostics, in the failure message itself so CI reports show them: the
      // codes received, and every gap between key events over 30 ms (this is how the 41 ms
      // delivery stall was found).
      const keys = await page.evaluate(() => window.__keys);
      const slow = keys.flatMap(([k, t], i) => {
        const prev = i > 0 ? keys[i - 1] : undefined;
        return prev && t - prev[1] > 30 ? [`${k}+${Math.round(t - prev[1])}ms`] : [];
      });
      why = `suffix ${suffix}: got ${JSON.stringify(got)}; gaps over 30 ms: ${slow.join(" ") || "none"}`;
    }
    expect(got, why).toEqual(CODES);
    const ids = await page.evaluate(() => new Set(window.__scans.map((s) => s.scanEventId)).size);
    expect(ids).toBe(20);
    await page.close();
  }, 60_000);
});

describe("a scan that stalls past maxStallMs", () => {
  it("is dropped whole: no truncated code, and its Tab does not move focus into a field", async () => {
    const page = await open("Tab");
    await page.keyboard.type("501234", { delay: 5 });
    await page.waitForTimeout(250); // longer than maxStallMs: the burst breaks
    await page.keyboard.type("5678900", { delay: 5 });
    await page.keyboard.press("Tab");
    expect(await scans(page)).toEqual([]);
    expect(await page.evaluate(() => document.activeElement?.tagName)).toBe("BODY");
    // The next clean scan is read normally.
    await page.waitForTimeout(300);
    await page.keyboard.type("4006381333931", { delay: 5 });
    await page.keyboard.press("Tab");
    expect(await scans(page)).toEqual(["4006381333931"]);
    await page.close();
  }, 30_000);
});

describe("human typing", () => {
  it("is never a scan inside a search field, and the text stays in the field", async () => {
    const page = await open("Enter");
    await page.getByTestId("search").click();
    await page.keyboard.type("abcd", { delay: 150 });
    await page.keyboard.press("Enter");
    // Even a fast burst in an ordinary field is left alone.
    await page.keyboard.type("4006381333931", { delay: 5 });
    await page.keyboard.press("Enter");
    expect(await scans(page)).toEqual([]);
    expect(await page.getByTestId("search").inputValue()).toBe("abcd4006381333931");
    await page.close();
  }, 30_000);

  it("is not a scan in scanner-ready mode either", async () => {
    const page = await open("Enter");
    await page.keyboard.type("abc1", { delay: 150 });
    await page.keyboard.press("Enter");
    expect(await scans(page)).toEqual([]);
    await page.close();
  }, 30_000);
});

describe("scan field", () => {
  it("delivers a scan typed into the scan field and the owner clears it", async () => {
    const page = await open("Enter");
    await page.getByTestId("scan-field").click();
    await page.keyboard.type("ABC-12345", { delay: 5 });
    await page.keyboard.press("Enter");
    expect(await page.evaluate(() => window.__scans.map((s) => [s.raw, s.inScanField]))).toEqual([["ABC-12345", true]]);
    expect(await page.getByTestId("scan-field").inputValue()).toBe("");
    await page.close();
  }, 30_000);
});

describe("E21-T03: robustness with real key events", () => {
  it("delivers every scan from a slow scanner (50 ms/char) instead of losing them silently", async () => {
    const page = await open("Enter");
    const codes = ["4006381333931", "036000291452", "ABC-1234", "10012345678902"];
    for (const code of codes) {
      await page.keyboard.type(code, { delay: 50 });
      await page.keyboard.press("Enter", { delay: 0 });
      await page.waitForTimeout(300);
    }
    expect(await page.evaluate(() => window.__scans.map((s) => [s.raw, s.speed]))).toEqual(
      codes.map((c) => [c, "slow"]),
    );
    await page.close();
  }, 60_000);

  it("delivers a 3-character code ended by the suffix", async () => {
    const page = await open("Enter");
    await page.keyboard.type("A12", { delay: 5 });
    await page.keyboard.press("Enter");
    expect(await scans(page)).toEqual(["A12"]);
    await page.close();
  }, 30_000);

  it("keeps the GS1 separator a wedge scanner types as Ctrl+]", async () => {
    const page = await open("Enter");
    await page.keyboard.type("0109501101530003", { delay: 5 });
    await page.keyboard.press("Control+BracketRight", { delay: 0 });
    await page.keyboard.type("10AB12", { delay: 5 });
    await page.keyboard.press("Enter");
    expect(await scans(page)).toEqual(["0109501101530003\x1d10AB12"]);
    await page.close();
  }, 30_000);

  it("still never captures human typing in scanner-ready mode at a person's rhythm", async () => {
    const page = await open("Enter");
    for (const [c, d] of [["a", 0], ["b", 90], ["c", 40], ["d", 160], ["e", 70], ["f", 30], ["g", 110]] as const) {
      await page.waitForTimeout(d);
      await page.keyboard.press(c);
    }
    await page.keyboard.press("Enter");
    expect(await scans(page)).toEqual([]);
    await page.close();
  }, 30_000);
});
