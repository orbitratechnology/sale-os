# Pilot runbook (E19-T11)

For whoever installs and supports Sale OS in a pilot shop. Restoring backups has its own runbook:
[`restore-drill.md`](restore-drill.md). What data is kept, and for how long:
[`../policies/data-retention.md`](../policies/data-retention.md).

`<app data>` is `%APPDATA%\dev.saleos.pos` on Windows: the database (`pos.db`), `backups\`,
`logs\` and `backup-config.json`.

## 1. Install

1. **The PC.**
   - Windows 10/11, 64-bit, with the local disk for `<app data>`. A network drive is refused.
   - The Windows account that runs the till **has a password**: it protects the database key
     (ADR-023).
   - BitLocker or device encryption is recommended on top.
2. **Install** the signed installer.
   - The pilot build comes from the GitHub Actions workflow "Release build (Windows)": the
     artefact `sale-os-windows-release-<commit>` holds the NSIS installer (`.exe`, per user,
     recommended), the `.msi`, `SHA256SUMS.txt` (compare it with `Get-FileHash` before
     installing), and in `pilot-tools/` the two performance checks of §11: `pos-bench.exe`
     (the database) and `ui-latency.zip` (the screen).
   - It includes WebView2 (ADR-021), so no internet is needed.
   - Installers are signed once E19-T06 is done. Until then, Windows SmartScreen warns about
     an unknown publisher: check the file came from us, then choose "Run anyway".
3. **Hardware.**
   - Plug in the receipt printer and the barcode scanner (keyboard mode, Enter after each code).
   - In Settings → Scanner, check the suffix matches the scanner's.
4. **Start Sale OS.** It creates the encrypted database and stores its key in the Windows
   Credential Manager. If Settings → Backups says the data is not encrypted, the keychain
   could not be used: find out why before going live.

## 2. First run

1. The setup wizard asks for:
   - the business (name, country, time zone, currency, whether prices include tax);
   - the first location and till;
   - the owner's name and PIN;
   - the tax rates.

   It works offline and takes a few minutes. The till starts in **SANDBOX** mode (practice
   sales, excluded from reports), and **Getting started** lists what is left, each with a
   button (also on the SANDBOX banner until Live).
2. **Products.** Products (**F6**): add them one by one, or import a CSV file there.
3. **Staff.** Settings → Staff: add cashiers and managers with their own PINs. Nobody shares
   the owner's PIN.
4. **Recovery key.**
   - The owner opens Settings → Backups → Show recovery key, writes it on paper, keeps it away
     from the till, and presses "I have written it down".
   - **Without it, backups cannot be opened on another computer.**
5. **Logo.** Settings → Business → Choose a logo… (PNG or JPEG). It is printed on receipts
   saved as PDF.
6. **Second backup folder.** Settings → Backups → Choose a folder… (a USB stick, ideally
   BitLocker To Go).
7. **Practise** a few sales, a return, and a drawer open and close. Then **Go live** (the
   SANDBOX banner → Go live…). Sandbox sales stay excluded from reports.

## 3. Linking to the cloud

Only when the shop uses the cloud (several tills, or reports away from the shop).

- **The first till** (Settings → Cloud → Link this till → Create the cloud business from this
  till):
  - the owner signs in with the cloud account;
  - this till's business, products and staff become the cloud business, and nothing on the
    till changes;
  - then its sales are sent (the status bar shows "Sync pending (n)", then "Synced").
- **Another till** (Link this till → Join a business): it downloads the business, then numbers
  its own receipts with its own till code.
- **"Linking was interrupted":** sign in again; it resumes where it stopped.
- **Linking needs the internet.** Afterwards the till sells offline as before and sends when it
  can.

## 4. The daily close

1. **F10** (Cash drawer) → **Count the drawer** → **Close the drawer**.
   - Cashiers count blind.
   - A manager sees the expected cash, the variance and the Z summary.
2. If the variance is not zero, count again before closing. The Z summary is kept: "Last Z
   summary" shows it again.
3. Check the status bar.
   - "Synced" is good.
   - "Offline (n pending)" is fine overnight: it sends when the internet is back.
   - "Attention needed": see §7.
4. Backups happen on their own: daily, hourly when something changed, and when the app closes.
   Once a week, check Settings → Backups shows recent ones in both places.

## 5. A replaced or broken PC

**Linked till.**
1. Install Sale OS on the new PC (§1).
2. Settings → Cloud → Link this till → Join a business. It downloads the business.
3. Ask support (level 2) to revoke the old device in the cloud
   (`POST /v1/devices/{id}/revoke`), so the old PC can never sync again. The tills have no
   button for this yet.

Sales the old PC had sent are in the cloud. Sales it had not sent are lost unless its disk can
be read: then restore its last backup with the old till's recovery key (restore-drill.md, "If
the app does not start").

**Unlinked till.**
1. Install Sale OS on the new PC.
2. Copy the newest backup from the second folder to `<app data>\pos.db`.
3. Start Sale OS. It asks for the old till's **recovery key**, which the owner wrote down.

## 6. A revoked device

The status bar says "Device revoked · saved on this computer".
- **What happened:** the owner disconnected this till in the cloud (or a business deletion
  revoked it).
- **What still works:** it keeps selling and keeps everything locally, but sends nothing.
- **Fix:**
  1. Find out why it was revoked.
  2. If it should sync again, link it again (Settings → Cloud → Link this till → Join a
     business). The owner signs in.
  3. Anything it sold while revoked is sent after linking.

## 7. "Sync attention needed"

The status bar says "Attention needed (n conflicts, m refused)".

1. **Conflicts.** The same thing was changed here and in the cloud at the same time.
   - A manager opens "Resolve conflicts" (status bar) or Settings → Cloud → Conflicts.
   - For each field they choose what to keep. Nothing is overwritten until they do.
2. **Refused by the cloud** (Settings → Cloud).
   - An event the cloud would not accept, for example a duplicate barcode or SKU. "View what
     was sent" shows it.
   - Usually fixed by changing the product on the till; the change is sent as a new event.
   - If it is not clear, save a diagnostics file (§9) and escalate (§10).
3. **"Update required · saved on this computer".**
   - The cloud needs a newer Sale OS (E19-T09). Selling goes on.
   - Install the update. Everything queued is then sent; nothing is lost.
4. **"This computer's clock is more than 10 minutes off":** correct the Windows date and time.
5. **"Offline" for more than a day:** check the shop's internet, then save diagnostics.

## 8. A locked database

The till opens on **"This till's data is locked"**. The data is encrypted, and this Windows
account does not have the key: a new account, a reinstalled Windows, or a backup brought from
another till.
1. Type the **recovery key** the owner wrote down. It is checked, kept, and the app restarts.
2. No recovery key: do not delete anything. Escalate (§10).

## 9. Diagnostics

Anyone signed in clicks **Save diagnostics…** in the status bar and saves the ZIP. Send it to
support with the time and what happened.

It holds:
- the till's state: versions, schema, the integrity check, stock drift, sync counts, print
  jobs, settings;
- the last 100 audit actions, as identifiers only;
- the last 7 days of logs, already redacted.

It holds no business records, PINs, tokens or card numbers.

## 10. Support and escalation

See [`support-process.md`](support-process.md).

## 11. Performance check (before going live)

Run both checks on the pilot PC, with the shop's own catalogue, while the till is still in
**SANDBOX** mode. Keep the files they write: they go into `docs/perf/` with the date.

**1. The database:** `pilot-tools\pos-bench.exe --scale full` (E19-T01). It writes its own
report.

**2. The screen.** This measures what a cashier feels: from a key press (a scan, F9, an arrow,
Esc) to the result drawn on screen, plus how long Sale OS takes to start and how much memory
it uses.

1. Close Sale OS (it must not be running).
2. Unzip `pilot-tools\ui-latency.zip` into a folder, e.g. `C:\saleos-perf`. It contains its own
   `node.exe`; nothing is installed.
3. Find the program file: right-click Sale OS in the Start menu → **Open file location** →
   right-click the shortcut → **Properties** → copy **Target**.
4. Pick three barcodes of products in the catalogue, and the first two or three letters of a
   product name.
5. In PowerShell, in that folder:

   ```powershell
   .\node.exe ui-latency.mjs --launch "<Target>" --codes 5000159407236,4006381333931,7622210449283 --search co --json ui-latency-cold.json
   ```

6. Sign in when it says so. The till must be in SANDBOX mode with an **empty cart**, or the
   tool stops without touching anything. It then scans the barcodes, moves along the cart,
   opens and closes the payment window, opens Products and Sales and comes back, searches,
   clears its cart and **closes Sale OS**. It takes about two minutes.
7. Do it twice: once just after restarting the PC (**cold** start, `ui-latency-cold.json`), once
   straight after (**warm** start, `ui-latency-warm.json`).
8. Start Sale OS again from the Start menu.

What it prints, and what to expect (PRD §14 and the measurements of 2026-11-20):

| Line | Expect | If not |
|---|---|---|
| Scan → line and total painted | p95 under 100 ms (`NFR-add-to-cart`) | Escalate with both files |
| Arrow, F9, Esc, typing | p95 under 100 ms | Escalate with both files |
| Products / Sales screen and back | p95 under 150 ms | Note it; escalate if over 300 ms |
| Cold start (launch → first screen) | Record it | Escalate if over 5 s |
| Memory (working set, all processes) | Record it | Escalate if the PC has under 1 GB free while selling |

**Safety.** For that run only, the tool starts Sale OS with the WebView2 debugging port open
(on this PC only, 127.0.0.1), which lets a program on the PC drive the till; nothing on the PC
is changed. The tool closes Sale OS at the end so the port closes. If the run is interrupted
(Ctrl+C, an error), close Sale OS yourself and start it again from the Start menu. Never set
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` in Windows' own environment settings.

If the tool says there is no DevTools endpoint, the WebView2 runtime ignored the argument:
note it and send the output; the database check still counts.
