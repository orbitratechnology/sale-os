/**
 * Keyboard-only use and accessibility across the app: every screen and dialog passes axe with
 * no violations at all; dialogs keep Tab inside and give focus back when they close; tab lists
 * move with the arrow keys; screens start focus somewhere useful; the product list can be
 * walked end to end with the keyboard even though only the visible rows are rendered.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { chromium, type Browser, type Page } from "playwright";
import { createServer, type ViteDevServer } from "vite";
import { createRequire } from "node:module";
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

const COLA = "4006381333931";
const CRISPS = "5000159407236";
const axePath = createRequire(import.meta.url).resolve("axe-core/axe.min.js");

async function open(preset: "fresh" | "signIn" | "checkout"): Promise<Page> {
  const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
  page.on("pageerror", (e) => {
    throw e;
  });
  await page.goto(`${base}/app-harness.html?preset=${preset}`);
  return page;
}

async function till(): Promise<Page> {
  const page = await open("checkout");
  await page.getByRole("region", { name: "Cart lines" }).waitFor();
  return page;
}

async function scan(page: Page, code: string) {
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.type(code, { delay: 5 });
  await page.keyboard.press("Enter");
}

/** Every axe violation, of any impact, as "impact:rule(count) target". */
async function axe(page: Page): Promise<string[]> {
  // Measure the settled screen: a dialog fading in has, for a moment, low-contrast text.
  await page.evaluate(() => Promise.all(document.getAnimations().map((a) => a.finished.catch(() => undefined))));
  if (!(await page.evaluate(() => "axe" in window))) await page.addScriptTag({ path: axePath });
  return page.evaluate(async () => {
    type Result = { violations: { id: string; impact: string; nodes: { target: string[] }[] }[] };
    const res = await (window as unknown as { axe: { run: (c: Document) => Promise<Result> } }).axe.run(document);
    return res.violations.map((v) => `${v.impact}:${v.id}(${v.nodes.length}) ${v.nodes[0]?.target.join(" ") ?? ""}`);
  });
}

/** What has focus, as "tag[name]"; "BODY" when nothing does (the till's scanner-ready state). */
function focused(page: Page): Promise<string> {
  return page.evaluate(() => {
    const e = document.activeElement;
    if (!e || e === document.body) return "BODY";
    const name = e.getAttribute("aria-label") ?? e.textContent?.trim().replace(/\s+/g, " ").slice(0, 40) ?? "";
    return `${e.tagName.toLowerCase()}[${name}]`;
  });
}

/**
 * Opens a dialog, then Tabs 25 times: focus must never land on the page behind it (it may pass
 * through the browser's own chrome, reported as BODY, as native modal dialogs allow).
 * Escape must close it and put focus back where it was.
 */
async function checkDialog(page: Page, name: string | RegExp, opener: { prepare: () => Promise<void>; open: () => Promise<void> }) {
  await opener.prepare();
  const before = await focused(page);
  await opener.open();
  const dialog = page.getByRole("dialog", { name });
  await dialog.waitFor();
  expect(await page.evaluate(() => document.querySelector("dialog:modal") !== null), "opened as a modal").toBe(true);
  expect(await axe(page), `axe in dialog ${String(name)}`).toEqual([]);
  const leaks: string[] = [];
  for (let i = 0; i < 25; i++) {
    await page.keyboard.press("Tab");
    const where = await page.evaluate(() => {
      const d = document.querySelector("dialog:modal");
      const e = document.activeElement;
      return !e || e === document.body ? "BODY" : d?.contains(e) ? "IN" : (e.textContent ?? e.tagName).trim().slice(0, 30);
    });
    if (where !== "IN" && where !== "BODY") leaks.push(where);
  }
  expect(leaks, `Tab reached the page behind ${String(name)}`).toEqual([]);
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "detached" });
  expect(await focused(page), `focus after closing ${String(name)}`).toBe(before);
}

describe("axe: no violations on any screen", () => {
  it("setup, sign-in and the PIN pad", async () => {
    let page = await open("fresh");
    await page.getByRole("heading", { name: "Set up Sale OS" }).waitFor();
    expect(await axe(page)).toEqual([]);
    await page.close();
    page = await open("signIn");
    await page.getByRole("heading", { name: "Who is signing in?" }).waitFor();
    expect(await axe(page)).toEqual([]);
    await page.keyboard.press("Enter");
    await page.getByRole("group", { name: "PIN" }).waitFor();
    expect(await axe(page)).toEqual([]);
    await page.close();
  });

  it("the till, empty and with lines, and every screen it opens", async () => {
    const page = await till();
    expect(await axe(page)).toEqual([]);
    await scan(page, COLA);
    await scan(page, CRISPS);
    await page.locator("[data-testid=cart-line]").nth(1).waitFor();
    expect(await axe(page)).toEqual([]);
    for (const [key, heading] of [
      ["F6", "Products"],
      ["F8", "Sales"],
      ["F7", "Returns"],
      ["F10", "Cash drawer"],
      ["F11", "Stock"],
    ] as const) {
      await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
      await page.keyboard.press(key);
      await page.getByRole("heading", { name: heading, level: 1 }).waitFor();
      expect(await axe(page), heading).toEqual([]);
      await page.keyboard.press("Escape");
      await page.getByRole("region", { name: "Cart lines" }).waitFor();
    }
    // Products and the screens it leads to.
    await page.keyboard.press("F6");
    await page.getByTestId("product-row").first().click();
    await page.getByRole("form", { name: "Product editor" }).waitFor();
    expect(await axe(page), "product editor").toEqual([]);
    await page.keyboard.press("Escape");
    for (const [button, heading] of [
      ["Import CSV", "Import products"],
      ["Categories", "Categories"],
    ] as const) {
      await page.getByRole("button", { name: button }).click();
      await page.getByRole("heading", { name: heading, level: 1 }).waitFor();
      expect(await axe(page), heading).toEqual([]);
      await page.keyboard.press("Escape");
      await page.getByRole("heading", { name: "Products", level: 1 }).waitFor();
    }
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    const screens = page.getByRole("navigation", { name: "Screens" });
    await screens.getByRole("button", { name: "Reports" }).click();
    await page.getByRole("heading", { name: "Reports", level: 1 }).waitFor();
    for (const tab of ["Daily sales", "Payments", "Top products", "Stock"]) {
      await page.getByRole("tab", { name: tab }).click();
      expect(await axe(page), `report ${tab}`).toEqual([]);
    }
    await page.getByRole("button", { name: "Audit log" }).click();
    await page.getByRole("heading", { name: "Audit log", level: 1 }).waitFor();
    expect(await axe(page), "audit log").toEqual([]);
    await page.keyboard.press("Escape");
    await page.getByRole("heading", { name: "Reports", level: 1 }).waitFor();
    await page.keyboard.press("Escape");
    await screens.getByRole("button", { name: "Settings" }).click();
    await page.getByRole("heading", { name: "Settings", level: 1 }).waitFor();
    const tabs = await page.getByRole("tab").allTextContents();
    expect(tabs.length).toBeGreaterThan(5);
    for (const tab of tabs) {
      await page.getByRole("tab", { name: tab, exact: true }).click();
      await page.getByRole("tabpanel", { name: tab, exact: true }).waitFor();
      expect(await axe(page), `settings ${tab}`).toEqual([]);
    }
    await page.close();
  }, 120_000);
});

describe("dialogs: Tab stays inside, Escape closes, focus goes back", () => {
  it("every dialog on the till", async () => {
    const page = await till();
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    const key = (k: string) => ({
      prepare: () => page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur()),
      open: () => page.keyboard.press(k),
    });
    const button = (name: string, within = page.getByRole("complementary", { name: "Totals and actions" })) => ({
      prepare: () => within.getByRole("button", { name, exact: true }).focus(),
      open: () => page.keyboard.press("Enter"),
    });
    // From scanner-ready mode (focus on the page) focus returns to the page: scanning goes on.
    await checkDialog(page, "Payment", key("F9"));
    await checkDialog(page, "Quick items", key("Shift+F2"));
    await checkDialog(page, "Parked carts", key("Shift+F5"));
    await checkDialog(page, "Park this cart?", key("F5"));
    // From a button, focus returns to that button.
    await checkDialog(page, "Void this sale?", button("Void sale"));
    await checkDialog(page, "Clear the cart?", button("Clear cart"));
    await checkDialog(page, "Order discount", button("Order discount"));
    await checkDialog(page, /^New price for/, button("Change price"));
    const banner = page.getByRole("note");
    await checkDialog(page, "Getting started", button("Getting started", banner));
    await checkDialog(page, "Go live", button("Go live…", banner));
    await page.close();
  }, 120_000);

  it("Enter on a focused button in Parked carts does that button's job, not 'resume'", async () => {
    const page = await till();
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.keyboard.press("F5");
    await page.getByRole("dialog", { name: "Park this cart?" }).getByRole("button", { name: "Park" }).click();
    await page.locator("[data-testid=cart-line]").waitFor({ state: "detached" });
    await page.keyboard.press("Shift+F5");
    const parked = page.getByRole("dialog", { name: "Parked carts" });
    await parked.getByRole("button", { name: "Discard…" }).focus();
    await page.keyboard.press("Enter");
    await parked.getByRole("group", { name: "Discard a parked cart" }).waitFor();
    expect(await page.locator("[data-testid=cart-line]").count()).toBe(0);
    await page.close();
  });
});

describe("tabs and choices: arrow keys, one Tab stop", () => {
  it("settings pages: Down and Up move and select; only the selected page is in the Tab order", async () => {
    const page = await till();
    await page.getByRole("navigation", { name: "Screens" }).getByRole("button", { name: "Settings" }).click();
    const list = page.getByRole("tablist", { name: "Settings pages" });
    await list.waitFor();
    const first = list.getByRole("tab").first();
    await first.focus();
    expect(await list.locator('[role="tab"][tabindex="0"]').count()).toBe(1);
    await page.keyboard.press("ArrowDown");
    const second = list.getByRole("tab").nth(1);
    expect(await second.getAttribute("aria-selected")).toBe("true");
    expect(await second.evaluate((e) => e === document.activeElement)).toBe(true);
    await page.keyboard.press("End");
    expect(await list.getByRole("tab").last().getAttribute("aria-selected")).toBe("true");
    await page.keyboard.press("Home");
    expect(await first.getAttribute("aria-selected")).toBe("true");
    // The panel is named by its tab.
    const name = (await first.textContent()) ?? "";
    await page.getByRole("tabpanel", { name, exact: true }).waitFor();
    await page.close();
  });

  it("stock: the view tabs keep focus when the view changes; the document kind is a radio group", async () => {
    const page = await till();
    await page.keyboard.press("F11");
    const tabs = page.getByRole("tablist", { name: "Stock" });
    await tabs.getByRole("tab", { name: "Receive and adjust" }).focus();
    await page.keyboard.press("ArrowRight");
    await page.getByRole("tabpanel", { name: "On hand" }).waitFor();
    expect(await focused(page)).toBe("button[On hand]");
    await page.keyboard.press("ArrowLeft");
    await page.getByRole("tabpanel", { name: "Receive and adjust" }).waitFor();
    expect(await focused(page)).toBe("button[Receive and adjust]");
    const kinds = page.getByRole("group", { name: "What are you doing?" });
    await kinds.getByRole("radio", { name: "Receive goods" }).focus();
    await page.keyboard.press("ArrowRight");
    expect(await kinds.getByRole("radio", { name: "Opening stock" }).isChecked()).toBe(true);
    expect(await focused(page)).toBe("input[]");
    await page.close();
  });
});

describe("where focus starts", () => {
  it("sign-in starts on the first person; choosing one focuses the PIN pad", async () => {
    const page = await open("signIn");
    await page.getByRole("heading", { name: "Who is signing in?" }).waitFor();
    expect(await focused(page)).toMatch(/^button\[O ?Olu/);
    await page.keyboard.press("Enter");
    await page.getByRole("group", { name: "PIN" }).waitFor();
    expect(await focused(page)).toBe("div[PIN]");
    await page.close();
  });

  it("screens without a field to type in start at their heading; the window title names the screen", async () => {
    const page = await till();
    expect(await page.title()).toBe("Till · Sale OS");
    await page.getByRole("navigation", { name: "Screens" }).getByRole("button", { name: "Reports" }).click();
    await page.getByRole("heading", { name: "Reports", level: 1 }).waitFor();
    await expect.poll(() => focused(page)).toBe("h1[Reports]");
    expect(await page.title()).toBe("Reports · Sale OS");
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    // Back on the till, focus is on the page itself: the scanner types there.
    expect(await focused(page)).toBe("BODY");
    await page.close();
  });

  it("arrow keys move the selected cart line, and the selection is announced", async () => {
    const page = await till();
    await scan(page, COLA);
    await scan(page, CRISPS);
    await page.locator("[data-testid=cart-line]").nth(1).waitFor();
    await page.keyboard.press("ArrowUp");
    await expect.poll(() => page.getByTestId("selected-line").textContent()).toMatch(/^Selected: Cola 330ml, quantity 1, /);
    await page.close();
  });
});

describe("the product list with the keyboard", () => {
  it("arrow keys walk every product, scrolling rows into view; Enter opens one", async () => {
    const page = await till();
    await page.keyboard.press("F6");
    const rows = page.getByTestId("product-row");
    await rows.first().waitFor();
    const count = parseInt((await page.getByTestId("product-total").textContent()) ?? "0", 10);
    expect(count).toBeGreaterThan(1);
    await rows.first().focus();
    await page.keyboard.press("End");
    await expect.poll(() => page.evaluate(() => document.activeElement?.closest("[data-row]")?.getAttribute("data-row"))).toBe(String(count - 1));
    await page.keyboard.press("Home");
    await expect.poll(() => page.evaluate(() => document.activeElement?.closest("[data-row]")?.getAttribute("data-row"))).toBe("0");
    await page.keyboard.press("ArrowDown");
    await expect.poll(() => page.evaluate(() => document.activeElement?.closest("[data-row]")?.getAttribute("data-row"))).toBe("1");
    await page.keyboard.press("Enter");
    await page.getByRole("form", { name: "Product editor" }).waitFor();
    await page.close();
  });
});
