/**
 * E20-T11 / E22-T02 / E21 in Chromium against the scripted backend: setting up a printer
 * (network, found by discovery), the status bar, "No sale", the drawer after a cash sale,
 * a USB HID-POS scanner (picked from the detected ones, its status), a scale (set up, tested, items sold by weight
 * weighed at the till, refusals and the typed fallback, the Weigh key), the customer display (set up, tested, total due at payment), and card payments on the sandbox terminal: approve, decline and an unknown outcome
 * settled by the operator. Rust's behaviour behind each command is tested in Rust.
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

const COLA = "4006381333931";

async function till(): Promise<Page> {
  const page = await browser.newPage({ viewport: { width: 1366, height: 768 } });
  page.on("pageerror", (e) => {
    throw e;
  });
  await page.goto(`${base}/app-harness.html?preset=checkout`);
  await page.getByRole("region", { name: "Cart lines" }).waitFor();
  return page;
}

async function scan(page: Page, code: string) {
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.type(code, { delay: 5 });
  await page.keyboard.press("Enter");
}

async function calls(page: Page, cmd: string): Promise<Record<string, unknown>[]> {
  return page.evaluate((c) => window.__ipc.filter((x) => x.cmd === c).map((x) => x.args), cmd);
}

async function devicesPage(page: Page) {
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("tab", { name: "Devices" }).click();
  await page.getByRole("heading", { name: "Receipt printer", exact: true }).waitFor();
}

describe("Settings → Devices (E20-T11)", () => {
  it("finds a network printer, uses its identified model, saves it and test-prints", async () => {
    const page = await till();
    await devicesPage(page);
    await page.getByText("No printer: receipts can be previewed and saved as PDF.").waitFor();
    // A missing address is refused at the field.
    await page.getByRole("button", { name: "Save printer" }).click();
    await page.getByRole("alert").filter({ hasText: "VALIDATION" }).or(page.locator(".field-error.inline")).first().waitFor();
    // Discovery fills address and model.
    await page.getByText("Find printers and ports").click();
    await page.getByLabel(/Also search the network/).fill("192.168.1.0/24");
    await page.getByRole("button", { name: "Search" }).click();
    await page.getByRole("list", { name: "Found devices" }).getByText("192.168.1.50:9100").waitFor();
    await page.getByRole("list", { name: "Found devices" }).getByRole("button", { name: "Use" }).first().click();
    expect(await page.getByLabel("IP address or name").inputValue()).toBe("192.168.1.50");
    expect(await page.getByLabel(/^Model/).inputValue()).toBe("TM-T88V");
    await page.getByRole("button", { name: "Save printer" }).click();
    await page.getByText("Saved. The next receipt prints here.").waitFor();
    const saved = await calls(page, "device_save");
    const config = JSON.parse(String((saved.at(-1)?.input as { configJson: string }).configJson)) as Record<string, unknown>;
    expect(config).toMatchObject({ connection: { kind: "network", host: "192.168.1.50", port: 9100 }, profileId: "TM-T88V" });
    await page.getByRole("button", { name: "Test print" }).click();
    await page.getByText(/Printed\. Check the ruler/).waitFor();
    await page.close();
  }, 60_000);

  it("offers USB-COM ports and system printer queues, and flags image-only models", async () => {
    const page = await till();
    await devicesPage(page);
    await page.getByText("Find printers and ports").click();
    await page.getByRole("button", { name: "Search" }).click();
    const found = page.getByRole("list", { name: "Found devices" });
    await found.getByText("COM5: Epson TM-T20III (likely printer)").waitFor();
    await found.getByText("Printer queue: EPSON TM-T20III Receipt").waitFor();
    await found.getByRole("listitem").filter({ hasText: "Printer queue" }).getByRole("button", { name: "Use" }).click();
    expect(await page.getByLabel("Connection").inputValue()).toBe("spooler");
    expect(await page.getByLabel("Printer", { exact: true }).inputValue()).toBe("EPSON TM-T20III Receipt");
    expect(await page.locator("option[value=TSP143IIIU]").evaluate((o) => (o as HTMLOptionElement).disabled)).toBe(true);
    await page.close();
  }, 60_000);
});

describe("scale (E20-T06)", () => {
  it("sets up a serial scale, tests it, and the till weighs items sold by weight", async () => {
    const page = await till();
    await page.evaluate(() => {
      window.__fake.products.push({ variantId: crypto.randomUUID(), productId: crypto.randomUUID(), name: "Bananas", sku: "BANANA", barcodes: {}, priceMinor: 299n, unit: "kg" });
    });
    // No scale yet: a scanned item sold by weight is added as one unit, as before.
    await scan(page, "BANANA");
    await expect.poll(() => page.evaluate(() => window.__fake.lines.map((l) => l.qtyMilli.toString()))).toEqual(["1000"]);
    await page.evaluate(() => {
      window.__fake.lines = [];
      window.__fake.revision += 1;
    });

    await devicesPage(page);
    const section = page.getByRole("region", { name: "Scale" });
    expect(await section.getByRole("button", { name: "Save scale" }).isDisabled()).toBe(true);
    await section.getByLabel("Port").fill("/dev/ttyUSB2");
    await section.getByLabel("Data format").selectOption("7E1");
    await section.getByLabel("Protocol").selectOption("toledo8217");
    await section.getByRole("button", { name: "Save scale" }).click();
    await page.getByText("Saved. Items sold by weight are weighed on this scale.").waitFor();
    const input = (await calls(page, "device_save")).at(-1)?.input as { kind: string; configJson: string };
    expect(input.kind).toBe("scale");
    expect(JSON.parse(input.configJson)).toEqual({
      serial: { port: "/dev/ttyUSB2", baud: 9600, dataBits: "seven", parity: "even" },
      protocol: "toledo8217",
      unit: "kg",
    });
    const saved = page.getByRole("region", { name: "Scale" });
    await saved.getByRole("button", { name: "Test scale" }).click();
    await page.getByText("The scale reads 1.234 kg (stable).").waitFor();
    await saved.getByTestId("device-scale-status").filter({ hasText: "/dev/ttyUSB2 @ 9600, Toledo 8217 (kg) · Ready" }).waitFor();
    // A reading still moving says so.
    await page.evaluate(() => (window.__fake.scaleReadings = [{ outcome: "moving", grams: "1230" }, { outcome: "stable", grams: "1234" }]));
    await saved.getByRole("button", { name: "Test scale" }).click();
    await page.getByText("The scale reads 1.230 kg and is still moving.").waitFor();

    // At the till: a scan of bananas is weighed under that scan.
    await page.getByRole("button", { name: /Back to till/ }).click();
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await expect.poll(async () => (await calls(page, "scale_status")).length).toBeGreaterThan(1);
    await scan(page, "BANANA");
    await expect.poll(() => page.evaluate(() => window.__fake.lines.map((l) => l.qtyMilli.toString()))).toEqual(["1234"]);
    const weighed = (await calls(page, "cart_add_weighed")).at(-1);
    const scanned = (await calls(page, "scan_submit")).at(-1);
    expect(weighed?.via).toBe("scanner");
    expect(weighed?.scanEventId).toBe(scanned?.scanEventId);
    await page.getByRole("region", { name: "Cart lines" }).getByText("1.234").waitFor();

    // Still moving: refused with a clear message; the weight is typed instead.
    await page.evaluate(() => (window.__fake.scaleReadings = [{ outcome: "moving", grams: "800" }]));
    await page.keyboard.press("F2");
    await page.keyboard.type("Banan");
    await page.getByRole("listbox", { name: "Search results" }).waitFor();
    await page.keyboard.press("Enter");
    const dialog = page.getByRole("dialog", { name: "Weigh Bananas" });
    await dialog.getByText("The weight did not settle. Hold the item still on the scale and weigh again.").waitFor();
    expect(await page.evaluate(() => window.__fake.lines.length)).toBe(1);
    // Nothing on it, then weighed again.
    await page.evaluate(() => (window.__fake.scaleReadings = [{ outcome: "stable", grams: "0" }]));
    await dialog.getByRole("button", { name: "Weigh again" }).click();
    await dialog.getByText("Nothing is on the scale. Put the item on it and weigh again.").waitFor();
    await dialog.getByRole("button", { name: "Type the weight" }).click();
    await dialog.getByLabel("Weight in kg").fill("0.5");
    await dialog.getByRole("button", { name: "Add" }).click();
    await dialog.waitFor({ state: "detached" });
    const typed = (await calls(page, "cart_add_weighed")).at(-1);
    expect(typed).toEqual({ variantId: weighed?.variantId, via: "search", scanEventId: null, typedQtyMilli: "500" });
    // A line of its own, selected.
    await expect.poll(() => page.evaluate(() => window.__fake.lines.map((l) => l.qtyMilli.toString()))).toEqual(["1234", "500"]);

    // The Weigh key on the selected line: weighed again.
    await page.evaluate(() => (window.__fake.scaleReadings = [{ outcome: "stable", grams: "750" }]));
    await page.keyboard.press("Shift+F3");
    await expect
      .poll(() => page.evaluate(() => window.__fake.lines.map((l) => l.qtyMilli.toString())))
      .toEqual(["1234", "750"]);
    expect((await calls(page, "cart_weigh_line")).length).toBe(1);
    // Items sold by the piece are scanned as before.
    await scan(page, COLA);
    await expect.poll(() => page.evaluate(() => window.__fake.lines.length)).toBe(3);
    await page.close();
  }, 60_000);
});

describe("USB HID-POS scanner", () => {
  it("picks a detected scanner, saves it beside the serial one and shows its status", async () => {
    const page = await till();
    await devicesPage(page);
    const section = page.getByRole("region", { name: "USB HID-POS scanner" });
    expect(await section.getByRole("button", { name: "Save USB scanner" }).isDisabled()).toBe(true);
    await section.getByRole("button", { name: "Find USB scanners" }).click();
    expect(await section.getByRole("combobox").inputValue()).toBe("/dev/hidraw3");
    await section.getByRole("option", { name: "Honeywell Xenon 1900 (0c2e:0b61)" }).waitFor({ state: "attached" });
    await section.getByRole("button", { name: "Save USB scanner" }).click();
    await page.getByText("Saved. Scans from Honeywell Xenon 1900 (0c2e:0b61) go to the till.").waitFor();
    const input = (await calls(page, "device_save")).at(-1)?.input as { kind: string; role: string; configJson: string };
    expect(input.kind).toBe("scanner");
    expect(input.role).toBe("hidpos");
    expect(JSON.parse(input.configJson)).toEqual({
      hidPos: { vendorId: 0x0c2e, productId: 0x0b61, serialNumber: "19200B1234", path: "/dev/hidraw3", label: "Honeywell Xenon 1900 (0c2e:0b61)" },
    });
    await section.getByTestId("device-hidpos-status").filter({ hasText: "Honeywell Xenon 1900 (0c2e:0b61) · Ready" }).waitFor();

    // A serial scanner as well: both are kept.
    const serial = page.getByRole("region", { name: "Serial scanner" });
    await serial.getByLabel("Port").fill("COM4");
    await serial.getByRole("button", { name: "Save scanner" }).click();
    await expect
      .poll(() => page.evaluate(() => window.__fake.devices.filter((d) => d.kind === "scanner").map((d) => d.role).sort()))
      .toEqual(["hidpos", "main"]);

    // Unplugged: the status says so, by name.
    await page.evaluate(() => (window.__fake.hidScanners = []));
    await page.getByRole("tab", { name: "Scanner", exact: true }).click();
    await page.getByRole("tab", { name: "Devices" }).click();
    await page
      .getByRole("region", { name: "USB HID-POS scanner" })
      .getByTestId("device-hidpos-status")
      .filter({ hasText: "Not working: Honeywell Xenon 1900 (0c2e:0b61) is not connected" })
      .waitFor();
    // Nothing attached: Find says how to switch a scanner to HID-POS.
    await page.getByRole("region", { name: "USB HID-POS scanner" }).getByRole("button", { name: "Find USB scanners" }).click();
    await page.getByText(/^No HID-POS scanner found/).waitFor();
    await page.close();
  }, 60_000);
});

describe("customer display", () => {
  it("sets up a pole display with its command set, tests it, and shows the total due at payment", async () => {
    const page = await till();
    await devicesPage(page);
    const section = page.getByRole("region", { name: "Customer display" });
    expect(await section.getByRole("button", { name: "Save display" }).isDisabled()).toBe(true);
    await section.getByLabel("Port").fill("/dev/ttyUSB1");
    await section.getByLabel("Command set").selectOption("cd5220");
    await section.getByRole("button", { name: "Save display" }).click();
    await page.getByText("Saved. The display greets customers and shows each item and the total.").waitFor();
    const saved = await calls(page, "device_save");
    const input = saved.at(-1)?.input as { kind: string; configJson: string };
    expect(input.kind).toBe("customer_display");
    expect(JSON.parse(input.configJson)).toEqual({ serial: { port: "/dev/ttyUSB1", baud: 9600 }, protocol: "cd5220" });

    await section.getByRole("button", { name: "Test display" }).click();
    await page.getByText(/^Sent\. The display should show SALE OS DISPLAY TEST/).waitFor();
    await section.getByTestId("device-display-status").filter({ hasText: "Ready" }).waitFor();
    await section.getByTestId("device-display-status").filter({ hasText: "/dev/ttyUSB1 @ 9600, CD5220" }).waitFor();

    // Payment puts the total due on the display.
    await page.getByRole("button", { name: /Back to till/ }).click();
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await scan(page, COLA);
    await page.keyboard.press("F9");
    await page.getByRole("button", { name: /^Exact/ }).waitFor();
    await expect.poll(() => page.evaluate(() => window.__fake.displayShown.at(-1))).toBe("total_due");
    expect(await page.evaluate(() => window.__fake.displayShown[0])).toBe("test");
    await page.close();
  }, 60_000);

  it("a display that is not there says so on the settings page, and payment goes on", async () => {
    const page = await till();
    await devicesPage(page);
    const section = page.getByRole("region", { name: "Customer display" });
    await section.getByLabel("Port").fill("/dev/missing0");
    await section.getByRole("button", { name: "Save display" }).click();
    await page.getByText("Saved. The display greets customers and shows each item and the total.").waitFor();
    await section.getByRole("button", { name: "Test display" }).click();
    await page.getByText(/^The display did not take it: .*not found/).waitFor();
    await section.getByTestId("device-display-status").filter({ hasText: /Not working: .*not found/ }).waitFor();

    await page.getByRole("button", { name: /Back to till/ }).click();
    await scan(page, COLA);
    await page.keyboard.press("F9");
    await page.getByRole("button", { name: /^Exact/ }).click();
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    await page.close();
  }, 60_000);
});

describe("drawer and status (E20-T05, DEV-006)", () => {
  it("shows the printer in the status bar, opens the drawer after cash sales and on No sale", async () => {
    const page = await till();
    await page.evaluate(() => {
      window.__fake.devices.push({
        deviceId: "0190a5b0-0000-7000-8000-00000000d001", kind: "receipt_printer", role: "main", name: "Front",
        configJson: JSON.stringify({ connection: { kind: "network", host: "192.168.1.50", port: 9100 }, profileId: "TM-T88V" }), enabled: true, version: 1,
      });
    });
    // Leave and come back: the status bar asks again when it is shown.
    await page.getByRole("button", { name: "Settings" }).click();
    await page.getByRole("button", { name: /Back to till/ }).click();
    await page.getByRole("region", { name: "Cart lines" }).waitFor();
    await page.getByTestId("printer-status").filter({ hasText: "Printer ready" }).waitFor();

    await scan(page, COLA);
    await page.keyboard.press("F9");
    await page.getByRole("button", { name: /^Exact/ }).click();
    await page.getByText("Cash drawer opened.").waitFor();
    await page.getByRole("button", { name: /New sale/ }).click();

    await page.getByRole("button", { name: "No sale" }).click();
    await page.getByRole("textbox").last().fill("change for the float");
    await page.getByRole("button", { name: "Continue" }).click();
    await page.getByText("Cash drawer opened.").waitFor();
    expect(await page.evaluate(() => window.__fake.drawerOpens)).toEqual(["cash_sale", "no_sale:change for the float"]);
    await page.close();
  }, 60_000);
});

describe("card terminal (E22-T02)", () => {
  async function withTerminal() {
    const page = await till();
    await page.evaluate(() => {
      window.__fake.devices.push({
        deviceId: "0190a5b0-0000-7000-8000-00000000d002", kind: "payment_terminal", role: "main", name: "Sandbox",
        configJson: JSON.stringify({ provider: "sandbox" }), enabled: true, version: 1,
      });
    });
    await scan(page, COLA);
    await page.keyboard.press("F9");
    await page.getByRole("button", { name: "Card (terminal)" }).click();
    return page;
  }

  it("an approved terminal payment completes the sale as provider-approved", async () => {
    const page = await withTerminal();
    await page.getByRole("button", { name: "Send to terminal" }).click();
    await page.getByText("Approved by terminal").waitFor();
    await page.getByRole("button", { name: "Complete sale" }).click();
    await page.getByRole("dialog", { name: "Sale completed" }).waitFor();
    const done = await calls(page, "checkout_complete");
    expect(done.at(-1)?.tenders).toEqual([]);
    expect((done.at(-1)?.terminal as unknown[]).length).toBe(1);
    await page.close();
  }, 60_000);

  it("a decline is shown and nothing is added", async () => {
    const page = await withTerminal();
    await page.getByLabel(/Amount to charge/).fill("0.01");
    await page.getByRole("button", { name: "Send to terminal" }).click();
    await page.getByText(/Declined: Declined by the sandbox terminal/).waitFor();
    expect(await page.getByText("Approved by terminal").count()).toBe(0);
    await page.close();
  }, 60_000);

  it("an unknown outcome is never treated as declined: the operator settles it", async () => {
    const page = await withTerminal();
    await page.getByLabel(/Amount to charge/).fill("0.02");
    await page.getByRole("button", { name: "Send to terminal" }).click();
    await page.getByText("Outcome unknown.").waitFor();
    await page.getByRole("button", { name: "It was approved" }).click();
    await page.getByLabel("Reason (kept in the audit log)").fill("slip shows approved");
    await page.getByRole("button", { name: "Record as approved" }).click();
    await page.getByText("Approved by terminal").waitFor();
    const resolved = await calls(page, "payment_resolve");
    expect(resolved.at(-1)).toMatchObject({ resolution: { outcome: "approved" }, reason: "slip shows approved" });
    await page.close();
  }, 60_000);
});

describe("scan hints (E21-T03)", () => {
  it("explains a scanner on the wrong keyboard layout instead of a bare not-found", async () => {
    const page = await till();
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    // What a US-configured scanner types for digits on a French AZERTY layout.
    await page.locator("[data-scan-field]").first().fill("àéçù");
    await page.locator("[data-scan-field]").first().press("Enter");
    await page.getByText(/types as if the keyboard were US while Windows uses French AZERTY/).waitFor();
    await page.close();
  }, 60_000);
});
