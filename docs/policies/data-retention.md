# Data retention and deletion (E19-T10)

> **Status: draft for legal review.** It describes what Sale OS does today. The launch
> country is Sri Lanka (D-02, 2026-11-20). Tax records must be kept there for **five years
> from the end of the taxable period** they belong to (VAT Act No. 14 of 2002; Gazette
> 2463/05, 6.3). That covers sales, returns, tax invoices and the records behind them. A
> lawyer or the shop's accountant should confirm this before production.

## On the till (the shop's own computer)

| Data | Kept | Removed by |
|---|---|---|
| Sales, returns, refunds, cash sessions, stock movements, audit log | For as long as the shop keeps the till's data: these are the shop's business records, often required by tax law | The owner (uninstall, or a new database); never by Sale OS on its own |
| Products, prices, staff, settings | While in use; archived products stay for history | The owner |
| Carts | Open and parked carts until finished or discarded; finished carts stay as audit history | — |
| Logs (`<app data>\logs`) | **7 days** (daily files; older ones deleted automatically) | Sale OS (enforced by the log writer) |
| Backups (`<app data>\backups` and the second folder) | Hourly 24, daily 14, at shutdown 5, before an update 10, made by hand 20, before a restore 5 | Sale OS, per kind, as new ones are made |
| Diagnostics bundles and data exports | Where the user saved them | The user |

Everything on the till is encrypted at rest (ADR-023). Staff PINs are stored only as Argon2id
hashes. No card data is ever stored (PCI scope statement, `docs/security/threat-model.md`).

## In the cloud

| Data | Kept | Removed by |
|---|---|---|
| A business's synced records (sales, returns, stock, master data, audit entries, the event inbox and change feed) | While the business is active | Deletion of the business (below) |
| Device registrations and refresh tokens (stored as SHA-256 hashes) | While the business is active; revoked ones stay as history | Deletion of the business |
| Owner accounts (email, Argon2id password hash) | Until the account is deleted | Account deletion: not built yet (see Gaps) |
| Database backups and point-in-time recovery | Set by the hosting provider (D-06, not chosen yet) | The provider's retention window |

### Deleting a cloud business

1. The owner first exports what they need (Settings → Backups → Export all data on a till;
   the cloud keeps nothing that the tills did not send).
2. The owner asks for the deletion and types the business's name to confirm
   (`POST /v1/businesses/{id}/deletion`). From that moment:
   - every till of the business is revoked and stops syncing (each keeps its own data and
     goes on selling locally);
   - the business no longer appears in the owner's reports or listings.
3. **Grace period: 30 days** (`POS_DELETION_GRACE_DAYS`). The owner can cancel during it
   (`DELETE /v1/businesses/{id}/deletion`). The tills must then be linked again.
4. After the grace period an hourly job removes **every row of the business from every
   table** that holds a `business_id`. The job runs as `pos_retention`, a role that can see
   and delete only the one business it is purging. Other businesses are untouched
   (`apps/cloud-api/tests/retention.rs`).
5. Copies in the hosting provider's backups expire with that provider's retention window.

### What the five years mean for a shop

- **On the till:** Sale OS never deletes sales, returns, tax invoices, cash sessions or the
  audit log on its own, so the till keeps them for as long as the shop keeps the till's data.
  Before replacing or wiping a till, the owner exports all data (Settings > Backups > Export
  all data) and keeps the file for five years after the last taxable period in it.
- **In the cloud:** deleting a business removes its cloud copy after the grace period. The
  owner exports first (step 1 above), and that export, not the cloud, is what they keep for
  the five years.
- **Tax invoices** are kept on the till only for now (not yet synced to the cloud). Backups
  and the full export carry them.

## Gaps before production

- Legal review of the five-year rule above, by the shop's accountant or a lawyer.
- Deleting an owner account (after its businesses), with the same export → request → grace →
  purge steps.
- An owner-facing button for the business deletion request. Today the request is made
  through the cloud API (by support on the owner's written request).
- The hosting provider's backup retention, once D-06 is decided.
