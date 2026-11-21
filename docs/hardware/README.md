# Hardware set-up and support (PRD §12.7, ADR-024)

What the till works with, how to connect it, and what to do when something doesn't work.
`registry.yaml` records what has actually been tested; until real devices are tested (D-07)
its entries are emulator results only.

## Receipt printers

Settings → Devices → Receipt printer. Choose how the printer is connected, then its model.
Use **Test print**: the ruler must fit on one line, and the accents must print correctly.

| Connection | Use it for | Notes |
|---|---|---|
| Network (Ethernet / Wi-Fi) | Any printer with a network port: Epson UB-E/Wi-Fi, Star LAN, Bixolon, Citizen, Xprinter… | Raw TCP, port 9100. "Find printers" searches your /24 network and asks each printer its model. |
| Installed printer (system queue) | USB printers with a Windows driver (also "Generic / Text Only"), Bluetooth-paired printers, anything with a driver | The till sends the receipt raw through the queue. This direction is one-way, so the till can't read status. |
| Serial / COM port | RS-232 printers, USB printers that appear as COM ports, Bluetooth SPP | Set the baud rate and flow control to match the printer (often 9600 or 38400). |
| Device file | Linux USB (`/dev/usb/lp0`), parallel (`/dev/lp0`, `LPT1`) | |

**Models.** About 70 profiles: Epson TM-T20/T70/T82/T88/m30/U220/P80, Star TSP600/650/700/800/SP700/mC-Print, Bixolon,
Citizen, Seiko, Sunmi, Rongta, Munbyn, Xprinter, Zjiang/POS-5890, HPRT, Wincor Nixdorf and others. There are also generic
ESC/POS profiles (58 mm/32, 80 mm/42 and 48, 76 mm impact/40), Star Line Mode and plain text. Models marked
"not yet verified" were added by hand from datasheets.

**If unsure, choose the generic ESC/POS profile for your paper width.** If long lines wrap, set *Characters per line*
to what the test print shows. Many 80 mm printers have 42 columns, not 48.

**Star printers.** Choose the Star *Line Mode* profile. If the printer has been switched to ESC/POS emulation (DIP
switch or utility), choose its ESC/POS profile instead. Star TSP100/TSP143 (futurePRNT, up to gen III) print images
only and are not supported for text receipts.

**Languages.** The till picks one of the printer's code pages for each line: Western European, Central European,
Cyrillic, Greek, Turkish, Baltic, Hebrew, Arabic (CP864), Vietnamese (CP1258) and Japanese half-width katakana. On an
ESC/POS printer, a line with a character none of the printer's code pages has (Sinhala, Tamil, Arabic, Devanagari,
Chinese, Japanese, Korean, or an accented letter the printer has no page for) prints as an image drawn with the till's
own fonts: letters joined, Arabic right to left, in the same columns as the text lines, so amounts stay aligned. Arabic
always prints as an image, because a code page prints its letters unjoined and in typed order. Lines the code pages can
print stay text. Star Line Mode and plain-text printers transliterate what they can't print (`€`→`E`, `ß`→`s`,
Chinese→pinyin), never `?`; so does every printer for Hebrew, which has no bundled font yet.

**Printer status.** Network, serial and two-way USB printers that support Epson's *automatic status back* report
"out of paper", "cover open" and errors the moment they happen, and the status bar shows it at once. Other printers
are asked every 15 seconds. Printers that answer no status questions at all show "Printer set up" and still print.

**What happens when it fails.**

| You see | Meaning | What to do |
|---|---|---|
| "Out of paper" / "The cover is open" | The printer said so before anything was sent | Fix it and press Retry. It prints as the original. |
| "Printer offline" / not connected | Unreachable: off, unplugged, wrong address or port | Check power, cable and IP. Retry is safe. |
| "Did it print?" (unknown) | The receipt may be partly printed | Look at the printer. Answer *Printed* or *Not printed* (*Not printed* prints a REPRINT copy). The till never prints twice by itself. |

## Cash drawer

Plug the drawer into the printer's kick port (RJ11/RJ12). It opens after cash sales; this can be turned off in
Settings → Cash drawer. **No sale** opens it on demand, with a reason; a cashier needs a manager's PIN. Both are
recorded in the audit log. If it doesn't open, try *Kick port pin 5*.

## Barcode scanners

**USB scanners in keyboard mode work as soon as they are plugged in.** For best results set the scanner to:

1. Send **Enter** after each code (Settings → Scanner must match).
2. Use the same keyboard country as Windows. If not, the till still sells but shows a hint.
3. Optionally, transmit UPC-E expanded and turn off add-on codes for non-book shops. The till handles both settings
   anyway.

**Supported symbologies.** EAN-13/8, UPC-A/E (with 2/5-digit add-ons), ISBN-10/ISSN, ITF/ITF-14, Code 39 (with or
without start/stop and check character), Codabar (with or without start/stop), Code 93, Code 11, MSI, Code 128,
GS1-128, GS1 DataBar, GS1 DataMatrix and QR (FNC1 kept), GS1 Digital Link, QR/DataMatrix/PDF417/Aztec text, and AIM
symbology-identifier prefixes.

**Older and slower scanners** (40–120 ms per character, e.g. older cordless or Bluetooth models) are recognised by
their steady rhythm. If scans still go missing, raise *Fastest typing that counts as a scan* (Settings → Scanner).

**Scale labels** (price or weight in the barcode). Add a rule per label type in Settings → Scanner → *Scale and price
labels*, e.g. `21 PPIIIIIVVVVVC price 2` or `22 PPIIIIIVVVVVC weight 3`. Register the item with its PLU barcode (the
value digits as zeros) or its item code. The till also reads the weight and price in GS1 DataBar labels.

**Serial scanners** (RS-232, USB-COM, Bluetooth SPP). Set them up in Settings → Devices. They reconnect by themselves
after being unplugged.

**Not supported yet:** USB HID-POS scanners. Most can be switched to keyboard or COM mode with a set-up barcode.

## Customer displays

Settings → Devices → Customer display. For the common 2-line, 20-character pole displays (VFD) on a COM port: RS-232
or USB-serial (they appear as COM ports). Enter the port, the speed (usually 9600) and the command set:

| Command set | Use it for |
|---|---|
| Epson ESC/POS | Epson DM-D110/D210 and displays sold as "ESC/POS compatible" |
| CD5220 | Many unbranded pole displays; often chosen with a DIP switch on the display |

Use **Test display**: the first line must read `SALE OS DISPLAY TEST` and the second a ruler `1234…890` filling the
whole line. If you see odd characters or nothing, try the other command set, then the speed.

The display shows the item just scanned with the running total, the total due at payment, the change after the sale,
and "WELCOME" with the shop's name when the till is idle. It shows plain Latin letters only: accents are dropped
(`Café` → `Cafe`) and Sinhala, Tamil and other scripts are written by sound (`කිරි` → `kiri`). Long names are shortened.
A display that is unplugged or broken never stops a sale; Settings → Devices shows what is wrong.

## Card terminals

- **Standalone terminal (any brand):** take the payment on the terminal, then record it as *Card (separate terminal)*
  with the approval code.
- **Integrated terminal:** connects through its provider once there is an agreement and test credentials (Adyen,
  Stripe, SumUp, Square, ZVT, OPI, PAX, Verifone, Ingenico; task E22-T03). The till's side is built: the amount goes to
  the terminal, only the terminal's approval becomes a tender, and a timeout is settled by a manager, never assumed to
  be declined.
- **Practice:** the sandbox terminal (sandbox mode only). Amounts ending in .01 decline, .02 time out and .03 take a
  few seconds.
