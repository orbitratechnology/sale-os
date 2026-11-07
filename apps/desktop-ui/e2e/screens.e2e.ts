/**
 * Stage 1 screens in Chromium (the engine behind WebView2) against a scripted backend:
 * first run (E04-T02), sign-in (E05-T06), checkout (E09-T03..T06).
 *
 * The backend fake records every IPC call; these tests assert what the UI asked Rust to do
 * and what it showed. Rust's behaviour behind each command is tested in Rust.
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
const CHOC = "7622210449283";

async function open(preset: "fresh" | "signIn" | "checkout", viewport = { width: 1366, height: 768 }): Promise<Page> {
  const page = await browser.newPage({ viewport });
  page.on("pageerror", (e) => {
    throw e;
  });
  await page.goto(`${base}/app-harness.html?preset=${preset}`);
  return page;
}

async function calls(page: Page, cmd: string): Promise<Record<string, unknown>[]> {
  return page.evaluate((c) => window.__ipc.filter((x) => x.cmd === c).map((x) => x.args), cmd);
}

/** A keyboard-wedge scanner: 5 ms per key, then Enter, with focus outside text fields. */
async function scan(page: Page, code: string) {
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.type(code, { delay: 5 });
  await page.keyboard.press("Enter");
}

async function lines(page: Page): Promise<string[]> {
  return page.locator("[data-testid=cart-line]").evaluateAll((rows) =>
    rows.map((r) => `${r.querySelector(".item-name")?.textContent} x${r.querySelector("[data-testid=qty]")?.textContent ?? "?"}`),
  );
}

async function checkout(preset: "checkout" = "checkout", viewport?: { width: number; height: number }) {
  const page = await open(preset, viewport);
  await page.getByRole("region", { name: "Cart lines" }).waitFor();
  return page;
}

describe("first run (E04-T02)", () => {
  it("completes with the keyboard only, offline, well under 3 minutes", async () => {
    const started = Date.now();
    const page = await open("fresh");
    await page.getByRole("heading", { name: "Set up Sale OS" }).waitFor();
    const k = page.keyboard;
    // Business
    await k.type("Corner Shop");
    await k.press("Enter");
    // Region (replace the locale defaults)
    await page.getByLabel("Country code").waitFor();
    await k.press("ControlOrMeta+a");
    await k.type("gb");
    await k.press("Tab");
    await k.press("ControlOrMeta+a");
    await k.type("Europe/London");
    await k.press("Tab");
    await k.press("ControlOrMeta+a");
    await k.type("gbp");
    await k.press("Enter");
    // Tax: inclusive (default), one rate
    await page.getByRole("radio", { name: /Include tax/ }).waitFor();
    await k.press("Tab");
    await k.press("ControlOrMeta+a");
    await k.type("VAT 20%");
    await k.press("Tab");
    await k.type("20");
    await k.press("Enter");
    // Owner: a mismatched PIN is caught before anything is written
    await page.getByLabel("Owner's name").waitFor();
    await k.type("Olu");
    await k.press("Tab");
    await k.type("1234");
    await k.press("Tab");
    await k.type("1235");
    await k.press("Enter");
    await page.getByText("The two PINs do not match.").waitFor();
    await k.press("Backspace");
    await k.type("4");
    await k.press("Enter");
    // Review → create
    await page.getByRole("heading", { name: "Review" }).waitFor();
    await k.press("Enter");
    await page.getByRole("note").filter({ hasText: "SANDBOX" }).waitFor();
    expect(Date.now() - started).toBeLessThan(180_000);

    const [created] = await calls(page, "setup_create_workspace");
    expect(created?.input).toMatchObject({
      businessName: "Corner Shop",
      countryCode: "GB",
      timezone: "Europe/London",
      currencyCode: "GBP",
      pricesIncludeTax: true,
      ownerName: "Olu",
      ownerPin: "1234",
      taxRates: [{ name: "VAT 20%", ratePpm: 200000 }],
    });
    // The owner is signed in with a real sign-in, not a session made up by setup.
    const [login] = await calls(page, "auth_login");
    expect(login).toMatchObject({ pin: "1234" });
    await page.getByLabel("Status").getByText("Olu").waitFor();
  });
});

describe("sign-in (E05-T06)", () => {
  it("shows tries left, then a lockout countdown that blocks the pad", async () => {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Olu/ }).click();
    for (let i = 1; i <= 4; i++) {
      await page.keyboard.type("9999");
      await page.keyboard.press("Enter");
      await page.getByRole("alert").filter({ hasText: `${5 - i} ${5 - i === 1 ? "try" : "tries"} left` }).waitFor();
    }
    await page.keyboard.type("9999");
    await page.keyboard.press("Enter");
    await page.getByRole("alert").filter({ hasText: /Try again in (29|30) s/ }).waitFor();
    expect(await page.getByRole("button", { name: "7" }).isDisabled()).toBe(true);
    // Typing is ignored while locked.
    await page.keyboard.type("1234");
    await page.keyboard.press("Enter");
    expect(await calls(page, "auth_login")).toHaveLength(5);
  });

  it("signs in by touch on the pad and lands on checkout with the user shown", async () => {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Cal/ }).click();
    for (const d of "3333") await page.getByRole("button", { name: d, exact: true }).click();
    await page.getByRole("button", { name: "Sign in" }).click();
    const status = page.getByLabel("Status");
    await status.getByText("Cal").waitFor();
    expect(await status.textContent()).toContain("cashier");
    expect(await status.textContent()).toContain("Not linked");
  });
});

describe("checkout (E09-T03..T06)", () => {
  it("scan 3 items, change quantity, discount, remove: keyboard only", async () => {
    const page = await checkout();
    await scan(page, COLA);
    await scan(page, CRISPS);
    await scan(page, CHOC);
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 3);
    const scans = await calls(page, "scan_submit");
    expect(scans.map((s) => [s.raw, s.source])).toEqual([
      [COLA, "scanner"],
      [CRISPS, "scanner"],
      [CHOC, "scanner"],
    ]);
    expect(new Set(scans.map((s) => s.scanEventId)).size).toBe(3);

    // The last scanned line is selected. Up selects crisps; F3 edits its quantity inline.
    await page.keyboard.press("ArrowUp");
    await page.keyboard.press("F3");
    await page.getByLabel("Quantity for Crisps").waitFor();
    await page.keyboard.press("ControlOrMeta+a");
    await page.keyboard.type("3");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.body.textContent?.includes("Crisps"));
    expect((await calls(page, "cart_set_quantity")).at(-1)).toMatchObject({ qtyMilli: "3000" });

    // F4: 10% off that line.
    await page.keyboard.press("F4");
    await page.getByLabel("Discount % for Crisps").waitFor();
    await page.keyboard.type("10");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => window.__ipc.some((c) => c.cmd === "cart_set_line_discount"));
    expect((await calls(page, "cart_set_line_discount"))[0]).toMatchObject({ discount: { kind: "percent", ratePpm: 100000 } });

    // Numpad + adds one; Delete removes the selected line.
    await page.keyboard.press("NumpadAdd");
    await page.waitForFunction(() => window.__ipc.filter((c) => c.cmd === "cart_set_quantity").length === 2);
    expect((await calls(page, "cart_set_quantity")).at(-1)).toMatchObject({ qtyMilli: "4000" });
    await page.keyboard.press("Delete");
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 2);
    expect(await lines(page)).toEqual(["Cola 330ml x1", "Chocolate bar x1"]);
    // 1.20 + 0.99, as Rust (here: the fake) reported it.
    expect(await page.getByTestId("total").textContent()).toBe("2.19");

    // F9 opens payment; Enter on the empty cash field pays the exact amount and completes.
    await page.keyboard.press("F9");
    await page.getByRole("dialog", { name: "Payment" }).waitFor();
    await page.keyboard.press("Enter");
    const done = page.getByRole("dialog", { name: "Sale completed" });
    await done.waitFor();
    expect(await done.getByTestId("change").textContent()).toBe("0.00");
    await done.getByText("R01-000001").waitFor();
    expect(await done.getByTestId("saved").textContent()).toContain("Saved on this device");
    expect((await calls(page, "checkout_complete"))[0]).toMatchObject({ tenders: [{ kind: "cash", amountMinor: "219" }] });
    // The receipt, as it prints, at both paper widths.
    await done.getByRole("button", { name: "Show receipt" }).click();
    const paper = done.getByTestId("receipt-text");
    await paper.getByText("Receipt R01-000001").waitFor();
    expect(Math.max(...(await paper.textContent() ?? "").split("\n").map((l) => l.length))).toBe(48);
    await done.getByRole("button", { name: "58 mm" }).click();
    await page.waitForFunction(() => window.__ipc.some((c) => c.cmd === "receipt_get" && c.args.widthMm === 58));
    await page.waitForFunction(() => Math.max(...(document.querySelector("[data-testid=receipt-text]")?.textContent ?? "").split("\n").map((l) => l.length)) === 32);
    await done.getByRole("button", { name: /New sale/ }).focus();
    // Enter starts the next sale with an empty cart.
    await page.keyboard.press("Enter");
    await done.waitFor({ state: "detached" });
    expect(await lines(page)).toEqual([]);
  });

  it("highlights a changed line for at most 150 ms, without animation", async () => {
    const page = await checkout();
    await scan(page, COLA);
    const row = page.locator("[data-testid=cart-line]").first();
    await row.waitFor();
    expect(await row.evaluate((r) => getComputedStyle(r).transitionDuration)).toBe("0s");
    await page.waitForTimeout(250);
    expect(await row.getAttribute("class")).not.toContain("flash");
  });

  it("an unknown code changes nothing and offers a pre-filled search", async () => {
    const page = await checkout();
    await scan(page, "999999");
    const toast = page.getByRole("alert").filter({ hasText: "No product with code 999999" });
    await toast.waitFor();
    expect(await lines(page)).toEqual([]);
    await toast.getByRole("button", { name: "Search for it" }).click();
    const search = page.getByRole("combobox", { name: /Search products/ });
    expect(await search.inputValue()).toBe("999999");
    expect(await search.evaluate((el) => el === document.activeElement)).toBe(true);
  });

  it("search: type, arrow down, Enter adds; manual code entry uses the scan path", async () => {
    const page = await checkout();
    await page.keyboard.press("F2");
    await page.keyboard.type("c");
    await page.getByRole("listbox", { name: "Search results" }).waitFor();
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => window.__ipc.some((c) => c.cmd === "cart_add_variant"));
    expect((await calls(page, "cart_add_variant"))[0]).toMatchObject({ via: "search" });

    const field = page.getByLabel("Scan or type a code");
    await field.click();
    await field.pressSequentially("COLA", { delay: 60 }); // human speed: not a scan
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => window.__ipc.some((c) => c.cmd === "scan_submit"));
    expect((await calls(page, "scan_submit"))[0]).toMatchObject({ raw: "COLA", source: "manual" });
    expect(await field.inputValue()).toBe("");
  });

  it("clear and void ask first; a cashier's void needs a manager", async () => {
    const page = await checkout();
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.getByRole("button", { name: "Clear cart" }).click();
    await page.getByRole("dialog", { name: "Clear the cart?" }).waitFor();
    await page.keyboard.press("Escape");
    expect(await calls(page, "cart_clear")).toEqual([]);
    expect(await lines(page)).toHaveLength(1);

    await page.getByRole("button", { name: "Void sale" }).click();
    const dialog = page.getByRole("dialog", { name: "Void this sale?" });
    expect(await dialog.getByRole("button", { name: "Void sale" }).isDisabled()).toBe(true); // reason required
    await page.evaluate(() => {
      if (window.__fake.session) window.__fake.session.role = "cashier";
    });
    await dialog.getByLabel("Reason").fill("customer left");
    await dialog.getByRole("button", { name: "Void sale" }).click();
    const approval = page.getByRole("dialog", { name: "Manager approval" });
    await approval.getByText("voiding this sale").waitFor();
    await page.keyboard.press("Escape"); // cancelled: nothing voided
    await approval.waitFor({ state: "detached" });
    expect(await lines(page)).toHaveLength(1);

    await page.getByRole("button", { name: "Clear cart" }).click();
    await page.getByRole("dialog", { name: "Clear the cart?" }).getByRole("button", { name: "Clear cart" }).click();
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 0);
    expect((await calls(page, "cart_clear"))[0]).toMatchObject({ revision: 1 });
  });

  it("F12 locks: back to sign-in", async () => {
    const page = await checkout();
    await page.keyboard.press("F12");
    await page.getByRole("heading", { name: "Who is signing in?" }).waitFor();
    expect(await calls(page, "auth_logout")).toHaveLength(1);
  });
});

/** Fills the cart with `n` distinct lines in the fake, then scans once to show them. */
async function fill(page: Page, n: number) {
  await page.evaluate((count) => {
    const f = window.__fake;
    const p = f.products[0];
    if (!p) return;
    for (let i = 0; i < count; i++) {
      f.lines.push({ lineId: crypto.randomUUID(), variantId: p.variantId, qtyMilli: 1000n, discountPpm: 50_000 });
    }
  }, n);
  await scan(page, CRISPS);
  await page.waitForFunction((count) => document.querySelectorAll("[data-testid=cart-line]").length === count + 1, n);
}

function luminance(rgb: string): number {
  const [r, g, b] = (rgb.match(/\d+(\.\d+)?/g) ?? []).slice(0, 3).map((v) => {
    const c = parseInt(v, 10) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * (r ?? 0) + 0.7152 * (g ?? 0) + 0.0722 * (b ?? 0);
}

describe("layout and accessibility (E09-T03, E09-T05)", () => {
  it("at 1366×768 with 40 lines: table, totals and actions visible, no page scroll", async () => {
    const page = await checkout();
    await fill(page, 40);
    const fits = await page.evaluate(() => ({
      h: document.documentElement.scrollHeight <= innerHeight,
      w: document.documentElement.scrollWidth <= innerWidth,
    }));
    expect(fits).toEqual({ h: true, w: true });
    for (const target of [
      page.getByRole("columnheader", { name: "Item" }),
      page.getByTestId("total"),
      page.getByRole("button", { name: /Pay/ }),
      page.getByRole("button", { name: "Void sale" }),
      page.getByLabel("Status"),
    ]) {
      const box = await target.boundingBox();
      expect(box, await target.textContent() ?? "").not.toBeNull();
      if (box) expect(box.y + box.height).toBeLessThanOrEqual(768);
    }
  });

  it("the total due is at least 48 px with a contrast ratio of 7:1 or more", async () => {
    const page = await checkout();
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    const style = await page.getByTestId("total").evaluate((el) => {
      let bg = "rgba(0, 0, 0, 0)";
      for (let n: Element | null = el; n && /rgba\(.*, 0\)/.test(bg); n = n.parentElement) bg = getComputedStyle(n).backgroundColor;
      const s = getComputedStyle(el);
      return { size: parseInt(s.fontSize, 10), color: s.color, bg };
    });
    expect(style.size).toBeGreaterThanOrEqual(48);
    const [hi, lo] = [luminance(style.color), luminance(style.bg)].sort((a, b) => b - a);
    expect(((hi ?? 0) + 0.05) / ((lo ?? 0) + 0.05)).toBeGreaterThanOrEqual(7);
  });

  it("axe finds no serious or critical issues on checkout", async () => {
    const page = await checkout();
    await fill(page, 3);
    const axePath = createRequire(import.meta.url).resolve("axe-core/axe.min.js");
    await page.addScriptTag({ path: axePath });
    const violations = await page.evaluate(async () => {
      const axe = (window as unknown as { axe: { run: (ctx: Document) => Promise<{ violations: { id: string; impact: string; nodes: unknown[] }[] }> } }).axe;
      const res = await axe.run(document);
      return res.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => `${v.id}: ${JSON.stringify(v.nodes.map((n) => [(n as { html: string }).html, (n as { failureSummary?: string }).failureSummary]))}`);
    });
    expect(violations).toEqual([]);
  });

  it("150% text at 1920×1080: no horizontal overflow, totals and actions still visible", async () => {
    const page = await checkout("checkout", { width: 1920, height: 1080 });
    await page.addStyleTag({ content: "html { font-size: 150%; }" });
    await fill(page, 10);
    const w = await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);
    expect(w).toBe(true);
    for (const target of [page.getByTestId("total"), page.getByRole("button", { name: /Pay/ })]) {
      const box = await target.boundingBox();
      expect(box).not.toBeNull();
      if (box) expect(box.y + box.height).toBeLessThanOrEqual(1080);
    }
  });
});

describe("approvals and discount limits (E05-T04/T06, E08-T02)", () => {
  /** Signs Cal (cashier) in through the sign-in screen, so the UI and backend agree. */
  async function cashierCheckout(): Promise<Page> {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Cal/ }).click();
    await page.keyboard.type("3333");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    return page;
  }

  it("15% from a cashier: reason, manager PIN, then the same command is retried with the token", async () => {
    const page = await cashierCheckout();
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();

    await page.keyboard.press("F4");
    await page.keyboard.type("15");
    await page.keyboard.press("Enter");
    // Above the 10% limit: a reason first (it goes to the audit log).
    const reason = page.getByRole("dialog", { name: "Reason for 15% off" });
    await reason.getByLabel(/Reason/).fill("damaged box");
    await page.keyboard.press("Enter");

    // Rust (here the fake) answers APPROVAL_REQUIRED; the dialog names what is approved.
    const approval = page.getByRole("dialog", { name: "Manager approval" });
    await approval.getByText("a 15% discount").waitFor();
    // Only managers and owners are offered, never the requester (a cashier).
    expect(await approval.getByRole("button", { name: /Cal/ }).count()).toBe(0);
    await approval.getByRole("button", { name: /Olu/ }).waitFor();
    await approval.getByRole("button", { name: /Mia/ }).click();
    await page.keyboard.type("9999");
    await page.keyboard.press("Enter");
    await approval.getByRole("alert").filter({ hasText: "4 tries left" }).waitFor();
    await page.keyboard.type("2222");
    await page.keyboard.press("Enter");
    await approval.waitFor({ state: "detached" });

    const sent = await calls(page, "cart_set_line_discount");
    expect(sent).toHaveLength(2);
    expect(sent[0]).toMatchObject({ discount: { ratePpm: 150000, reason: "damaged box" }, approvalTokenId: null });
    expect(sent[1]).toMatchObject({ discount: { ratePpm: 150000, reason: "damaged box" } });
    expect(typeof sent[1]?.approvalTokenId).toBe("string");
    const [request] = await calls(page, "auth_request_approval");
    expect(request?.scope).toMatchObject({ permission: "discount_above_threshold", maxPpm: 150000 });
    // 1.20 less 15% = 1.02
    expect(await page.getByTestId("total").textContent()).toBe("1.02");
  });

  it("at or below the limit there is no reason prompt and no approval", async () => {
    const page = await cashierCheckout();
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.keyboard.press("F4");
    await page.keyboard.type("10");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => window.__ipc.some((c) => c.cmd === "cart_set_line_discount"));
    expect(await page.getByRole("dialog").count()).toBe(0);
    expect((await calls(page, "cart_set_line_discount"))[0]).toMatchObject({ discount: { reason: null } });
  });
});

describe("payment (E10-T03, E09-T08)", () => {
  async function payable(): Promise<Page> {
    const page = await checkout();
    await scan(page, COLA); // 1.20
    await scan(page, CHOC); // 0.99
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 2);
    await page.getByRole("button", { name: /^Pay/ }).click();
    await page.getByRole("dialog", { name: "Payment" }).waitFor();
    return page;
  }

  it("split: card recorded by the operator, then cash with change; tenders can be removed", async () => {
    const page = await payable();
    const pay = page.getByRole("dialog", { name: "Payment" });
    await pay.getByRole("button", { name: "Card (separate terminal)" }).click();
    await pay.getByLabel(/^Amount/).fill("1.00");
    await pay.getByLabel(/^Reference/).fill("AUTH 0042");
    await pay.getByRole("button", { name: /^Record/ }).click();
    const tenders = pay.getByRole("list", { name: "Tenders" });
    await tenders.getByText("Externally confirmed by operator").waitFor();
    expect(await pay.getByTestId("remaining").textContent()).toBe("1.19");
    // Never presented as a provider approval (PAY-003).
    expect(await pay.textContent()).not.toMatch(/approved|authori[sz]ed/i);
    // Remove and re-add, then cash 5.00 covers the rest with change.
    await tenders.getByRole("button", { name: /^Remove/ }).click();
    expect(await pay.getByTestId("remaining").textContent()).toBe("2.19");
    await pay.getByRole("button", { name: "Card (separate terminal)" }).click();
    await pay.getByLabel(/^Amount/).fill("1.00");
    await pay.getByRole("button", { name: /^Record/ }).click();
    await pay.getByLabel("Cash received").fill("5");
    await page.keyboard.press("Enter");
    const done = page.getByRole("dialog", { name: "Sale completed" });
    await done.waitFor();
    expect(await done.getByTestId("change").textContent()).toBe("3.81");
    expect((await calls(page, "checkout_complete"))[0]).toMatchObject({
      tenders: [
        { kind: "card_external", amountMinor: "100", reference: null },
        { kind: "cash", amountMinor: "500" },
      ],
    });
  });

  it("a refusal keeps the cart and the panel: a card number typed as the reference", async () => {
    const page = await payable();
    const pay = page.getByRole("dialog", { name: "Payment" });
    await pay.getByRole("button", { name: "Card (separate terminal)" }).click();
    await pay.getByLabel(/^Reference/).fill("4111 1111 1111 1111");
    await pay.getByRole("button", { name: /^Record/ }).click();
    await pay.getByRole("button", { name: "Complete sale" }).click();
    await pay.getByRole("alert").filter({ hasText: "not valid" }).waitFor();
    expect(await page.getByRole("dialog", { name: "Sale completed" }).count()).toBe(0);
    await page.keyboard.press("Escape");
    await pay.waitFor({ state: "detached" });
    expect(await lines(page)).toHaveLength(2);
  });
});
