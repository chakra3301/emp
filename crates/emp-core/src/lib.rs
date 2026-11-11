//! EMP's own state. Harness stores stay the source of truth for transcripts; this crate only
//! keeps metadata, links and provenance ("index, don't move").

pub mod paths;
pub mod store;

pub use store::{AgentUpsert, COORDINATOR_KEY, Grants, Store, ThreadUpsert, UpsertOutcome};

/// Current time in Unix epoch milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
