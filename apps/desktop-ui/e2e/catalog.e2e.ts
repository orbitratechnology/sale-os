/**
 * Product list (E06-T06a), product editor (E06-T06b) and the unknown-barcode flow (E07-T04)
 * in Chromium against the recording fake backend.
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

async function till(query = ""): Promise<Page> {
  const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
  page.on("pageerror", (e) => {
    throw e;
  });
  await page.goto(`${base}/app-harness.html?preset=checkout${query}`);
  await page.getByRole("region", { name: "Cart lines" }).waitFor();
  return page;
}

async function calls(page: Page, cmd: string): Promise<Record<string, unknown>[]> {
  return page.evaluate((c) => window.__ipc.filter((x) => x.cmd === c).map((x) => x.args), cmd);
}

async function scan(page: Page, code: string) {
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.type(code, { delay: 5 });
  await page.keyboard.press("Enter");
}

async function products(page: Page) {
  await page.keyboard.press("F6");
  await page.getByRole("heading", { name: "Products" }).waitFor();
}

describe("product list (E06-T06a)", () => {
  it("lists, filters by text and status, and opens a product", async () => {
    const page = await till();
    await products(page);
    await page.getByTestId("product-total").filter({ hasText: "3 products" }).waitFor();
    await page.getByLabel("Filter by name, SKU or barcode").fill("crisp");
    await page.getByTestId("product-total").filter({ hasText: "1 product" }).waitFor();
    const filters = (await calls(page, "product_list")).map((c) => (c.filter as { query: string }).query);
    expect(filters.at(-1)).toBe("crisp");
    // Typing is debounced: not one request per key.
    expect(filters.filter((q) => q !== "" && q !== "crisp")).toEqual([]);
    await page.getByTestId("product-row").click();
    await page.getByRole("heading", { name: "Crisps" }).waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("heading", { name: "Products" }).waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
  });

  it("stays light with 50,000 products: a window of rows, pages fetched as you scroll", async () => {
    const page = await till("&big=50000");
    await products(page);
    await page.getByTestId("product-total").filter({ hasText: "50000 products" }).waitFor();
    const rendered = () => page.locator("[role=row].plist-row").count();
    expect(await rendered()).toBeLessThan(40);
    const viewport = page.getByTestId("product-viewport");
    const started = Date.now();
    await viewport.evaluate((el) => el.scrollTo({ top: el.scrollHeight }));
    await page.getByText("Item 049999").waitFor();
    expect(Date.now() - started).toBeLessThan(2000);
    expect(await rendered()).toBeLessThan(40);
    // Only the pages needed were fetched: the first and the last.
    const offsets = (await calls(page, "product_list")).map((c) => c.offset);
    expect(offsets).toEqual([0, 49800]);
  });
});

describe("product editor (E06-T06b)", () => {
  it("creates a simple product; a scan adds its barcode; saving says so", async () => {
    const page = await till();
    await products(page);
    await page.getByRole("button", { name: "New product" }).click();
    await page.getByLabel("Name").fill("Sparkling water");
    await page.getByLabel("Price for Sparkling water").fill("0.95");
    await page.getByLabel("SKU for Sparkling water").fill("WATER");
    await scan(page, "5000112546415"); // scanner-ready: goes to the editor, not the cart
    await page.getByRole("list", { name: "Barcodes for Sparkling water" }).getByText("5000112546415").waitFor();
    await page.getByRole("button", { name: "Save" }).click();
    await page.getByText("Saved on this device").waitFor();
    const [created] = await calls(page, "product_create");
    expect(created?.product).toMatchObject({
      name: "Sparkling water",
      variants: [{ priceMinor: "95", sku: "WATER", barcodes: [{ raw: "5000112546415", qualifier: "primary" }] }],
    });
    expect(await calls(page, "scan_submit")).toEqual([]);
  });

  it("adds options to make 3 sizes × 2 colours = 6 variants with their own prices and SKUs", async () => {
    const page = await till();
    await products(page);
    await page.getByRole("button", { name: "New product" }).click();
    await page.getByLabel("Name").fill("T-shirt");
    await page.getByLabel("Price for T-shirt").fill("15");
    await page.getByRole("button", { name: "Add option" }).click();
    await page.getByLabel("Option 1", { exact: true }).fill("Size");
    await page.getByLabel("Option 1 values").fill("S, M, L");
    await page.getByRole("button", { name: "Add option" }).click();
    await page.getByLabel("Option 2", { exact: true }).fill("Colour");
    await page.getByLabel("Option 2 values").fill("Red, Blue");
    await page.getByRole("heading", { name: "Variants (6)" }).waitFor();
    await page.getByLabel("Price for L / Blue").fill("17");
    for (const label of ["S / Red", "S / Blue", "M / Red", "M / Blue", "L / Red", "L / Blue"]) {
      await page.getByLabel(`SKU for ${label}`).fill(`TEE-${label.replace(" / ", "-")}`);
    }
    await page.getByRole("button", { name: "Save" }).click();
    await page.getByText("Saved on this device").waitFor();
    const variants = ((await calls(page, "product_create"))[0]?.product as { variants: { options: object; priceMinor: string; sku: string }[] }).variants;
    expect(variants).toHaveLength(6);
    expect(variants[0]).toMatchObject({ options: { Size: "S", Colour: "Red" }, priceMinor: "1500", sku: "TEE-S-Red" });
    expect(variants[5]).toMatchObject({ options: { Size: "L", Colour: "Blue" }, priceMinor: "1700", sku: "TEE-L-Blue" });
  });

  it("shows Rust's errors next to the input they are about", async () => {
    const page = await till();
    await products(page);
    await page.getByRole("button", { name: "New product" }).click();
    await page.getByLabel("Name").fill("Cola copy");
    await page.getByLabel("Price for Cola copy").fill("1");
    const add = page.getByLabel("Add barcode for Cola copy");
    await add.fill("4006381333931"); // belongs to Cola 330ml
    await add.press("Enter");
    await page.getByRole("button", { name: "Save" }).click();
    await page.locator(".chip.bad").filter({ hasText: "4006381333931" }).waitFor();
    await page.getByText("Barcode 4006381333931 already belongs to another product.").first().waitFor();
    // Client-side checks name their input too, before anything is sent.
    await page.getByLabel("Price for Cola copy").fill("1.234");
    await page.getByRole("button", { name: "Save" }).click();
    await page.getByText("Enter a price like 1.20.").first().waitFor();
    expect(await calls(page, "product_create")).toHaveLength(1);
  });
});

describe("unknown barcode (E07-T04)", () => {
  it("an owner creates the product from the prompt, and saving puts it in the cart", async () => {
    const page = await till();
    await scan(page, "9300633603209");
    await page.getByRole("button", { name: "Create product" }).click();
    await page.getByRole("heading", { name: "New product" }).waitFor();
    await page.getByRole("list", { name: /Barcodes for/ }).getByText("9300633603209").waitFor();
    await page.getByLabel("Name").fill("Lamington");
    await page.getByLabel("Price for Lamington").fill("2.50");
    await page.getByRole("button", { name: "Save" }).click();
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 1);
    expect(await page.getByTestId("total").textContent()).toBe("2.50");
    const scans = await calls(page, "scan_submit");
    expect(scans.at(-1)).toMatchObject({ raw: "9300633603209", source: "manual" });
  });

  it("a cashier is told to ask a manager instead", async () => {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    await page.goto(`${base}/app-harness.html?preset=signIn`);
    await page.getByRole("button", { name: /Cal/ }).click();
    await page.keyboard.type("3333");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await scan(page, "9300633603209");
    await page.getByText("A manager can add it as a new product.").waitFor();
    expect(await page.getByRole("button", { name: "Create product" }).count()).toBe(0);
  });
});
