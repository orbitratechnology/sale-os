# Error codes

<!-- GENERATED from `pos_domain::ErrorCode::doc()`. Regenerate with
`UPDATE_ERRORS_DOC=1 cargo test -p pos-domain errors_doc`. Do not edit by hand. -->

Every error that reaches the UI or the REST API carries exactly one of these codes
(docs/backlog/07-contracts.md §2). `message` strings are for logs; the UI maps the code
to the action below.

| Code | Retryable | Meaning | UI action |
|---|:-:|---|---|
| `SESSION_LOCKED` |  | No unlocked staff session. | Show the lock screen. |
| `AUTH_PIN_INVALID` |  | Wrong PIN. `details.remainingAttempts` says how many tries are left. | Let the user retry. |
| `AUTH_LOCKED_OUT` |  | Too many wrong PINs. `details.retryAt` says when to try again. | Show a countdown. |
| `PERMISSION_DENIED` |  | The signed-in role lacks the permission. | Explain, and offer a manager sign-in. |
| `APPROVAL_REQUIRED` |  | A manager approval is needed (`details.permission`, `details.scope`). | Open the approval dialog, then retry with the token. |
| `APPROVAL_INVALID` |  | The approval token expired, was already used, or is out of scope. | Ask for approval again. |
| `VALIDATION_FAILED` |  | Input failed validation. `details.fields` lists the field errors. | Show inline field errors. |
| `CATALOG_DUPLICATE_BARCODE` |  | The barcode belongs to another active product (`details.owner`). | Link to the owning product. |
| `CATALOG_DUPLICATE_SKU` |  | The SKU belongs to another active product. | Show the conflicting product. |
| `CATALOG_NOT_FOUND` |  | The product or variant does not exist. | Refresh the list. |
| `SCAN_NOT_FOUND` |  | No product matches the scanned or typed code. | Offer search, or create a product if permitted. |
| `SCAN_INVALID` |  | The scanned input is malformed (bad check digit, partial read). | Ask for a re-scan. |
| `CART_REVISION_MISMATCH` |  | The UI acted on an out-of-date cart. | Reload the cart and show it again. |
| `CART_EMPTY` |  | The cart has no lines. | Nothing to complete; keep the cart open. |
| `PRICING_DISCOUNT_EXCEEDS` |  | The discount is larger than the line or order. | Show the maximum allowed. |
| `TENDER_INSUFFICIENT` |  | Tenders do not cover the total (`details.remaining`). | Show the remaining amount due. |
| `TENDER_CHANGE_FROM_NON_CASH` |  | Change can only be given from cash. | Reduce the non-cash amount. |
| `TENDER_OVERPAY_NON_CASH` |  | A non-cash tender exceeds the amount due. | Reduce the non-cash amount. |
| `STOCK_NEGATIVE_BLOCKED` |  | The negative-stock policy blocks this sale (`details.variants`). | Show the affected items; the cart is kept. |
| `RETURN_QTY_EXCEEDS` |  | More is being returned than remains unreturned (`details.available`). | Show the available quantity. |
| `RETURN_REFUND_MISMATCH` |  | Refund tenders do not add up to the return total. | Show the expected refund. |
| `CASH_SESSION_REQUIRED` |  | A sale needs an open cash session. | Offer to open a session. |
| `CASH_SESSION_ALREADY_OPEN` |  | This register already has an open cash session. | Show the open session. |
| `PRINT_ADAPTER_UNAVAILABLE` |  | No printer adapter is available. | Offer PDF export instead. |
| `PRINT_UNKNOWN_OUTCOME` |  | The printer did not confirm the job. | Ask whether it printed; reprint only on request. |
| `STORE_BUSY` | yes | The local database is briefly busy. Retryable. | Retry automatically up to 3 times, then show. |
| `STORE_IO` |  | The local database could not be read or written. Nothing was saved. | Show the error; offer diagnostics. |
| `STORE_CORRUPT` |  | The local database failed an integrity check. | Stop and guide the owner to restore a backup. |
| `STORE_SCHEMA_TOO_NEW` |  | The database was written by a newer app version. It was not modified. | Ask the user to update the app. |
| `SYNC_NOT_LINKED` |  | This register is not linked to a cloud account. | Open the link wizard. |
| `SYNC_DEVICE_REVOKED` |  | The owner disconnected this register from the cloud. | Explain; local sales continue. |
| `SYNC_UPGRADE_REQUIRED` |  | The cloud requires a newer app (`details.minVersion`). | Prompt to update; local sales continue. |
| `SYNC_OUT_OF_ORDER` | yes | The server expected a different event sequence (REST only). | Resend automatically. |
| `AUTH_CREDENTIALS_INVALID` |  | Cloud sign-in failed: the email or the password is wrong (never says which). | Let the owner retry or reset the password. |
| `AUTH_EMAIL_TAKEN` |  | A cloud account with this email already exists. | Offer to sign in instead. |
| `AUTH_RATE_LIMITED` |  | Too many cloud sign-in attempts. The `Retry-After` header says when to try again. | Show when to try again. |
| `AUTH_REQUIRED` |  | The cloud request has no valid token (missing, expired or wrong kind). | Sign in to the cloud again. |
| `CONFLICT_VERSION` |  | Someone else changed this record first. | Open the conflict view. |
| `INTERNAL` |  | A bug. The correlation ID matches a log line. | Offer the diagnostics bundle. |
