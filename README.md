# EMP

A local workspace for coding agents and terminals on macOS. Organize projects and conversations, manage agent profiles, watch work in progress, and take over a real terminal when needed. A background daemon keeps work running when the window closes.

[emp.orionterminal.app](https://emp.orionterminal.app)

## Development

Requires Node.js 22+, npm, and Rust 1.95+. The native desktop app targets macOS and requires the Xcode command line tools.

```sh
npm ci
npm run dev
```

Open `http://127.0.0.1:5174/?mock` for the interface with synthetic demo data and no daemon connection.

To work with a separate development daemon:

```sh
EMP_DATA_DIR=/tmp/emp-dev cargo run -p empd -- serve
```

Use an isolated data directory when experimenting. Running a daemon enables its local session importers; it does not require sending prompts to an agent.

## Checks

```sh
npm run check
cargo test --workspace
cargo clippy --workspace --all-targets
```

## Structure

- `crates/empd`: background daemon, harness drivers, terminals, scheduling, and RPC.
- `crates/emp-core`: SQLite store and application data.
- `crates/emp-import`: read-only session importers.
- `crates/emp-protocol` and `packages/protocol`: shared Rust and TypeScript protocol.
- `packages/client`: transport-independent client and mock server.
- `packages/ui`: React workspace.
- `apps/desktop`: Tauri desktop host and browser development host.

## License

[Apache License 2.0](LICENSE). Bundled font notices and terms are in [packages/ui/FONTS-LICENSE.txt](packages/ui/FONTS-LICENSE.txt).
