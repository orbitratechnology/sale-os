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
| `F9` | Pay | Payment arrives in Stage 2 (E10) |
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
