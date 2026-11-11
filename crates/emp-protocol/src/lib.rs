//! EMP wire protocol.
//!
//! Every client (EMP.app, the Orion plugin, the CLI) talks to `empd` with JSON-RPC 2.0
//! messages over a transport that only has to move JSON values:
//! a WebSocket, a Unix socket with newline-delimited JSON, or Orion's `localService` broker.
//!
//! Types here are the single source of truth. `cargo test -p emp-protocol` regenerates the
//! TypeScript bindings in `packages/protocol/src/generated`.

pub mod live;
pub mod rpc;
pub mod schedules;
pub mod types;

pub use live::*;
pub use rpc::*;
pub use schedules::*;
pub use types::*;

/// Bumped on any breaking change to methods, params, results or notifications.
/// Clients send theirs in `hello`; the daemon refuses a mismatched major version.
pub const PROTOCOL_VERSION: u32 = 1;
