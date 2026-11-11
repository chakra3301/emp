//! JSON-RPC 2.0 envelopes and method catalogue.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::types::*;

/// A request from client to daemon. `id` is echoed back in the response.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RpcRequest {
    pub jsonrpc: String,
    #[ts(type = "number | string")]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    #[ts(type = "unknown")]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown", optional)]
    pub data: Option<Value>,
}

/// A response to one request. Exactly one of `result` / `error` is present.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[ts(type = "number | string | null")]
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown", optional)]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<RpcError>,
}

/// A daemon → client push with no `id` (progress, state changes).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[ts(type = "unknown")]
    pub params: Value,
}

impl RpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }

    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(RpcError { code, message: message.into(), data: None }),
        }
    }
}

impl RpcNotification {
    pub fn new(method: &str, params: impl Serialize) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params: serde_json::to_value(params).unwrap_or(Value::Null),
        }
    }
}

/// Standard JSON-RPC codes plus EMP-specific ones (-32000..-32099).
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL: i32 = -32603;
    /// `hello` has not been completed on this connection.
    pub const NOT_AUTHENTICATED: i32 = -32001;
    /// Client protocol version is incompatible with the daemon.
    pub const INCOMPATIBLE_VERSION: i32 = -32002;
    pub const NOT_FOUND: i32 = -32004;
    /// The thread's session is busy (turn running, or held by another owner).
    pub const BUSY: i32 = -32010;
    /// The harness has no structured driver (yet), or the operation does not apply.
    pub const UNSUPPORTED: i32 = -32011;
    /// Spawning or talking to a harness process failed.
    pub const HARNESS_ERROR: i32 = -32012;
}

/// Method names. Keep in sync with `packages/protocol/src/methods.ts`.
pub mod methods {
    pub const HELLO: &str = "hello";
    pub const DOCTOR: &str = "doctor";
    pub const IMPORT_RUN: &str = "import.run";
    pub const PROJECTS_LIST: &str = "projects.list";
    pub const AGENTS_LIST: &str = "agents.list";
    pub const THREADS_LIST: &str = "threads.list";
    pub const THREADS_GET: &str = "threads.get";
    pub const THREADS_TRANSCRIPT: &str = "threads.transcript";
    pub const THREADS_RESUME_COMMAND: &str = "threads.resumeCommand";
    /// `ThreadIdParams` → `ContextUsage | null`.
    pub const THREADS_CONTEXT: &str = "threads.context";
    /// `AttachmentUploadParams` → `Attachment`.
    pub const ATTACHMENTS_UPLOAD: &str = "attachments.upload";
    /// `LocateFolderParams` → `string[]`: where a dropped folder may be, likeliest first.
    pub const ATTACHMENTS_LOCATE_FOLDER: &str = "attachments.locateFolder";
    /// `LocateFileParams` → `Attachment | null`: the file on this Mac, attached where it is.
    pub const ATTACHMENTS_LOCATE_FILE: &str = "attachments.locateFile";
    /// `CommandsListParams` → `SlashCommand[]`.
    pub const COMMANDS_LIST: &str = "commands.list";
    /// `CommandsRunParams` → `TerminalInfo`.
    pub const COMMANDS_RUN: &str = "commands.run";
    /// `()` → `DevServer[]`: the owner's listening processes.
    pub const SERVERS_LIST: &str = "servers.list";
    /// `ServersKillParams` → `ServersKillResult`.
    pub const SERVERS_KILL: &str = "servers.kill";
    // P1: terminals
    pub const TERMINALS_OPEN: &str = "terminals.open";
    pub const TERMINALS_LIST: &str = "terminals.list";
    pub const TERMINALS_ATTACH: &str = "terminals.attach";
    pub const TERMINALS_WRITE: &str = "terminals.write";
    pub const TERMINALS_RESIZE: &str = "terminals.resize";
    pub const TERMINALS_KILL: &str = "terminals.kill";
    // P1: live sessions
    pub const THREADS_START: &str = "threads.start";
    pub const THREADS_SEND: &str = "threads.send";
    pub const THREADS_INTERRUPT: &str = "threads.interrupt";
    /// `ThreadAnswerParams` → `LiveThread`.
    pub const THREADS_ANSWER: &str = "threads.answer";
    /// `ThreadCommandParams` → `LiveThread`.
    pub const THREADS_COMMAND: &str = "threads.command";
    pub const THREADS_LIVE: &str = "threads.live";
    /// `ThreadIdParams` → `ThreadGoal | null` (Codex goals; live when running, else from Codex's store).
    pub const THREADS_GOAL: &str = "threads.goal";
    /// `ThreadIdParams` → `SessionHolder[]`: processes outside EMP with the session open.
    pub const THREADS_HOLDERS: &str = "threads.holders";
    /// `ThreadIdParams` → `CloseElsewhereResult`: closes those processes (as closing their window would).
    pub const THREADS_CLOSE_ELSEWHERE: &str = "threads.closeElsewhere";
    /// `ThreadIdParams` → `boolean`: when Codex is working on the thread right now (a goal, a
    /// turn from its TUI), EMP attaches and streams it; true if it did.
    pub const THREADS_WATCH: &str = "threads.watch";
    pub const THREADS_TAKEOVER: &str = "threads.takeover";
    pub const THREADS_RELEASE: &str = "threads.release";
    // P1: agent editor
    pub const AGENTS_SAVE: &str = "agents.save";
    pub const AGENTS_DELETE: &str = "agents.delete";
    pub const MODELS_LIST: &str = "models.list";
    pub const PROJECTS_CREATE: &str = "projects.create";
    pub const PROJECTS_FILES: &str = "projects.files";
    // Phone access
    pub const PHONE_STATUS: &str = "phone.status";
    pub const PHONE_ENABLE: &str = "phone.enable";
    pub const PHONE_DISABLE: &str = "phone.disable";
    pub const PHONE_PAIR: &str = "phone.pair";
    pub const DEVICES_REVOKE: &str = "devices.revoke";
    pub const SETTINGS_GET: &str = "settings.get";
    pub const SETTINGS_SET: &str = "settings.set";
    // Thread groups
    pub const GROUPS_LIST: &str = "groups.list";
    pub const GROUPS_CREATE: &str = "groups.create";
    pub const GROUPS_UPDATE: &str = "groups.update";
    pub const GROUPS_DELETE: &str = "groups.delete";
    pub const GROUPS_ADD: &str = "groups.add";
    pub const GROUPS_REMOVE: &str = "groups.remove";
    // Scheduled tasks
    /// `()` → `Schedule[]`, next run first.
    pub const SCHEDULES_LIST: &str = "schedules.list";
    /// `ScheduleSaveParams` → `Schedule`.
    pub const SCHEDULES_SAVE: &str = "schedules.save";
    /// `ScheduleIdParams` → `null`.
    pub const SCHEDULES_DELETE: &str = "schedules.delete";
    /// `ScheduleEnableParams` → `Schedule` (pause / resume).
    pub const SCHEDULES_ENABLE: &str = "schedules.enable";
    /// `ScheduleIdParams` → `Schedule`: runs it now; the timetable carries on as before.
    pub const SCHEDULES_RUN: &str = "schedules.run";
    /// `SchedulePreviewParams` → `SchedulePreview`.
    pub const SCHEDULES_PREVIEW: &str = "schedules.preview";
    /// No params → `ExternalSchedule[]`: jobs other tools schedule themselves (Hermes cron), read-only.
    pub const SCHEDULES_EXTERNAL: &str = "schedules.external";
}

/// Notification names.
pub mod notifications {
    pub const IMPORT_PROGRESS: &str = "import.progress";
    pub const IMPORT_FINISHED: &str = "import.finished";
    /// `TerminalOutput`
    pub const TERMINAL_OUTPUT: &str = "terminal.output";
    /// `TerminalInfo`, on open, exit and kill.
    pub const TERMINAL_STATE: &str = "terminal.state";
    /// `ThreadEvent`
    pub const THREAD_EVENT: &str = "thread.event";
    /// `DaemonSettings`, after any window or phone changes them.
    pub const SETTINGS_CHANGED: &str = "settings.changed";
    /// `ThreadGroup[]`, the full list after any group change.
    pub const GROUPS_CHANGED: &str = "groups.changed";
    /// `Schedule[]`, the full list after any schedule change or run.
    pub const SCHEDULES_CHANGED: &str = "schedules.changed";
    /// `ThreadIdParams`: the coordinator asked windows to show a thread.
    pub const UI_OPEN_THREAD: &str = "ui.openThread";
}

// ---- method params / results -------------------------------------------------------------

/// First message on every connection. Authenticates and negotiates the protocol version.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    pub protocol_version: u32,
    pub token: String,
    pub client: ClientInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// `emp-desktop`, `orion-plugin`, `browser`, `cli`.
    pub kind: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HelloResult {
    pub protocol_version: u32,
    pub daemon_version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportRunParams {
    /// Limit to these sources; all sources when empty.
    #[serde(default)]
    pub sources: Vec<ImportSourceKind>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness: Option<Harness>,
    /// Case-insensitive match on title and cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub query: Option<String>,
    /// Include machine-started runs (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub include_automated: Option<bool>,
    /// Leave out subagent threads (they're listed under their parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub top_level: Option<bool>,
    /// Only this thread's subagents (machine-started ones included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent_id: Option<String>,
    /// `true`: only the coordinator's (Suki's) own chats; `false`: leave them out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinator: Option<bool>,
    /// Leave out threads that are in a group (the sidebar shows those under their group).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ungrouped: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIdParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub offset: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u32,
    pub offset: u32,
}
