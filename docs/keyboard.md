# Checkout keyboard map (E09-T05)

Every checkout action works from the keyboard. The screen is never left to do any of them.

| Key | Action | Notes |
|---|---|---|
| *scan* | Add the item | Any time focus is not in a text field ("Scanner ready" in the status bar), or in the code field |
| `F2` | Search products | Type; `↑`/`↓` choose; `Enter` adds; `Esc` closes |
| `F3` | Change the selected line's quantity | Typed inline; `Enter` saves, `Esc` cancels. Zero is refused: use `Del` |
| `F4` | Discount on the selected line (%) | Empty removes it |
| `Del` | Remove the selected line | |
| `↑` / `↓` | Select the previous or next line | |
| `Numpad +` / `Numpad −` | One more or one fewer of the selected line | Whole-unit items only, never below one |
| `F6` | Products | The product list and editor; `Esc` comes back to the till |
| `F7` | Returns | Find a sale by its receipt number (or scan the receipt), choose what comes back, refund; `Esc` steps back |
| `F8` | Sales | Sales history: filter by date, receipt number, cashier and payment; open a sale for its detail, receipt, reprint and "Return items"; `Esc` steps back |
| `F9` | Pay | The payment panel: cash (empty + `Enter` pays exact), card and other external tenders, splits |
| `F10` | Cash drawer | Open with a float, see the session, close with a count (blind for cashiers); managers see expected cash, variance and the Z summary; `Esc` comes back |
| `F11` | Stock | Managers and owners: receive goods, opening stock, adjustments with a reason, write-offs; scan to add lines (a second scan adds to the line); `Esc` comes back |
| `F12` | Lock | Signs out; the cart is kept for the next sign-in |
| `Esc` | Back | Closes an editor, search or message, and returns to scanner-ready |

## Why `+`/`−` are on the numpad

A keyboard-wedge scanner types like a keyboard. In scanner-ready mode the app treats every
printable key as the possible start of a scan (E07-T01), and Code 39 barcodes can contain
`-` and `+`. Wedge scanners send main-row keys, never numpad keys, so the numpad `+`/`−`
cannot be confused with scanner input. The scan capture hook lets those two keys through.

## Sign-in

The PIN pad takes digits from the keyboard, `Backspace` deletes the last one, and `Enter`
signs in once four or more digits are entered.

## Products and the editor

`Esc` goes back (editor → list → till). In the editor, a scan while no text field has focus
adds the barcode to the selected variant; a scan into a variant's barcode field adds it to that
variant.
