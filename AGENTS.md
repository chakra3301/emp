# EMP

EMP is a local workspace for coding-agent harnesses and terminals. A Rust daemon owns sessions, processes, and persistence; the React workspace is a host-independent client. The native desktop host uses Tauri 2.

## Orientation

- Read this file and the relevant package before changing code.
- Design values live in `design/tokens.json`; interface guidance is in `design/DESIGN.md`.
- Architecture and embedding constraints are in `design/FOUNDATION.md`.
- Preserve existing work. Commit or push only when explicitly requested.

## Commands

Node 22+, npm workspaces, Rust 1.95+.

- `npm ci`: install JavaScript dependencies.
- `npm run dev`: browser development host at http://127.0.0.1:5174/; `?mock` uses synthetic fixtures without a daemon.
- `npm run check`: TypeScript, tests, generated token/font checks, sandbox checks, and both UI builds.
- `cargo test --workspace`: Rust tests.
- `cargo clippy --workspace --all-targets`: Rust lint.
- `cargo test -p emp-protocol`: regenerate TypeScript protocol types with ts-rs.
- `npm run tokens` / `npm run fonts`: regenerate checked-in CSS.

Prefer focused checks while developing. Do not install or restart the desktop application unless requested.

## Boundaries

- `@emp/ui` must not import Tauri or Node APIs. Host functions go through `EmpHost`; daemon calls provide filesystem and process authority.
- The UI must work in the plugin sandbox: no fetch, WebSocket, Workers, dynamic import, localStorage, or window.open. Fonts and images use data/blob URLs. The single-file build must stay within 4 MiB.
- Scope styles under `.emp-root` and use existing `--emp-*` tokens. Do not invent design tokens.
- Create Zustand stores per workspace instance. Use the memory router, container-relative layout, and full unmount cleanup. Do not add window-level key listeners.
- `@emp/client` uses the `Transport` interface. Terminal bytes travel over that transport.
- `emp-protocol` is the protocol source of truth. Keep `packages/protocol/src/methods.ts` in sync with Rust RPC methods and export new generated types.
- Database migrations in `crates/emp-core/src/store.rs` are append-only.
- Import external harness data read-only through its supported interfaces or copies. Foreign SQLite stores must use `emp_import::util::open_db`.
- Never read or log credentials. Use synthetic fixtures and temporary storage for tests; do not access personal harness sessions for routine validation.
- The chat driver and terminal takeover must never own the same harness session concurrently.
- Kill only processes you started, by PID. Do not use broad process-name matching.
- Do not run `hermes resume`; use `--resume <id>` or ACP `session/load`.
