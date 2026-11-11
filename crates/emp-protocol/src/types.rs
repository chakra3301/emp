//! Domain types shared by the daemon and every client.
//!
//! Timestamps are Unix epoch milliseconds (`i64`, exported to TypeScript as `number`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The agent program behind a thread or profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    Pi,
    Hermes,
    Claude,
    Codex,
    Gemini,
    Cursor,
    /// Google Antigravity's CLI (`agy`).
    Antigravity,
    Shell,
}

impl Harness {
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Pi => "pi",
            Harness::Hermes => "hermes",
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Gemini => "gemini",
            Harness::Cursor => "cursor",
            Harness::Antigravity => "antigravity",
            Harness::Shell => "shell",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pi" => Harness::Pi,
            "hermes" => Harness::Hermes,
            "claude" => Harness::Claude,
            "codex" => Harness::Codex,
            "gemini" => Harness::Gemini,
            "cursor" => Harness::Cursor,
            "antigravity" => Harness::Antigravity,
            "shell" => Harness::Shell,
            _ => return None,
        })
    }
}

/// Where an imported record came from. One importer per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum ImportSourceKind {
    Pi,
    Hermes,
    Claude,
    Codex,
    Orion,
    Antigravity,
}

impl ImportSourceKind {
    pub const ALL: [ImportSourceKind; 6] = [
        ImportSourceKind::Pi,
        ImportSourceKind::Hermes,
        ImportSourceKind::Claude,
        ImportSourceKind::Codex,
        ImportSourceKind::Orion,
        ImportSourceKind::Antigravity,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ImportSourceKind::Pi => "pi",
            ImportSourceKind::Hermes => "hermes",
            ImportSourceKind::Claude => "claude",
            ImportSourceKind::Codex => "codex",
            ImportSourceKind::Orion => "orion",
            ImportSourceKind::Antigravity => "antigravity",
        }
    }
}

/// A working directory that threads are grouped under. Usually a git checkout root.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: String,
    /// Threads excluding automated runs.
    pub thread_count: u32,
    pub automated_count: u32,
    #[ts(type = "number | null")]
    pub last_activity_at: Option<i64>,
}

/// Agent autonomy at which nothing waits on the owner: Codex runs unsandboxed with approvals
/// off, Claude skips permission checks, Hermes and Antigravity allow everything. The default
/// for every agent.
pub const FULL_ACCESS: u8 = 3;

/// A configurable agent: a harness plus model, role, instructions and defaults.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub harness: Harness,
    /// `provider/model` where the harness supports it.
    pub model: Option<String>,
    /// Reasoning effort / thinking level as the harness names it (`high`, `xhigh`, …).
    pub effort: Option<String>,
    /// Short description of the agent's job.
    pub role: Option<String>,
    /// System prompt addition (Hermes SOUL.md, Orion Command Center charter, …).
    pub instructions: Option<String>,
    pub cwd: Option<String>,
    /// 0 = ask for everything … 3 = full access, never asks ([`FULL_ACCESS`], the default).
    pub autonomy: u8,
    /// Harness-side profile name, e.g. the Hermes profile passed with `-p`.
    pub harness_profile: Option<String>,
    /// Compact the agent's context once it passes this many tokens; overrides the global
    /// `compactAt` setting. `None` = use the global setting.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub compact_at: Option<u64>,
    /// Set when this profile was imported; re-import updates it in place.
    pub imported_from: Option<ImportOrigin>,
    /// EMP's coordinator (Suki): its chats get the `emp` tools and live in the Suki panel.
    #[serde(default)]
    pub coordinator: bool,
}

/// Provenance of an imported record.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportOrigin {
    pub source: ImportSourceKind,
    /// Stable key, unique per record across all imports (e.g. `claude:<sessionId>`).
    pub key: String,
    /// File or database the record was read from.
    pub path: String,
}

/// Locates a harness-native session so it can be read or resumed.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SessionRef {
    pub harness: Harness,
    pub session_id: String,
    /// Session file, when the harness stores one file per session.
    pub path: Option<String>,
    /// Harness home override, e.g. `CODEX_HOME` or `HERMES_HOME`.
    pub home: Option<String>,
    /// Harness-side profile, e.g. the Hermes profile name.
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum ThreadStatus {
    /// Imported history with no live process.
    Idle,
    Running,
    NeedsInput,
    Done,
    Failed,
}

/// One conversation with one agent on one task.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub harness: Harness,
    pub title: String,
    pub project_id: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub agent_id: Option<String>,
    pub parent_id: Option<String>,
    /// Subagent threads under this one.
    pub child_count: u32,
    /// The coordinator chat that started this thread through the `emp` tools.
    #[serde(default)]
    pub assigned_by: Option<String>,
    pub status: ThreadStatus,
    /// Machine-started run (e.g. a tool's `codex exec` call), hidden from lists by default.
    pub automated: bool,
    pub session: SessionRef,
    #[ts(type = "number | null")]
    pub message_count: Option<i64>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub imported_from: Option<ImportOrigin>,
}

/// One normalized entry of a transcript, independent of the harness format.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TranscriptItem {
    User {
        text: String,
        #[ts(type = "number | null")]
        ts: Option<i64>,
    },
    Assistant {
        text: String,
        model: Option<String>,
        #[ts(type = "number | null")]
        ts: Option<i64>,
    },
    Thinking {
        text: String,
        #[ts(type = "number | null")]
        ts: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    ToolCall {
        call_id: Option<String>,
        name: String,
        /// Tool input, pretty-printed or truncated for display.
        input: String,
        #[ts(type = "number | null")]
        ts: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        call_id: Option<String>,
        output: String,
        is_error: bool,
        #[ts(type = "number | null")]
        ts: Option<i64>,
    },
    /// Harness bookkeeping worth showing (model change, compaction, session start).
    System {
        text: String,
        #[ts(type = "number | null")]
        ts: Option<i64>,
    },
}

/// A program to run in a terminal, e.g. the harness TUI resuming a session.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    /// Shell-quoted one-liner for display or copy/paste.
    pub display: String,
}

/// Installed-state of one harness CLI.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HarnessStatus {
    pub harness: Harness,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
}

/// What an import source has on this machine.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub source: ImportSourceKind,
    pub found: bool,
    /// Paths the importer reads (homes, databases, session folders).
    pub paths: Vec<String>,
    /// Threads indexed from this source so far.
    pub thread_count: u32,
    pub agent_count: u32,
    #[ts(type = "number | null")]
    pub last_import_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub daemon_version: String,
    pub data_dir: String,
    pub harnesses: Vec<HarnessStatus>,
    pub sources: Vec<SourceStatus>,
}

/// Outcome of one importer run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportStats {
    pub threads_seen: u32,
    pub threads_added: u32,
    pub threads_updated: u32,
    pub threads_unchanged: u32,
    pub agents_seen: u32,
    /// Records skipped because they could not be parsed; the first few reasons.
    pub skipped: u32,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportSourceReport {
    pub source: ImportSourceKind,
    pub stats: ImportStats,
    pub error: Option<String>,
    #[ts(type = "number")]
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub sources: Vec<ImportSourceReport>,
}

/// `import.progress` notification payload.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub source: ImportSourceKind,
    pub message: String,
}
