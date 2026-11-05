# Sync event schemas

This folder holds the JSON Schema for each sync event type and version, in the form
`<event_type>.v<n>.schema.json`. The files are generated from the Rust payload types in
`crates/pos-sync` by `cargo xtask gen-schemas`.

CI checks that the files are up to date (`cargo xtask gen-schemas --check`). The generator
arrives with task E17-T01. The event catalogue is in `docs/backlog/07-contracts.md` §4.
