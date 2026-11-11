# Architecture

EMP is a local, single-user workspace for existing coding-agent harnesses. The daemon owns processes and persistence; the user interface can disconnect and reconnect without taking ownership of those processes.

## Packages

- `empd`: local RPC server, agent drivers, PTYs, schedules, and coordination.
- `emp-core`: SQLite domain store and data-directory layout.
- `emp-import`: read-only session metadata and transcript importers.
- `emp-protocol`: Rust protocol types, with TypeScript generated through ts-rs.
- `@emp/client`: typed requests and notifications over an injected transport.
- `@emp/ui`: React workspace shared by the desktop host and plugin build.
- `apps/desktop`: Tauri shell and browser development host.

## Ownership

Harnesses retain ownership of their native session stores. EMP indexes metadata and reads transcripts without modifying those stores. A session has one active writer: either its structured chat driver or terminal takeover.

## Persistence

The daemon stores application metadata in SQLite with append-only migrations. UI preferences use the injected host preference interface. The protocol crate defines RPC data, while the hand-maintained TypeScript method map stays aligned with the Rust method catalogue.

## 4a. Host and sandbox boundaries

The React workspace receives an `EmpHost` and a transport-independent client. Host-specific authority stays outside `@emp/ui`: browser and native operations go through the host, and filesystem/process operations go through the daemon.

The plugin target is a single HTML file with inline JavaScript and embedded fonts, within a 4 MiB budget. It cannot use network APIs, workers, dynamic imports, browser storage, or window-level routing/listeners. Styles are scoped under `.emp-root`; stores are isolated per workspace instance.

Use `?mock` in the browser development host for deterministic synthetic data without a daemon.
