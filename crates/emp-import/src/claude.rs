//! Claude Code (2.1.x) importer.
//!
//! Layout under `$CLAUDE_CONFIG_DIR` (default `~/.claude`):
//! - `projects/<cwd with / → ->/<sessionId>.jsonl` — one main session per file.
//! - `projects/<…>/<sessionId>/subagents/agent-<id>.jsonl` (+ `agent-<id>.meta.json` with
//!   `description`, `agentType`) — Task subagents. Imported as child threads. Main files carry
//!   no `isSidechain` records in 2.1.x; any that appear are left out of the main thread.
//! - `agents/*.md` — custom subagent definitions (YAML frontmatter + instructions).
//!
//! `sessions/*.json` holds peer tokens and is never read.

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

const MAX_INSTRUCTIONS: usize = 20_000;

#[derive(Default)]
pub struct ClaudeImporter {
    /// Overrides the config root (tests).
    root: Option<PathBuf>,
}

impl ClaudeImporter {
    fn root(&self) -> PathBuf {
        if let Some(root) = &self.root {
            return root.clone();
        }
        match std::env::var("CLAUDE_CONFIG_DIR") {
            Ok(dir) if !dir.is_empty() => util::expand_home(&dir),
            _ => emp_core::paths::home_dir().join(".claude"),
        }
    }

    fn projects_dir(&self) -> PathBuf {
        self.root().join("projects")
    }

    fn agents_dir(&self) -> PathBuf {
        self.root().join("agents")
    }

    fn scan_file(&self, path: &Path, sub: Option<&SubagentFile>, ctx: &mut ScanCtx) -> Result<()> {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
        let (key, session_id, parent_key) = match sub {
            None => (format!("claude:{stem}"), stem, None),
            Some(s) => (
                format!("claude:{}/{}", s.parent_id, stem),
                s.parent_id.clone(),
                Some(format!("claude:{}", s.parent_id)),
            ),
        };
        let fingerprint = util::file_fingerprint(path)?;
        if ctx.is_unchanged(&key, &fingerprint) {
            return Ok(());
        }

        let mut s = FileSummary::default();
        let mut bad = 0usize;
        each_line(path, |line| {
            match serde_json::from_slice::<ScanRecord>(line) {
                Ok(r) => s.add(r, line, sub.is_some()),
                Err(_) => bad += 1,
            }
            ControlFlow::Continue(())
        })?;
        if bad > 0 {
            ctx.skip(format!("claude: {bad} unparseable line(s) in {}", display_name(path)));
        }

        let meta = fs::metadata(path)?;
        let updated_at = util::mtime_ms(&meta).unwrap_or(0);
        let title = match sub {
            None => s.ai_title.or(s.first_user).unwrap_or_else(|| "Untitled Claude session".into()),
            Some(sub) => {
                let base = sub.description.clone().or(s.first_user).unwrap_or_else(|| "Subagent".into());
                format!("↳ {base}")
            }
        };
        let path_str = path.to_string_lossy().into_owned();
        ctx.thread(ThreadUpsert {
            import_source: ImportSourceKind::Claude,
            import_key: key,
            import_path: path_str.clone(),
            fingerprint,
            harness: Harness::Claude,
            title,
            project_root: s.cwd.as_deref().map(util::project_root_for),
            cwd: s.cwd,
            model: s.model,
            session: SessionRef {
                harness: Harness::Claude,
                session_id,
                path: Some(path_str),
                home: None,
                profile: None,
            },
            message_count: Some(s.message_count),
            created_at: s.first_ts.unwrap_or(updated_at),
            updated_at,
            parent_key,
            automated: false,
        });
        Ok(())
    }

    /// `claude (default)`: Claude Code as configured in `settings.json` (model, effort).
    fn default_agent(&self, ctx: &mut ScanCtx) {
        let root = self.root();
        if !root.is_dir() {
            return;
        }
        let settings_path = root.join("settings.json");
        let settings: serde_json::Value = fs::read(&settings_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let text = |k: &str| settings.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned);
        ctx.agent(AgentUpsert {
            import_source: ImportSourceKind::Claude,
            import_key: "claude:default".into(),
            import_path: settings_path.to_string_lossy().into_owned(),
            name: "claude (default)".into(),
            harness: Harness::Claude,
            model: text("model"),
            effort: text("effortLevel"),
            role: None,
            instructions: None,
            cwd: None,
            autonomy: emp_protocol::FULL_ACCESS,
            harness_profile: None,
        });
    }

    fn scan_agents(&self, ctx: &mut ScanCtx) {
        self.default_agent(ctx);
        let Ok(entries) = fs::read_dir(self.agents_dir()) else { return };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
            .collect();
        paths.sort();
        for path in paths {
            match fs::read_to_string(&path) {
                Ok(text) => match parse_agent(&path, &text) {
                    Some(a) => ctx.agent(a),
                    None => ctx.skip(format!("claude: no frontmatter in agents/{}", display_name(&path))),
                },
                Err(e) => ctx.skip(format!("claude: agents/{}: {e}", display_name(&path))),
            }
        }
    }
}

impl Importer for ClaudeImporter {
    fn kind(&self) -> ImportSourceKind {
        ImportSourceKind::Claude
    }

    fn paths(&self) -> Vec<PathBuf> {
        vec![self.projects_dir(), self.agents_dir()]
    }

    fn scan(&self, ctx: &mut ScanCtx) -> Result<()> {
        let dir = self.projects_dir();
        if dir.exists() {
            fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))?;
            // depth 2: <project>/<sid>.jsonl; depth 4: <project>/<sid>/subagents/agent-*.jsonl
            for entry in walkdir::WalkDir::new(&dir).min_depth(2).max_depth(4) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        ctx.warn(format!("claude: {e}"));
                        continue;
                    }
                };
                let path = entry.path();
                if !entry.file_type().is_file() || path.extension().is_none_or(|e| e != "jsonl") {
                    continue;
                }
                let sub = match entry.depth() {
                    2 => None,
                    4 => match SubagentFile::for_path(path) {
                        Some(s) => Some(s),
                        None => continue,
                    },
                    _ => continue,
                };
                ctx.progress(&format!("claude: {}", display_name(path)));
                if let Err(e) = self.scan_file(path, sub.as_ref(), ctx) {
                    ctx.skip(format!("claude: {}: {e:#}", display_name(path)));
                }
            }
        }
        self.scan_agents(ctx);
        Ok(())
    }

    /// The last main-chain assistant record's `message.usage`.
    fn context(&self, thread: &Thread) -> Result<Option<emp_protocol::ContextUsage>> {
        let path = thread.session.path.as_deref().context("Claude thread has no session path")?;
        let is_subagent = Path::new(path).parent().and_then(Path::file_name).is_some_and(|n| n == "subagents");
        crate::util::last_jsonl(Path::new(path), "\"usage\"", |r| {
            let m = &r["message"];
            let u = &m["usage"];
            let model = m["model"].as_str().filter(|m| !m.starts_with('<'));
            (r["type"] == "assistant" && u.is_object() && model.is_some() && (is_subagent || r["isSidechain"] != true)).then(|| {
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                emp_protocol::ContextUsage {
                    used: n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens") + n("output_tokens"),
                    window: None,
                    model: model.map(str::to_owned),
                }
            })
        })
    }

    fn transcript(&self, thread: &Thread) -> Result<Vec<TranscriptItem>> {
        let path = thread.session.path.as_deref().context("Claude thread has no session path")?;
        let path = Path::new(path);
        let is_subagent = path.parent().and_then(Path::file_name).is_some_and(|n| n == "subagents");
        let mut items = Vec::new();
        each_line(path, |line| {
            if let Ok(r) = serde_json::from_slice::<Value>(line)
                && (is_subagent || r["isSidechain"] != true)
            {
                record_items(&r, &mut items);
            }
            ControlFlow::Continue(())
        })?;
        Ok(items)
    }
}

/// One record, minus message content.
#[derive(Deserialize)]
struct ScanRecord {
    #[serde(rename = "type", default)]
    ty: String,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<Value>,
    cwd: Option<Value>,
    timestamp: Option<Value>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<Value>,
    summary: Option<Value>,
    message: Option<ScanMessage>,
}

#[derive(Deserialize)]
struct ScanMessage {
    model: Option<Value>,
}

#[derive(Default)]
struct FileSummary {
    ai_title: Option<String>,
    first_user: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    first_ts: Option<i64>,
    message_count: i64,
}

impl FileSummary {
    fn add(&mut self, r: ScanRecord, line: &[u8], subagent_file: bool) {
        let str_of = |v: &Option<Value>| v.as_ref().and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
        if self.first_ts.is_none() {
            self.first_ts = r.timestamp.as_ref().and_then(Value::as_str).and_then(util::parse_ts);
        }
        if self.cwd.is_none() {
            self.cwd = str_of(&r.cwd);
        }
        match r.ty.as_str() {
            "ai-title" => {
                if let Some(t) = str_of(&r.ai_title).as_deref().and_then(util::title_from) {
                    self.ai_title = Some(t);
                }
            }
            "summary" => {
                if let Some(t) = str_of(&r.summary).as_deref().and_then(util::title_from) {
                    self.ai_title = Some(t);
                }
            }
            "user" | "assistant" => {
                if !subagent_file && r.is_sidechain.as_ref().and_then(Value::as_bool) == Some(true) {
                    return;
                }
                self.message_count += 1;
                if r.ty == "assistant" {
                    if let Some(m) = r.message.as_ref().and_then(|m| str_of(&m.model)).filter(|m| m != "<synthetic>") {
                        self.model = Some(m);
                    }
                } else if self.first_user.is_none() {
                    // Content was skipped by the light parse; re-read just this line.
                    self.first_user = serde_json::from_slice::<Value>(line).ok().and_then(|v| user_title(&v));
                }
            }
            _ => {}
        }
    }
}

/// A subagent transcript: `<project>/<parentId>/subagents/agent-<id>.jsonl`.
struct SubagentFile {
    parent_id: String,
    description: Option<String>,
}

impl SubagentFile {
    fn for_path(path: &Path) -> Option<Self> {
        let subagents = path.parent()?;
        if subagents.file_name()? != "subagents" {
            return None;
        }
        let parent_id = subagents.parent()?.file_name()?.to_str()?.to_owned();
        let description = fs::read(path.with_extension("meta.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|m| m["description"].as_str().and_then(util::title_from));
        Some(Self { parent_id, description })
    }
}

/// Title from a typed-in user prompt; `None` for tool results, meta records, slash-command
/// wrappers and harness notices.
fn user_title(r: &Value) -> Option<String> {
    if r["isMeta"] == true || r["isCompactSummary"] == true {
        return None;
    }
    let text = util::content_text(&r["message"]["content"]);
    let t = text.trim_start();
    const NOISE: [&str; 6] = [
        "<",
        "[Request interrupted",
        "[acp_delegate",
        "[Your session was",
        "[Usage limit",
        "[Earlier usage",
    ];
    if NOISE.iter().any(|p| t.starts_with(p)) {
        return None;
    }
    util::title_from(t)
}

/// Transcript items for one Claude Code record. Session-file records and `--output-format
/// stream-json` records share this shape, so the live Claude driver uses it too.
pub fn record_items(r: &Value, items: &mut Vec<TranscriptItem>) {
    let ts = r["timestamp"].as_str().and_then(util::parse_ts);
    let content = &r["message"]["content"];
    match r["type"].as_str().unwrap_or("") {
        "user" => {
            if r["isMeta"] == true {
                return;
            }
            if r["isCompactSummary"] == true {
                let summary = util::truncate(&util::content_text(content), MAX_TOOL_TEXT);
                items.push(TranscriptItem::System { text: format!("Compaction summary: {summary}"), ts });
                return;
            }
            let Some(blocks) = content.as_array() else {
                if let Some(text) = content.as_str().filter(|t| !t.is_empty()) {
                    items.push(TranscriptItem::User { text: text.to_owned(), ts });
                }
                return;
            };
            let mut text = Vec::new();
            for b in blocks {
                match b["type"].as_str().unwrap_or("") {
                    "text" => text.extend(b["text"].as_str()),
                    "image" => text.push("[image]"),
                    "tool_result" => items.push(TranscriptItem::ToolResult {
                        call_id: b["tool_use_id"].as_str().map(str::to_owned),
                        output: util::truncate(&tool_output(&b["content"]), MAX_TOOL_TEXT),
                        is_error: b["is_error"].as_bool().unwrap_or(false),
                        ts,
                    }),
                    _ => {}
                }
            }
            let text = text.join("\n");
            if !text.is_empty() {
                items.push(TranscriptItem::User { text, ts });
            }
        }
        "assistant" => {
            let model = r["message"]["model"].as_str().filter(|m| *m != "<synthetic>").map(str::to_owned);
            for b in content.as_array().into_iter().flatten() {
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
                    "tool_use" | "server_tool_use" => items.push(TranscriptItem::ToolCall {
                        call_id: b["id"].as_str().map(str::to_owned),
                        name: b["name"].as_str().unwrap_or("tool").to_owned(),
                        input: util::display_json(&b["input"]),
                        ts,
                    }),
                    _ => {}
                }
            }
        }
        "system" => {
            let text = match r["subtype"].as_str().unwrap_or("") {
                "compact_boundary" => "Context compacted".to_owned(),
                "api_error" => match r["content"].as_str().filter(|c| !c.is_empty()) {
                    Some(c) => format!("API error: {}", util::truncate(c, 500)),
                    None => "API error".to_owned(),
                },
                _ => return,
            };
            items.push(TranscriptItem::System { text, ts });
        }
        // attachment, queue-operation, cost-state, last-prompt, ai-title, mode, …: not shown.
        _ => {}
    }
}

/// `tool_result.content`: a string or text/image blocks.
fn tool_output(content: &Value) -> String {
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

#[derive(Deserialize)]
struct AgentFrontmatter {
    name: Option<String>,
    description: Option<String>,
    model: Option<String>,
}

/// `---\n<yaml>\n---\n<instructions>` → agent profile.
fn parse_agent(path: &Path, text: &str) -> Option<AgentUpsert> {
    let rest = text.trim_start_matches('\u{feff}').strip_prefix("---")?;
    let rest = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n'))?;
    let (yaml, body) = match rest.find("\n---") {
        Some(i) => {
            let after = &rest[i + 4..];
            (&rest[..i], after.split_once('\n').map_or("", |(_, b)| b))
        }
        None => return None,
    };
    let fm: AgentFrontmatter = serde_yaml::from_str(yaml).ok()?;
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let body = body.trim();
    Some(AgentUpsert {
        import_source: ImportSourceKind::Claude,
        import_key: format!("claude:agent:{stem}"),
        import_path: path.to_string_lossy().into_owned(),
        name: fm.name.filter(|n| !n.trim().is_empty()).unwrap_or(stem),
        harness: Harness::Claude,
        model: fm.model.filter(|m| !m.is_empty()),
        effort: None,
        role: fm.description.map(|d| d.trim().to_owned()).filter(|d| !d.is_empty()),
        instructions: (!body.is_empty()).then(|| util::truncate(body, MAX_INSTRUCTIONS)),
        cwd: None,
        autonomy: emp_protocol::FULL_ACCESS,
        harness_profile: None,
    })
}

/// `<project>/<file>` for messages, without the home path.
fn display_name(path: &Path) -> String {
    let mut parts: Vec<String> =
        path.iter().rev().take(2).map(|p| p.to_string_lossy().into_owned()).collect();
    parts.reverse();
    parts.join("/")
}

/// Raw JSONL lines, split on `\n` only (as [`util::for_each_jsonl`]) and trimmed, so callers
/// can parse them into light typed structs.
fn each_line(path: &Path, mut f: impl FnMut(&[u8]) -> ControlFlow<()>) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        let line = buf.trim_ascii();
        if !line.is_empty() && f(line).is_break() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    const SID: &str = "11111111-2222-3333-4444-555555555555";

    fn importer(root: &Path) -> ClaudeImporter {
        ClaudeImporter { root: Some(root.to_path_buf()) }
    }

    fn write_jsonl(path: &Path, lines: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    }

    fn rec(ty: &str, ts: &str, message: Value) -> Value {
        json!({"type":ty,"uuid":ts,"parentUuid":null,"isSidechain":false,"sessionId":SID,
               "cwd":"/work/real-dir","timestamp":ts,"message":message})
    }

    fn sample(root: &Path) -> PathBuf {
        let path = root.join(format!("projects/-work-real-dir/{SID}.jsonl"));
        write_jsonl(&path, &[
            json!({"type":"queue-operation","operation":"enqueue","sessionId":SID,"timestamp":"2026-01-01T00:00:00.000Z"}),
            {
                let mut r = rec("user", "2026-01-01T00:00:01.000Z", json!({"role":"user","content":"<command-name>/clear</command-name>"}));
                r["isMeta"] = json!(false);
                r
            },
            rec("user", "2026-01-01T00:00:02.000Z", json!({"role":"user","content":"  Add dark   mode\nplease"})),
            rec("assistant", "2026-01-01T00:00:03.000Z", json!({"role":"assistant","model":"claude-a","content":[
                {"type":"thinking","thinking":"plan"},
                {"type":"text","text":"Sure"},
                {"type":"tool_use","id":"tu1","name":"Read","input":{"file_path":"/x"}}]})),
            rec("user", "2026-01-01T00:00:04.000Z", json!({"role":"user","content":[
                {"type":"tool_result","tool_use_id":"tu1","content":[{"type":"text","text":"file body"}],"is_error":true}]})),
            rec("assistant", "2026-01-01T00:00:05.000Z", json!({"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"x"}]})),
            {
                let mut r = rec("assistant", "2026-01-01T00:00:06.000Z", json!({"role":"assistant","model":"claude-side","content":[{"type":"text","text":"side"}]}));
                r["isSidechain"] = json!(true);
                r
            },
            json!({"type":"attachment","attachment":{},"timestamp":"2026-01-01T00:00:07.000Z"}),
        ]);
        path
    }

    #[test]
    fn scan_extracts_metadata_and_skips_noise_for_title() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sample(tmp.path());
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.threads.len(), 1);
        let t = &ctx.threads[0];
        assert_eq!(t.import_key, format!("claude:{SID}"));
        assert_eq!(t.title, "Add dark mode");
        assert_eq!(t.cwd.as_deref(), Some("/work/real-dir"), "cwd comes from records, not the folder");
        assert_eq!(t.model.as_deref(), Some("claude-a"), "synthetic and sidechain models ignored");
        assert_eq!(t.message_count, Some(5));
        assert_eq!(t.created_at, util::parse_ts("2026-01-01T00:00:00.000Z").unwrap());
        assert_eq!(t.session.session_id, SID);
        assert_eq!(t.session.path.as_deref(), Some(path.to_str().unwrap()));
        assert!(t.parent_key.is_none());
    }

    #[test]
    fn ai_title_wins_latest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sample(tmp.path());
        let mut body = fs::read_to_string(&path).unwrap();
        body.push_str(&format!("{}\n", json!({"type":"ai-title","aiTitle":"Old title","sessionId":SID})));
        body.push_str(&format!("{}\n", json!({"type":"ai-title","aiTitle":"Dark mode toggle","sessionId":SID})));
        fs::write(&path, body).unwrap();
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.threads[0].title, "Dark mode toggle");
    }

    #[test]
    fn untitled_when_only_tool_results() {
        let tmp = tempfile::tempdir().unwrap();
        write_jsonl(&tmp.path().join(format!("projects/p/{SID}.jsonl")), &[rec(
            "user",
            "2026-01-01T00:00:00.000Z",
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"ok"}]}),
        )]);
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.threads[0].title, "Untitled Claude session");
    }

    #[test]
    fn subagents_become_child_threads() {
        let tmp = tempfile::tempdir().unwrap();
        sample(tmp.path());
        let dir = tmp.path().join(format!("projects/-work-real-dir/{SID}/subagents"));
        let mut r = rec("user", "2026-01-01T00:01:00.000Z", json!({"role":"user","content":"Explore the repo"}));
        r["isSidechain"] = json!(true);
        write_jsonl(&dir.join("agent-abc.jsonl"), &[r]);
        fs::write(dir.join("agent-abc.meta.json"), r#"{"agentType":"Explore","description":"Find theme code"}"#).unwrap();
        // Not a session: other files under the session folder are ignored.
        write_jsonl(&tmp.path().join(format!("projects/-work-real-dir/{SID}/tool-results/x.jsonl")), &[json!({})]);

        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.threads.len(), 2);
        let sub = ctx.threads.iter().find(|t| t.parent_key.is_some()).unwrap();
        assert_eq!(sub.import_key, format!("claude:{SID}/agent-abc"));
        assert_eq!(sub.parent_key.as_deref(), Some(format!("claude:{SID}").as_str()));
        assert_eq!(sub.title, "↳ Find theme code");
        assert_eq!(sub.message_count, Some(1));
        assert_eq!(sub.session.session_id, SID);

        let items = importer(tmp.path()).transcript(&thread_for(&dir.join("agent-abc.jsonl"))).unwrap();
        assert!(matches!(&items[..], [TranscriptItem::User { text, .. }] if text == "Explore the repo"));
    }

    #[test]
    fn unchanged_files_are_not_reparsed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sample(tmp.path());
        let known = HashMap::from([(format!("claude:{SID}"), util::file_fingerprint(&path).unwrap())]);
        let mut ctx = ScanCtx::new(known);
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert!(ctx.threads.is_empty());
        assert_eq!(ctx.stats.threads_seen, 1);
        assert_eq!(ctx.stats.threads_unchanged, 1);
    }

    #[test]
    fn bad_lines_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sample(tmp.path());
        let mut body = fs::read_to_string(&path).unwrap();
        body.insert_str(0, "{truncated\n");
        fs::write(&path, body).unwrap();
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.threads.len(), 1);
        assert_eq!(ctx.threads[0].title, "Add dark mode");
        assert_eq!(ctx.stats.skipped, 1);
    }

    #[test]
    fn agents_from_markdown_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("reviewer.md"),
            "---\nname: code-reviewer\ndescription: Reviews diffs\nmodel: sonnet\ntools: Read, Grep\n---\n\nYou review code.\n",
        )
        .unwrap();
        fs::write(dir.join("plain.md"), "no frontmatter").unwrap();
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert_eq!(ctx.agents.len(), 2);
        assert_eq!(ctx.agents[0].import_key, "claude:default");
        let a = &ctx.agents[1];
        assert_eq!(a.import_key, "claude:agent:reviewer");
        assert_eq!(a.name, "code-reviewer");
        assert_eq!(a.role.as_deref(), Some("Reviews diffs"));
        assert_eq!(a.model.as_deref(), Some("sonnet"));
        assert_eq!(a.instructions.as_deref(), Some("You review code."));
        assert_eq!(ctx.stats.skipped, 1);
    }

    #[test]
    fn no_agents_dir_imports_only_the_default_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        assert!(ctx.threads.is_empty());
        assert_eq!(ctx.agents.len(), 1);
        assert_eq!(ctx.agents[0].name, "claude (default)");
        assert_eq!(ctx.agents[0].model, None);
    }

    #[test]
    fn default_agent_reads_model_and_effort_from_settings() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("settings.json"), r#"{"model":"opus[1m]","effortLevel":"xhigh","permissions":{"allow":[]}}"#).unwrap();
        let mut ctx = ScanCtx::new(HashMap::new());
        importer(tmp.path()).scan(&mut ctx).unwrap();
        let a = &ctx.agents[0];
        assert_eq!(a.harness, Harness::Claude);
        assert_eq!(a.model.as_deref(), Some("opus[1m]"));
        assert_eq!(a.effort.as_deref(), Some("xhigh"));
    }

    fn thread_for(path: &Path) -> Thread {
        Thread {
            id: "t".into(),
            harness: Harness::Claude,
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
                harness: Harness::Claude,
                session_id: SID.into(),
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
    fn transcript_maps_records_and_drops_sidechains() {
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
        assert_eq!(kinds, ["user", "user", "thinking", "assistant", "call", "result", "assistant"]);
        assert!(matches!(&items[3], TranscriptItem::Assistant { model: Some(m), .. } if m == "claude-a"));
        assert!(matches!(&items[4], TranscriptItem::ToolCall { call_id: Some(id), name, input, .. }
            if id == "tu1" && name == "Read" && input.contains("\"file_path\": \"/x\"")));
        assert!(matches!(&items[5], TranscriptItem::ToolResult { call_id: Some(id), output, is_error: true, .. }
            if id == "tu1" && output == "file body"));
        assert!(matches!(&items[6], TranscriptItem::Assistant { model: None, .. }), "synthetic model hidden");
    }
}
