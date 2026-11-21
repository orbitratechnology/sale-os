# ADR-023: Local database encryption

- **Status:** Accepted (2026-11-19). It replaces the first proposal of 2026-11-18 (full-disk
  encryption only), at the owner's request.
- **Source:** Task E19-T05; PRD §13 ("evaluate database encryption against the threat model");
  `docs/security/threat-model.md` F-05

## Context

Each till keeps its whole business history in one SQLite file, with backups beside it and
optional copies on a USB stick. The threat model (§6) shows that the Argon2id PIN hashes in it
hold out for minutes to days, not longer. Anyone with the file has the business records and,
soon after, the staff PINs. The realistic ways a file leaves the shop:
1. a stolen or discarded till PC or disk;
2. a lost backup stick;
3. malware or a remote-access session on the till while it runs.

SQLCipher turned out to be nearly built in:
- `libsqlite3-sys`, which `sqlx` already uses, can build SQLCipher in place of SQLite
  (`bundled-sqlcipher-vendored-openssl`).
- `sqlx` already sends a `key` pragma first on every connection.
- `VACUUM INTO` on a keyed connection writes a backup encrypted with the same key.
- No query, migration or test had to change.

## Decision

- **SQLCipher 4 (AES-256-CBC, HMAC-SHA512 per page)** for the till database and every backup.
- **The key** is 256 random bits from the operating system. It is kept in the OS keychain
  (Windows Credential Manager, protected by the Windows account; macOS Keychain; Secret
  Service on Linux), one entry per data folder, and passed as a raw key, so opening costs no
  key stretching.
  - On the first start the key is stored, then read back, before anything is encrypted. A
    database is never encrypted with a key that could not be read again.
  - If the keychain cannot be used, the till runs unencrypted and Settings → Backups says why.
- **A database from before encryption**, and the plain backups beside it, are converted on the
  first start: `sqlcipher_export` to a temporary file, checked with the key, then renamed over
  the original. Plain copies already in the second folder are not converted; they age out under
  retention.
- **The recovery key** is the same key, as 8 groups of 8 characters.
  - Settings → Backups shows it to the owner (audited `security.recovery_key_viewed`; the key is
    never recorded). It keeps asking until the owner confirms writing it down.
  - When the keychain lacks the key (a new Windows account, a reinstalled PC, a backup brought
    from another till), the app opens on a "data is locked" screen. Typing the recovery key
    checks it against the data, stores it, and restarts.
- **Performance:**
  - `cipher_memory_security` is off: wiping freed memory protects only against reading this
    process's memory, which the key in memory does not resist anyway.
  - Each connection caches 32 MiB of decrypted pages (SQLite's default is 2 MiB).
  - Measured on this container (`docs/perf/2026-11-19-encryption.md`): reads are as fast as
    plain SQLite; a sale commit is 1.5–2 ms slower; a cold open about 4 ms slower. Every
    target still has a wide margin.
- **Not encrypted, by design:**
  - the full data export (E19-T04), which the owner asks for in order to read it elsewhere;
  - CSV exports and receipt PDFs;
  - logs, which never hold card data or secrets.
- Full-disk encryption (BitLocker) is still recommended on top, but no longer required.

## Consequences

- Cases 1 and 2 above are covered: a stolen disk or a lost stick holds only ciphertext. The
  keychain entry is protected by the Windows account's password; a PC without a password
  protects it less (stated in the runbook).
- Case 3 is not: anything running as the till's Windows user can read what the app reads.
- **Losing the key loses the backups.** If the PC dies and the recovery key was not written
  down, its backups cannot be read anywhere. A linked till still has its sales in the cloud
  (restore by re-linking), but an unlinked one does not. This is why the Backups page insists,
  and why the runbook checks it at install.
- The build compiles OpenSSL from source (`openssl-src`): about two minutes on a cold build.
  Windows needs Perl, as the CI runners have.
- RUSTSEC-2026-0459 also affects the bundled SQLCipher (SQLite 3.45.3). The private-copy rule
  for opening other files (F-11) still applies.
- Revisit when:
  - the cloud should hold an escrow copy of the key (for linked tills);
  - a payment terminal integration brings cardholder data into scope.
