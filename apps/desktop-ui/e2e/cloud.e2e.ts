/**
 * Settings → Cloud (E17-T08) in Chromium against the scripted backend: linking by creating
 * the cloud business, joining (offered only to a till with nothing of its own), a wrong
 * password, and the message after the owner disconnected the till.
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

async function cloudPage(before?: (page: Page) => Promise<void>): Promise<Page> {
  const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
  page.on("pageerror", (e) => {
    throw e;
  });
  await page.goto(`${base}/app-harness.html?preset=signIn`);
  if (before) await before(page);
  await page.getByRole("button", { name: /Olu/ }).click();
  await page.keyboard.type("1234");
  await page.keyboard.press("Enter");
  await page.getByRole("region", { name: "Cart lines" }).waitFor();
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("tab", { name: "Cloud" }).click();
  await page.getByRole("heading", { name: "Cloud" }).waitFor();
  return page;
}

async function calls(page: Page, cmd: string): Promise<Record<string, unknown>[]> {
  return page.evaluate((c) => window.__ipc.filter((x) => x.cmd === c).map((x) => x.args), cmd);
}

describe("cloud link (E17-T08)", () => {
  it("the owner signs in and creates the cloud business from this till", async () => {
    const page = await cloudPage();
    await page.getByLabel("Cloud account email").fill("olu@example.com");
    await page.getByLabel("Password").fill("correct horse battery staple");
    await page.getByRole("button", { name: "Sign in to link" }).click();
    // This till has products and sales: joining is shown but not offered.
    const join = page.getByRole("button", { name: "Join Harbour Café" });
    await join.waitFor();
    expect(await join.isDisabled()).toBe(true);
    await page.getByText(/only possible before this till has live sales or products/).waitFor();
    await page.getByRole("button", { name: "Create the cloud business from this till" }).click();
    await page.getByText(/Linked\. This till is R01/).waitFor();
    await page.getByText("Linked as R01").waitFor();
    expect(await calls(page, "sync_link_complete")).toEqual([{ mode: "create", businessId: null, replaceExisting: false }]);
    // The password went to Rust once and is not kept on the screen.
    const starts = await calls(page, "sync_link_start");
    expect(starts).toEqual([{ email: "olu@example.com", password: "correct horse battery staple" }]);
    expect(await page.getByRole("form", { name: "Cloud sign-in" }).count()).toBe(0);
  });

  it("a wrong password is refused without saying which part was wrong", async () => {
    const page = await cloudPage();
    await page.getByLabel("Cloud account email").fill("olu@example.com");
    await page.getByLabel("Password").fill("nope");
    await page.getByRole("button", { name: "Sign in to link" }).click();
    await page.getByRole("alert").filter({ hasText: "That email or password is not right" }).waitFor();
    expect(await page.getByLabel("Password").inputValue()).toBe("");
  });

  it("a till with nothing of its own joins a business after confirming, then everyone signs in again", async () => {
    const page = await cloudPage(async (p) => {
      await p.evaluate(() => {
        window.__fake.cloud.mayJoin = true;
      });
    });
    await page.getByLabel("Cloud account email").fill("olu@example.com");
    await page.getByLabel("Password").fill("correct horse battery staple");
    await page.getByRole("button", { name: "Sign in to link" }).click();
    await page.getByRole("button", { name: "Join Harbour Café" }).click();
    const dialog = page.getByRole("alertdialog", { name: "Join a business" });
    await dialog.getByText(/replaces everything on this till/).waitFor();
    await dialog.getByRole("button", { name: "Join Harbour Café" }).click();
    // Back to the sign-in screen: the staff are the joined business's now.
    await page.getByRole("button", { name: /Olu/ }).waitFor();
    expect(await calls(page, "sync_link_complete")).toEqual([
      { mode: "join", businessId: "0190f0c2-0000-7000-8000-00000000b2b2", replaceExisting: false },
    ]);
  });

  it("a till the owner disconnected says so, and can be linked again", async () => {
    const page = await cloudPage(async (p) => {
      await p.evaluate(() => {
        window.__fake.cloud.linkState = "revoked";
      });
    });
    await page.getByRole("alert").filter({ hasText: "This register was disconnected by the owner" }).waitFor();
    await page.getByRole("form", { name: "Cloud sign-in" }).waitFor();
  });
});
