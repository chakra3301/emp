//! Hermes Agent importer (verified against Hermes 0.21.6).
//!
//! Layout: `~/.hermes` is the profile `default`; every directory in `~/.hermes/profiles/` is a
//! named profile. Each profile has `config.yaml`, optional `profile.yaml` + `SOUL.md`, and a
//! `state.db` (SQLite, WAL mode, written live by the gateway).
//!
//! Schema facts (`state.db`):
//! - `sessions.started_at / ended_at / last_activity_at` and `messages.timestamp` are `REAL`
//!   epoch seconds (Python `time.time()`).
//! - Subagent sessions carry `parent_session_id`; `hidden = 1` sessions are not listed.
//! - Message visibility mirrors Hermes's own display projection
//!   (`hermes_state_messages._display_rows_from_conn`): rows with `active = 1 OR compacted = 1`
//!   (live rows plus compaction-archived history; rewind/undo rows are `active = 0, compacted = 0`
//!   and are dropped), minus `display_metadata.model_only` rows, one row per `display_order`
//!   group preferring the active, newest row. `display_kind = 'hidden'` rows are model-facing
//!   scaffolding and are skipped. Compression lineages spanning several sessions are not merged;
//!   each session shows its own rows.
//! - `content` may carry the prefix `\0json:` followed by JSON (structured content blocks).
//!
//! Databases are opened with [`util::open_db`], which never touches files next to them.
//!
//! Never read: `auth.json`, `*.lock`, tokens, pairing data. From `config.yaml` only
//! `model.default`, `model.provider`, `agent.reasoning_effort` and `terminal.cwd` are used.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use emp_core::{AgentUpsert, ThreadUpsert};
use emp_protocol::{Harness, ImportSourceKind, SessionRef, Thread, TranscriptItem};
use rusqlite::{Connection, OptionalExtension, params};

use crate::util::{
    self, col, columns, content_text, open_db, display_json, expand_home, project_root_for, secs_to_ms, title_from, truncate,
};
use crate::{Importer, ScanCtx};

const DEFAULT_PROFILE: &str = "default";
const MAX_INSTRUCTIONS: usize = 20_000;
const CONTENT_JSON_PREFIX: &str = "\u{0}json:";

/// Hermes closes a failed turn with a display-only boundary, not a model reply.
pub fn is_failed_turn_notice(text: &str) -> bool {
    matches!(
        text.trim(),
        "Your request was not processed. Send it again if you still want me to carry it out."
            | "This turn did not complete. Some actions may already have run; verify their effects before resending."
    )
}

fn failed_turn_error(notice: &str, metadata: Option<&str>) -> String {
    // Hermes force-redacts this display metadata before storing it. Never read request dumps
    // (which include headers/credentials) to explain a failed turn.
    let meta = metadata
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .unwrap_or_default();
    let error = meta["error"].as_str().filter(|s| !s.trim().is_empty());
    let surface = &meta["error_surface"];
    let route = [
        surface["provider_label"]
            .as_str()
            .or(surface["provider"].as_str()),
        surface["model"].as_str(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    match error {
        Some(error) => format!(
            "Hermes request failed{}: {}\n{notice}",
            if route.is_empty() {
                String::new()
            } else {
                format!(" ({route})")
            },
            truncate(error, 2000)
        ),
        None => format!("Hermes request failed. {notice}"),
    }
}

/// Detail for the boundary just streamed by ACP. Older Hermes versions return `end_turn`
/// even on provider failure, so the live driver reads the same redacted display row as import.
pub fn latest_turn_failure(profile: &str, session_id: &str, started_at: i64) -> Result<Option<String>> {
    let conn = open_db(&profile_dir(profile).join("state.db"))?;
    read_latest_turn_failure(&conn, session_id, started_at)
}

fn profile_dir(profile: &str) -> PathBuf {
    let root = emp_core::paths::home_dir().join(".hermes");
    if profile.is_empty() || profile == DEFAULT_PROFILE {
        root
    } else {
        root.join("profiles").join(profile)
    }
}

/// A provider can advertise a model its inference endpoint rejects. Exclude recent explicit
/// model rejections from the picker even when that stale catalogue continues to list them.
pub fn rejected_models(profile: &str, since: i64) -> Result<HashSet<String>> {
    let conn = open_db(&profile_dir(profile).join("state.db"))?;
    read_rejected_models(&conn, since)
}

fn read_rejected_models(conn: &Connection, since: i64) -> Result<HashSet<String>> {
    let cols = columns(conn, "messages")?;
    if !["display_kind", "display_metadata", "timestamp"].iter().all(|c| cols.contains(*c)) {
        return Ok(HashSet::new());
    }
    let mut stmt = conn.prepare(
        "SELECT display_metadata FROM messages WHERE display_kind = 'failed_turn' AND timestamp >= ?1 ORDER BY id DESC LIMIT 100",
    )?;
    let rows = stmt.query_map([since as f64 / 1000.0], |r| r.get::<_, Option<String>>(0))?;
    let mut rejected = HashSet::new();
    for row in rows {
        let Some(meta) = row?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else { continue };
        let error = meta["error"].as_str().unwrap_or_default().to_ascii_lowercase();
        if !error.contains("model is not supported") && !error.contains("model_not_supported") {
            continue;
        }
        if let (Some(provider), Some(model)) = (meta["error_surface"]["provider"].as_str(), meta["error_surface"]["model"].as_str()) {
            rejected.insert(format!("{provider}:{model}"));
        }
    }
    Ok(rejected)
}

fn read_latest_turn_failure(conn: &Connection, session_id: &str, started_at: i64) -> Result<Option<String>> {
    let cols = columns(conn, "messages")?;
    let active = if cols.contains("active") {
        " AND m.active = 1"
    } else {
        ""
    };
    let sql = format!(
        "SELECT m.role, m.content, {kind}, {metadata}, {timestamp} FROM messages m WHERE m.session_id = ?1{active} ORDER BY m.id DESC LIMIT 1",
        kind = mcol(&cols, "display_kind"),
        metadata = mcol(&cols, "display_metadata"),
        timestamp = mcol(&cols, "timestamp"),
    );
    let row = conn
        .query_row(&sql, [session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<f64>>(4)?,
            ))
        })
        .optional()?;
    Ok(row.and_then(|(role, content, kind, metadata, timestamp)| {
        let content = decode_content(&content.unwrap_or_default());
        (role == "assistant"
            && timestamp.is_some_and(|ts| secs_to_ms(ts) >= started_at)
            && (kind.as_deref() == Some("failed_turn") || is_failed_turn_notice(&content)))
        .then(|| failed_turn_error(&content, metadata.as_deref()))
    }))
}

pub struct HermesImporter {
    root: PathBuf,
}

impl Default for HermesImporter {
    fn default() -> Self {
        Self { root: emp_core::paths::home_dir().join(".hermes") }
    }
}

impl HermesImporter {
    /// Importer over a Hermes root other than `~/.hermes` (tests).
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `(profile name, profile dir)`: the root first, then `profiles/*` sorted by name.
    fn profiles(&self) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        if self.root.is_dir() {
            out.push((DEFAULT_PROFILE.to_string(), self.root.clone()));
        }
        if let Ok(entries) = fs::read_dir(self.root.join("profiles")) {
            let mut named: Vec<(String, PathBuf)> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
                .filter(|(name, _)| !name.starts_with('.'))
                .collect();
            named.sort();
            out.extend(named);
        }
        out
    }
}

impl Importer for HermesImporter {
    fn kind(&self) -> ImportSourceKind {
        ImportSourceKind::Hermes
    }

    fn paths(&self) -> Vec<PathBuf> {
        vec![self.root.clone(), self.root.join("profiles")]
    }

    fn scan(&self, ctx: &mut ScanCtx) -> Result<()> {
        let mut dbs = 0usize;
        let mut failed = 0usize;
        let mut last_err = None;
        for (name, dir) in self.profiles() {
            ctx.progress(&format!("hermes profile {name}"));
            ctx.agent(profile_agent(&name, &dir));
            let db = dir.join("state.db");
            if !db.exists() {
                continue;
            }
            dbs += 1;
            if let Err(e) = scan_profile(ctx, &name, &dir, &db) {
                ctx.warn(format!("hermes profile {name}: {e:#}"));
                failed += 1;
                last_err = Some(e);
            }
        }
        match last_err {
            // Every database failed: the source as a whole is unreadable.
            Some(e) if failed == dbs => Err(e),
            _ => Ok(()),
        }
    }

    fn transcript(&self, thread: &Thread) -> Result<Vec<TranscriptItem>> {
        let dir = match &thread.session.home {
            Some(home) => PathBuf::from(home),
            None => self.root.clone(),
        };
        let conn = open_db(&dir.join("state.db"))?;
        read_transcript(&conn, &thread.session.session_id, thread.model.as_deref())
    }
}

fn scan_profile(ctx: &mut ScanCtx, profile: &str, dir: &Path, db: &Path) -> Result<()> {
    let conn = open_db(db)?;
    let cols = columns(&conn, "sessions")?;
    if cols.is_empty() {
        return Err(anyhow!("no sessions table"));
    }
    // ACP sessions often leave `last_activity_at` empty; the newest message says when the chat
    // last changed (EMP caches transcripts by that time).
    let last_message = if columns(&conn, "messages")?.contains("timestamp") {
        "(SELECT MAX(m.timestamp) FROM messages m WHERE m.session_id = sessions.id)"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT id, {title}, {model}, {parent}, {cwd}, {started}, {ended}, {count}, {activity}, {hidden}, {last_message}
         FROM sessions ORDER BY {started}",
        title = col(&cols, "title"),
        model = col(&cols, "model"),
        parent = col(&cols, "parent_session_id"),
        cwd = col(&cols, "cwd"),
        started = col(&cols, "started_at"),
        ended = col(&cols, "ended_at"),
        count = col(&cols, "message_count"),
        activity = col(&cols, "last_activity_at"),
        hidden = col(&cols, "hidden"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(SessionRow {
            id: r.get(0)?,
            title: r.get(1)?,
            model: r.get(2)?,
            parent: r.get(3)?,
            cwd: r.get(4)?,
            started_at: r.get(5)?,
            ended_at: r.get(6)?,
            message_count: r.get(7)?,
            last_activity_at: r.get(8)?,
            hidden: r.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
            last_message_at: r.get(10)?,
        })
    })?;
    let db_path = db.to_string_lossy().into_owned();
    for row in rows {
        let s = match row {
            Ok(s) => s,
            Err(e) => {
                ctx.skip(format!("hermes {profile}: bad session row: {e}"));
                continue;
            }
        };
        if s.hidden {
            continue;
        }
        let key = format!("hermes:{profile}:{}", s.id);
        let last = [s.last_activity_at, s.last_message_at, s.ended_at, s.started_at].into_iter().flatten().fold(0.0, f64::max);
        let fingerprint = format!(
            "{}:{}:{}",
            s.message_count.unwrap_or(0),
            secs_to_ms(last),
            s.title.as_deref().unwrap_or("")
        );
        if ctx.is_unchanged(&key, &fingerprint) {
            continue;
        }
        let title = s
            .title
            .as_deref()
            .and_then(title_from)
            .or_else(|| first_user_text(&conn, &s.id).as_deref().and_then(title_from))
            .unwrap_or_else(|| "Untitled Hermes session".to_string());
        let created = s.started_at.map(secs_to_ms).unwrap_or(0);
        let updated = [s.last_activity_at, s.last_message_at, s.ended_at, s.started_at]
            .into_iter()
            .flatten()
            .map(secs_to_ms)
            .max()
            .unwrap_or(created);
        let cwd = s.cwd.filter(|c| !c.trim().is_empty());
        ctx.thread(ThreadUpsert {
            import_source: ImportSourceKind::Hermes,
            import_key: key,
            import_path: db_path.clone(),
            fingerprint,
            harness: Harness::Hermes,
            title,
            project_root: cwd.as_deref().map(project_root_for),
            cwd,
            model: s.model.filter(|m| !m.is_empty()),
            session: SessionRef {
                harness: Harness::Hermes,
                session_id: s.id.clone(),
                path: None,
                home: Some(dir.to_string_lossy().into_owned()),
                profile: Some(profile.to_string()),
            },
            message_count: s.message_count,
            created_at: created,
            updated_at: updated,
            parent_key: s.parent.filter(|p| !p.is_empty()).map(|p| format!("hermes:{profile}:{p}")),
            automated: false,
        });
    }
    Ok(())
}

struct SessionRow {
    id: String,
    title: Option<String>,
    model: Option<String>,
    parent: Option<String>,
    cwd: Option<String>,
    started_at: Option<f64>,
    ended_at: Option<f64>,
    message_count: Option<i64>,
    last_activity_at: Option<f64>,
    hidden: bool,
    last_message_at: Option<f64>,
}

/// First human-typed user message (notification rows carry a `display_kind`).
fn first_user_text(conn: &Connection, session_id: &str) -> Option<String> {
    let cols = columns(conn, "messages").ok()?;
    let mut sql = "SELECT content FROM messages WHERE session_id = ?1 AND role = 'user'".to_string();
    if cols.contains("display_kind") {
        sql.push_str(" AND display_kind IS NULL");
    }
    if cols.contains("active") && cols.contains("compacted") {
        sql.push_str(" AND (active = 1 OR compacted = 1)");
    }
    sql.push_str(" ORDER BY id LIMIT 1");
    let content: Option<String> =
        conn.query_row(&sql, params![session_id], |r| r.get(0)).optional().ok().flatten().flatten();
    content.map(|c| decode_content(&c)).filter(|t| !t.trim().is_empty())
}

/// Reverses Hermes's `_encode_content`: `\0json:<json>` → text of the decoded blocks.
fn decode_content(raw: &str) -> String {
    match raw.strip_prefix(CONTENT_JSON_PREFIX) {
        Some(json) => match serde_json::from_str::<serde_json::Value>(json) {
            Ok(v) => content_text(&v),
            Err(_) => raw.to_string(),
        },
        None => raw.to_string(),
    }
}

fn read_transcript(conn: &Connection, session_id: &str, model: Option<&str>) -> Result<Vec<TranscriptItem>> {
    let cols = columns(conn, "messages")?;
    if cols.is_empty() {
        return Err(anyhow!("no messages table"));
    }
    let projected = ["active", "compacted", "display_order"].iter().all(|c| cols.contains(*c));
    let model_only = |alias: &str| {
        if cols.contains("display_metadata") {
            format!(
                " AND COALESCE(json_extract(CASE WHEN json_valid({alias}.display_metadata) \
                 THEN {alias}.display_metadata ELSE '{{}}' END, '$.model_only'), 0) = 0"
            )
        } else {
            String::new()
        }
    };
    let mut filter = String::new();
    if projected {
        filter.push_str(" AND (m.active = 1 OR m.compacted = 1)");
        filter.push_str(&model_only("m"));
        // One row per display_order group: the active, newest one (legacy NULL orders kept as is).
        filter.push_str(&format!(
            " AND (m.display_order IS NULL OR m.id = (SELECT c.id FROM messages c \
             WHERE c.session_id = m.session_id AND c.display_order = m.display_order \
             AND (c.active = 1 OR c.compacted = 1){} ORDER BY c.active DESC, c.id DESC LIMIT 1))",
            model_only("c")
        ));
    } else if cols.contains("active") {
        filter.push_str(" AND m.active = 1");
    }
    if cols.contains("display_kind") {
        filter.push_str(" AND COALESCE(m.display_kind, '') <> 'hidden'");
    }
    let order = if projected { "COALESCE(m.display_order, m.id), m.id" } else { "m.id" };
    let sql = format!(
        "SELECT m.role, m.content, {tool_calls}, {tool_call_id}, {reasoning}, {reasoning_content}, \
         {timestamp}, {display_kind}, {display_metadata} FROM messages m WHERE m.session_id = ?1{filter} ORDER BY {order}",
        tool_calls = mcol(&cols, "tool_calls"),
        tool_call_id = mcol(&cols, "tool_call_id"),
        reasoning = mcol(&cols, "reasoning"),
        reasoning_content = mcol(&cols, "reasoning_content"),
        timestamp = mcol(&cols, "timestamp"),
        display_kind = mcol(&cols, "display_kind"),
        display_metadata = mcol(&cols, "display_metadata"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![session_id])?;
    let mut items = Vec::new();
    while let Some(r) = rows.next()? {
        let role: String = r.get(0)?;
        let content = r.get::<_, Option<String>>(1)?.map(|c| decode_content(&c)).unwrap_or_default();
        let tool_calls: Option<String> = r.get(2)?;
        let tool_call_id: Option<String> = r.get(3)?;
        let reasoning: Option<String> = r.get(4)?;
        let reasoning_content: Option<String> = r.get(5)?;
        let ts = r.get::<_, Option<f64>>(6)?.map(secs_to_ms);
        let display_kind: Option<String> = r.get(7)?;
        match role.as_str() {
            "user" => {
                if content.trim().is_empty() {
                    continue;
                }
                // Notifications injected as user turns (subagent done, background process done).
                if matches!(display_kind.as_deref(), Some("async_delegation_complete" | "process_complete")) {
                    items.push(TranscriptItem::System { text: content, ts });
                } else {
                    items.push(TranscriptItem::User { text: content, ts });
                }
            }
            "assistant" => {
                if display_kind.as_deref() == Some("failed_turn") || is_failed_turn_notice(&content)
                {
                    let metadata: Option<String> = r.get(8)?;
                    items.push(TranscriptItem::System {
                        text: failed_turn_error(&content, metadata.as_deref()),
                        ts,
                    });
                    continue;
                }
                let thinking = reasoning.filter(|t| !t.trim().is_empty()).or(reasoning_content);
                if let Some(text) = thinking.filter(|t| !t.trim().is_empty()) {
                    items.push(TranscriptItem::Thinking { text, ts });
                }
                if !content.trim().is_empty() {
                    items.push(TranscriptItem::Assistant { text: content, model: model.map(Into::into), ts });
                }
                if let Some(calls) = tool_calls.as_deref() {
                    push_tool_calls(&mut items, calls, ts);
                }
            }
            "tool" => items.push(TranscriptItem::ToolResult {
                call_id: tool_call_id,
                output: truncate(&content, util::MAX_TOOL_TEXT),
                is_error: false,
                ts,
            }),
            "system" if !content.trim().is_empty() => items.push(TranscriptItem::System { text: content, ts }),
            _ => {}
        }
    }
    Ok(items)
}

fn mcol(cols: &HashSet<String>, name: &str) -> String {
    if cols.contains(name) { format!("m.{name}") } else { "NULL".into() }
}

/// OpenAI-style `[{id, call_id?, type, function: {name, arguments}}]`.
fn push_tool_calls(items: &mut Vec<TranscriptItem>, raw: &str, ts: Option<i64>) {
    let Ok(serde_json::Value::Array(calls)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    for call in calls {
        let call_id = ["id", "call_id"]
            .iter()
            .find_map(|k| call.get(*k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
            .map(Into::into);
        let func = call.get("function");
        let name = func
            .and_then(|f| f.get("name"))
            .or_else(|| call.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("tool")
            .to_string();
        let args = func.and_then(|f| f.get("arguments")).or_else(|| call.get("arguments"));
        let input = match args {
            Some(serde_json::Value::String(s)) => match serde_json::from_str::<serde_json::Value>(s) {
                Ok(v) => display_json(&v),
                Err(_) => truncate(s, util::MAX_TOOL_TEXT),
            },
            Some(v) => display_json(v),
            None => String::new(),
        };
        items.push(TranscriptItem::ToolCall { call_id, name, input, ts });
    }
}

fn profile_agent(name: &str, dir: &Path) -> AgentUpsert {
    let config = fs::read_to_string(dir.join("config.yaml"))
        .ok()
        .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok());
    let get = |path: &[&str]| -> Option<String> {
        let mut v = config.as_ref()?;
        for p in path {
            v = v.get(*p)?;
        }
        v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(Into::into)
    };
    let model = get(&["model", "default"]).map(|m| match get(&["model", "provider"]) {
        Some(provider) if !m.contains('/') => format!("{provider}/{m}"),
        _ => m,
    });
    let role = fs::read_to_string(dir.join("profile.yaml"))
        .ok()
        .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok())
        .and_then(|v| v.get("description")?.as_str().map(str::trim).map(String::from))
        .filter(|s| !s.is_empty());
    let instructions = fs::read_to_string(dir.join("SOUL.md"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| truncate(&s, MAX_INSTRUCTIONS));
    AgentUpsert {
        import_source: ImportSourceKind::Hermes,
        import_key: format!("hermes:profile:{name}"),
        import_path: dir.to_string_lossy().into_owned(),
        name: if name == DEFAULT_PROFILE { "hermes (default)".into() } else { name.into() },
        harness: Harness::Hermes,
        model,
        effort: get(&["agent", "reasoning_effort"]),
        role,
        instructions,
        cwd: get(&["terminal", "cwd"]).map(|c| expand_home(&c).to_string_lossy().into_owned()),
        autonomy: emp_protocol::FULL_ACCESS,
        harness_profile: Some(name.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SCHEMA: &str = "
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY, source TEXT NOT NULL, model TEXT, parent_session_id TEXT,
            started_at REAL NOT NULL, ended_at REAL, message_count INTEGER DEFAULT 0, cwd TEXT,
            title TEXT, last_activity_at REAL, profile_name TEXT,
            archived INTEGER NOT NULL DEFAULT 0, hidden INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, role TEXT NOT NULL,
            content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT, timestamp REAL NOT NULL,
            reasoning TEXT, reasoning_content TEXT, active INTEGER NOT NULL DEFAULT 1,
            compacted INTEGER NOT NULL DEFAULT 0, display_kind TEXT, display_metadata TEXT,
            display_order INTEGER);";

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".hermes");
        let studio = root.join("profiles/studio");
        fs::create_dir_all(&studio).unwrap();
        fs::write(root.join("config.yaml"), "model:\n  default: claude-x\n  provider: anthropic\n").unwrap();
        fs::write(
            studio.join("config.yaml"),
            "model:\n  default: openai/gpt-y\n  provider: openai\nagent:\n  reasoning_effort: high\nterminal:\n  cwd: ~/Studio\n",
        )
        .unwrap();
        fs::write(studio.join("profile.yaml"), "description: Design studio lead\n").unwrap();
        fs::write(studio.join("SOUL.md"), "You are the studio.").unwrap();
        // Unrelated files that must never be read.
        fs::write(studio.join("auth.json"), "{\"secret\":1}").unwrap();

        let conn = Connection::open(studio.join("state.db")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO sessions (id, source, model, started_at, ended_at, message_count, cwd, title, last_activity_at)
               VALUES ('s1', 'cli', 'claude-x', 1000.5, 1010.0, 6, '/work/emp-hermes-nowhere/proj', 'Fix the build', 1020.25);
             INSERT INTO sessions (id, source, started_at, message_count, title)
               VALUES ('s2', 'oneshot', 2000.0, 1, NULL);
             INSERT INTO sessions (id, source, started_at, message_count)
               VALUES ('s3', 'oneshot', 3000.0, 0);
             INSERT INTO sessions (id, source, started_at, hidden) VALUES ('s4', 'cli', 4000.0, 1);
             INSERT INTO sessions (id, source, parent_session_id, started_at, title)
               VALUES ('s5', 'subagent', 's1', 5000.0, 'child');
             INSERT INTO messages (session_id, role, content, timestamp, display_kind)
               VALUES ('s2', 'user', 'done notification', 2000.1, 'process_complete');
             INSERT INTO messages (session_id, role, content, timestamp)
               VALUES ('s2', 'user', '\n  second  line title\nmore', 2000.2);",
        )
        .unwrap();
        // s1 transcript: compaction archive + live twin share display_order 2; a rewind row; a
        // hidden placeholder; a model_only row; tool call + result; JSON-encoded content.
        // (role, content, tool_calls, tool_call_id, reasoning, active, compacted, display_kind,
        //  display_metadata, display_order)
        type Opt<'a> = Option<&'a str>;
        type Row<'a> = (&'a str, &'a str, Opt<'a>, Opt<'a>, Opt<'a>, i64, i64, Opt<'a>, Opt<'a>, i64);
        let rows: &[Row] = &[
            ("user", "hello", None, None, None, 1, 0, None, None, 1),
            ("assistant", "old answer", None, None, None, 0, 1, None, None, 2),
            ("assistant", "live answer", None, None, Some("let me think"), 1, 0, None, None, 2),
            ("user", "rewound", None, None, None, 0, 0, None, None, 3),
            ("assistant", "", None, None, None, 1, 0, Some("hidden"), None, 4),
            ("user", "model only", None, None, None, 1, 0, None, Some("{\"model_only\":true}"), 5),
            (
                "assistant",
                "",
                Some(r#"[{"id":"call_1","call_id":"call_1","type":"function","function":{"name":"terminal","arguments":"{\"cmd\":\"ls\"}"}}]"#),
                None,
                None,
                1,
                0,
                None,
                None,
                6,
            ),
            ("tool", "file.txt", None, Some("call_1"), None, 1, 0, None, None, 7),
            ("user", "\u{0}json:[{\"type\":\"text\",\"text\":\"blocks\"}]", None, None, None, 1, 0, None, None, 8),
        ];
        for (i, (role, content, calls, call_id, reasoning, active, compacted, kind, meta, order)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, reasoning, active, compacted, display_kind, display_metadata, display_order, timestamp)
                 VALUES ('s1', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![role, content, calls, call_id, reasoning, active, compacted, kind, meta, order, 1000.0 + i as f64],
            )
            .unwrap();
        }
        drop(conn);
        (tmp, root)
    }

    fn scan(root: &Path, known: HashMap<String, String>) -> ScanCtx {
        let mut ctx = ScanCtx::new(known);
        HermesImporter::with_root(root).scan(&mut ctx).unwrap();
        ctx
    }

    #[test]
    fn scans_profiles_sessions_and_agents() {
        let (_tmp, root) = fixture();
        let ctx = scan(&root, HashMap::new());
        let by_key: HashMap<_, _> = ctx.threads.iter().map(|t| (t.import_key.as_str(), t)).collect();
        assert_eq!(ctx.threads.len(), 4, "hidden session skipped");
        assert!(!by_key.contains_key("hermes:studio:s4"));

        let s1 = by_key["hermes:studio:s1"];
        assert_eq!(s1.title, "Fix the build");
        assert_eq!(s1.created_at, 1_000_500);
        assert_eq!(s1.updated_at, 1_020_250);
        assert_eq!(s1.cwd.as_deref(), Some("/work/emp-hermes-nowhere/proj"));
        assert_eq!(s1.project_root.as_deref(), Some("/work/emp-hermes-nowhere/proj"));
        assert_eq!(s1.session.profile.as_deref(), Some("studio"));
        assert_eq!(s1.session.home.as_deref(), Some(root.join("profiles/studio").to_str().unwrap()));
        assert_eq!(s1.message_count, Some(6));

        assert_eq!(by_key["hermes:studio:s2"].title, "second line title", "first human user message");
        assert_eq!(by_key["hermes:studio:s2"].updated_at, 2_000_200, "no last_activity_at: the newest message");
        assert_eq!(by_key["hermes:studio:s3"].title, "Untitled Hermes session");
        assert_eq!(by_key["hermes:studio:s3"].project_root, None);
        assert_eq!(by_key["hermes:studio:s5"].parent_key.as_deref(), Some("hermes:studio:s1"));

        assert_eq!(ctx.agents.len(), 2);
        let default = &ctx.agents[0];
        assert_eq!((default.name.as_str(), default.import_key.as_str()), ("hermes (default)", "hermes:profile:default"));
        assert_eq!(default.model.as_deref(), Some("anthropic/claude-x"));
        let studio = &ctx.agents[1];
        assert_eq!(studio.harness_profile.as_deref(), Some("studio"));
        assert_eq!(studio.model.as_deref(), Some("openai/gpt-y"), "no double provider prefix");
        assert_eq!(studio.effort.as_deref(), Some("high"));
        assert_eq!(studio.role.as_deref(), Some("Design studio lead"));
        assert_eq!(studio.instructions.as_deref(), Some("You are the studio."));
        assert_eq!(studio.cwd.as_deref(), Some(expand_home("~/Studio").to_str().unwrap()));
    }

    #[test]
    fn second_scan_is_all_unchanged() {
        let (_tmp, root) = fixture();
        let first = scan(&root, HashMap::new());
        let known = first.threads.iter().map(|t| (t.import_key.clone(), t.fingerprint.clone())).collect();
        let second = scan(&root, known);
        assert!(second.threads.is_empty());
        assert_eq!(second.stats.threads_unchanged, 4);
        assert_eq!(second.stats.threads_seen, 4);
    }

    #[test]
    fn transcript_follows_display_projection() {
        let (_tmp, root) = fixture();
        let ctx = scan(&root, HashMap::new());
        let s1 = ctx.threads.iter().find(|t| t.import_key == "hermes:studio:s1").unwrap();
        let conn = open_db(&root.join("profiles/studio/state.db")).unwrap();
        let items = read_transcript(&conn, &s1.session.session_id, Some("m")).unwrap();
        let kinds: Vec<String> = items
            .iter()
            .map(|i| match i {
                TranscriptItem::User { text, .. } => format!("user:{text}"),
                TranscriptItem::Assistant { text, model, .. } => format!("assistant:{text}:{}", model.as_deref().unwrap_or("")),
                TranscriptItem::Thinking { text, .. } => format!("thinking:{text}"),
                TranscriptItem::ToolCall { call_id, name, input, .. } => {
                    format!("call:{}:{name}:{}", call_id.as_deref().unwrap_or(""), input.contains("\"ls\""))
                }
                TranscriptItem::ToolResult { call_id, output, .. } => format!("result:{}:{output}", call_id.as_deref().unwrap_or("")),
                TranscriptItem::System { text, .. } => format!("system:{text}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "user:hello",
                "thinking:let me think",
                "assistant:live answer:m",
                "call:call_1:terminal:true",
                "result:call_1:file.txt",
                "user:blocks",
            ]
        );
    }

    #[test]
    fn failed_turns_show_the_provider_error_and_preserve_the_boundary() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let notice =
            "Your request was not processed. Send it again if you still want me to carry it out.";
        let meta = r#"{"error":"HTTP 400: The requested model is not supported.","error_surface":{"provider_label":"GitHub Copilot","model":"gpt-6.1-sol"}}"#;
        conn.execute(
            "INSERT INTO messages (session_id,role,content,timestamp,display_kind,display_metadata) VALUES ('s','assistant',?1,1,'failed_turn',?2)",
            params![notice, meta],
        ).unwrap();
        let detail = read_latest_turn_failure(&conn, "s", 0).unwrap().unwrap();
        assert!(read_latest_turn_failure(&conn, "s", 1001).unwrap().is_none(), "old failures must not affect a later slash command");
        assert!(detail.contains("GitHub Copilot · gpt-6.1-sol"));
        assert!(detail.contains("HTTP 400: The requested model is not supported."));
        assert!(detail.ends_with(notice));
        assert!(
            matches!(&read_transcript(&conn, "s", None).unwrap()[0], TranscriptItem::System { text, .. } if text == &detail)
        );
        // An older boundary must not mark a later successful turn as failed.
        conn.execute("INSERT INTO messages (session_id,role,content,timestamp) VALUES ('s','assistant','success',2)", []).unwrap();
        assert!(read_latest_turn_failure(&conn, "s", 0).unwrap().is_none());
    }

    #[test]
    fn rejected_models_are_scoped_to_the_provider_and_expire() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        for (error, provider) in [("HTTP 400: The requested model is not supported.", "copilot"), ("HTTP 401: Sign in again", "openai-codex")] {
            let meta = serde_json::json!({"error":error,"error_surface":{"provider":provider,"model":"gpt-6.1-sol"}});
            conn.execute("INSERT INTO messages (session_id,role,timestamp,display_kind,display_metadata) VALUES ('s','assistant',100,'failed_turn',?1)", [meta.to_string()]).unwrap();
        }
        assert_eq!(read_rejected_models(&conn, 99_000).unwrap(), HashSet::from(["copilot:gpt-6.1-sol".to_string()]));
        assert!(read_rejected_models(&conn, 100_001).unwrap().is_empty());
    }

    #[test]
    fn legacy_failed_turns_and_malformed_metadata_remain_readable() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT)").unwrap();
        let notice = "This turn did not complete. Some actions may already have run; verify their effects before resending.";
        conn.execute(
            "INSERT INTO messages VALUES (1,'s','assistant',?1)",
            [notice],
        )
        .unwrap();
        assert!(
            matches!(&read_transcript(&conn, "s", None).unwrap()[0], TranscriptItem::System { text, .. } if text.contains(notice))
        );
        assert_eq!(
            failed_turn_error(notice, Some("invalid JSON")),
            format!("Hermes request failed. {notice}")
        );
    }

    #[test]
    fn notifications_become_system_items() {
        let (_tmp, root) = fixture();
        let conn = open_db(&root.join("profiles/studio/state.db")).unwrap();
        let items = read_transcript(&conn, "s2", None).unwrap();
        assert!(matches!(&items[0], TranscriptItem::System { text, ts: Some(2_000_100) } if text == "done notification"));
        assert!(matches!(&items[1], TranscriptItem::User { .. }));
    }

    #[test]
    fn opens_closed_wal_db_without_creating_files() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t (x); INSERT INTO t VALUES (1);").unwrap();
        }
        let before: Vec<_> = fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(before.len(), 1, "clean close removes -wal/-shm");
        let conn = open_db(&db).unwrap();
        let n: i64 = conn.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        drop(conn);
        let after: Vec<_> = fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn reads_wal_content_from_a_snapshot_without_touching_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        let writer = Connection::open(&db).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE t (x); INSERT INTO t VALUES (1), (2);")
            .unwrap();
        let listing = || -> Vec<(std::ffi::OsString, std::time::SystemTime, u64)> {
            let mut v: Vec<_> = fs::read_dir(tmp.path())
                .unwrap()
                .map(|e| {
                    let e = e.unwrap();
                    let m = e.metadata().unwrap();
                    (e.file_name(), m.modified().unwrap(), m.len())
                })
                .collect();
            v.sort();
            v
        };
        let before = listing();
        assert!(before.iter().any(|(n, _, _)| n == "state.db-wal"));
        let conn = open_db(&db).unwrap();
        let snapshot_dir = conn.snapshot_dir().expect("read from a copy").to_path_buf();
        let n: i64 = conn.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "un-checkpointed rows are visible");
        drop(conn);
        assert!(!snapshot_dir.exists(), "temp copy removed");
        assert_eq!(before, listing());
        drop(writer);
    }
}
