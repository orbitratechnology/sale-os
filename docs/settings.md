# Settings

<!-- GENERATED from crates/pos-application/src/settings.rs (REGISTRY). Do not edit:
     run `UPDATE_GOLDEN=1 cargo test -p pos-application settings_doc` after changing the registry. -->

Every business setting, its type and default. Owners change them in Settings (E04-T03);
each change is one transaction with an audit row (`settings.updated`) and a `setting.updated`
outbox event, and is refused with `CONFLICT_VERSION` if someone changed it meanwhile.

## Inventory

| Key | Type | Default | What it does |
|---|---|---|---|
| `inventory.allow_negative` | true / false | `true` | **Allow selling below zero stock.** On: sell anyway and flag the item for a stock check. Off: refuse the sale and list the items. |

## Cash drawer

| Key | Type | Default | What it does |
|---|---|---|---|
| `cash.require_session_for_sale` | true / false | `true` | **Sales need an open drawer.** In live mode, a sale or a cash refund needs an open cash drawer session. Practice sales never do. |

## Security

| Key | Type | Default | What it does |
|---|---|---|---|
| `discount.cashier_max_ppm` | rate (ppm) | `100000` | **Largest discount a cashier may give.** Above this, a cashier needs a manager's approval and a reason. |
| `returns.cashier_requires_approval` | true / false | `true` | **Cashier returns need a manager.** On: a cashier's return asks for a manager's PIN. Off: cashiers make returns on their own. |
| `payments.cashier_external_tender` | true / false | `true` | **Cashiers may record card and other external payments.** Off: only managers and owners can record a card, transfer or QR payment; cashiers take cash. |
| `security.lock_timeout_secs` | whole number 0–3600 | `300` | **Lock the till after this many idle seconds.** 0 never locks. The cart is kept for the next sign-in. |

## Receipts

| Key | Type | Default | What it does |
|---|---|---|---|
| `receipt.paper_width_mm` | one of 58, 80 | `80` | **Paper width (mm).** 58 mm prints 32 characters a line, 80 mm prints 48. |
| `receipt.auto_print` | true / false | `false` | **Print the receipt after every sale.** Off: the cashier prints when asked. |
| `receipt.header_lines` | up to 5 lines × 48 chars | `[]` | **Extra lines at the top of receipts.** Up to 5 lines, e.g. opening hours. |
| `receipt.footer_lines` | up to 5 lines × 48 chars | `[]` | **Lines at the bottom of receipts.** Up to 5 lines, e.g. the returns policy. |

## Scanner

| Key | Type | Default | What it does |
|---|---|---|---|
| `scanner.suffix` | one of "Enter", "Tab", "none" | `"Enter"` | **Key the scanner sends after a code.** Match the scanner's setting. "none" ends a code by the pause after it. |
| `scanner.max_key_gap_ms` | whole number 10–100 | `35` | **Fastest typing that counts as a scan (ms between keys).** Raise it for slow wireless scanners; people type far slower than any scanner. |
