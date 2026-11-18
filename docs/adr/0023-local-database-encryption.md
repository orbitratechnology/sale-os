# ADR-023: Local database encryption

- **Status:** Proposed (needs the product owner's acceptance of the residual risk, F-05)
- **Date:** 2026-11-18
- **Source:** Task E19-T05; PRD §13 ("evaluate database encryption against the threat model");
  `docs/security/threat-model.md`

## Context

Each till keeps its whole business history in a plain SQLite file, with backups beside it and
optional copies on a USB stick. The threat model (§6) shows that the Argon2id PIN hashes in it
hold out for minutes to days, not longer. Anyone with the file has the business records and,
soon after, the staff PINs. The realistic ways a file leaves the shop:
1. a stolen or discarded till PC or disk;
2. a lost backup stick;
3. malware or a remote-access session on the till while it runs.

Options:

| Option | Protects against | Costs |
|---|---|---|
| A. SQLCipher (app-level encryption), key in the OS keychain (DPAPI) | 1, 2 only if the key is not on the same disk. With DPAPI, the key opens for whoever logs in as that Windows user | A different SQLite build (static OpenSSL or a crypto provider) on every platform; about 5–15 % slower I/O; `VACUUM INTO` backups must re-key; lost key = lost data; backups on USB need a key that survives the PC's death (owner passphrase) |
| B. Full-disk encryption (BitLocker / Windows device encryption) on the till, BitLocker To Go on backup sticks | 1 and 2, with the TPM and the stick's password | None in the app. An install-time step, and a check in the runbook |
| C. Encrypt only the second-folder copies, with an owner passphrase | 2 | A passphrase to keep; forgetting it loses those backups; restore needs it |
| None | — | — |

Neither A nor B helps against 3: anything that runs as the till's user can read what the app
reads.

## Decision

- **For the pilot, B.**
  - Every till runs with BitLocker or Windows device encryption on the system drive, and backup
    sticks use BitLocker To Go.
  - The install runbook (not written yet) checks both, and the pilot go/no-go list includes them.
  - The app does not encrypt the database itself.
- Card data never enters the database (PCI scope statement), so the most sensitive class of data
  does not depend on this decision.
- **Revisit A or C** if:
  - a pilot shop cannot use BitLocker (for example Windows Home without device encryption);
  - customer personal data (loyalty, contacts) is added;
  - a regulator or the acquirer asks for application-level encryption.

  If C is chosen later, the restore flow takes the passphrase, and the full export (E19-T04)
  stays plain by design: the owner asks for it.

## Consequences

- No change to the database engine, performance or backups.
- The shop's safety now depends on a setting outside the app:
  - The runbook must check it.
  - The app could warn on the Backups page when the system drive is not encrypted (a follow-up,
    Windows-only, via `manage-bde -status` or WMI).
- A lost unencrypted stick, or a till set up without BitLocker, still exposes the data. This is
  the residual risk the product owner accepts or refuses (threat model F-05).
