//! Codex CLI importer (verified against Codex 0.162.1).
//!
//! Homes indexed: the default home (`$CODEX_HOME` or `~/.codex`, `SessionRef.home = None`) and
//! Orion Terminal's engine home `…/com.lucaorion.orion-terminal/cli-engines/codex-home`
//! (`SessionRef.home = Some(path)` so resume sets `CODEX_HOME`). RepoLens scan homes
//! (`cli-engines/repolens-codex/*`) are deliberately not indexed.
//!
//! Threads come from `<home>/state_5.sqlite` table `threads` (`created_at_ms`/`updated_at_ms`
//! epoch ms, `created_at`/`updated_at` epoch s; `rollout_path` absolute). When the table is
//! missing or unreadable, `<home>/sessions/YYYY/MM/DD/rollout-*.jsonl` is walked instead.
//!
//! Subagents: `threads.source` is either a bare string (`cli`, `exec`, `vscode`, …) or JSON
//! `{"subagent":{"thread_spawn":{"parent_thread_id","depth","agent_path","agent_nickname",
//! "agent_role"}}}` (or `{"subagent":{"other":…}}`). The table `thread_spawn_edges
//! (parent_thread_id, child_thread_id)` carries the same link and is used as a fallback; rollout
//! `session_meta` has `parent_thread_id` too.
//!
//! Rollout records are `{timestamp (RFC 3339), type, payload}`. The transcript is built from
//! `response_item`s only; `event_msg`s (`item_completed`, `token_count`, …) duplicate them.
//! Injected context is dropped: every `developer`/`system` message, and `user` messages that open
//! with a harness tag (`<environment_context>`, `<user_instructions>`, `# AGENTS.md`, …, see
//! [`INJECTED_PREFIXES`]). Reasoning is encrypted; only a non-empty `summary` becomes Thinking.
//!
//! The SQLite store is read via [`crate::util::open_db`] (never touches files next to it).
//!
//! Never read: `auth.json`, `*.lock`, `mcp-oauth-locks/`. From `config.toml` only the top-level
//! `model` and `model_reasoning_effort` lines are parsed.

use std::collections::HashMap;
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use emp_core::{AgentUpsert, ThreadUpsert};
use emp_protocol::{Harness, ImportSourceKind, SessionRef, Thread, TranscriptItem};
use rusqlite::Connection;
use serde_json::Value;

use crate::util::{
    self, content_text, display_json, file_fingerprint, for_each_jsonl, mtime_ms, parse_ts, project_root_for,
    title_from, truncate,
};
use crate::util::{col, columns, open_db};
use crate::{Importer, ScanCtx};

const ORION_HOME: &str = "Library/Application Support/com.lucaorion.orion-terminal/cli-engines/codex-home";

/// User-role messages that Codex injects rather than the person typing them.
const INJECTED_PREFIXES: &[&str] = &[
    "<environment_context>",
    "<user_instructions>",
    "<permissions instructions>",
    "<recommended_plugins>",
    "<skills_instructions>",
    "<turn_aborted>",
    "<app-context>",
    "<model_switch>",
    "# AGENTS.md",
];

struct CodexHome {
    path: PathBuf,
    /// Value for `SessionRef.home`; `None` for the default home.
    attributed: Option<String>,
}

pub struct CodexImporter {
    homes: Vec<CodexHome>,
    /// `config.toml` of the default home, for the default agent profile.
    config: PathBuf,
}

impl Default for CodexImporter {
    fn default() -> Self {
        let home = emp_core::paths::home_dir();
        let default_home = std::env::var_os("CODEX_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        Self::with_homes(default_home, Some(home.join(ORION_HOME)))
    }
}

impl CodexImporter {
    /// Importer over a default home plus an optional attributed (non-default) home.
    pub fn with_homes(default_home: impl Into<PathBuf>, extra_home: Option<PathBuf>) -> Self {
        let default_home = default_home.into();
        let config = default_home.join("config.toml");
        let mut homes = vec![CodexHome { path: default_home, attributed: None }];
        if let Some(extra) = extra_home.filter(|p| *p != homes[0].path) {
            let attributed = Some(extra.to_string_lossy().into_owned());
            homes.push(CodexHome { path: extra, attributed });
        }
        Self { homes, config }
    }
}

impl Importer for CodexImporter {
    fn kind(&self) -> ImportSourceKind {
        ImportSourceKind::Codex
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.homes.iter().map(|h| h.path.clone()).collect()
    }

    fn scan(&self, ctx: &mut ScanCtx) -> Result<()> {
        if self.homes[0].path.is_dir() {
            ctx.agent(default_agent(&self.config));
        }
        let mut present = 0usize;
        let mut failed = 0usize;
        let mut last_err = None;
        for home in &self.homes {
            if !home.path.is_dir() {
                continue;
            }
            present += 1;
            ctx.progress(&format!("codex home {}", home.path.display()));
            let db = home.path.join("state_5.sqlite");
            if db.exists() {
                match scan_db(ctx, home, &db) {
                    Ok(()) => continue,
                    Err(e) => ctx.warn(format!("codex {}: {e:#}; walking rollouts instead", db.display())),
                }
            }
            if let Err(e) = scan_rollouts(ctx, home) {
                ctx.warn(format!("codex {}: {e:#}", home.path.display()));
                failed += 1;
                last_err = Some(e);
            }
        }
        match last_err {
            Some(e) if failed == present => Err(e),
            _ => Ok(()),
        }
    }

    /// The last `token_count` event: `last_token_usage` (input includes cached) and the
    /// model's context window.
    fn context(&self, thread: &Thread) -> Result<Option<emp_protocol::ContextUsage>> {
        let Some(path) = thread.session.path.as_deref() else { return Ok(None) };
        util::last_jsonl(Path::new(path), "\"token_count\"", |r| {
            let info = &r["payload"]["info"];
            let u = &info["last_token_usage"];
            (r["payload"]["type"] == "token_count" && u.is_object()).then(|| emp_protocol::ContextUsage {
                used: u["input_tokens"].as_u64().unwrap_or(0) + u["output_tokens"].as_u64().unwrap_or(0),
                window: info["model_context_window"].as_u64(),
                model: None,
            })
        })
    }

    fn transcript(&self, thread: &Thread) -> Result<Vec<TranscriptItem>> {
        let path = thread
            .session
            .path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("codex thread {} has no rollout path", thread.session.session_id))?;
        read_transcript(&path)
    }
}

struct ThreadRow {
    id: String,
    rollout_path: String,
    created_ms: Option<i64>,
    updated_ms: Option<i64>,
    created_s: Option<i64>,
    updated_s: Option<i64>,
    cwd: Option<String>,
    title: Option<String>,
    name: Option<String>,
    first_user_message: Option<String>,
    source: Option<String>,
    model: Option<String>,
    /// Subagents have a nickname but often no title or first message.
    nickname: Option<String>,
    /// `--thread-source` given at creation; EMP passes `emp`.
    thread_source: Option<String>,
}

fn scan_db(ctx: &mut ScanCtx, home: &CodexHome, db: &Path) -> Result<()> {
    let conn = open_db(db)?;
    let cols = columns(&conn, "threads")?;
    if !(cols.contains("id") && cols.contains("rollout_path")) {
        return Err(anyhow!("no usable threads table"));
    }
    let edges = spawn_edges(&conn);
    let sql = format!(
        "SELECT id, rollout_path, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM threads",
        col(&cols, "created_at_ms"),
        col(&cols, "updated_at_ms"),
        col(&cols, "created_at"),
        col(&cols, "updated_at"),
        col(&cols, "cwd"),
        col(&cols, "title"),
        col(&cols, "name"),
        col(&cols, "first_user_message"),
        col(&cols, "source"),
        col(&cols, "model"),
        col(&cols, "agent_nickname"),
        col(&cols, "thread_source"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(ThreadRow {
            id: r.get(0)?,
            rollout_path: r.get(1)?,
            created_ms: r.get(2)?,
            updated_ms: r.get(3)?,
            created_s: r.get(4)?,
            updated_s: r.get(5)?,
            cwd: r.get(6)?,
            title: r.get(7)?,
            name: r.get(8)?,
            first_user_message: r.get(9)?,
            source: r.get(10)?,
            model: r.get(11)?,
            nickname: r.get(12)?,
            thread_source: r.get(13)?,
        })
    })?;
    let db_path = db.to_string_lossy().into_owned();
    for row in rows {
        let t = match row {
            Ok(t) => t,
            Err(e) => {
                ctx.skip(format!("codex: bad threads row: {e}"));
                continue;
            }
        };
        let key = format!("codex:{}", t.id);
        let rollout = resolve(&home.path, &t.rollout_path);
        let updated = t.updated_ms.or(t.updated_s.map(|s| s * 1000)).unwrap_or(0);
        // Rollout mtime:size catches new turns; updated_at catches renames that only touch the db.
        let fingerprint = match file_fingerprint(&rollout) {
            Ok(fp) => format!("{fp}:{updated}"),
            Err(_) => format!("db:{updated}"),
        };
        if ctx.is_unchanged(&key, &fingerprint) {
            continue;
        }
        let title = [&t.name, &t.title, &t.first_user_message]
            .into_iter()
            .flatten()
            .find_map(|s| title_from(s))
            .or_else(|| t.nickname.as_deref().and_then(title_from).map(|n| format!("Subagent: {n}")))
            .unwrap_or_else(|| "Untitled Codex session".to_string());
        let parent = t.source.as_deref().and_then(parent_from_source).or_else(|| edges.get(&t.id).cloned());
        let cwd = t.cwd.filter(|c| !c.trim().is_empty());
        let cwd_for_flag = cwd.clone();
        let created = t.created_ms.or(t.created_s.map(|s| s * 1000)).unwrap_or(updated);
        ctx.thread(ThreadUpsert {
            import_source: ImportSourceKind::Codex,
            import_key: key,
            import_path: db_path.clone(),
            fingerprint,
            harness: Harness::Codex,
            title,
            project_root: cwd.as_deref().map(project_root_for),
            cwd,
            model: t.model.filter(|m| !m.is_empty()),
            session: SessionRef {
                harness: Harness::Codex,
                session_id: t.id.clone(),
                path: Some(rollout.to_string_lossy().into_owned()),
                home: home.attributed.clone(),
                profile: None,
            },
            message_count: None,
            created_at: created,
            updated_at: updated.max(created),
            parent_key: parent.map(|p| format!("codex:{p}")),
            automated: t.thread_source.as_deref() != Some("emp")
                && is_automated(t.source.as_deref(), cwd_for_flag.as_deref(), t.first_user_message.as_deref()),
        });
    }
    Ok(())
}

/// `child → parent` from `thread_spawn_edges`, if that table exists.
/// The thread's goal (`/goal`) from Codex's `goals_1.sqlite`, read like every foreign store:
/// through `open_db`. `home` is the session's `CODEX_HOME`, `None` for the default one.
pub fn goal(home: Option<&Path>, thread_id: &str) -> Result<Option<emp_protocol::ThreadGoal>> {
    let home = match home {
        Some(h) => h.to_path_buf(),
        None => std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| emp_core::paths::home_dir().join(".codex")),
    };
    let path = home.join("goals_1.sqlite");
    if !path.exists() {
        return Ok(None);
    }
    let db = open_db(&path)?;
    let row = db.query_row(
        "SELECT objective, status, tokens_used, token_budget, time_used_seconds FROM thread_goals WHERE thread_id = ?1",
        [thread_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, i64>(4)?)),
    );
    let (objective, status, tokens, budget, secs) = match row {
        Ok(r) => r,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(emp_protocol::GoalStatus::parse(&status).map(|status| emp_protocol::ThreadGoal {
        objective,
        status,
        tokens_used: tokens.max(0) as u64,
        token_budget: budget.map(|b| b.max(0) as u64),
        time_used_seconds: secs.max(0) as u64,
    }))
}

fn spawn_edges(conn: &Connection) -> HashMap<String, String> {
    let Ok(mut stmt) = conn.prepare("SELECT child_thread_id, parent_thread_id FROM thread_spawn_edges") else {
        return HashMap::new();
    };
    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Parent thread id from a `source` value (bare string or JSON).
fn parent_from_source(source: &str) -> Option<String> {
    let v: Value = serde_json::from_str(source).ok()?;
    parent_from_source_value(&v)
}

fn parent_from_source_value(v: &Value) -> Option<String> {
    v.pointer("/subagent/thread_spawn/parent_thread_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(Into::into)
}

fn resolve(home: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() { path } else { home.join(path) }
}

/// Fallback when `state_5.sqlite` is missing or unreadable: one thread per rollout file.
fn scan_rollouts(ctx: &mut ScanCtx, home: &CodexHome) -> Result<()> {
    let sessions = home.path.join("sessions");
    if !sessions.is_dir() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(&sessions).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                ctx.warn(format!("codex walk: {e}"));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy();
        if !entry.file_type().is_file() || !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }
        let path = entry.path();
        // rollout-<timestamp>-<uuid>.jsonl: the uuid is the thread id.
        let stem = name.trim_end_matches(".jsonl");
        let Some(id) = stem.get(stem.len().saturating_sub(36)..).filter(|s| s.len() == 36 && s.contains('-')) else {
            ctx.skip(format!("codex: no thread id in {name}"));
            continue;
        };
        let key = format!("codex:{id}");
        let Ok(fingerprint) = file_fingerprint(path) else {
            ctx.skip(format!("codex: unreadable {name}"));
            continue;
        };
        if ctx.is_unchanged(&key, &fingerprint) {
            continue;
        }
        let head = match rollout_head(path) {
            Ok(h) => h,
            Err(e) => {
                ctx.skip(format!("codex {name}: {e:#}"));
                continue;
            }
        };
        let mtime = fs::metadata(path).ok().as_ref().and_then(mtime_ms).unwrap_or(0);
        let created = head.created.unwrap_or(mtime);
        let cwd = head.cwd.filter(|c| !c.trim().is_empty());
        let head_cwd = cwd.clone();
        ctx.thread(ThreadUpsert {
            import_source: ImportSourceKind::Codex,
            import_key: key,
            import_path: path.to_string_lossy().into_owned(),
            fingerprint,
            harness: Harness::Codex,
            title: head
                .first_user
                .as_deref()
                .and_then(title_from)
                .unwrap_or_else(|| "Untitled Codex session".to_string()),
            project_root: cwd.as_deref().map(project_root_for),
            cwd,
            model: head.model,
            session: SessionRef {
                harness: Harness::Codex,
                session_id: head.id.unwrap_or_else(|| id.to_string()),
                path: Some(path.to_string_lossy().into_owned()),
                home: home.attributed.clone(),
                profile: None,
            },
            message_count: None,
            created_at: created,
            updated_at: mtime.max(created),
            automated: is_automated(None, head_cwd.as_deref(), head.first_user.as_deref()),
            parent_key: head.parent.map(|p| format!("codex:{p}")),
        });
    }
    Ok(())
}

/// Machine-started runs: other tools call `codex exec` with a canned system-instructions prompt,
/// usually from a throwaway temp dir (608 such "text analysis" calls on the owner's machine).
fn is_automated(source: Option<&str>, cwd: Option<&str>, first_user: Option<&str>) -> bool {
    let canned = first_user.is_some_and(|m| m.trim_start().starts_with("[System instructions]"));
    let exec = source.is_some_and(|s| s.trim_matches('"') == "exec");
    canned || (exec && cwd.is_some_and(util::is_temp_dir))
}

#[derive(Default)]
struct RolloutHead {
    id: Option<String>,
    cwd: Option<String>,
    created: Option<i64>,
    parent: Option<String>,
    model: Option<String>,
    first_user: Option<String>,
}

/// Reads a rollout until `session_meta`, the first `turn_context` model and the first typed user
/// message are known.
fn rollout_head(path: &Path) -> Result<RolloutHead> {
    let mut head = RolloutHead::default();
    let mut saw_meta = false;
    for_each_jsonl(
        path,
        |v| {
            let payload = v.get("payload").unwrap_or(&Value::Null);
            match v.get("type").and_then(Value::as_str) {
                Some("session_meta") if !saw_meta => {
                    saw_meta = true;
                    head.id = payload.get("id").and_then(Value::as_str).map(Into::into);
                    head.cwd = payload.get("cwd").and_then(Value::as_str).map(Into::into);
                    head.created = payload
                        .get("timestamp")
                        .or_else(|| v.get("timestamp"))
                        .and_then(Value::as_str)
                        .and_then(parse_ts);
                    head.parent = payload
                        .get("parent_thread_id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(Into::into)
                        .or_else(|| payload.get("source").and_then(parent_from_source_value));
                }
                Some("turn_context") if head.model.is_none() => {
                    head.model = payload.get("model").and_then(Value::as_str).map(Into::into);
                }
                Some("response_item")
                    if head.first_user.is_none()
                        && payload.get("type").and_then(Value::as_str) == Some("message")
                        && payload.get("role").and_then(Value::as_str) == Some("user") =>
                {
                    let text = content_text(payload.get("content").unwrap_or(&Value::Null));
                    if !text.trim().is_empty() && !is_injected(&text) {
                        head.first_user = Some(text);
                    }
                }
                _ => {}
            }
            if saw_meta && head.first_user.is_some() && head.model.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
        |_| {},
    )?;
    if !saw_meta {
        return Err(anyhow!("no session_meta record"));
    }
    Ok(head)
}

fn is_injected(text: &str) -> bool {
    let t = text.trim_start();
    INJECTED_PREFIXES.iter().any(|p| t.starts_with(p))
}

fn read_transcript(path: &Path) -> Result<Vec<TranscriptItem>> {
    let mut items = Vec::new();
    let mut model: Option<String> = None;
    for_each_record(path, |v| {
        let ts = v.get("timestamp").and_then(Value::as_str).and_then(parse_ts);
        let payload = v.get("payload").unwrap_or(&Value::Null);
        match v.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                if let Some(m) = payload.get("model").and_then(Value::as_str) {
                    model = Some(m.to_string());
                }
            }
            Some("response_item") => push_response_item(&mut items, payload, model.as_deref(), ts),
            Some("compacted") => items.push(TranscriptItem::System { text: "Context compacted".into(), ts }),
            _ => {}
        }
    })?;
    Ok(items)
}

/// Record types the transcript never uses. They are most of a rollout's bytes (`event_msg`
/// repeats every item), so their lines are skipped without JSON parsing.
const SKIPPED_RECORDS: &[&[u8]] = &[
    b"event_msg",
    b"token_usage_record",
    b"world_state",
    b"inter_agent_communication_metadata",
];

/// Like [`for_each_jsonl`] (splits on `\n` only, skips bad lines) but drops [`SKIPPED_RECORDS`]
/// before parsing. The record's own `"type"` precedes `payload`, so the first `"type":"` in a
/// line is the record type; payload types never collide with the skipped names.
fn for_each_record(path: &Path, mut on_value: impl FnMut(Value)) -> Result<()> {
    use std::io::BufRead;
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        if record_type(&buf).is_some_and(|t| SKIPPED_RECORDS.contains(&t)) {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(buf.trim_ascii()) {
            on_value(v);
        }
    }
    Ok(())
}

fn record_type(line: &[u8]) -> Option<&[u8]> {
    const KEY: &[u8] = b"\"type\":\"";
    let head = &line[..line.len().min(256)];
    let start = head.windows(KEY.len()).position(|w| w == KEY)? + KEY.len();
    let len = line[start..].iter().position(|&b| b == b'"')?;
    Some(&line[start..start + len])
}

fn push_response_item(items: &mut Vec<TranscriptItem>, p: &Value, model: Option<&str>, ts: Option<i64>) {
    let str_of = |k: &str| p.get(k).and_then(Value::as_str);
    let call_id = || str_of("call_id").or_else(|| str_of("id")).map(String::from);
    match str_of("type").unwrap_or("") {
        "message" => {
            let text = content_text(p.get("content").unwrap_or(&Value::Null));
            if text.trim().is_empty() {
                return;
            }
            match str_of("role") {
                Some("user") if !is_injected(&text) => items.push(TranscriptItem::User { text, ts }),
                Some("assistant") => items.push(TranscriptItem::Assistant { text, model: model.map(Into::into), ts }),
                _ => {} // developer / system / injected context
            }
        }
        "reasoning" => {
            let text = ["summary", "content"]
                .iter()
                .filter_map(|k| p.get(*k).and_then(Value::as_array))
                .flatten()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !text.trim().is_empty() {
                items.push(TranscriptItem::Thinking { text, ts });
            }
        }
        "function_call" => {
            let input = match str_of("arguments") {
                Some(s) => serde_json::from_str::<Value>(s)
                    .map(|v| display_json(&v))
                    .unwrap_or_else(|_| truncate(s, util::MAX_TOOL_TEXT)),
                None => p.get("arguments").map(display_json).unwrap_or_default(),
            };
            items.push(TranscriptItem::ToolCall {
                call_id: call_id(),
                name: str_of("name").unwrap_or("tool").into(),
                input,
                ts,
            });
        }
        "custom_tool_call" => items.push(TranscriptItem::ToolCall {
            call_id: call_id(),
            name: str_of("name").unwrap_or("tool").into(),
            input: p.get("input").map(display_json).unwrap_or_default(),
            ts,
        }),
        "local_shell_call" => items.push(TranscriptItem::ToolCall {
            call_id: call_id(),
            name: "shell".into(),
            input: p.get("action").map(display_json).unwrap_or_default(),
            ts,
        }),
        "function_call_output" | "custom_tool_call_output" => {
            let (output, is_error) = tool_output(p.get("output").unwrap_or(&Value::Null));
            items.push(TranscriptItem::ToolResult {
                call_id: str_of("call_id").map(Into::into),
                output: truncate(&output, util::MAX_TOOL_TEXT),
                is_error,
                ts,
            });
        }
        "agent_message" => {
            // Inter-agent message (multi-agent mode); encrypted parts are skipped.
            let text = content_text(p.get("content").unwrap_or(&Value::Null));
            if !text.trim().is_empty() {
                let who = |k: &str| p.get(k).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()));
                let header = match (who("author"), who("recipient")) {
                    (Some(a), Some(r)) => format!("{a} → {r}: "),
                    (Some(a), None) => format!("{a}: "),
                    _ => String::new(),
                };
                items.push(TranscriptItem::System { text: format!("{header}{text}"), ts });
            }
        }
        _ => {}
    }
}

/// Tool output: a string (sometimes JSON `{output, metadata:{exit_code}}`), a list of
/// `input_text` blocks, or `{content, success}`.
fn tool_output(out: &Value) -> (String, bool) {
    match out {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(o)) if o.get("output").is_some_and(Value::is_string) => {
                let code = o.get("metadata").and_then(|m| m.get("exit_code")).and_then(Value::as_i64);
                (o["output"].as_str().unwrap_or_default().to_string(), code.is_some_and(|c| c != 0))
            }
            _ => (s.clone(), false),
        },
        Value::Array(_) => (content_text(out), false),
        Value::Object(o) => {
            let failed = o.get("success").and_then(Value::as_bool) == Some(false);
            let text = o.get("content").map(|c| match c {
                Value::String(s) => s.clone(),
                other => content_text(other),
            });
            (text.unwrap_or_else(|| display_json(out)), failed)
        }
        Value::Null => (String::new(), false),
        other => (other.to_string(), false),
    }
}

fn default_agent(config: &Path) -> AgentUpsert {
    let (model, effort) = fs::read_to_string(config).map(|s| parse_config(&s)).unwrap_or_default();
    AgentUpsert {
        import_source: ImportSourceKind::Codex,
        import_key: "codex:default".into(),
        import_path: config.to_string_lossy().into_owned(),
        name: "codex (default)".into(),
        harness: Harness::Codex,
        model,
        effort,
        role: None,
        instructions: None,
        cwd: None,
        autonomy: emp_protocol::FULL_ACCESS,
        harness_profile: None,
    }
}

/// Top-level `model` and `model_reasoning_effort` from `config.toml` (lines before the first
/// `[section]` only; nothing else is looked at).
fn parse_config(toml: &str) -> (Option<String>, Option<String>) {
    let (mut model, mut effort) = (None, None);
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let slot = match k.trim() {
            "model" => &mut model,
            "model_reasoning_effort" => &mut effort,
            _ => continue,
        };
        *slot = toml_string(v.trim());
    }
    (model, effort)
}

/// A basic or literal TOML string value, ignoring a trailing comment.
fn toml_string(v: &str) -> Option<String> {
    let quote = v.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &v[1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automated_runs_are_flagged() {
        assert!(is_automated(Some("exec"), Some("/work/x"), Some("[System instructions] You are running a text analysis call")));
        assert!(is_automated(Some("\"exec\""), Some("/private/var/folders/lf/x/T/run"), Some("analyze this")));
        assert!(!is_automated(Some("exec"), Some("/Users/me/repo"), Some("fix the bug")));
        assert!(!is_automated(Some("cli"), Some("/private/tmp/scratch"), Some("hello")));
        assert!(!is_automated(None, None, None));
    }

    const SCHEMA: &str = "
        CREATE TABLE threads (
            id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL, source TEXT NOT NULL, model_provider TEXT NOT NULL,
            cwd TEXT NOT NULL, title TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0,
            first_user_message TEXT NOT NULL DEFAULT '', agent_nickname TEXT, model TEXT,
            created_at_ms INTEGER, updated_at_ms INTEGER, name TEXT);
        CREATE TABLE thread_spawn_edges (
            parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL PRIMARY KEY, status TEXT NOT NULL);";

    const P: &str = "11111111-1111-1111-1111-111111111111";
    const C: &str = "22222222-2222-2222-2222-222222222222";
    const E: &str = "33333333-3333-3333-3333-333333333333";
    const U: &str = "44444444-4444-4444-4444-444444444444";
    const O: &str = "55555555-5555-5555-5555-555555555555";

    fn rollout_lines(id: &str, parent: Option<&str>) -> String {
        let meta = match parent {
            Some(p) => serde_json::json!({"id": id, "cwd": "/work/emp-codex-nowhere", "timestamp": "2026-10-14T10:00:00.000Z",
                "source": {"subagent": {"thread_spawn": {"parent_thread_id": p, "depth": 1}}}, "parent_thread_id": p}),
            None => serde_json::json!({"id": id, "cwd": "/work/emp-codex-nowhere", "timestamp": "2026-10-14T10:00:00.000Z", "source": "cli"}),
        };
        let lines = [
            serde_json::json!({"timestamp":"2026-10-14T10:00:00.000Z","type":"session_meta","payload":meta}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:00.100Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>x"}]}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:00.200Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /x"}]}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:00.300Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/x</cwd>"}]}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:01.000Z","type":"turn_context","payload":{"model":"gpt-test","cwd":"/x"}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Refactor   the parser\nplease"}]}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:02.000Z","type":"event_msg","payload":{"type":"user_message","message":"Refactor the parser"}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:03.000Z","type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"zzz"}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:03.500Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"plan it"}]}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:04.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"cmd\":[\"ls\"]}","call_id":"c1"}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:05.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"{\"output\":\"boom\",\"metadata\":{\"exit_code\":2}}"}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:06.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch","call_id":"c2"}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:07.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c2","output":[{"type":"input_text","text":"applied"}]}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:08.000Z","type":"compacted","payload":{"message":""}}),
            serde_json::json!({"timestamp":"2026-10-14T10:00:09.000Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Done."}]}}),
        ];
        let mut s: String = lines.iter().map(|l| format!("{l}\n")).collect();
        s.push_str("not json\n");
        s
    }

    fn write_rollout(home: &Path, id: &str, parent: Option<&str>) -> PathBuf {
        let dir = home.join("sessions/2026/10/14");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-10-14T10-00-00-{id}.jsonl"));
        fs::write(&path, rollout_lines(id, parent)).unwrap();
        path
    }

    /// Default home with a `state_5.sqlite`; Orion home with rollouts only (fallback walk).
    fn fixture() -> (tempfile::TempDir, CodexImporter) {
        let tmp = tempfile::tempdir().unwrap();
        let default_home = tmp.path().join(".codex");
        let orion_home = tmp.path().join("orion/codex-home");
        fs::create_dir_all(&default_home).unwrap();
        fs::create_dir_all(&orion_home).unwrap();
        fs::write(
            default_home.join("config.toml"),
            "model = \"gpt-6.1-sol\" # main\nmodel_reasoning_effort = 'high'\n\n[projects.\"/x\"]\nmodel = \"nope\"\n",
        )
        .unwrap();
        fs::write(default_home.join("auth.json"), "{\"secret\":1}").unwrap();

        let parent_rollout = write_rollout(&default_home, P, None);
        write_rollout(&default_home, C, Some(P));
        let conn = Connection::open(default_home.join("state_5.sqlite")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        // (id, source, title, first_user_message, cwd, name)
        type Row<'a> = (&'a str, &'a str, &'a str, &'a str, &'a str, Option<&'a str>);
        let rows: &[Row] = &[
            (P, "cli", "Codex title", "first msg", "/work/emp-codex-nowhere/a", Some("Named thread")),
            (C, r#"{"subagent":{"thread_spawn":{"parent_thread_id":"11111111-1111-1111-1111-111111111111","depth":1}}}"#, "", "child first msg", "", None),
            (E, "exec", "", "", "", None),
            (U, "exec", "Only a title", "", "/work/emp-codex-nowhere/b", None),
        ];
        for (id, source, title, first, cwd, name) in rows {
            let rollout = if *id == P {
                parent_rollout.to_string_lossy().into_owned()
            } else {
                default_home.join(format!("sessions/2026/10/14/rollout-2026-10-14T10-00-00-{id}.jsonl")).to_string_lossy().into_owned()
            };
            conn.execute(
                "INSERT INTO threads (id, rollout_path, created_at, updated_at, source, model_provider, cwd, title, first_user_message, model, created_at_ms, updated_at_ms, name)
                 VALUES (?1, ?2, 100, 200, ?3, 'openai', ?4, ?5, ?6, 'gpt-x', 100500, 200500, ?7)",
                rusqlite::params![id, rollout, source, cwd, title, first, name],
            )
            .unwrap();
        }
        conn.execute("INSERT INTO thread_spawn_edges VALUES (?1, ?2, 'open')", rusqlite::params![P, U]).unwrap();
        drop(conn);

        write_rollout(&orion_home, O, None);
        (tmp, CodexImporter::with_homes(default_home, Some(orion_home)))
    }

    fn scan(importer: &CodexImporter, known: HashMap<String, String>) -> ScanCtx {
        let mut ctx = ScanCtx::new(known);
        importer.scan(&mut ctx).unwrap();
        ctx
    }

    #[test]
    fn scans_threads_titles_parents_and_homes() {
        let (_tmp, importer) = fixture();
        let ctx = scan(&importer, HashMap::new());
        assert_eq!(ctx.stats.skipped, 0, "{:?}", ctx.stats.warnings);
        let by_key: HashMap<_, _> = ctx.threads.iter().map(|t| (t.import_key.clone(), t)).collect();
        assert_eq!(by_key.len(), 5);

        let p = &by_key[&format!("codex:{P}")];
        assert_eq!(p.title, "Named thread");
        assert_eq!((p.created_at, p.updated_at), (100_500, 200_500));
        assert_eq!(p.model.as_deref(), Some("gpt-x"));
        assert_eq!(p.project_root.as_deref(), Some("/work/emp-codex-nowhere/a"));
        assert_eq!(p.session.home, None, "default home is not attributed");
        assert!(p.session.path.as_deref().unwrap().ends_with(&format!("{P}.jsonl")));
        assert!(p.fingerprint.ends_with(":200500"));

        let c = &by_key[&format!("codex:{C}")];
        assert_eq!(c.title, "child first msg");
        assert_eq!(c.parent_key.as_deref(), Some(format!("codex:{P}").as_str()));
        assert_eq!((c.cwd.as_deref(), c.project_root.as_deref()), (None, None));

        let e = &by_key[&format!("codex:{E}")];
        assert_eq!(e.title, "Untitled Codex session");
        assert_eq!(e.fingerprint, "db:200500", "missing rollout falls back to updated_at");
        assert_eq!(by_key[&format!("codex:{U}")].title, "Only a title");
        assert_eq!(by_key[&format!("codex:{U}")].parent_key.as_deref(), Some(format!("codex:{P}").as_str()), "spawn edge");

        // Orion home has no db: rollout walk, attributed home.
        let o = &by_key[&format!("codex:{O}")];
        assert_eq!(o.title, "Refactor the parser");
        assert_eq!(o.model.as_deref(), Some("gpt-test"));
        assert_eq!(o.created_at, parse_ts("2026-10-14T10:00:00.000Z").unwrap());
        assert_eq!(o.session.home.as_deref(), Some(importer.homes[1].path.to_str().unwrap()));
        assert_eq!(o.session.session_id, O);

        assert_eq!(ctx.agents.len(), 1);
        let a = &ctx.agents[0];
        assert_eq!((a.import_key.as_str(), a.name.as_str()), ("codex:default", "codex (default)"));
        assert_eq!((a.model.as_deref(), a.effort.as_deref()), (Some("gpt-6.1-sol"), Some("high")));
    }

    #[test]
    fn second_scan_is_all_unchanged() {
        let (_tmp, importer) = fixture();
        let first = scan(&importer, HashMap::new());
        let known = first.threads.iter().map(|t| (t.import_key.clone(), t.fingerprint.clone())).collect();
        let second = scan(&importer, known);
        assert!(second.threads.is_empty());
        assert_eq!(second.stats.threads_unchanged, 5);
        assert_eq!(second.stats.threads_seen, 5);
    }

    #[test]
    fn walk_reads_parent_from_session_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("h");
        write_rollout(&home, C, Some(P));
        let ctx = scan(&CodexImporter::with_homes(&home, None), HashMap::new());
        assert_eq!(ctx.threads.len(), 1);
        assert_eq!(ctx.threads[0].parent_key.as_deref(), Some(format!("codex:{P}").as_str()));
    }

    #[test]
    fn transcript_maps_response_items_and_drops_injected_context() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_rollout(tmp.path(), P, None);
        let items = read_transcript(&path).unwrap();
        let kinds: Vec<String> = items
            .iter()
            .map(|i| match i {
                TranscriptItem::User { text, .. } => format!("user:{}", text.lines().next().unwrap()),
                TranscriptItem::Assistant { text, model, .. } => format!("assistant:{text}:{}", model.as_deref().unwrap_or("")),
                TranscriptItem::Thinking { text, .. } => format!("thinking:{text}"),
                TranscriptItem::ToolCall { call_id, name, input, .. } => {
                    format!("call:{}:{name}:{}", call_id.as_deref().unwrap_or(""), input.lines().next().unwrap_or(""))
                }
                TranscriptItem::ToolResult { call_id, output, is_error, .. } => {
                    format!("result:{}:{output}:{is_error}", call_id.as_deref().unwrap_or(""))
                }
                TranscriptItem::System { text, .. } => format!("system:{text}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "user:Refactor   the parser",
                "thinking:plan it",
                "call:c1:shell:{",
                "result:c1:boom:true",
                "call:c2:apply_patch:*** Begin Patch",
                "result:c2:applied:false",
                "system:Context compacted",
                "assistant:Done.:gpt-test",
            ]
        );
        assert!(matches!(items[0], TranscriptItem::User { ts: Some(ts), .. } if ts == parse_ts("2026-10-14T10:00:02.000Z").unwrap()));
    }

    #[test]
    fn config_parser_reads_top_level_only() {
        assert_eq!(
            parse_config("# c\nmodel=\"a/b\"\n[profiles.x]\nmodel_reasoning_effort = \"low\"\n"),
            (Some("a/b".into()), None)
        );
        assert_eq!(parse_config("model = bare\n"), (None, None));
    }

    #[test]
    fn opens_closed_wal_db_without_creating_files() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state_5.sqlite");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t (x); INSERT INTO t VALUES (1);").unwrap();
        }
        let conn = open_db(&db).unwrap();
        let n: i64 = conn.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1, "no -wal/-shm while open");
        drop(conn);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
}
