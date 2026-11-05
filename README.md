# sale-os

Universal Retail POS: a desktop-first, offline-first retail point of sale. It is built with
Tauri 2, React/TypeScript and Rust, with SQLite locally and an Axum/PostgreSQL cloud for sync.

- Product requirements: [docs/PRD.md](docs/PRD.md)
- Implementation backlog: [docs/backlog/README.md](docs/backlog/README.md) — epics, migrations,
  interfaces, contracts, and tasks with acceptance criteria
- Progress: [docs/backlog/status.md](docs/backlog/status.md)
- Architecture decisions: [docs/adr/](docs/adr/README.md)

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | 1.97.0 | Pinned in `rust-toolchain.toml`; `rustup` installs it automatically |
| Node.js | ≥ 22 | |
| pnpm | 10.28 | `corepack enable` picks the version from `package.json` |
| Windows | 11 23H2+ | WebView2 is preinstalled |
| Linux (dev only) | — | `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev` |
| PostgreSQL | 16 | Optional locally. Only needed for the cloud database tests. |

## Everyday commands

```sh
pnpm install                      # once
pnpm dev                          # run the desktop app (Vite + Tauri, hot reload)

cargo test --workspace            # all Rust tests
pnpm test && pnpm lint && pnpm typecheck

cargo xtask check-deps            # crate layering rules (docs/backlog/02 §3)
cargo xtask gen-bindings          # regenerate TS IPC bindings after changing a command
cargo xtask trace                 # every PRD P0 requirement is traced to a task

pnpm tauri build --debug          # Windows: NSIS + MSI installers in target/debug/bundle
```

Cloud API:

```sh
cargo run -p cloud-api                                   # serves on 0.0.0.0:8080 (/v1/health)
DATABASE_URL=postgres://… cargo run -p cloud-api migrate
TEST_DATABASE_URL=postgres://… cargo test -p cloud-api  # runs the DB-backed tests too
```

## Layout

See [docs/backlog/02-architecture-and-modules.md](docs/backlog/02-architecture-and-modules.md).
In short:

- `crates/pos-domain`: pure rules
- `crates/pos-local-db`: SQLite
- `apps/desktop-tauri`: the shell and IPC
- `apps/desktop-ui`: React
- `apps/cloud-api`: Axum
- `xtask`: repository automation
