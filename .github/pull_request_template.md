## What & why

<!-- Task ID(s) from docs/backlog, e.g. E06-T03, and a short summary. -->

## Acceptance criteria

<!-- Copy the task's checklist and tick what this PR satisfies. -->

## Checklist

- [ ] Does this change an ADR? If yes, the ADR in `docs/adr/` is updated in this PR.
- [ ] Contract changes (IPC, REST, events, migrations) are called out above and generated files are committed.
- [ ] Migrations are new files only; no merged migration was edited.
- [ ] No money or quantity handled as a binary float (Rust or TypeScript).
- [ ] State changes write an audit event and an outbox event in the same transaction (where applicable).
