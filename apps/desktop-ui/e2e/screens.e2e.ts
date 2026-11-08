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

describe("printing (E13-T06)", () => {
  async function sold(script: string[]): Promise<Page> {
    const page = await checkout();
    await page.evaluate((sc) => {
      window.__fake.printScript = sc;
    }, script);
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.keyboard.press("F9");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    return page;
  }

  it("no answer: asks whether it printed; 'not printed' makes a reprint", async () => {
    const page = await sold(["timeout"]);
    const done = page.getByRole("dialog", { name: "Sale completed" });
    await done.getByRole("button", { name: "Print receipt" }).click();
    await done.getByText("Print status unknown — did it print?").waitFor();
    await done.getByRole("button", { name: "Not printed — reprint" }).click();
    await done.getByTestId("print-status").filter({ hasText: "Reprint printed." }).waitFor();
    expect((await calls(page, "print_job_resolve"))[0]).toMatchObject({ printed: false });
  });

  it("paper out: says what to do; trying again prints the original", async () => {
    const page = await sold(["paper_out"]);
    const done = page.getByRole("dialog", { name: "Sale completed" });
    await done.getByRole("button", { name: "Print receipt" }).click();
    await done.getByText("The printer is out of paper. Load paper, then try again.").waitFor();
    await done.getByRole("button", { name: "Try again" }).click();
    await done.getByTestId("print-status").filter({ hasText: /^Printed\.$/ }).waitFor();
    expect(await calls(page, "print_job_retry")).toHaveLength(1);
  });
});

describe("returns (E12-T04)", () => {
  /** Three colas sold for cash by whoever is signed in; back on the till afterwards. */
  async function sellThreeColas(page: Page) {
    for (let i = 0; i < 3; i++) await scan(page, COLA);
    await page.waitForFunction(() => document.querySelector("[data-testid=cart-line] [data-testid=qty]")?.textContent === "3");
    await page.keyboard.press("F9");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor({ state: "detached" });
  }

  async function findSale(page: Page, number: string) {
    await page.keyboard.press("F7");
    await page.getByRole("heading", { name: "Returns" }).waitFor();
    // Focus is in the receipt number field: a receipt-barcode scan types there and ends with Enter.
    await page.keyboard.type(number, { delay: 5 });
    await page.keyboard.press("Enter");
  }

  it("a cashier's return: quantities, refund preview, manager approval, refund receipt", async () => {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Cal/ }).click();
    await page.keyboard.type("3333");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await sellThreeColas(page);

    await findSale(page, "r01-999999");
    await page.getByRole("alert").filter({ hasText: "No sale with that receipt number" }).waitFor();
    await page.getByLabel("Receipt number").fill("");
    await page.keyboard.type("R01-000001");
    await page.keyboard.press("Enter");
    await page.getByText("Sale R01-000001").waitFor();

    await page.getByLabel("Quantity to return: Cola 330ml").fill("2");
    await page.getByTestId("refund-total").filter({ hasText: "2.40" }).waitFor();
    await page.getByLabel("Reason").selectOption("defective");
    await page.getByRole("button", { name: "Refund 2.40" }).click();

    // A cashier: Rust (the fake) answers APPROVAL_REQUIRED for this sale.
    const approval = page.getByRole("dialog", { name: "Manager approval" });
    await approval.getByText("a return").waitFor();
    await approval.getByRole("button", { name: /Mia/ }).click();
    await page.keyboard.type("2222");
    await page.keyboard.press("Enter");

    const done = page.getByRole("region", { name: "Return completed" });
    await done.waitFor();
    expect(await done.getByTestId("refund").textContent()).toBe("2.40");
    await done.getByText("R01-R-000001", { exact: true }).waitFor();
    await done.getByText("approved by a manager", { exact: false }).waitFor();
    expect(await done.getByTestId("receipt-text").textContent()).toContain("REFUND TOTAL");
    await done.getByRole("button", { name: "Print receipt" }).click();
    await done.getByTestId("print-status").filter({ hasText: /^Printed\.$/ }).waitFor();

    const sent = await calls(page, "return_create");
    expect(sent).toHaveLength(2);
    expect(sent[0]).toMatchObject({
      lines: [{ quantityMilli: "2000", disposition: "restock" }],
      reason: "defective",
      refunds: [{ kind: "cash", amountMinor: "240", reference: null }],
      approvalTokenId: null,
    });
    expect(typeof sent[1]?.approvalTokenId).toBe("string");
    const [request] = await calls(page, "auth_request_approval");
    expect(request?.scope).toMatchObject({ permission: "return_create" });
    expect(await calls(page, "return_receipt_print")).toHaveLength(1);

    // Only one cola is left to return: asking for two is refused before anything is written.
    await done.getByRole("button", { name: "Another return" }).click();
    await page.keyboard.type("R01-000001");
    await page.keyboard.press("Enter");
    const row = page.getByRole("row", { name: /Cola 330ml/ });
    expect(await row.getByRole("cell").nth(1).textContent()).toBe("2");
    await page.getByLabel("Quantity to return: Cola 330ml").fill("2");
    await page.getByRole("alert").filter({ hasText: "more than is left to return" }).waitFor();
    expect(await page.getByRole("button", { name: /^Refund/ }).isDisabled()).toBe(true);
    // Esc steps back: the sale, then the till.
    await page.keyboard.press("Escape");
    await page.getByLabel("Receipt number").waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
  });

  it("an owner refunds to a card with no approval; the screen passes axe", async () => {
    const page = await checkout();
    await sellThreeColas(page);
    await findSale(page, "R01-000001");
    await page.getByLabel("Quantity to return: Cola 330ml").fill("1");
    await page.getByLabel("Refund by").selectOption("card_external");
    await page.getByLabel(/^Reference/).fill("AUTH 77");
    await page.getByTestId("refund-total").filter({ hasText: "1.20" }).waitFor();

    await page.addScriptTag({ path: createRequire(import.meta.url).resolve("axe-core/axe.min.js") });
    const serious = await page.evaluate(async () => {
      const axe = (window as unknown as { axe: { run: (ctx: Document) => Promise<{ violations: { id: string; impact: string; nodes: { html: string }[] }[] }> } }).axe;
      const res = await axe.run(document);
      return res.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => `${v.id}: ${v.nodes.map((n) => n.html).join(" | ")}`);
    });
    expect(serious).toEqual([]);

    await page.getByRole("button", { name: "Refund 1.20" }).click();
    await page.getByRole("region", { name: "Return completed" }).waitFor();
    expect(await page.getByRole("dialog", { name: "Manager approval" }).count()).toBe(0);
    expect((await calls(page, "return_create"))[0]).toMatchObject({
      refunds: [{ kind: "card_external", amountMinor: "120", reference: "AUTH 77" }],
      approvalTokenId: null,
    });
    // Enter goes back to the till.
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
  });
});

describe("sales history (E12-T03)", () => {
  /** Two sales: a cola for cash 5.00, then crisps + chocolate (1.84) with card 1.00 + cash. */
  async function twoSales(): Promise<Page> {
    const page = await checkout();
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.keyboard.press("F9");
    await page.getByLabel("Cash received").fill("5");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.keyboard.press("Escape");
    await scan(page, CRISPS);
    await scan(page, CHOC);
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 2);
    await page.keyboard.press("F9");
    const pay = page.getByRole("dialog", { name: "Payment" });
    await pay.getByRole("button", { name: "Card (separate terminal)" }).click();
    await pay.getByLabel(/^Amount/).fill("1.00");
    await pay.getByLabel(/^Reference/).fill("AUTH 9");
    await pay.getByRole("button", { name: /^Record/ }).click();
    await pay.getByLabel("Cash received").fill("1");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.keyboard.press("Escape");
    await page.keyboard.press("F8");
    await page.getByRole("heading", { name: "Sales" }).waitFor();
    return page;
  }

  const receipts = (page: Page) => page.getByTestId("sale-row").locator("button.link").allTextContents();

  it("newest first; filters by payment and receipt number; detail, reprint and return", async () => {
    const page = await twoSales();
    await page.getByTestId("sale-row").nth(1).waitFor();
    expect(await receipts(page)).toEqual(["R01-000002", "R01-000001"]);
    expect(await page.getByTestId("sale-row").first().textContent()).toContain("Card + Cash");

    await page.getByLabel("Payment").selectOption("card_external");
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=sale-row]").length === 1);
    expect(await receipts(page)).toEqual(["R01-000002"]);
    await page.getByLabel("Payment").selectOption("");
    // The receipt field has focus on arrival; digits are enough.
    await page.getByLabel("Receipt number").fill("1");
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=sale-row]").length === 1);
    expect(await receipts(page)).toEqual(["R01-000001"]);
    await page.getByLabel("Receipt number").fill("");
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=sale-row]").length === 2);

    await page.getByRole("button", { name: "R01-000002" }).click();
    const detail = page.getByRole("region", { name: "Sale detail" });
    await detail.waitFor();
    expect(await detail.getByTestId("sale-total").textContent()).toBe("1.84");
    const tenders = detail.getByRole("list", { name: "Tenders" });
    // Who confirmed an external payment is always shown, never as a provider approval.
    expect(await tenders.textContent()).toContain("Externally confirmed by operator");
    expect(await tenders.textContent()).toContain("ref AUTH 9");
    expect(await tenders.textContent()).toContain("change 0.16");
    await detail.getByText("None.").waitFor();
    await detail.getByRole("button", { name: "Print receipt" }).click();
    await detail.getByTestId("print-status").filter({ hasText: /printed/i }).waitFor();
    expect(await calls(page, "receipt_print")).toEqual([expect.objectContaining({ saleId: expect.any(String) })]);

    // "Return items" opens the return with this sale already found.
    await detail.getByRole("button", { name: "Return items" }).click();
    await page.getByText("Sale R01-000002").waitFor();
    await page.getByLabel("Quantity to return: Crisps").fill("1");
    await page.getByRole("button", { name: "Refund 0.85" }).click();
    await page.getByRole("region", { name: "Return completed" }).waitFor();
    await page.keyboard.press("Enter");
    await page.keyboard.press("F8");
    await page.getByRole("button", { name: "R01-000002" }).click();
    const returns = page.getByRole("list", { name: "Returns" });
    await returns.getByText("R01-R-000001", { exact: false }).waitFor();
    expect(await returns.textContent()).toContain("refunded 0.85 (Cash)");
    // Esc: back to the list, then the till.
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Sales list" }).waitFor();
    expect(await page.getByTestId("sale-row").first().textContent()).toContain("-0.85");
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
  });

  it("pages with 'Show more'; the screen passes axe", async () => {
    const page = await checkout();
    await page.evaluate(() => {
      for (let i = 1; i <= 60; i++) {
        window.__fake.sales.push({
          saleId: `0190a5b0-0000-7000-8000-9${String(i).padStart(11, "0")}`,
          receiptNumber: `R01-${String(i).padStart(6, "0")}`,
          lines: [{ saleLineId: `l${i}`, name: "Cola 330ml", qtyMilli: 1000n, totalMinor: 120n, returnedMilli: 0n }],
          localTime: "2026-11-07 08:00",
          cashierUserId: "x",
          cashierName: "Olu",
          tenders: [{ kind: "cash", amountMinor: 120n, reference: null }],
          totalMinor: 120n,
          changeMinor: 0n,
          returns: [],
        });
      }
    });
    await page.keyboard.press("F8");
    await page.getByTestId("sale-row").nth(49).waitFor();
    expect(await page.getByTestId("sale-row").count()).toBe(50);
    await page.getByRole("button", { name: "Show more" }).click();
    await page.getByTestId("sale-row").nth(59).waitFor();
    expect(await page.getByRole("button", { name: "Show more" }).count()).toBe(0);
    expect((await calls(page, "sales_list")).at(-1)).toMatchObject({ after: "50" });

    await page.addScriptTag({ path: createRequire(import.meta.url).resolve("axe-core/axe.min.js") });
    const serious = await page.evaluate(async () => {
      const axe = (window as unknown as { axe: { run: (ctx: Document) => Promise<{ violations: { id: string; impact: string; nodes: { html: string }[] }[] }> } }).axe;
      const res = await axe.run(document);
      return res.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => `${v.id}: ${v.nodes.map((n) => n.html).join(" | ")}`);
    });
    expect(serious).toEqual([]);
  });
});

describe("cash drawer (E15-T02)", () => {
  async function till(preset: "signIn" | "checkout", query = ""): Promise<Page> {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    page.on("pageerror", (e) => {
      throw e;
    });
    await page.goto(`${base}/app-harness.html?preset=${preset}${query}`);
    return page;
  }

  it("a cashier: no sale until the drawer is open; a blind count to close", async () => {
    const page = await till("signIn", "&drawer=required");
    await page.getByRole("button", { name: /Cal/ }).click();
    await page.keyboard.type("3333");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.getByText("Open the cash drawer before selling.").waitFor();
    expect(await page.getByTestId("drawer-status").textContent()).toBe("Drawer closed");
    // Rust (the fake) refuses the sale; the panel says what to do.
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.keyboard.press("F9");
    await page.keyboard.press("Enter");
    await page.getByRole("alert").filter({ hasText: "The cash drawer is not open" }).waitFor();
    await page.keyboard.press("Escape");

    await page.keyboard.press("F10");
    await page.getByRole("heading", { name: "Cash drawer" }).waitFor();
    await page.keyboard.type("100");
    await page.keyboard.press("Enter");
    await page.getByText("R01-Z-0001").waitFor();
    // A cashier never sees what the drawer should hold.
    expect(await page.getByText("Expected in the drawer").count()).toBe(0);
    await page.keyboard.press("Escape");
    expect(await page.getByTestId("drawer-status").textContent()).toBe("Drawer open · R01-Z-0001");
    expect(await page.getByText("Open the cash drawer before selling.").count()).toBe(0);
    await page.keyboard.press("F9");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.keyboard.press("Escape");

    await page.keyboard.press("F10");
    await page.getByLabel(/Counted cash/).fill("101.20");
    await page.keyboard.press("Enter");
    const confirm = page.getByRole("dialog", { name: "Close the drawer?" });
    await confirm.getByText("Closing is final").waitFor();
    await confirm.getByRole("button", { name: "Close drawer" }).click();
    const closed = page.getByRole("region", { name: "Drawer closed" });
    await closed.waitFor();
    expect(await closed.getByTestId("counted").textContent()).toBe("101.20");
    await closed.getByText("A manager will check the count").waitFor();
    expect(await closed.getByTestId("variance").count()).toBe(0);
    expect(await closed.getByRole("button", { name: "Print receipt" }).count()).toBe(0);
    expect((await calls(page, "cash_session_close"))[0]).toMatchObject({ countedMinor: "10120", note: null });
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    expect(await page.getByTestId("drawer-status").textContent()).toBe("Drawer closed");
  });

  it("an owner sees expected cash and the variance, then prints the Z summary", async () => {
    const page = await till("checkout");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.keyboard.press("F10");
    await page.getByLabel(/Opening float/).fill("50");
    await page.keyboard.press("Enter");
    await page.getByText("R01-Z-0001").waitFor();
    await page.keyboard.press("Escape");
    await scan(page, COLA); // 1.20, cash 5.00
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.keyboard.press("F9");
    await page.getByLabel("Cash received").fill("5");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.keyboard.press("Escape");

    await page.keyboard.press("F10");
    expect(await page.getByTestId("running-expected").textContent()).toBe("51.20");
    await page.getByLabel(/Counted cash/).fill("51");
    await page.getByLabel("Note (optional)").fill("coin short");
    await page.getByRole("button", { name: "Close drawer" }).click();
    await page.getByRole("dialog", { name: "Close the drawer?" }).getByRole("button", { name: "Close drawer" }).click();
    const closed = page.getByRole("region", { name: "Drawer closed" });
    await closed.waitFor();
    expect(await closed.getByTestId("expected").textContent()).toBe("51.20");
    expect(await closed.getByTestId("variance").textContent()).toBe("0.20 short");
    expect(await closed.getByTestId("receipt-text").textContent()).toContain("Z SUMMARY");
    await closed.getByRole("button", { name: "Print receipt" }).click();
    await closed.getByTestId("print-status").filter({ hasText: /printed/i }).waitFor();
    expect(await calls(page, "cash_session_z_print")).toHaveLength(1);

    await page.addScriptTag({ path: createRequire(import.meta.url).resolve("axe-core/axe.min.js") });
    const serious = await page.evaluate(async () => {
      const axe = (window as unknown as { axe: { run: (ctx: Document) => Promise<{ violations: { id: string; impact: string; nodes: { html: string }[] }[] }> } }).axe;
      const res = await axe.run(document);
      return res.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => `${v.id}: ${v.nodes.map((n) => n.html).join(" | ")}`);
    });
    expect(serious).toEqual([]);
  });
});

describe("stock (E11-T02/T03)", () => {
  it("receive goods by scanning: a second scan adds to the line; then an adjustment with a reason", async () => {
    const page = await checkout();
    await page.keyboard.press("F11");
    await page.getByRole("heading", { name: "Stock" }).waitFor();
    // The code field has focus: a scanner types there and ends with Enter.
    await page.keyboard.type(COLA, { delay: 5 });
    await page.keyboard.press("Enter");
    await page.getByTestId("stock-line").first().waitFor();
    // Scanner-ready elsewhere on the screen works too, and a case barcode adds 12.
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await page.keyboard.type(COLA, { delay: 5 });
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => (document.querySelector("[aria-label='Quantity: Cola 330ml']") as HTMLInputElement | null)?.value === "2");
    await page.getByLabel("Scan or type a code").fill("CASE12");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => (document.querySelector("[aria-label='Quantity: Cola 330ml']") as HTMLInputElement | null)?.value === "14");
    await page.getByLabel("Scan or type a code").fill("CRISP");
    await page.keyboard.press("Enter");
    await page.getByLabel("Quantity: Crisps").fill("24");
    await page.getByLabel("Unit cost: Crisps").fill("0.40");
    await page.getByLabel("Scan or type a code").fill("NOPE");
    await page.keyboard.press("Enter");
    await page.getByRole("alert").filter({ hasText: "No item has the code NOPE" }).waitFor();
    await page.getByLabel("Supplier (optional)").fill("Wholesale Ltd");
    await page.getByRole("button", { name: "Post goods received" }).click();
    await page.getByTestId("posted").filter({ hasText: "R01-IN-000001" }).waitFor();
    expect(await page.getByTestId("stock-line").count()).toBe(0);
    expect((await calls(page, "inventory_post_document"))[0]).toMatchObject({
      draft: {
        docType: "receipt",
        reason: null,
        supplierName: "Wholesale Ltd",
        lines: [
          { quantityMilli: "14000", unitCostMinor: null },
          { quantityMilli: "24000", unitCostMinor: "40" },
        ],
      },
    });

    // An "other" adjustment needs a note; a count correction may be negative.
    await page.getByRole("radio", { name: "Adjust stock" }).click();
    await page.getByLabel("Reason").selectOption("other");
    await page.getByLabel("Scan or type a code").fill("CHOC");
    await page.keyboard.press("Enter");
    await page.getByLabel("Quantity: Chocolate bar").fill("-1");
    expect(await page.getByLabel("Unit cost: Chocolate bar").count()).toBe(0);
    await page.getByRole("button", { name: "Post adjustment" }).click();
    await page.getByRole("alert").filter({ hasText: "not valid" }).waitFor();
    await page.getByLabel(/^Note/).fill("used for the window display");
    await page.getByRole("button", { name: "Post adjustment" }).click();
    await page.getByTestId("posted").filter({ hasText: "R01-IN-000002" }).waitFor();
    expect((await calls(page, "inventory_post_document")).at(-1)).toMatchObject({
      draft: { docType: "adjustment", reason: "other", note: "used for the window display", lines: [{ quantityMilli: "-1000" }] },
    });
    // A loss cannot be negative: refused on the screen before Rust.
    await page.getByLabel("Reason").selectOption("loss");
    await page.getByLabel("Scan or type a code").fill("CHOC");
    await page.keyboard.press("Enter");
    await page.getByLabel("Quantity: Chocolate bar").fill("-1");
    await page.getByRole("button", { name: "Post adjustment" }).click();
    await page.getByRole("alert").filter({ hasText: "Check the quantity for Chocolate bar" }).waitFor();

    await page.addScriptTag({ path: createRequire(import.meta.url).resolve("axe-core/axe.min.js") });
    const serious = await page.evaluate(async () => {
      const axe = (window as unknown as { axe: { run: (ctx: Document) => Promise<{ violations: { id: string; impact: string; nodes: { html: string }[] }[] }> } }).axe;
      const res = await axe.run(document);
      return res.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => `${v.id}: ${v.nodes.map((n) => n.html).join(" | ")}`);
    });
    expect(serious).toEqual([]);
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
  });

  it("hints when the cart holds more than is in stock, and flags negative stock in the product list", async () => {
    const page = await checkout();
    for (let i = 0; i < 3; i++) await scan(page, CRISPS); // 3 in stock: no hint yet
    await page.waitForFunction(() => document.querySelector("[data-testid=cart-line] [data-testid=qty]")?.textContent === "3");
    expect(await page.getByTestId("stock-hint").count()).toBe(0);
    await scan(page, CRISPS);
    await page.getByTestId("stock-hint").filter({ hasText: "Only 3 in stock" }).waitFor();
    await scan(page, CHOC); // already below zero
    await page.getByTestId("stock-hint").filter({ hasText: "None in stock" }).waitFor();
    // A hint only: paying is still possible.
    expect(await page.getByRole("button", { name: /^Pay/ }).isDisabled()).toBe(false);

    await page.keyboard.press("F6");
    const row = page.getByRole("row", { name: /Chocolate bar/ });
    await row.getByText("Negative stock").waitFor();
    expect(await page.getByRole("row", { name: /Crisps/ }).getByText("Negative stock").count()).toBe(0);
  });

  it("a cashier sees stock on hand and its history, but cannot receive or adjust", async () => {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Cal/ }).click();
    await page.keyboard.type("3333");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.keyboard.press("F11");
    const onHand = page.getByRole("region", { name: "Stock on hand" });
    await onHand.getByText("calculated from recorded movements").waitFor();
    expect(await page.getByRole("tab", { name: "Receive and adjust" }).count()).toBe(0);
    expect(await page.getByLabel("Scan or type a code").count()).toBe(0);
    await onHand.getByTestId("onhand-row").nth(2).waitFor();
    expect(await onHand.getByTestId("onhand-row").nth(0).textContent()).toContain("Negative");
    await onHand.getByRole("button", { name: "Cola 330ml" }).click();
    const history = page.getByRole("region", { name: "History: Cola 330ml" });
    await history.getByTestId("history-row").nth(1).waitFor();
    const rows = await history.getByTestId("history-row").allTextContents();
    expect(rows[0]).toContain("Sale");
    expect(rows[0]).toContain("-2");
    expect(rows[0]).toContain("R01-000004");
    expect(rows[1]).toContain("+24");
    expect(rows[1]).toContain("Goods received");
    await onHand.getByLabel("Low and negative only").check();
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=onhand-row]").length === 2);
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
  });
});

describe("reports (E14-T03)", () => {
  it("daily sales, payments, top products and stock, each marked 'This device only'", async () => {
    const page = await checkout();
    // Cola 1.20 for cash 5.00; Crisps 0.85 + Chocolate 0.99 = 1.84 by card 1.00 + cash.
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.keyboard.press("F9");
    await page.getByLabel("Cash received").fill("5");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.keyboard.press("Escape");
    await scan(page, CRISPS);
    await scan(page, CHOC);
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 2);
    await page.keyboard.press("F9");
    const pay = page.getByRole("dialog", { name: "Payment" });
    await pay.getByRole("button", { name: "Card (separate terminal)" }).click();
    await pay.getByLabel(/^Amount/).fill("1.00");
    await pay.getByRole("button", { name: /^Record/ }).click();
    await pay.getByLabel("Cash received").fill("1");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.keyboard.press("Escape");

    await page.getByRole("button", { name: "Reports" }).click();
    await page.getByRole("heading", { name: "Reports" }).waitFor();
    await page.getByTestId("scope-badge").filter({ hasText: "This device only" }).waitFor();
    const sales = page.getByRole("region", { name: "Daily sales" });
    expect(await sales.getByTestId("sales-total").textContent()).toBe("3.04");
    expect(await sales.getByTestId("net-incl").textContent()).toBe("3.04");
    expect(await sales.getByTestId("day-row").count()).toBe(1);
    const range = (await calls(page, "reports_sales_summary"))[0]?.range as { from: string; to: string };
    expect(range.from).toBe(range.to); // "Today" by default
    await page.getByRole("button", { name: "Last 7 days" }).click();
    // The last request is for a 7-day range ending today.
    await page.waitForFunction(() => {
      const r = window.__ipc.filter((c) => c.cmd === "reports_sales_summary").at(-1)?.args.range as { from: string; to: string } | undefined;
      return r !== undefined && r.from < r.to;
    });
    const week = (await calls(page, "reports_sales_summary")).at(-1)?.range as { from: string; to: string };
    expect(week.to).toBe(range.to);

    await page.getByRole("tab", { name: "Payments" }).click();
    const mix = page.getByRole("region", { name: "Payments" });
    await mix.getByTestId("mix-row").nth(1).waitFor();
    const rows = await mix.getByTestId("mix-row").allTextContents();
    expect(rows[0]).toContain("Card");
    expect(rows[0]).toContain("Recorded by staff");
    expect(rows[1]).toContain("Cash");
    expect(rows[1]).toContain("2.04"); // 1.20 + 0.84 kept
    await page.getByTestId("scope-badge").filter({ hasText: "This device only" }).waitFor();

    await page.getByRole("tab", { name: "Top products" }).click();
    const top = page.getByRole("region", { name: "Top products" });
    await top.getByTestId("top-row").nth(2).waitFor();
    expect(await top.getByTestId("top-row").first().textContent()).toContain("Cola 330ml");

    await page.getByRole("tab", { name: "Stock" }).click();
    const stock = page.getByRole("region", { name: "Stock" });
    await stock.getByText("calculated from recorded movements").waitFor();
    await stock.getByTestId("stock-row").nth(2).waitFor();
    await stock.getByLabel("Low and negative only").check();
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=stock-row]").length === 2);
    expect(await stock.getByTestId("stock-row").allTextContents()).toEqual([
      expect.stringContaining("Negative"),
      expect.stringContaining("Low"),
    ]);

    await page.addScriptTag({ path: createRequire(import.meta.url).resolve("axe-core/axe.min.js") });
    const serious = await page.evaluate(async () => {
      const axe = (window as unknown as { axe: { run: (ctx: Document) => Promise<{ violations: { id: string; impact: string; nodes: { html: string }[] }[] }> } }).axe;
      const res = await axe.run(document);
      return res.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => `${v.id}: ${v.nodes.map((n) => n.html).join(" | ")}`);
    });
    expect(serious).toEqual([]);
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
  });

  it("exports the report on screen and the records for the dates as CSV", async () => {
    const page = await checkout();
    await page.getByRole("button", { name: "Reports" }).click();
    await page.getByRole("heading", { name: "Reports" }).waitFor();
    await page.getByRole("button", { name: "Export this report (CSV)" }).click();
    await page.getByTestId("export-saved").filter({ hasText: "sales_summary_" }).waitFor();
    await page.getByRole("tab", { name: "Stock" }).click();
    await page.getByRole("button", { name: "Export this report (CSV)" }).click();
    await page.getByTestId("export-saved").filter({ hasText: "stock.csv" }).waitFor();
    await page.getByRole("region", { name: "Data exports" }).getByRole("button", { name: "Products" }).click();
    await page.getByTestId("export-saved").filter({ hasText: "Saved 3 rows" }).waitFor();
    const sent = await calls(page, "export_csv");
    expect(sent.map((c) => c.dataset)).toEqual(["sales_summary", "stock", "products"]);
    expect(sent[0]?.range).toMatchObject({ from: expect.any(String), to: expect.any(String) });
    expect(sent[1]?.range).toBeNull();
  });

  it("the owner reads the audit log, filters it and opens what an entry is about", async () => {
    const page = await checkout();
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.keyboard.press("F9");
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "Reports" }).click();
    await page.getByRole("button", { name: "Audit log" }).click();
    await page.getByRole("heading", { name: "Audit log" }).waitFor();
    const entries = page.getByRole("region", { name: "Audit entries" });
    await entries.getByTestId("audit-row").nth(2).waitFor();
    expect(await entries.getByTestId("audit-row").first().textContent()).toContain("Sale");
    await page.getByLabel("What").selectOption("auth.");
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=audit-row]").length === 1);
    expect(await entries.getByTestId("audit-row").first().textContent()).toContain("Signed in");
    expect((await calls(page, "audit_list")).at(-1)).toMatchObject({ filter: { action: "auth." } });
    await page.getByLabel("What").selectOption("");
    // A sale opens its detail; Esc comes back to the log.
    await entries.getByRole("button", { name: "R01-000001" }).click();
    await page.getByRole("region", { name: "Sale detail" }).waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("heading", { name: "Audit log" }).waitFor();
    // A product opens the editor.
    await page.getByRole("region", { name: "Audit entries" }).getByRole("button", { name: "Cola 330ml" }).click();
    await page.getByLabel(/^Name/).first().waitFor();
  });

  it("a manager has Reports but no Audit log", async () => {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Mia/ }).click();
    await page.keyboard.type("2222");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.getByRole("button", { name: "Reports" }).click();
    await page.getByRole("heading", { name: "Reports" }).waitFor();
    expect(await page.getByRole("button", { name: "Audit log" }).count()).toBe(0);
  });

  it("a cashier has no Reports action", async () => {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Cal/ }).click();
    await page.keyboard.type("3333");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    expect(await page.getByRole("button", { name: "Reports" }).count()).toBe(0);
  });
});

describe("going live (E04-T02)", () => {
  it("the owner confirms with their PIN; the till turns live and needs a drawer", async () => {
    const page = await checkout();
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    // With an item in the cart it is refused, with what to do.
    await scan(page, COLA);
    await page.locator("[data-testid=cart-line]").first().waitFor();
    await page.getByRole("button", { name: "Go live…" }).click();
    const dialog = page.getByRole("dialog", { name: "Go live" });
    await dialog.getByText("This cannot be undone").waitFor();
    await page.keyboard.type("1234");
    await page.keyboard.press("Enter");
    await dialog.getByRole("alert").filter({ hasText: "Finish or clear the cart" }).waitFor();
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    // The PIN was not taken as a scan.
    expect(await page.locator("[data-testid=cart-line]").count()).toBe(1);
    await page.getByRole("button", { name: /^Clear cart/ }).click();
    await page.getByRole("dialog", { name: "Clear the cart?" }).getByRole("button", { name: "Clear cart" }).click();
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 0);

    await page.getByRole("button", { name: "Go live…" }).click();
    await page.keyboard.type("9999");
    await page.keyboard.press("Enter");
    await dialog.getByRole("alert").filter({ hasText: "Wrong PIN" }).waitFor();
    await page.keyboard.type("1234");
    await page.keyboard.press("Enter");
    await dialog.waitFor({ state: "detached" });
    expect(await page.getByText("SANDBOX: practice mode").count()).toBe(0);
    await page.getByText("Open the cash drawer before selling.").waitFor();
    await page.getByRole("contentinfo", { name: "Status" }).getByText("LIVE").waitFor();
    expect((await calls(page, "register_go_live")).map((c) => c.pinCode)).toEqual(["1234", "9999", "1234"]);
  });

  it("only the owner is offered the switch", async () => {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Mia/ }).click();
    await page.keyboard.type("2222");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.getByText("SANDBOX: practice mode").waitFor();
    expect(await page.getByRole("button", { name: "Go live…" }).count()).toBe(0);
  });
});

describe("settings (E04-T03)", () => {
  async function signInAs(name: RegExp, pin: string, before?: (page: Page) => Promise<void>): Promise<Page> {
    const page = await open("signIn");
    if (before) await before(page);
    await page.getByRole("button", { name }).click();
    await page.keyboard.type(pin);
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    return page;
  }

  it("the owner changes typed settings, each saved with the version it showed", async () => {
    const page = await checkout();
    await page.getByRole("button", { name: "Settings" }).click();
    await page.getByRole("heading", { name: "Settings" }).waitFor();
    // Inventory: a switch saves at once.
    await page.getByLabel(/Allow selling below zero stock/).uncheck();
    await page.getByText("Saved.").waitFor();
    // Security: a percentage, sent as ppm; a bad number is caught on the screen.
    await page.getByRole("tab", { name: "Security" }).click();
    const limit = page.getByLabel(/Largest discount a cashier may give/);
    await limit.fill("15");
    await limit.press("Enter");
    await page.waitForFunction(() => window.__ipc.filter((c) => c.cmd === "settings_update").length === 2);
    const lock = page.getByLabel(/Lock the till after/);
    await lock.fill("soon");
    await lock.press("Enter");
    await page.getByRole("alert").filter({ hasText: "whole number from 0 to 3600" }).waitFor();
    // Receipts: footer lines become a JSON list.
    await page.getByRole("tab", { name: "Receipts" }).click();
    const footer = page.getByLabel(/Lines at the bottom of receipts/);
    await footer.fill("Thank you\n\nNo refunds without a receipt");
    await page.getByRole("region", { name: "Receipts" }).getByRole("button", { name: "Save" }).click();
    await page.waitForFunction(() => window.__ipc.filter((c) => c.cmd === "settings_update").length === 3);
    expect(await calls(page, "settings_update")).toEqual([
      { key: "inventory.allow_negative", valueJson: "false", baseVersion: 1 },
      { key: "discount.cashier_max_ppm", valueJson: "150000", baseVersion: 1 },
      { key: "receipt.footer_lines", valueJson: JSON.stringify(["Thank you", "No refunds without a receipt"]), baseVersion: 1 },
    ]);
    // Someone else changed the width meanwhile: refused, and the current value is shown.
    await page.evaluate(() => {
      window.__fake.settings.set("receipt.paper_width_mm", { valueJson: "58", version: 2 });
    });
    await page.getByLabel(/Paper width/).selectOption("80");
    await page.getByRole("alert").filter({ hasText: "Someone changed this meanwhile" }).waitFor();
    await page.waitForFunction(() => (document.querySelector("select[id='setting-receipt.paper_width_mm']") as HTMLSelectElement | null)?.value === "58");
  });

  it("idle for the lock timeout: back to sign-in, through auth_lock", async () => {
    const page = await signInAs(/Cal/, "3333", async (p) => {
      await p.evaluate(() => window.__fake.settings.set("security.lock_timeout_secs", { valueJson: "1", version: 2 }));
    });
    await page.getByRole("button", { name: /Mia/ }).waitFor({ timeout: 5000 });
    expect(await calls(page, "auth_lock")).toHaveLength(1);
  });

  it("the scanner suffix setting is used: a Tab-terminated scan adds the item", async () => {
    const page = await signInAs(/Cal/, "3333", async (p) => {
      await p.evaluate(() => window.__fake.settings.set("scanner.suffix", { valueJson: '"Tab"', version: 2 }));
    });
    await page.waitForFunction(() => window.__ipc.some((c) => c.cmd === "app_preferences"));
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await page.keyboard.type(COLA, { delay: 5 });
    await page.keyboard.press("Tab");
    await page.locator("[data-testid=cart-line]").first().waitFor();
    expect(await page.getByRole("button", { name: "Settings" }).count()).toBe(0); // cashier
  });
});

describe("taxes (E06-T05)", () => {
  it("a VAT change on a date: end the old rate, add the new one, swap it into the category", async () => {
    const page = await checkout();
    await page.getByRole("button", { name: "Settings" }).click();
    await page.getByRole("tab", { name: "Taxes" }).click();
    const rates = page.getByRole("table", { name: "Tax rates" });
    await rates.getByText("VAT 20%").waitFor();
    // The old rate ends on the day the new one starts.
    await page.getByTestId("rate-row").filter({ hasText: "VAT 20%" }).getByRole("button", { name: "Edit" }).click();
    await page.getByLabel("Until (not including)").fill("2026-04-01");
    await page.getByRole("button", { name: "Save rate" }).click();
    await page.getByTestId("rate-row").filter({ hasText: "2026-04-01" }).waitFor();
    const form = page.getByRole("form", { name: "Add a rate" });
    await form.getByLabel("Name").fill("VAT 22%");
    await form.getByLabel("Rate (%)").fill("22");
    await form.getByLabel("From").fill("2026-04-01");
    await page.getByRole("button", { name: "Add rate" }).click();
    await page.getByTestId("rate-row").filter({ hasText: "VAT 22%" }).waitFor();
    // A second "VAT 22%" over the same days is refused, naming the clash.
    await form.getByLabel("Name").fill("vat 22%");
    await form.getByLabel("Rate (%)").fill("22");
    await form.getByLabel("From").fill("2026-05-01");
    await page.getByRole("button", { name: "Add rate" }).click();
    await page.getByRole("alert").filter({ hasText: 'Another active rate called "VAT 22%"' }).waitFor();
    // Standard takes both rates (each applies on its own dates).
    const standard = page.getByRole("group", { name: "Rates in Standard" });
    await standard.getByLabel("VAT 22%").check();
    await page.waitForFunction(() => window.__ipc.some((c) => c.cmd === "tax_category_save"));
    // The old rate is still in Standard: archiving it is refused, naming the category.
    await page.getByRole("button", { name: "Archive VAT 20%" }).click();
    await page.getByRole("alert").filter({ hasText: "Still used by Standard" }).waitFor();
    // A new category made the default takes the flag from Standard.
    await page.getByLabel("New tax category").fill("Zero-rated");
    await page.getByRole("button", { name: "Add tax category" }).click();
    await page.getByTestId("taxcat-row").filter({ hasText: "Zero-rated" }).getByRole("button", { name: "Make default" }).click();
    await page.getByTestId("taxcat-row").filter({ hasText: "Standard" }).getByRole("button", { name: "Make default" }).waitFor();
    const rate20 = "0190a5b0-0000-7000-8000-00000000ra01";
    expect(await calls(page, "tax_rate_save")).toEqual([
      { taxRateId: rate20, name: "VAT 20%", ratePpm: 200000, from: null, to: "2026-04-01", baseVersion: 1 },
      { taxRateId: null, name: "VAT 22%", ratePpm: 220000, from: "2026-04-01", to: null, baseVersion: null },
      { taxRateId: null, name: "vat 22%", ratePpm: 220000, from: "2026-05-01", to: null, baseVersion: null },
    ]);
    const saves = await calls(page, "tax_category_save");
    expect(saves[0]).toEqual({ taxCategoryId: expect.any(String), name: "Standard", rateIds: [rate20, expect.any(String)], isDefault: true, baseVersion: 1 });
    expect(saves.at(-1)).toMatchObject({ name: "Zero-rated", isDefault: true, baseVersion: 1 });
    const axePath = createRequire(import.meta.url).resolve("axe-core/axe.min.js");
    await page.addScriptTag({ path: axePath });
    const violations = await page.evaluate(async () => {
      const axe = (window as unknown as { axe: { run: (ctx: Document) => Promise<{ violations: { id: string; impact: string }[] }> } }).axe;
      const res = await axe.run(document);
      return res.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => v.id);
    });
    expect(violations).toEqual([]);
  });

  it("a manager has no Taxes page; Settings is the owner's", async () => {
    const page = await open("signIn");
    await page.getByRole("button", { name: /Mia/ }).click();
    await page.keyboard.type("2222");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    expect(await page.getByRole("button", { name: "Settings" }).count()).toBe(0);
  });
});
