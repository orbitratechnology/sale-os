/**
 * F-08 (docs/security/threat-model.md): the production Content Security Policy allows no
 * inline styles, and the app still works under it.
 *
 * The UI is built for production (`vite build`, the app harness with the fake backend as
 * the entry, since a plain browser has no Tauri), served by a small static server that sends
 * the CSP from `tauri.conf.json` on every response, and the main screens are opened in
 * Chromium. Any CSP violation fails the test. React's `style` props are applied through the
 * CSSOM, which CSP allows; a `<style>` element or a `style="…"` attribute would be refused.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { chromium, type Browser, type Page } from "playwright";
import { build } from "vite";
import { createServer, type Server } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));

/** The production policy, exactly as Tauri is configured to send it. */
async function productionCsp(): Promise<string> {
  const conf = JSON.parse(await readFile(resolve(root, "../desktop-tauri/tauri.conf.json"), "utf8")) as {
    app: { security: { csp: Record<string, string> } };
  };
  return Object.entries(conf.app.security.csp)
    .map(([directive, value]) => `${directive} ${value}`)
    .join("; ");
}

const TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".woff2": "font/woff2",
  ".ttf": "font/ttf",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

let out: string;
let server: Server;
let browser: Browser;
let base: string;
let csp: string;

beforeAll(async () => {
  csp = await productionCsp();
  expect(csp).toContain("style-src 'self'");
  expect(csp).not.toContain("unsafe-inline");
  out = await mkdtemp(join(tmpdir(), "sale-os-csp-"));
  await build({
    root,
    logLevel: "error",
    build: {
      outDir: out,
      emptyOutDir: true,
      sourcemap: false,
      rollupOptions: { input: { harness: resolve(root, "app-harness.html") } },
    },
  });
  server = createServer((req, res) => {
    const path = normalize(decodeURIComponent(new URL(req.url ?? "/", "http://x").pathname)).replace(/^[/\\]+/, "");
    const file = resolve(out, path || "app-harness.html");
    if (!file.startsWith(out + sep)) {
      res.writeHead(404).end();
      return;
    }
    readFile(file).then(
      (body) => {
        res.writeHead(200, {
          "content-type": TYPES[extname(file)] ?? "application/octet-stream",
          "content-security-policy": csp,
        });
        res.end(body);
      },
      () => res.writeHead(404, { "content-security-policy": csp }).end(),
    );
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("no server address");
  base = `http://127.0.0.1:${address.port}`;
  browser = await chromium.launch();
}, 180_000);

afterAll(async () => {
  await browser?.close();
  await new Promise((done) => server?.close(done));
  if (out) await rm(out, { recursive: true, force: true });
});

/** Opens the built app and records every CSP violation, from the DOM event and the console. */
async function open(preset: "signIn" | "checkout"): Promise<{ page: Page; violations: () => Promise<string[]> }> {
  const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
  const consoleViolations: string[] = [];
  page.on("console", (m) => {
    if (/Content Security Policy/i.test(m.text())) consoleViolations.push(m.text());
  });
  page.on("pageerror", (e) => {
    throw e;
  });
  await page.addInitScript(() => {
    const seen: string[] = [];
    (window as unknown as { __csp: string[] }).__csp = seen;
    document.addEventListener("securitypolicyviolation", (e) => seen.push(`${e.effectiveDirective}: ${e.blockedURI || "inline"} ${e.sample}`));
  });
  await page.goto(`${base}/app-harness.html?preset=${preset}`);
  const violations = async () => [...consoleViolations, ...(await page.evaluate(() => (window as unknown as { __csp: string[] }).__csp))];
  return { page, violations };
}

describe("content security policy without inline styles (F-08)", () => {
  it("the policy is in force: an inline style element is refused", async () => {
    const { page, violations } = await open("checkout");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    expect(await violations()).toEqual([]);
    await page.evaluate(() => {
      const s = document.createElement("style");
      s.textContent = "body { outline: 1px solid red; }";
      document.head.append(s);
    });
    await expect.poll(async () => (await violations()).some((v) => v.includes("style-src"))).toBe(true);
  });

  it("the sign-in screen and the main screens render with no violation", async () => {
    const signIn = await open("signIn");
    await signIn.page.getByRole("heading", { name: "Who is signing in?" }).waitFor();
    expect(await signIn.violations()).toEqual([]);

    const { page, violations } = await open("checkout");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    // A product in the cart: lines, totals, the tender buttons.
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await page.keyboard.type("4006381333931", { delay: 5 });
    await page.keyboard.press("Enter");
    await page.locator("[data-testid=cart-line]").first().waitFor();
    // A dialog (the no-sale reason).
    await page.getByRole("button", { name: "No sale" }).click();
    await page.getByRole("dialog").waitFor();
    await page.keyboard.press("Escape");
    const nav = page.getByRole("navigation", { name: "Screens" });
    for (const [screen, heading] of [
      ["Products", /Products/],
      ["Sales", /Sales/],
      ["Returns", /Return/],
      ["Drawer", /drawer/i],
      ["Stock", /Stock/],
      ["Reports", /Reports/],
    ] as const) {
      await nav.getByRole("button", { name: new RegExp(`^${screen}`) }).click();
      await page.getByRole("heading", { level: 1, name: heading }).first().waitFor();
      await page.waitForTimeout(150);
      await page.keyboard.press("Escape");
      await page.getByRole("region", { name: "Cart lines" }).waitFor();
    }
    await nav.getByRole("button", { name: "Settings" }).click();
    const pages = page.getByRole("tablist", { name: "Settings pages" }).getByRole("tab");
    const names = await pages.allTextContents();
    expect(names.length).toBeGreaterThan(5);
    for (let i = 0; i < names.length; i++) {
      await pages.nth(i).click();
      await page.getByRole("tabpanel").waitFor();
      await page.waitForTimeout(100);
    }
    expect(await violations()).toEqual([]);
  });
});
