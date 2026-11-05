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
    if (JSON.stringify(got) !== JSON.stringify(CODES)) {
      // Timing diagnostics: this is how the 41 ms delivery stall was found.
      const keys = await page.evaluate(() => window.__keys);
      console.error(`suffix ${suffix}: got`, got, "keys:", JSON.stringify(keys.map(([k, t]) => [k, Math.round(t)])));
    }
    expect(got).toEqual(CODES);
    const ids = await page.evaluate(() => new Set(window.__scans.map((s) => s.scanEventId)).size);
    expect(ids).toBe(20);
    await page.close();
  }, 60_000);
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
