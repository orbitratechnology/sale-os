/**
 * Settings → Cloud (E17-T08) in Chromium against the scripted backend: linking by creating
 * the cloud business, joining (offered only to a till with nothing of its own), a wrong
 * password, and the message after the owner disconnected the till.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { chromium, type Browser, type Page } from "playwright";
import { createServer, type ViteDevServer } from "vite";
import { fileURLToPath } from "node:url";
import { RECOVERY_KEY } from "./app/fakeBackend";

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
  await page.getByRole("heading", { name: "Cloud", exact: true }).waitFor();
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

describe("sync status and repair queue (E17-T09)", () => {
  it("the status bar shows each sync state", async () => {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    await page.clock.install();
    await page.goto(`${base}/app-harness.html?preset=checkout`);
    const bar = page.getByTestId("sync-status");
    await bar.waitFor();
    expect(await bar.textContent()).toBe("Not linked · saved on this computer");
    const states: [() => void, RegExp][] = [
      [() => ((window.__fake.cloud.linkState = "linked"), (window.__fake.cloud.pending = 3)), /^Sync pending \(3\)$/],
      [() => (window.__fake.cloud.pending = 0), /^Synced \(last at \d\d:\d\d\)$/],
      [() => (window.__fake.cloud.conflicts = 2), /^Attention needed \(2 conflicts\)$/],
      [() => (window.__fake.cloud.linkState = "revoked"), /^Device revoked/],
    ];
    for (const [set, text] of states) {
      await page.evaluate(set);
      // The bar asks Rust again every 10 s.
      await page.clock.fastForward(10_000);
      await expect.poll(async () => (await bar.textContent()) ?? "").toMatch(text);
    }
  });

  it("a completed sale says sync pending until the cloud has it, then synced (AT-13)", async () => {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    await page.goto(`${base}/app-harness.html?preset=checkout`);
    await page.evaluate(() => {
      window.__fake.cloud.linkState = "linked";
    });
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await page.keyboard.type("4006381333931", { delay: 5 });
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=cart-line]").length === 1);
    await page.keyboard.press("F9");
    await page.getByRole("dialog", { name: "Payment" }).waitFor();
    await page.keyboard.press("Enter");
    const saved = page.getByRole("dialog", { name: "Sale completed" }).getByTestId("saved");
    await saved.getByText("Saved on this device · Sync pending").waitFor();
    // The cloud applies it: the next check says so.
    await page.evaluate(() => {
      const asked = window.__ipc.find((c) => c.cmd === "sale_sync_state");
      if (asked) window.__fake.cloud.synced.add(String(asked.args.saleId));
    });
    await saved.getByText("Synced").waitFor({ timeout: 10_000 });
  });

  it("the owner sees refused events with their reason, the payload, and exports a support file", async () => {
    const page = await cloudPage(async (p) => {
      await p.evaluate(() => {
        const c = window.__fake.cloud;
        c.linkState = "linked";
        c.pending = 0;
        c.conflicts = 1;
        c.refused = [{ eventId: "e1", localSequence: 41, eventType: "sale.completed", code: "SNAPSHOT_MISMATCH", kind: "rejected" }];
        window.__fake.saveAs = ["support.json"];
      });
    });
    const refused = page.getByRole("region", { name: "Refused by the cloud" });
    await refused.getByText("SNAPSHOT_MISMATCH").waitFor();
    await refused.getByText(/totals of this record and they did not add up/).waitFor();
    await refused.getByRole("button", { name: "View what was sent" }).click();
    const sent = await page.getByLabel("What was sent for #41").textContent();
    expect(sent).toContain("[REDACTED]");
    await page.getByRole("region", { name: "Conflicts" }).getByText(/edited here from version 3, the cloud is at 4/).waitFor();
    await page.getByRole("button", { name: "Export for support" }).click();
    await page.getByText("Saved support.json. Send it to support.").waitFor();
    expect(await calls(page, "sync_repair_action")).toEqual([
      { action: "view_payload", eventId: "e1" },
      { action: "export_support", eventId: null },
    ]);
  });
});

describe("sync conflicts (E18-T02)", () => {
  it("a manager resolves a conflict field by field, with a custom price in pounds", async () => {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    page.on("pageerror", (e) => {
      throw e;
    });
    await page.goto(`${base}/app-harness.html?preset=signIn`);
    await page.evaluate(() => {
      window.__fake.cloud.linkState = "linked";
      window.__fake.cloud.pending = 1;
      window.__fake.cloud.conflicts = 1;
    });
    await page.getByRole("button", { name: /Mia/ }).click();
    await page.keyboard.type("2222");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    // The till keeps working; the product list badges the product, and the status bar
    // points to the conflict.
    await page.keyboard.press("F6");
    await page.getByRole("heading", { name: "Products" }).waitFor();
    await page.getByRole("table", { name: "Products" }).getByText("Sync conflict").waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.getByTestId("sync-status").getByText(/Attention needed \(1 conflict\)/).waitFor();
    await page.getByRole("button", { name: "Resolve conflicts" }).click();
    const conflict = page.getByRole("region", { name: "Conflict: Cola 330ml" });
    await conflict.getByRole("cell", { name: "1.50" }).waitFor();
    await conflict.getByRole("cell", { name: "1.30" }).waitFor();
    // Name: keep the cloud's. Price: something else, typed as money.
    await conflict.getByRole("group", { name: "Keep for Name" }).getByLabel("Cloud").check();
    await conflict.getByRole("group", { name: "Keep for Price" }).getByLabel("Other").check();
    await conflict.getByLabel("New Price").fill("1.40");
    await conflict.getByRole("button", { name: "Resolve" }).click();
    await page.getByText("Cola 330ml: resolved. It is being sent to the cloud.").waitFor();
    await page.getByText("No conflicts.", { exact: false }).waitFor();
    expect(await calls(page, "sync_conflict_resolve")).toEqual([
      {
        conflictId: "c1",
        choices: [
          { field: "name", keep: "server", valueJson: null },
          { field: "variants/v1/price", keep: "custom", valueJson: "140" },
        ],
        identifiers: null,
      },
    ]);
  });

  it("a cashier is not offered conflicts", async () => {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    await page.goto(`${base}/app-harness.html?preset=signIn`);
    await page.evaluate(() => {
      window.__fake.cloud.linkState = "linked";
      window.__fake.cloud.conflicts = 1;
    });
    await page.getByRole("button", { name: /Cal/ }).click();
    await page.keyboard.type("3333");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.getByTestId("sync-status").getByText(/Attention needed/).waitFor();
    expect(await page.getByRole("button", { name: "Resolve conflicts" }).count()).toBe(0);
  });
});

describe("consolidated reports (E14-T05)", () => {
  async function reports(setup: () => void): Promise<Page> {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    page.on("pageerror", (e) => {
      throw e;
    });
    await page.goto(`${base}/app-harness.html?preset=signIn`);
    await page.evaluate(setup);
    await page.getByRole("button", { name: /Olu/ }).click();
    await page.keyboard.type("1234");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.getByRole("button", { name: "Reports" }).click();
    await page.getByRole("heading", { name: "Reports" }).waitFor();
    return page;
  }

  it("a till that is not linked offers this register only", async () => {
    const page = await reports(() => undefined);
    await page.getByTestId("scope-badge").waitFor();
    expect(await page.getByRole("group", { name: "Scope" }).count()).toBe(0);
    expect(await calls(page, "reports_cloud_sales")).toEqual([]);
  });

  it("all registers, labelled with how complete they are, by day, register and payment", async () => {
    const page = await reports(() => {
      window.__fake.cloud.linkState = "linked";
      window.__fake.cloud.pending = 0;
    });
    const scope = page.getByRole("group", { name: "Scope" });
    await scope.getByRole("button", { name: "All registers (cloud)" }).click();
    const label = page.getByTestId("cloud-label");
    // The fake's times are on 5 November, so the date shows too.
    await label.getByText(/^All registers \(cloud\) · Last synced .*09:05 — 1 register has unsynced data$/).waitFor();
    expect(await page.getByTestId("cloud-total").innerText()).toContain("19.50");
    // The device's tabs and exports are for this register only.
    expect(await page.getByRole("tablist", { name: "Report" }).count()).toBe(0);
    expect(await page.getByRole("button", { name: "Export this report (CSV)" }).count()).toBe(0);

    await page.evaluate(() => (window.__fake.cloud.unsyncedRegisters = 0));
    await page.getByRole("button", { name: "By register" }).click();
    await label.getByText("All registers (cloud) · Fully synchronized").waitFor();
    const rows = await page.getByTestId("cloud-row").allInnerTexts();
    expect(rows.map((r) => r.split("\t")[0])).toEqual(["R01", "R02"]);
    await page.getByRole("button", { name: "By payment" }).click();
    await page.getByRole("rowheader", { name: "Card" }).waitFor();
    const last = (await calls(page, "reports_cloud_sales")).at(-1);
    expect(last?.groupBy).toBe("tender");

    // Back to this register: its own report, as before.
    await scope.getByRole("button", { name: "This register" }).click();
    await page.getByRole("tablist", { name: "Report" }).waitFor();
    await page.getByTestId("scope-badge").getByText(/This device only/).waitFor();
  });

  it("offline, the last figures fetched are shown marked stale", async () => {
    const page = await reports(() => {
      window.__fake.cloud.linkState = "linked";
    });
    await page.getByRole("button", { name: "All registers (cloud)" }).click();
    await page.getByTestId("cloud-label").getByText(/Last synced .*09:05/).waitFor();
    await page.getByRole("button", { name: "By register" }).click();
    await page.getByTestId("cloud-row").first().waitFor();

    await page.evaluate(() => (window.__fake.cloud.online = false));
    await page.getByRole("button", { name: "By day" }).click();
    const stale = page.getByRole("alert").filter({ hasText: "Stale" });
    await stale.waitFor();
    expect(await stale.innerText()).toMatch(/Stale: the cloud can't be reached\. Figures from .*09:30 \(Last synced .*09:05 — 1 register has unsynced data\)/);
    expect(await page.getByTestId("cloud-row").count()).toBe(1);

    // Nothing kept for this one.
    await page.getByRole("button", { name: "By payment" }).click();
    await page.getByRole("alert").filter({ hasNotText: "Stale" }).waitFor();
    expect(await page.getByTestId("cloud-row").count()).toBe(0);
  });
});

describe("backups (E19-T03)", () => {
  async function backupsPage(setup: () => void = () => undefined): Promise<Page> {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    page.on("pageerror", (e) => {
      throw e;
    });
    await page.goto(`${base}/app-harness.html?preset=signIn`);
    await page.evaluate(setup);
    await page.getByRole("button", { name: /Olu/ }).click();
    await page.keyboard.type("1234");
    await page.keyboard.press("Enter");
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.getByRole("button", { name: "Settings" }).click();
    await page.getByRole("tab", { name: "Backups" }).click();
    await page.getByRole("heading", { name: "Backups", exact: true }).waitFor();
    return page;
  }

  it("back up now, a second folder, a damaged backup, and a restore the owner confirms", async () => {
    const page = await backupsPage();
    const rows = page.getByTestId("backup-row");
    expect(await rows.count()).toBe(2);
    await page.getByRole("button", { name: "Back up now" }).click();
    await page.getByRole("status").getByText(/Backed up:/).waitFor();
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=backup-row]").length === 3);
    expect(await rows.first().innerText()).toContain("Made by hand");

    // No second folder yet: the risk is said; then one is chosen in the system dialog.
    await page.getByText(/if this computer's disk fails, the backups go with it/).waitFor();
    await page.getByRole("button", { name: "Choose a folder…" }).click();
    await page.getByText("/media/usb/SaleOS").waitFor();

    // Checking: the damaged one says so.
    const damaged = rows.filter({ hasText: "Hourly" });
    await damaged.getByRole("button", { name: "Check" }).click();
    await damaged.getByRole("alert").getByText("Damaged").waitFor();
    const daily = rows.filter({ hasText: "Daily" });
    await daily.getByRole("button", { name: "Check" }).click();
    await daily.getByText("Whole").waitFor();

    // Restoring needs the loss confirmed.
    await daily.getByRole("button", { name: "Restore…" }).click();
    const dialog = page.getByRole("alertdialog", { name: "Restore a backup" });
    await dialog.getByText(/will be lost/).waitFor();
    const go = dialog.getByRole("button", { name: "Restore and restart" });
    expect(await go.isDisabled()).toBe(true);
    await dialog.getByRole("checkbox").check();
    await go.click();
    await page.getByRole("status").getByText(/Restored\. Sale OS is restarting/).waitFor();
    expect(await page.evaluate(() => window.__fake.restartedFrom)).toBe("pos.daily.1793955600000.db");
    expect((await calls(page, "backup_restore")).at(-1)).toMatchObject({ confirmLoss: true });
  });

  it("encrypted: the owner reads the recovery key and confirms writing it down (ADR-023)", async () => {
    const page = await backupsPage();
    const section = page.getByRole("region", { name: "Encryption" });
    await section.getByText(/data and its backups are encrypted/).waitFor();
    await section.getByRole("alert").getByText(/Write the recovery key down/).waitFor();
    await section.getByRole("button", { name: "Show recovery key" }).click();
    expect(await section.getByTestId("recovery-key").textContent()).toBe(RECOVERY_KEY);
    await section.getByRole("button", { name: "I have written it down" }).click();
    await section.getByRole("button", { name: "Show recovery key" }).waitFor();
    expect(await section.getByRole("alert").count()).toBe(0);
    expect(await calls(page, "security_recovery_key_confirmed")).toHaveLength(1);
  });

  it("a locked till asks for the recovery key; a wrong one is refused", async () => {
    const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
    page.on("pageerror", (e) => {
      throw e;
    });
    await page.goto(`${base}/app-harness.html?preset=locked`);
    await page.getByRole("heading", { name: "This till's data is locked" }).waitFor();
    const input = page.getByLabel("Recovery key");
    const unlock = page.getByRole("button", { name: "Unlock" });
    expect(await unlock.isDisabled()).toBe(true);
    await input.fill("FFFFFFFF-".repeat(7) + "FFFFFFFF");
    await unlock.click();
    await page.getByRole("alert").getByText(/does not have its key/).waitFor();
    await input.fill(RECOVERY_KEY.toLowerCase().replace(/-/g, " "));
    await unlock.click();
    await page.getByText("Unlocked. Sale OS is restarting…").waitFor();
  });

  it("the owner exports all data; cancelling the save dialog does nothing", async () => {
    const page = await backupsPage(() => {
      window.__fake.saveAs = [null];
    });
    const button = page.getByRole("button", { name: "Export all data…" });
    await button.click();
    await page.waitForFunction(() => !document.querySelector("[aria-busy]"));
    expect(await page.evaluate(() => window.__fake.exports)).toBe(0);
    await button.click();
    await page.getByText("Exported 1234 rows from 44 tables to /home/user/Documents/sale-os-export-2026-11-05.zip.").waitFor();
    expect(await page.evaluate(() => window.__fake.exports)).toBe(1);
    expect(await page.getByText("PINs are not included.").count()).toBe(1);
  });

  it("a linked till restores only online, and loses nothing", async () => {
    const page = await backupsPage(() => {
      window.__fake.cloud.linkState = "linked";
      window.__fake.cloud.online = false;
    });
    await page.getByText(/its sales since the backup come back from the cloud/).waitFor();
    await page.getByTestId("backup-row").first().getByRole("button", { name: "Restore…" }).click();
    const dialog = page.getByRole("alertdialog", { name: "Restore a backup" });
    await dialog.getByText(/come back from the cloud after the restart/).waitFor();
    expect(await dialog.getByRole("checkbox").count()).toBe(0);
    await dialog.getByRole("button", { name: "Restore and restart" }).click();
    await page.getByText(/The cloud could not be reached/).waitFor();

    await page.evaluate(() => (window.__fake.cloud.online = true));
    await page.getByTestId("backup-row").first().getByRole("button", { name: "Restore…" }).click();
    await dialog.getByRole("button", { name: "Restore and restart" }).click();
    await page.getByText("Restored. Sale OS is restarting…").waitFor();
    expect((await calls(page, "backup_restore")).at(-1)).toMatchObject({ confirmLoss: false });
  });
});
