//! Read-only indexers for the harness stores already on this machine.
//!
//! Rules every importer follows:
//! - **Never write** to a harness store. SQLite files are opened with [`open_readonly`]; files
//!   are only read.
//! - **Index, don't copy.** Produce metadata ([`ThreadUpsert`], [`AgentUpsert`]); transcripts
//!   are read on demand from the source by [`Importer::transcript`].
//! - **Be incremental.** Compute a cheap fingerprint (e.g. `mtime:size`) first and skip parsing
//!   when [`ScanCtx::is_unchanged`] says the record hasn't changed.
//! - **Tolerate bad records.** A record that can't be parsed is skipped with [`ScanCtx::skip`];
//!   only an unreadable source fails the run.
//! - **Never read credentials** (`auth.json`, tokens, keychain items, `~/.claude/sessions/*.json`).

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod hermes;
pub mod orion;
pub mod pi;
pub mod resume;
pub mod util;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use emp_core::{AgentUpsert, ThreadUpsert};
use emp_protocol::{ContextUsage, ImportSourceKind, ImportStats, Thread, TranscriptItem};

/// One import source.
pub trait Importer: Send + Sync {
    fn kind(&self) -> ImportSourceKind;

    /// Files and folders this importer reads, for the Doctor screen.
    fn paths(&self) -> Vec<PathBuf>;

    /// Whether the source exists on this machine.
    fn found(&self) -> bool {
        self.paths().iter().any(|p| p.exists())
    }

    /// Walks the source and reports threads and agent profiles into `ctx`.
    fn scan(&self, ctx: &mut ScanCtx) -> Result<()>;

    /// Reads one thread's transcript from the source, normalized. `thread.imported_from`
    /// and `thread.session` carry everything needed to locate it.
    fn transcript(&self, thread: &Thread) -> Result<Vec<TranscriptItem>>;

    /// How full the thread's context was at its last model call, when the source records
    /// token usage. The window is filled in by the daemon when the source doesn't say.
    fn context(&self, _thread: &Thread) -> Result<Option<ContextUsage>> {
        Ok(None)
    }
}

/// Collects one importer's output. The daemon applies it to the store in one transaction.
pub struct ScanCtx {
    known: HashMap<String, String>,
    pub threads: Vec<ThreadUpsert>,
    pub agents: Vec<AgentUpsert>,
    pub stats: ImportStats,
    progress: Option<ProgressFn>,
}

type ProgressFn = Box<dyn FnMut(&str) + Send>;

const MAX_WARNINGS: usize = 20;

impl ScanCtx {
    /// `known` maps `import_key → fingerprint` from the previous import of this source.
    pub fn new(known: HashMap<String, String>) -> Self {
        Self { known, threads: Vec::new(), agents: Vec::new(), stats: ImportStats::default(), progress: None }
    }

    pub fn with_progress(mut self, f: impl FnMut(&str) + Send + 'static) -> Self {
        self.progress = Some(Box::new(f));
        self
    }

    /// True when `key` was imported before with the same fingerprint. Counts the thread as
    /// seen and unchanged, so the importer can `continue` without parsing.
    pub fn is_unchanged(&mut self, key: &str, fingerprint: &str) -> bool {
        if self.known.get(key).is_some_and(|fp| fp == fingerprint) {
            self.stats.threads_seen += 1;
            self.stats.threads_unchanged += 1;
            true
        } else {
            false
        }
    }

    pub fn thread(&mut self, t: ThreadUpsert) {
        self.stats.threads_seen += 1;
        self.threads.push(t);
    }

    pub fn agent(&mut self, a: AgentUpsert) {
        self.stats.agents_seen += 1;
        self.agents.push(a);
    }

    /// Records a skipped record with a short reason (kept for the first few).
    pub fn skip(&mut self, reason: impl Into<String>) {
        self.stats.skipped += 1;
        self.warn(reason);
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        if self.stats.warnings.len() < MAX_WARNINGS {
            self.stats.warnings.push(msg.into());
        }
    }

    pub fn progress(&mut self, msg: &str) {
        if let Some(f) = self.progress.as_mut() {
            f(msg);
        }
    }
}

/// Every importer, in the order they run.
pub fn all() -> Vec<Box<dyn Importer>> {
    vec![
        Box::new(pi::PiImporter::default()),
        Box::new(hermes::HermesImporter::default()),
        Box::new(claude::ClaudeImporter::default()),
        Box::new(codex::CodexImporter::default()),
        Box::new(antigravity::AntigravityImporter::default()),
        Box::new(orion::OrionImporter::default()),
    ]
}

pub fn by_kind(kind: ImportSourceKind) -> Box<dyn Importer> {
    all().into_iter().find(|i| i.kind() == kind).expect("every kind has an importer")
}
