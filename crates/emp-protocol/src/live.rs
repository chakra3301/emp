//! P1 contract: daemon-owned terminals (PTYs) and live agent sessions (structured drivers).
//!
//! Terminals carry raw bytes, base64-encoded on the wire, so split UTF-8 sequences and
//! escape codes survive intact. Every connection receives every terminal/thread push;
//! EMP is single-user, and clients filter by id.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::types::{SpawnSpec, ThreadStatus, TranscriptItem};

// ---- terminals --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalKind {
    /// Login shell in a directory (the thread's side terminal).
    Shell,
    /// The harness TUI resuming a thread's session (terminal takeover).
    Resume,
    /// An arbitrary program.
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalState {
    Running,
    Exited,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    pub kind: TerminalKind,
    pub title: String,
    pub thread_id: Option<String>,
    pub cwd: Option<String>,
    /// Shell-quoted command line that was started.
    pub command: String,
    pub pid: Option<u32>,
    pub state: TerminalState,
    pub exit_code: Option<i32>,
    pub cols: u16,
    pub rows: u16,
    #[ts(type = "number")]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenParams {
    pub kind: TerminalKind,
    /// `resume`: required. `shell`: optional; the shell starts in the thread's cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_id: Option<String>,
    /// `shell`: directory to start in (overrides the thread's cwd). Defaults to home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
    /// `command`: what to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub spec: Option<SpawnSpec>,
    pub cols: u16,
    pub rows: u16,
    /// Proceed even if the session looks active in another process (written to within the
    /// last 30 s by something other than EMP). Two writers can corrupt a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TerminalIdParams {
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWriteParams {
    pub terminal_id: String,
    /// Base64 bytes to send to the PTY (keystrokes, paste).
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizeParams {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

/// Result of `terminals.attach`: recent output to replay before live `terminal.output`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub info: TerminalInfo,
    /// Base64 of the last ≤1 MiB of output.
    pub scrollback: String,
    /// Sequence number of the last chunk included in `scrollback`; drop pushes with `seq <=` this.
    #[ts(type = "number")]
    pub seq: u64,
}

/// Push: new PTY output.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutput {
    pub terminal_id: String,
    /// Base64 bytes.
    pub data: String,
    /// Monotonic per terminal, starting at 1.
    #[ts(type = "number")]
    pub seq: u64,
}

// ---- live sessions ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum SendMode {
    /// Start a turn (or queue as follow-up if one is running).
    Prompt,
    /// Inject into the running turn after the current step.
    Steer,
    /// Run after the current turn finishes.
    FollowUp,
}

/// Start a brand-new thread with an agent and a first message.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    pub agent_id: String,
    pub text: String,
    /// Working directory. Defaults to the agent's cwd, then the project root, then home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_id: Option<String>,
    /// Overrides the agent's model for this thread (an id from `models.list`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// Overrides the agent's reasoning effort for this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<String>,
    /// Files from `attachments.upload`, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attachments: Option<Vec<String>>,
    /// What the owner is looking at (the coordinator panel sends it). It reaches the harness
    /// wrapped in `<emp-context>`, which the UI hides in the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSendParams {
    pub thread_id: String,
    pub text: String,
    /// Files from `attachments.upload`, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attachments: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<SendMode>,
    /// Proceed even if the session looks active in another process (written to within the
    /// last 30 s by something other than EMP). Two writers can corrupt a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub force: Option<bool>,
    /// What the owner is looking at (the coordinator panel sends it). It reaches the harness
    /// wrapped in `<emp-context>`, which the UI hides in the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context: Option<String>,
    /// Switch the chat to this model first (an id from `models.list` for the thread's harness).
    /// It stays switched for later messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
}

/// Live state of a thread whose session a driver currently owns.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LiveThread {
    pub thread_id: String,
    pub status: ThreadStatus,
    /// Who owns the session right now.
    pub owner: SessionOwner,
    /// Terminal id when `owner` is `terminal`.
    pub terminal_id: Option<String>,
    /// Messages waiting to be delivered (steer + follow-up), oldest first.
    pub queued: Vec<String>,
    pub error: Option<String>,
    /// Questions waiting for the owner (`threads.answer`), oldest first. The turn is paused
    /// on them; status is `needs-input` while any are open.
    #[serde(default)]
    pub questions: Vec<Question>,
    /// The long-running goal the harness pursues on its own (Codex `/goal`), as last reported.
    #[serde(default)]
    pub goal: Option<ThreadGoal>,
}

/// A Codex goal: the harness keeps starting turns while it is `active`.
/// A folder dropped on a composer. Web views only tell EMP its name and what's inside, so the
/// daemon finds where it lives.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LocateFolderParams {
    pub name: String,
    /// Names of (some of) the folder's direct children.
    #[serde(default)]
    pub entries: Vec<String>,
}

/// A file dropped or picked in a composer: web views give its name, size and modification
/// time, never its path; empd finds it on disk instead of taking a copy.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LocateFileParams {
    pub name: String,
    #[ts(type = "number")]
    pub size: u64,
    /// `File.lastModified`, ms since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub modified: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoal {
    pub objective: String,
    pub status: GoalStatus,
    #[ts(type = "number")]
    pub tokens_used: u64,
    #[ts(type = "number | null")]
    pub token_budget: Option<u64>,
    #[ts(type = "number")]
    pub time_used_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum GoalStatus {
    Active,
    Paused,
    /// The agent says it can't go on without help.
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl GoalStatus {
    /// Codex's names: `usageLimited` (app-server) and `usage_limited` (its database) both parse.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.replace(['_', '-'], "").to_ascii_lowercase().as_str() {
            "active" => Self::Active,
            "paused" => Self::Paused,
            "blocked" => Self::Blocked,
            "usagelimited" => Self::UsageLimited,
            "budgetlimited" => Self::BudgetLimited,
            "complete" => Self::Complete,
            _ => return None,
        })
    }
}

/// A process outside EMP that has a thread's session open (a harness TUI in a terminal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SessionHolder {
    pub pid: u32,
    /// The app it runs in (`Terminal`, `iTerm2`, …), when EMP can tell.
    pub app: Option<String>,
    pub command: String,
}

/// `threads.closeElsewhere`: what was closed and what is still open.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CloseElsewhereResult {
    pub closed: u32,
    pub remaining: Vec<SessionHolder>,
}

/// A question shown as a card in the chat: from the harness (a pi extension's dialog) or from
/// EMP itself (`/model` without a model picks from a list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub kind: QuestionKind,
    pub title: String,
    pub message: Option<String>,
    /// Choices for `select`.
    #[serde(default)]
    pub options: Vec<String>,
    pub placeholder: Option<String>,
    /// Starting text for `editor`.
    pub prefill: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum QuestionKind {
    Select,
    Confirm,
    Input,
    Editor,
}

/// `threads.answer`: answer (or dismiss) one of `LiveThread.questions`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadAnswerParams {
    pub thread_id: String,
    pub question_id: String,
    /// The picked option (`select`) or the text (`input`, `editor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub value: Option<String>,
    /// For `confirm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confirmed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cancelled: Option<bool>,
}

/// `threads.command`: a slash command EMP runs in the chat itself (`CommandRun::Native`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCommandParams {
    pub thread_id: String,
    /// The command line, slash included (`/compact focus on the API`).
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum SessionOwner {
    /// No process holds the session.
    None,
    /// EMP's structured driver (chat view).
    Driver,
    /// A resume TUI in an EMP terminal (takeover).
    Terminal,
}

/// Push: something happened in a live thread.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ThreadEvent {
    /// Status / owner / queue changed.
    #[serde(rename_all = "camelCase")]
    State { live: LiveThread },
    /// A finished transcript item, in order. Appended after the persisted transcript.
    #[serde(rename_all = "camelCase")]
    Item { thread_id: String, item: TranscriptItem },
    /// Streaming text for the in-progress assistant or thinking block. `text` is the
    /// whole block so far (not a delta), so a dropped push never corrupts the view.
    /// Cleared by the matching `item`.
    #[serde(rename_all = "camelCase")]
    Partial { thread_id: String, kind: PartialKind, text: String },
    /// The persisted transcript changed (turn ended, takeover returned). Refetch it.
    #[serde(rename_all = "camelCase")]
    TranscriptChanged { thread_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum PartialKind {
    Assistant,
    Thinking,
    /// Running tool output (bash streaming).
    Tool,
}

/// `threads.takeover`: hand the session to its harness TUI in an EMP terminal.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTakeoverParams {
    pub thread_id: String,
    pub cols: u16,
    pub rows: u16,
    /// Proceed even if the session looks active in another process (written to within the
    /// last 30 s by something other than EMP). Two writers can corrupt a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub force: Option<bool>,
}

// ---- agent editor ------------------------------------------------------------------------

/// `agents.save`: create (no `id`) or update an agent profile. Editing an imported agent
/// detaches it from re-import, so later imports don't overwrite the edits.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AgentSaveParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    pub name: String,
    pub harness: crate::types::Harness,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub role: Option<String>,
    pub instructions: Option<String>,
    pub cwd: Option<String>,
    pub autonomy: u8,
    pub harness_profile: Option<String>,
    /// Per-agent compaction limit in tokens; `None` = the global setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number | null")]
    pub compact_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AgentIdParams {
    pub agent_id: String,
}

// ---- models and projects -----------------------------------------------------------------

/// `models.list`: models a harness can run, for the composer's model picker.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ModelsListParams {
    pub harness: crate::types::Harness,
    /// Hermes profile whose provider catalogue to read (`default` = `~/.hermes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    /// Value to pass back as `ThreadStartParams.model`.
    pub id: String,
    pub label: String,
    /// Provider or family, for grouping.
    pub group: Option<String>,
    /// Context window in tokens, when the harness says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub context: Option<u64>,
}

/// `threads.context`: how full a thread's context window is, from the session's last model call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    /// Tokens in the context at the last model call (prompt incl. cache, plus its output).
    #[ts(type = "number")]
    pub used: u64,
    /// The model's context window, when known.
    #[ts(type = "number | null")]
    pub window: Option<u64>,
    /// Model the usage was measured on, as the harness names it.
    pub model: Option<String>,
}

/// `projects.create`: register a folder as a project (optionally creating it).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateParams {
    /// Absolute path or `~/…`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// Create the folder when it doesn't exist (otherwise that's an error).
    #[serde(default)]
    pub create_folder: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFilesParams {
    pub project_id: String,
}

/// Top level of a project folder, for the Projects page's expanded row.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFiles {
    pub root_path: String,
    /// False when the folder is gone (moved or deleted since import).
    pub exists: bool,
    /// Folders first, then files, by name. Hidden entries are skipped.
    pub entries: Vec<FileEntry>,
    /// More entries than were returned.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    #[ts(type = "number | null")]
    pub size: Option<u64>,
    #[ts(type = "number | null")]
    pub modified_at: Option<i64>,
}

/// Phone access (Settings → Phone): Tailscale state, the web address and paired devices.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PhoneStatus {
    /// `missing` (no CLI), `stopped`, `needs-login`, `running`, or `unknown`.
    pub tailscale: String,
    /// This Mac's tailnet name, e.g. `mac.tail1234.ts.net`.
    pub dns_name: Option<String>,
    /// `https://<dns name>` when `tailscale serve` points at this daemon.
    pub url: Option<String>,
    /// The local port the daemon serves the web app and WebSocket on.
    pub port: u16,
    /// False when the daemon picked a random port (a hand-started dev daemon): phone access then
    /// needs EMP's own daemon, which always uses the same port.
    pub port_fixed: bool,
    /// Something else is served on this Mac's HTTPS port (left alone).
    pub serving_other: Option<String>,
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number | null")]
    pub last_seen_at: Option<i64>,
}

/// A short-lived pairing code, shown on the Mac as text and as a QR code.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    /// `ABCD-EFGH`; typed on the phone or carried in the QR link.
    pub code: String,
    /// Link the QR code opens (the tailnet URL with `#pair=<code>`); None when phone access is off.
    pub url: Option<String>,
    /// The QR code as an SVG document (None without a URL).
    pub qr_svg: Option<String>,
    #[ts(type = "number")]
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIdParams {
    pub device_id: String,
}

/// Daemon-wide settings (shared by every window and paired phone).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DaemonSettings {
    /// Compact a chat's context once it passes this many tokens (all agents unless an agent sets
    /// its own). `None` = leave it to each harness's default (often near the full window).
    #[serde(default)]
    #[ts(type = "number | null")]
    pub compact_at: Option<u64>,
}

// ---- thread groups -----------------------------------------------------------------------

/// The owner's own group of threads. A thread is in at most one group.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGroup {
    pub id: String,
    pub name: String,
    /// Pinned groups sit at the top of the sidebar.
    pub pinned: bool,
    #[ts(type = "number")]
    pub created_at: i64,
    /// Newest first.
    pub threads: Vec<crate::types::Thread>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GroupCreateParams {
    pub name: String,
    #[serde(default)]
    #[ts(optional)]
    pub pinned: Option<bool>,
    /// Threads to move into the new group.
    #[serde(default)]
    #[ts(optional)]
    pub thread_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GroupUpdateParams {
    pub group_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GroupIdParams {
    pub group_id: String,
}

/// Moves threads into a group (out of any group they were in).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GroupAddParams {
    pub group_id: String,
    pub thread_ids: Vec<String>,
}

// ---- attachments and slash commands ------------------------------------------------------

/// `attachments.upload`: a dropped or pasted file. The daemon keeps a copy in its data folder
/// and hands the agent that path (and the image itself, where the harness takes images).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploadParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mime: Option<String>,
    /// File bytes, base64.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: Option<String>,
    #[ts(type = "number")]
    pub size: u64,
    /// Where the copy lives; this is what the agent sees.
    pub path: String,
}

/// Where a slash command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum CommandRun {
    /// Sent as a chat message; the harness expands it (templates, skills, custom commands).
    Chat,
    /// EMP runs it in the chat through the harness's own API (`threads.command`), or in the
    /// UI (`/new`, `/copy`).
    Native,
    /// Needs the harness's own terminal UI (`/login`, `/model`, …): `commands.run` opens it there.
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    /// Without the slash (`login`, `skill:wiki`).
    pub name: String,
    pub description: Option<String>,
    pub run: CommandRun,
    /// `built-in`, `skill`, `prompt`, `extension`, `custom`, …
    pub source: Option<String>,
}

/// `commands.list`: slash commands for an agent's harness.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CommandsListParams {
    pub harness: crate::types::Harness,
    /// Its cwd decides project-level commands; Hermes uses its profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
}

/// `commands.run`: open the harness's terminal UI and type a slash command into it. With a
/// `threadId` it's that thread's session (a takeover); otherwise a fresh session of the agent.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CommandsRunParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
    /// The command line, slash included (`/login openai-codex`).
    pub text: String,
    pub cols: u16,
    pub rows: u16,
}

// ---- dev servers ---------------------------------------------------------------------------

/// What a listening process looks like it is. Only `dev` shows by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ServerKind {
    /// A project's dev/preview/static server (node, vite, next, python, ruby, a cargo build, …).
    Dev,
    /// Databases, containers and other long-lived tools (postgres, redis, docker, adb).
    Service,
    /// Something a coding agent runs (codex, agy, pi extensions, Hermes's node).
    Agent,
    /// A Mac app or system process (anything in an `.app` bundle or under `/System`).
    App,
}

/// `servers.list`: a process of the owner's that listens on a TCP port.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DevServer {
    pub pid: u32,
    pub ppid: u32,
    /// Short label: `vite preview`, `next-server (v16.3.4)`, `python -m http.server`.
    pub name: String,
    /// Full command line, as `ps` shows it.
    pub command: String,
    pub cwd: Option<String>,
    /// Listening ports, ascending, without duplicates (IPv4 and IPv6 count once).
    pub ports: Vec<u16>,
    /// Some address is reachable from other machines (`*` or `0.0.0.0`), not just this Mac.
    pub exposed: bool,
    pub kind: ServerKind,
    /// Established TCP connections into its ports right now (an open browser tab keeps one).
    pub connections: u32,
    /// Unix ms.
    #[ts(type = "number")]
    pub started_at: i64,
    /// Resident memory in bytes.
    #[ts(type = "number")]
    pub memory: u64,
    /// The EMP terminal it runs in, when one is an ancestor.
    pub terminal_id: Option<String>,
    /// EMP's own daemon or its parents: shown, never killed.
    pub protected: bool,
}

/// `servers.kill`: stop one listener. The daemon re-scans first and refuses pids that aren't
/// listening any more or are protected. Without `force` it's SIGTERM (plus a package-runner
/// parent such as `npm run dev`); `force` is SIGKILL.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ServersKillParams {
    pub pid: u32,
    #[serde(default)]
    #[ts(optional)]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ServersKillResult {
    /// The process is gone (checked for up to 3 s).
    pub stopped: bool,
}
