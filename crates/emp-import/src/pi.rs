//! pi coding agent (1.3.x) importer.
//!
//! Sessions are JSONL v3 trees at `<sessionDir>/--<encoded-cwd>--/<ts>_<id>.jsonl`. The
//! session dir is `$PI_CODING_AGENT_SESSION_DIR`, else `sessionDir` in
//! `<agentDir>/settings.json`, else `<agentDir>/sessions`; the agent dir is
//! `$PI_CODING_AGENT_DIR` or `~/.pi/agent`. Format: pi's `docs/session-format.md`.
//!
//! Scanning parses lines into small typed structs, so serde skips message content without
//! allocating it; session files reach hundreds of MB.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use emp_core::{AgentUpsert, ThreadUpsert};
use emp_protocol::{Harness, ImportSourceKind, SessionRef, Thread, TranscriptItem};
use serde::Deserialize;
use serde_json::Value;

use crate::util::{self, MAX_TOOL_TEXT};
use crate::{Importer, ScanCtx};

#[derive(Default)]
pub struct PiImporter {
    /// Overrides the agent dir (tests).
    agent_dir: Option<PathBuf>,
    /// Overrides the session dir (tests).
    session_dir: Option<PathBuf>,
}

impl PiImporter {
    fn agent_dir(&self) -> PathBuf {
        if let Some(dir) = &self.agent_dir {
            return dir.clone();
        }
        match std::env::var("PI_CODING_AGENT_DIR") {
            Ok(dir) if !dir.is_empty() => util::expand_home(&dir),
            _ => emp_core::paths::home_dir().join(".pi/agent"),
        }
    }

    fn settings_path(&self) -> PathBuf {
        self.agent_dir().join("settings.json")
    }

    fn read_settings(&self) -> Option<Value> {
        let bytes = fs::read(self.settings_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn session_dir(&self) -> PathBuf {
        if let Some(dir) = &self.session_dir {
            return dir.clone();
        }
        if let Ok(dir) = std::env::var("PI_CODING_AGENT_SESSION_DIR")
            && !dir.is_empty()
        {
            return util::expand_home(&dir);
        }
        if let Some(dir) = self.read_settings().as_ref().and_then(|s| s.get("sessionDir")?.as_str().map(str::to_owned))
            && !dir.is_empty()
        {
            return util::expand_home(&dir);
        }
        self.agent_dir().join("sessions")
    }

    fn scan_session(&self, path: &Path, ctx: &mut ScanCtx) -> Result<()> {
        let Some(header) = read_header(path)? else {
            ctx.skip(format!("pi: no session header in {}", file_name(path)));
            return Ok(());
        };
        let key = format!("pi:{}", header.id);
        let fingerprint = util::file_fingerprint(path)?;
        if ctx.is_unchanged(&key, &fingerprint) {
            return Ok(());
        }

        let mut name: Option<String> = None;
        let mut first_user: Option<String> = None;
        let mut model_change: Option<String> = None;
        let mut assistant_model: Option<String> = None;
        let mut message_count = 0i64;
        let mut bad = 0usize;
        each_line(path, |_, line| {
            let Ok(entry) = serde_json::from_slice::<ScanEntry>(line) else {
                bad += 1;
                return ControlFlow::Continue(());
            };
            match entry.ty.as_str() {
                "session_info" => {
                    if let Some(n) = entry.name.as_ref().and_then(Value::as_str).and_then(util::title_from) {
                        name = Some(n);
                    }
                }
                "model_change" => {
                    if let Some(m) = provider_model(entry.provider.as_ref(), entry.model_id.as_ref()) {
                        model_change = Some(m);
                    }
                }
                "message" => {
                    let Some(msg) = entry.message else { return ControlFlow::Continue(()) };
                    let role = msg.role.as_ref().and_then(Value::as_str).unwrap_or("");
                    if role != "system" {
                        message_count += 1;
                    }
                    match role {
                        "assistant" => {
                            if let Some(m) = provider_model(msg.provider.as_ref(), msg.model.as_ref()) {
                                assistant_model = Some(m);
                            }
                        }
                        "user" if first_user.is_none() => {
                            // Content was skipped by the light parse; re-read just this line.
                            first_user = serde_json::from_slice::<Value>(line)
                                .ok()
                                .and_then(|v| title_candidate(&v["message"]["content"]));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            ControlFlow::Continue(())
        })?;
        if bad > 0 {
            ctx.skip(format!("pi: {bad} unparseable line(s) in {}", file_name(path)));
        }

        let meta = fs::metadata(path)?;
        let updated_at = util::mtime_ms(&meta).unwrap_or(0);
        let created_at = header.timestamp.as_deref().and_then(util::parse_ts).unwrap_or(updated_at);
        let cwd = header.cwd.filter(|c| !c.is_empty());
        let parent_key = header.parent_session.as_deref().and_then(parent_session_id).map(|id| format!("pi:{id}"));
        let path_str = path.to_string_lossy().into_owned();
        ctx.thread(ThreadUpsert {
            import_source: ImportSourceKind::Pi,
            import_key: key,
            import_path: path_str.clone(),
            fingerprint,
            harness: Harness::Pi,
            title: name.or(first_user).unwrap_or_else(|| "Untitled pi session".into()),
            project_root: cwd.as_deref().map(util::project_root_for),
            cwd,
            model: model_change.or(assistant_model),
            session: SessionRef {
                harness: Harness::Pi,
                session_id: header.id,
                path: Some(path_str),
                home: None,
                profile: None,
            },
            message_count: Some(message_count),
            created_at,
            updated_at,
            parent_key,
            automated: false,
        });
        Ok(())
    }

    fn scan_default_agent(&self, ctx: &mut ScanCtx) {
        let path = self.settings_path();
        if !path.exists() {
            return;
        }
        let Some(settings) = self.read_settings() else {
            ctx.warn(format!("pi: unreadable {}", path.display()));
            return;
        };
        let s = |k: &str| settings.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
        let model = match (s("defaultProvider"), s("defaultModel")) {
            (Some(p), Some(m)) => Some(format!("{p}/{m}")),
            (None, Some(m)) => Some(m),
            _ => None,
        };
        ctx.agent(AgentUpsert {
            import_source: ImportSourceKind::Pi,
            import_key: "pi:default".into(),
            import_path: path.to_string_lossy().into_owned(),
            name: "pi (default)".into(),
            harness: Harness::Pi,
            model,
            effort: s("defaultThinkingLevel"),
            role: None,
            instructions: None,
            cwd: None,
            autonomy: emp_protocol::FULL_ACCESS,
            harness_profile: None,
        });
    }
}

impl Importer for PiImporter {
    fn kind(&self) -> ImportSourceKind {
        ImportSourceKind::Pi
    }

    fn paths(&self) -> Vec<PathBuf> {
        vec![self.session_dir(), self.settings_path()]
    }

    fn scan(&self, ctx: &mut ScanCtx) -> Result<()> {
        let dir = self.session_dir();
        if dir.exists() {
            fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))?;
            // `--<cwd>--/<file>.jsonl`, or flat files in a custom session dir.
            for entry in walkdir::WalkDir::new(&dir).min_depth(1).max_depth(2) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        ctx.warn(format!("pi: {e}"));
                        continue;
                    }
                };
                let path = entry.path();
                if !entry.file_type().is_file() || path.extension().is_none_or(|e| e != "jsonl") {
                    continue;
                }
                ctx.progress(&format!("pi: {}", file_name(path)));
                if let Err(e) = self.scan_session(path, ctx) {
                    ctx.skip(format!("pi: {}: {e:#}", file_name(path)));
                }
            }
        }
        self.scan_default_agent(ctx);
        Ok(())
    }

    /// The last assistant message's `usage` (pi records it on every model call).
    fn context(&self, thread: &Thread) -> Result<Option<emp_protocol::ContextUsage>> {
        let path = thread.session.path.as_deref().context("pi thread has no session path")?;
        crate::util::last_jsonl(Path::new(path), "\"usage\"", |entry| {
            let m = &entry["message"];
            let u = &m["usage"];
            (m["role"] == "assistant" && u.is_object()).then(|| {
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                let model = match (m["provider"].as_str(), m["model"].as_str()) {
                    (Some(p), Some(m)) => Some(format!("{p}/{m}")),
                    (None, Some(m)) => Some(m.to_string()),
                    _ => None,
                };
                emp_protocol::ContextUsage { used: n("input") + n("cacheRead") + n("cacheWrite") + n("output"), window: None, model }
            })
            .filter(|c| c.used > 0)
        })
    }

    /// Follows the active branch: the parent chain from the last entry, which is the leaf pi
    /// resumes at (`SessionManager._buildIndex`). Abandoned branches are left out.
    fn transcript(&self, thread: &Thread) -> Result<Vec<TranscriptItem>> {
        let path = thread.session.path.as_deref().context("pi thread has no session path")?;
        let mut nodes: Vec<(Option<String>, Option<String>, Vec<TranscriptItem>)> = Vec::new();
        each_line(Path::new(path), |_, line| {
            if let Ok(entry) = serde_json::from_slice::<Value>(line)
                && entry["type"] != "session"
            {
                let id = entry["id"].as_str().map(str::to_owned);
                let parent = entry["parentId"].as_str().map(str::to_owned);
                nodes.push((id, parent, entry_items(&entry)));
            }
            ControlFlow::Continue(())
        })?;

        let index: HashMap<&str, usize> =
            nodes.iter().enumerate().filter_map(|(i, n)| Some((n.0.as_deref()?, i))).collect();
        let mut on_path = HashSet::new();
        let mut cur = nodes.len().checked_sub(1);
        while let Some(i) = cur {
            if !on_path.insert(i) {
                break; // cycle guard
            }
            cur = nodes[i].1.as_deref().and_then(|p| index.get(p).copied());
        }
        // Entries without ids (pre-v2 files) are linear: keep them all.
        let linear = nodes.iter().all(|n| n.0.is_none());
        Ok(nodes
            .into_iter()
            .enumerate()
            .filter(|(i, _)| linear || on_path.contains(i))
            .flat_map(|(_, n)| n.2)
            .collect())
    }
}

#[derive(Deserialize)]
struct Header {
    #[serde(rename = "type")]
    ty: String,
    id: String,
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "parentSession")]
    parent_session: Option<String>,
}

/// One entry, minus message content.
#[derive(Deserialize)]
struct ScanEntry {
    #[serde(rename = "type", default)]
    ty: String,
    name: Option<Value>,
    provider: Option<Value>,
    #[serde(rename = "modelId")]
    model_id: Option<Value>,
    message: Option<ScanMessage>,
}

#[derive(Deserialize)]
struct ScanMessage {
    role: Option<Value>,
    provider: Option<Value>,
    model: Option<Value>,
}

fn read_header(path: &Path) -> Result<Option<Header>> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut line = Vec::new();
    BufReader::new(file).read_until(b'\n', &mut line)?;
    Ok(serde_json::from_slice::<Header>(&line).ok().filter(|h| h.ty == "session" && !h.id.is_empty()))
}

/// Session id of a `parentSession` file: its header id, else the `<ts>_<id>` file name.
fn parent_session_id(parent: &str) -> Option<String> {
    let path = util::expand_home(parent);
    if let Ok(Some(h)) = read_header(&path) {
        return Some(h.id);
    }
    let stem = path.file_stem()?.to_str()?;
    stem.split_once('_').map(|(_, id)| id.to_owned()).filter(|id| !id.is_empty())
}

fn provider_model(provider: Option<&Value>, model: Option<&Value>) -> Option<String> {
    let model = model?.as_str().filter(|m| !m.is_empty())?;
    Some(match provider.and_then(Value::as_str).filter(|p| !p.is_empty()) {
        Some(p) => format!("{p}/{model}"),
        None => model.to_owned(),
    })
}

/// Title from a user message, ignoring injected delegate notifications.
fn title_candidate(content: &Value) -> Option<String> {
    let text = util::content_text(content);
    if text.trim_start().starts_with("[acp_delegate") {
        return None;
    }
    util::title_from(&text)
}

fn entry_items(e: &Value) -> Vec<TranscriptItem> {
    let ts = e["timestamp"].as_str().and_then(util::parse_ts);
    let system = |text: String| vec![TranscriptItem::System { text, ts }];
    match e["type"].as_str().unwrap_or("") {
        "message" => message_items(&e["message"], e["id"].as_str(), ts),
        "model_change" => match provider_model(e.get("provider"), e.get("modelId")) {
            Some(m) => system(format!("Model changed to {m}")),
            None => Vec::new(),
        },
        "thinking_level_change" => match e["thinkingLevel"].as_str() {
            Some(level) => system(format!("Thinking level set to {level}")),
            None => Vec::new(),
        },
        "compaction" => system(match e["tokensBefore"].as_i64() {
            Some(n) => format!("Context compacted ({n} tokens summarized)"),
            None => "Context compacted".into(),
        }),
        "branch_summary" => {
            system(format!("Branch summary: {}", util::truncate(e["summary"].as_str().unwrap_or(""), MAX_TOOL_TEXT)))
        }
        "session_info" => match e["name"].as_str() {
            Some(name) => system(format!("Session named \"{name}\"")),
            None => Vec::new(),
        },
        "custom_message" if e["display"].as_bool() == Some(true) => {
            let text = util::truncate(&util::content_text(&e["content"]), MAX_TOOL_TEXT);
            match e["customType"].as_str() {
                Some(ty) => system(format!("[{ty}] {text}")),
                None => system(text),
            }
        }
        // label, context_edit, custom, usage, hidden custom messages: not shown.
        _ => Vec::new(),
    }
}

/// Transcript items for one pi message (session entry or RPC `message_end`). Shared with the
/// live pi driver so live and imported transcripts render identically.
pub fn message_items(m: &Value, entry_id: Option<&str>, entry_ts: Option<i64>) -> Vec<TranscriptItem> {
    let ts = m["timestamp"].as_i64().or(entry_ts);
    let mut items = Vec::new();
    match m["role"].as_str().unwrap_or("") {
        "user" => {
            let text = text_with_images(&m["content"]);
            if !text.is_empty() {
                items.push(TranscriptItem::User { text, ts });
            }
        }
        "assistant" => {
            let model = provider_model(m.get("provider"), m.get("model"));
            for b in m["content"].as_array().into_iter().flatten() {
                match b["type"].as_str().unwrap_or("") {
                    "text" => {
                        if let Some(text) = b["text"].as_str().filter(|t| !t.is_empty()) {
                            items.push(TranscriptItem::Assistant { text: text.to_owned(), model: model.clone(), ts });
                        }
                    }
                    "thinking" => {
                        if let Some(text) = b["thinking"].as_str().filter(|t| !t.trim().is_empty()) {
                            items.push(TranscriptItem::Thinking { text: text.to_owned(), ts });
                        }
                    }
                    "toolCall" => items.push(TranscriptItem::ToolCall {
                        call_id: b["id"].as_str().map(str::to_owned),
                        name: b["name"].as_str().unwrap_or("tool").to_owned(),
                        input: util::display_json(&b["arguments"]),
                        ts,
                    }),
                    _ => {}
                }
            }
            if m["stopReason"] == "error"
                && let Some(err) = m["errorMessage"].as_str()
            {
                items.push(TranscriptItem::System { text: format!("Error: {}", util::truncate(err, 500)), ts });
            }
        }
        "toolResult" => items.push(TranscriptItem::ToolResult {
            call_id: m["toolCallId"].as_str().map(str::to_owned),
            output: util::truncate(&text_with_images(&m["content"]), MAX_TOOL_TEXT),
            is_error: m["isError"].as_bool().unwrap_or(false),
            ts,
        }),
        // `!cmd` run by the user in the TUI.
        "bashExecution" => {
            let call_id = entry_id.map(|id| format!("bash-{id}"));
            items.push(TranscriptItem::ToolCall {
                call_id: call_id.clone(),
                name: "bash".into(),
                input: util::truncate(m["command"].as_str().unwrap_or(""), MAX_TOOL_TEXT),
                ts,
            });
            let exit = m["exitCode"].as_i64();
            items.push(TranscriptItem::ToolResult {
                call_id,
                output: util::truncate(m["output"].as_str().unwrap_or(""), MAX_TOOL_TEXT),
                is_error: m["cancelled"].as_bool() == Some(true) || exit.is_some_and(|c| c != 0),
                ts,
            });
        }
        "custom" if m["display"].as_bool() == Some(true) => {
            let text = util::truncate(&util::content_text(&m["content"]), MAX_TOOL_TEXT);
            if !text.is_empty() {
                items.push(TranscriptItem::System { text, ts });
            }
        }
        // system (prompt + tool loadout) and anything unknown: not shown.
        _ => {}
    }
    items
}

/// Text blocks joined, with `[image]` placeholders for image blocks.
fn text_with_images(content: &Value) -> String {
    match content {
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match b["type"].as_str() {
                Some("text") => b["text"].as_str().map(str::to_owned),
                Some("image") => Some("[image]".to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => util::content_text(other),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Raw JSONL lines, split on `\n` only (as [`util::for_each_jsonl`]) and trimmed, so callers
/// can parse them into light typed structs.
fn each_line(path: &Path, mut f: impl FnMut(usize, &[u8]) -> ControlFlow<()>) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut buf = Vec::new();
    let mut line_no = 0usize;
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        line_no += 1;
        let line = buf.trim_ascii();
        if !line.is_empty() && f(line_no, line).is_break() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn importer(root: &Path) -> PiImporter {
        PiImporter { agent_dir: Some(root.to_path_buf()), session_dir: Some(root.join("sessions")) }
    }

    fn write_session(root: &Path, name: &str, lines: &[Value]) -> PathBuf {
        let dir = root.join("sessions/--tmp-proj--");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        fs::write(&path, body).unwrap();
        path
    }

    fn header(id: &str) -> Value {
        json!({"type":"session","version":3,"id":id,"timestamp":"2026-01-02T03:04:05.000Z","cwd":"/work/proj"})
    }

    fn msg(id: &str, parent: Option<&str>, message: Value) -> Value {
        json!({"type":"message","id":id,"parentId":parent,"timestamp":"2026-01-02T03:04:06.000Z","message":message})
    }

    fn sample(root: &Path) -> PathBuf {
        write_session(root, "2026-01-02T03-04-05-000Z_s1.jsonl", &[
            header("s1"),
            json!({"type":"model_change","id":"m0","parentId":null,"timestamp":"2026-01-02T03:04:05.500Z","provider":"anthropic","modelId":"claude-x"}),
            msg("a", Some("m0"), json!({"role":"system","content":"prompt"})),
            msg("b", Some("a"), json!({"role":"user","content":[{"type":"text","text":"\n  Fix the   login bug\nmore"}],"timestamp":1})),
            msg("c", Some("b"), json!({"role":"assistant","provider":"openai","model":"gpt-z","content":[
                {"type":"thinking","thinking":"hmm"},
                {"type":"text","text":"On it"},
                {"type":"toolCall","id":"call1","name":"bash","arguments":{"command":"ls"}}]})),
            msg("d", Some("c"), json!({"role":"toolResult","toolCallId":"call1","toolName":"bash","content":[{"type":"text","text":"out"}],"isError":true})),
            json!({"type":"compaction","id":"e","parentId":"d","timestamp":"2026-01-02T03:05:00.000Z","summary":"s","firstKeptEntryId":"d","tokensBefore":1234}),
        ])
    }

    #[test]
    fn scan_extracts_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sample(tmp.path());
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.threads.len(), 1);
        let t = &ctx.threads[0];
        assert_eq!(t.import_key, "pi:s1");
        assert_eq!(t.title, "Fix the login bug");
        assert_eq!(t.cwd.as_deref(), Some("/work/proj"));
        assert_eq!(t.project_root.as_deref(), Some("/work/proj"));
        assert_eq!(t.model.as_deref(), Some("anthropic/claude-x"), "model_change wins over assistant model");
        assert_eq!(t.message_count, Some(3));
        assert_eq!(t.created_at, util::parse_ts("2026-01-02T03:04:05.000Z").unwrap());
        assert_eq!(t.session.path.as_deref(), Some(path.to_str().unwrap()));
        assert_eq!(t.parent_key, None);
        assert_eq!(ctx.stats.skipped, 0);
    }

    #[test]
    fn session_name_wins_and_assistant_model_is_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        write_session(tmp.path(), "x_s2.jsonl", &[
            header("s2"),
            msg("a", None, json!({"role":"user","content":"hello"})),
            msg("b", Some("a"), json!({"role":"assistant","provider":"openai","model":"gpt-z","content":[]})),
            json!({"type":"session_info","id":"c","parentId":"b","timestamp":"2026-01-02T03:04:07.000Z","name":"Refactor auth"}),
        ]);
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.threads[0].title, "Refactor auth");
        assert_eq!(ctx.threads[0].model.as_deref(), Some("openai/gpt-z"));
    }

    #[test]
    fn untitled_and_fork_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = sample(tmp.path());
        let mut h = header("child");
        h["parentSession"] = json!(parent.to_str().unwrap());
        write_session(tmp.path(), "y_child.jsonl", &[h]);
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        let child = ctx.threads.iter().find(|t| t.import_key == "pi:child").unwrap();
        assert_eq!(child.title, "Untitled pi session");
        assert_eq!(child.parent_key.as_deref(), Some("pi:s1"));
        assert_eq!(child.message_count, Some(0));
    }

    #[test]
    fn unchanged_files_are_not_reparsed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sample(tmp.path());
        let known = HashMap::from([("pi:s1".to_string(), util::file_fingerprint(&path).unwrap())]);
        let mut ctx = ScanCtx::new(known);
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert!(ctx.threads.is_empty());
        assert_eq!(ctx.stats.threads_seen, 1);
        assert_eq!(ctx.stats.threads_unchanged, 1);
    }

    #[test]
    fn bad_lines_and_headerless_files_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sample(tmp.path());
        let mut body = fs::read_to_string(&path).unwrap();
        body.push_str("{not json\n");
        fs::write(&path, body).unwrap();
        write_session(tmp.path(), "z_nohdr.jsonl", &[msg("a", None, json!({"role":"user","content":"x"}))]);
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.threads.len(), 1);
        assert_eq!(ctx.threads[0].message_count, Some(3));
        assert_eq!(ctx.stats.skipped, 2);
    }

    #[test]
    fn default_agent_from_settings() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("settings.json"),
            r#"{"defaultProvider":"openai-codex","defaultModel":"gpt-6","defaultThinkingLevel":"high"}"#,
        )
        .unwrap();
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.agents.len(), 1);
        let a = &ctx.agents[0];
        assert_eq!(a.import_key, "pi:default");
        assert_eq!(a.model.as_deref(), Some("openai-codex/gpt-6"));
        assert_eq!(a.effort.as_deref(), Some("high"));
        assert_eq!(a.autonomy, emp_protocol::FULL_ACCESS);
        assert!(a.instructions.is_none());
    }

    fn thread_for(path: &Path) -> Thread {
        Thread {
            id: "t".into(),
            harness: Harness::Pi,
            title: "x".into(),
            project_id: None,
            cwd: None,
            model: None,
            agent_id: None,
            parent_id: None,
            child_count: 0,
            assigned_by: None,
            status: emp_protocol::ThreadStatus::Idle,
            automated: false,
            session: SessionRef {
                harness: Harness::Pi,
                session_id: "s".into(),
                path: Some(path.to_string_lossy().into_owned()),
                home: None,
                profile: None,
            },
            message_count: None,
            created_at: 0,
            updated_at: 0,
            imported_from: None,
        }
    }

    #[test]
    fn transcript_maps_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sample(tmp.path());
        let items = importer(tmp.path()).transcript(&thread_for(&path)).unwrap();
        let kinds: Vec<&str> = items
            .iter()
            .map(|i| match i {
                TranscriptItem::User { .. } => "user",
                TranscriptItem::Assistant { .. } => "assistant",
                TranscriptItem::Thinking { .. } => "thinking",
                TranscriptItem::ToolCall { .. } => "call",
                TranscriptItem::ToolResult { .. } => "result",
                TranscriptItem::System { .. } => "system",
            })
            .collect();
        assert_eq!(kinds, ["system", "user", "thinking", "assistant", "call", "result", "system"]);
        match &items[3] {
            TranscriptItem::Assistant { text, model, .. } => {
                assert_eq!(text, "On it");
                assert_eq!(model.as_deref(), Some("openai/gpt-z"));
            }
            other => panic!("{other:?}"),
        }
        match &items[4] {
            TranscriptItem::ToolCall { call_id, name, input, .. } => {
                assert_eq!(call_id.as_deref(), Some("call1"));
                assert_eq!(name, "bash");
                assert!(input.contains("\"command\": \"ls\""));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&items[5], TranscriptItem::ToolResult { output, is_error: true, .. } if output == "out"));
    }

    #[test]
    fn transcript_follows_active_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(tmp.path(), "b_s3.jsonl", &[
            header("s3"),
            msg("a", None, json!({"role":"user","content":"root"})),
            msg("b", Some("a"), json!({"role":"user","content":"abandoned"})),
            msg("c", Some("a"), json!({"role":"user","content":"active"})),
        ]);
        let items = importer(tmp.path()).transcript(&thread_for(&path)).unwrap();
        let texts: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::User { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["root", "active"]);
    }
}
