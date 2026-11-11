//! Google Antigravity CLI (`agy`) importer (verified against agy 1.4.3).
//!
//! Home: `~/.gemini/antigravity-cli`.
//! - `conversation_summaries.db`, table `conversation_summaries`: one row per conversation
//!   (`conversation_id`, `title`, `preview`, `step_count`, `last_modified_time` and
//!   `last_user_input_time` as `YYYY-MM-DD HH:MM:SS.ffffff+00:00`, `workspace_uris` JSON array
//!   of `file://` URIs, `parent_conversation_id`/`nesting_depth` for subagents).
//! - `conversations/<id>.db`, table `steps (idx, step_type, status, step_payload, …)`. Each
//!   payload is an undocumented protobuf; the fields read here were mapped by hand:
//!
//! | step_type | meaning | fields |
//! |---|---|---|
//! | 14 | user input | `19.2` text |
//! | 15 | agent response | `20.1` text, `20.3` thinking, `20.7` tool calls (repeated by the 132 steps) |
//! | 132 | tool | `5.4` call (`1` id, `2` name, `3` JSON args), `140.2.1` result text |
//! | 17 | error | `24.3.1` user-facing message |
//! | 23 | compaction summary | `30.5` summary |
//! | 101 | system notice | not shown |
//!
//! Every step has metadata in field `5`: `5.1` timestamp (`1` s, `2` ns) and, on model calls,
//! `5.9` usage (`2` input, `3` output, `5` cache-read tokens).
//!
//! Databases are read through [`open_db`]. `settings.json` is read for `model` only; nothing
//! else under the home is opened (`oauth_creds.json` and friends live one level up).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use emp_core::{AgentUpsert, ThreadUpsert};
use emp_protocol::{ContextUsage, Harness, ImportSourceKind, SessionRef, Thread, TranscriptItem};

use crate::util::{ctime_ms, open_db, project_root_for, truncate};
use crate::{Importer, ScanCtx};

pub struct AntigravityImporter {
    home: PathBuf,
}

impl Default for AntigravityImporter {
    fn default() -> Self {
        Self::with_home(emp_core::paths::home_dir().join(".gemini/antigravity-cli"))
    }
}

impl AntigravityImporter {
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    fn conversation_db(&self, id: &str) -> PathBuf {
        self.home.join("conversations").join(format!("{id}.db"))
    }
}

impl Importer for AntigravityImporter {
    fn kind(&self) -> ImportSourceKind {
        ImportSourceKind::Antigravity
    }

    fn paths(&self) -> Vec<PathBuf> {
        vec![self.home.clone()]
    }

    fn scan(&self, ctx: &mut ScanCtx) -> Result<()> {
        if !self.home.is_dir() {
            return Ok(());
        }
        ctx.agent(default_agent(&self.home));
        let summaries = self.home.join("conversation_summaries.db");
        if !summaries.exists() {
            return Ok(());
        }
        ctx.progress("antigravity conversations");
        let db = open_db(&summaries)?;
        let mut stmt = db.prepare(
            "SELECT conversation_id, title, preview, step_count, last_modified_time, workspace_uris, \
             parent_conversation_id, nesting_depth, last_user_input_time FROM conversation_summaries",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
            ))
        })?;
        for row in rows {
            let Ok((id, title, preview, steps, modified, uris, parent, depth, last_input)) = row else {
                ctx.skip("antigravity: unreadable summary row");
                continue;
            };
            let key = format!("antigravity:{id}");
            let fingerprint = format!("{modified}:{steps}");
            if ctx.is_unchanged(&key, &fingerprint) {
                continue;
            }
            let path = self.conversation_db(&id);
            let updated_at = parse_time(&modified).or_else(|| parse_time(&last_input)).unwrap_or(0);
            let created_at = std::fs::metadata(&path).ok().and_then(|m| ctime_ms(&m)).unwrap_or(updated_at).min(updated_at);
            let cwd = workspace_dir(&uris);
            let title = [title, preview].into_iter().map(|t| t.trim().to_string()).find(|t| !t.is_empty());
            ctx.thread(ThreadUpsert {
                import_source: ImportSourceKind::Antigravity,
                import_key: key,
                import_path: path.to_string_lossy().into_owned(),
                fingerprint,
                harness: Harness::Antigravity,
                title: title.unwrap_or_else(|| "Antigravity conversation".into()),
                project_root: cwd.as_deref().map(project_root_for),
                cwd,
                model: None,
                session: SessionRef {
                    harness: Harness::Antigravity,
                    session_id: id,
                    path: Some(path.to_string_lossy().into_owned()),
                    home: None,
                    profile: None,
                },
                message_count: None,
                created_at,
                updated_at,
                parent_key: (!parent.is_empty()).then(|| format!("antigravity:{parent}")),
                automated: depth > 0 || !parent.is_empty(),
            });
        }
        Ok(())
    }

    fn transcript(&self, thread: &Thread) -> Result<Vec<TranscriptItem>> {
        let mut items = Vec::new();
        for (kind, payload) in self.steps(thread)? {
            step_items(kind, &payload, &mut items);
        }
        Ok(items)
    }

    fn context(&self, thread: &Thread) -> Result<Option<ContextUsage>> {
        Ok(self.steps(thread)?.iter().rev().find_map(|(kind, payload)| (*kind == 15).then(|| usage(payload)).flatten()))
    }
}

impl AntigravityImporter {
    fn steps(&self, thread: &Thread) -> Result<Vec<(i64, Vec<u8>)>> {
        let path = thread
            .session
            .path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.conversation_db(&thread.session.session_id));
        let db = open_db(&path).with_context(|| format!("antigravity conversation {}", path.display()))?;
        let mut stmt = db.prepare("SELECT step_type, step_payload FROM steps ORDER BY idx")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?.unwrap_or_default())))?;
        Ok(rows.filter_map(Result::ok).collect())
    }
}

fn step_items(kind: i64, payload: &[u8], out: &mut Vec<TranscriptItem>) {
    let ts = pb::msg(payload, &[5, 1]).and_then(|t| {
        let secs = pb::uint(t, &[1])? as i64;
        Some(secs * 1000 + pb::uint(t, &[2]).unwrap_or(0) as i64 / 1_000_000)
    });
    match kind {
        14 => {
            if let Some(text) = pb::string(payload, &[19, 2]).filter(|t| !t.trim().is_empty()) {
                out.push(TranscriptItem::User { text, ts });
            }
        }
        15 => {
            if let Some(text) = pb::string(payload, &[20, 3]).filter(|t| !t.trim().is_empty()) {
                out.push(TranscriptItem::Thinking { text, ts });
            }
            if let Some(text) = pb::string(payload, &[20, 1]).filter(|t| !t.trim().is_empty()) {
                out.push(TranscriptItem::Assistant { text, model: None, ts });
            }
        }
        132 => {
            let Some(call) = pb::msg(payload, &[5, 4]) else { return };
            let call_id = pb::string(call, &[1]);
            let name = pb::string(call, &[2]).unwrap_or_else(|| "tool".into());
            let args = pb::string(call, &[3]).unwrap_or_default();
            let input = serde_json::from_str::<serde_json::Value>(&args)
                .map(|v| crate::util::display_json(&v))
                .unwrap_or(args);
            out.push(TranscriptItem::ToolCall {
                call_id: call_id.clone(),
                name,
                input: truncate(&input, crate::util::MAX_TOOL_TEXT),
                ts,
            });
            let output = pb::string(payload, &[140, 2, 1]).unwrap_or_default();
            let is_error = output.starts_with("Command exited with code") && !output.starts_with("Command exited with code 0");
            out.push(TranscriptItem::ToolResult { call_id, output: truncate(&output, crate::util::MAX_TOOL_TEXT), is_error, ts });
        }
        17 => {
            if let Some(text) = pb::string(payload, &[24, 3, 1]).filter(|t| !t.trim().is_empty()) {
                out.push(TranscriptItem::System { text, ts });
            }
        }
        23 => out.push(TranscriptItem::System { text: "Conversation compacted.".into(), ts }),
        _ => {}
    }
}

/// Context size at an agent-response step: prompt (incl. cache reads) plus output.
fn usage(payload: &[u8]) -> Option<ContextUsage> {
    let u = pb::msg(payload, &[5, 9])?;
    let input = pb::uint(u, &[2])?;
    let used = input + pb::uint(u, &[5]).unwrap_or(0) + pb::uint(u, &[3]).unwrap_or(0);
    Some(ContextUsage { used, window: None, model: None })
}

/// `2026-10-20 13:55:03.357198+00:00` → epoch ms.
fn parse_time(s: &str) -> Option<i64> {
    crate::util::parse_ts(&s.trim().replacen(' ', "T", 1))
}

/// First `file://` workspace URI, as a path.
fn workspace_dir(uris: &str) -> Option<String> {
    let list: Vec<String> = serde_json::from_str(uris).ok()?;
    list.iter().find_map(|u| u.strip_prefix("file://")).map(percent_decode).filter(|p| p.starts_with('/'))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The CLI's own default model (`settings.json` stores its label, e.g. `Gemini 4 Argon
/// (Medium)`, which `agy models` lists as `gemini-4-argon-medium`).
fn default_agent(home: &Path) -> AgentUpsert {
    let settings = home.join("settings.json");
    let model = std::fs::read(&settings)
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|v| v["model"].as_str().map(model_slug))
        .filter(|m| !m.is_empty());
    AgentUpsert {
        import_source: ImportSourceKind::Antigravity,
        import_key: "antigravity:default".into(),
        import_path: settings.to_string_lossy().into_owned(),
        name: "antigravity (default)".into(),
        harness: Harness::Antigravity,
        model,
        effort: None,
        role: None,
        instructions: None,
        cwd: None,
        autonomy: emp_protocol::FULL_ACCESS,
        harness_profile: None,
    }
}

/// `Gemini 4 Argon (Medium)` → `gemini-4-argon-medium`.
pub fn model_slug(label: &str) -> String {
    label
        .chars()
        .filter(|c| !matches!(c, '(' | ')'))
        .collect::<String>()
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// Just enough protobuf wire-format reading to walk field paths.
mod pb {
    enum Val<'a> {
        Int(u64),
        Bytes(&'a [u8]),
        Skip,
    }

    fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
        let mut r = 0u64;
        for shift in (0..64).step_by(7) {
            let x = *b.get(*i)?;
            *i += 1;
            r |= u64::from(x & 0x7f) << shift;
            if x < 0x80 {
                return Some(r);
            }
        }
        None
    }

    /// Top-level fields of a message; `None` if it isn't valid wire format.
    fn fields(b: &[u8]) -> Option<Vec<(u64, Val<'_>)>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let key = varint(b, &mut i)?;
            let (field, wire) = (key >> 3, key & 7);
            if field == 0 {
                return None;
            }
            let v = match wire {
                0 => Val::Int(varint(b, &mut i)?),
                1 => {
                    i = i.checked_add(8).filter(|n| *n <= b.len())?;
                    Val::Skip
                }
                2 => {
                    let n = usize::try_from(varint(b, &mut i)?).ok()?;
                    let end = i.checked_add(n).filter(|e| *e <= b.len())?;
                    let v = Val::Bytes(&b[i..end]);
                    i = end;
                    v
                }
                5 => {
                    i = i.checked_add(4).filter(|n| *n <= b.len())?;
                    Val::Skip
                }
                _ => return None,
            };
            out.push((field, v));
        }
        Some(out)
    }

    /// Bytes at a field path (first occurrence at each level).
    pub fn msg<'a>(b: &'a [u8], path: &[u64]) -> Option<&'a [u8]> {
        let mut cur = b;
        for f in path {
            cur = fields(cur)?.into_iter().find_map(|(n, v)| match v {
                Val::Bytes(x) if n == *f => Some(x),
                _ => None,
            })?;
        }
        Some(cur)
    }

    pub fn string(b: &[u8], path: &[u64]) -> Option<String> {
        msg(b, path).and_then(|x| std::str::from_utf8(x).ok()).map(str::to_owned)
    }

    pub fn uint(b: &[u8], path: &[u64]) -> Option<u64> {
        let (last, parent) = path.split_last()?;
        fields(msg(b, parent)?)?.into_iter().find_map(|(n, v)| match v {
            Val::Int(x) if n == *last => Some(x),
            _ => None,
        })
    }

    #[cfg(test)]
    pub mod enc {
        /// A field: varint or length-delimited.
        pub enum F {
            I(u64, u64),
            B(u64, Vec<u8>),
        }

        fn varint(mut v: u64, out: &mut Vec<u8>) {
            while v >= 0x80 {
                out.push((v as u8) | 0x80);
                v >>= 7;
            }
            out.push(v as u8);
        }

        pub fn msg(fields: Vec<F>) -> Vec<u8> {
            let mut out = Vec::new();
            for f in fields {
                match f {
                    F::I(n, v) => {
                        varint(n << 3, &mut out);
                        varint(v, &mut out);
                    }
                    F::B(n, b) => {
                        varint((n << 3) | 2, &mut out);
                        varint(b.len() as u64, &mut out);
                        out.extend(b);
                    }
                }
            }
            out
        }

        pub fn s(n: u64, text: &str) -> F {
            F::B(n, text.as_bytes().to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pb::enc::{F, msg, s};
    use super::*;
    use rusqlite::Connection;

    fn meta(secs: u64, usage: Option<(u64, u64, u64)>) -> F {
        let mut m = vec![F::B(1, msg(vec![F::I(1, secs), F::I(2, 500_000_000)]))];
        if let Some((input, output, cache)) = usage {
            m.push(F::B(9, msg(vec![F::I(1, 1320), F::I(2, input), F::I(3, output), F::I(5, cache)])));
        }
        F::B(5, msg(m))
    }

    fn fixture(dir: &Path) -> PathBuf {
        let home = dir.join("antigravity-cli");
        std::fs::create_dir_all(home.join("conversations")).unwrap();
        std::fs::write(home.join("settings.json"), r#"{"model":"Gemini 4 Argon (Medium)","colorScheme":"x"}"#).unwrap();
        let sums = Connection::open(home.join("conversation_summaries.db")).unwrap();
        sums.execute_batch(
            "CREATE TABLE conversation_summaries (conversation_id text, title text, preview text, step_count integer, \
             last_modified_time datetime, workspace_uris text, status text, source text, parent_conversation_id text, \
             nesting_depth integer, last_user_input_time datetime);
             INSERT INTO conversation_summaries VALUES ('c1','Word Pong','Word Pong',4,'2026-10-20 13:55:03.357198+00:00',
               '[\"file:///tmp/my%20dir\"]','CASCADE_RUN_STATUS_IDLE','','',0,'2026-10-20 13:54:58.59173+00:00');
             INSERT INTO conversation_summaries VALUES ('c2','','sub',1,'2026-10-20 14:00:00+00:00','[]','','','c1',1,
               '2026-10-20 14:00:00+00:00');",
        )
        .unwrap();
        let conv = Connection::open(home.join("conversations/c1.db")).unwrap();
        conv.execute_batch("CREATE TABLE steps (idx integer primary key, step_type integer, status integer, step_payload blob)").unwrap();
        let steps = [
            (14, msg(vec![F::I(1, 14), meta(1_792_504_485, None), F::B(19, msg(vec![s(2, "Read a.txt")]))])),
            (
                15,
                msg(vec![
                    F::I(1, 15),
                    meta(1_792_504_486, Some((6800, 25, 0))),
                    F::B(20, msg(vec![s(3, "I should read it.")])),
                ]),
            ),
            (
                132,
                msg(vec![
                    F::I(1, 132),
                    F::B(5, msg(vec![F::B(4, msg(vec![s(1, "call_1"), s(2, "read_file"), s(3, r#"{"Path":"/tmp/a.txt"}"#)]))])),
                    F::B(140, msg(vec![F::B(2, msg(vec![s(1, "hello")]))])),
                ]),
            ),
            (15, msg(vec![F::I(1, 15), meta(1_792_504_488, Some((6932, 1, 100))), F::B(20, msg(vec![s(1, "hello")]))])),
            (101, msg(vec![F::I(1, 101), F::B(114, msg(vec![s(1, "[Notice] ignored")]))])),
        ];
        for (i, (kind, payload)) in steps.into_iter().enumerate() {
            conv.execute("INSERT INTO steps VALUES (?1, ?2, 3, ?3)", rusqlite::params![i as i64, kind, payload]).unwrap();
        }
        home
    }

    #[test]
    fn imports_conversations_and_reads_steps() {
        let dir = tempfile::tempdir().unwrap();
        let imp = AntigravityImporter::with_home(fixture(dir.path()));
        let mut ctx = ScanCtx::new(Default::default());
        imp.scan(&mut ctx).unwrap();
        assert_eq!(ctx.agents[0].model.as_deref(), Some("gemini-4-argon-medium"));
        let t = ctx.threads.iter().find(|t| t.import_key == "antigravity:c1").unwrap();
        assert_eq!((t.title.as_str(), t.cwd.as_deref()), ("Word Pong", Some("/tmp/my dir")));
        assert_eq!(t.updated_at, 1_792_504_503_357);
        assert!(!t.automated);
        let sub = ctx.threads.iter().find(|t| t.import_key == "antigravity:c2").unwrap();
        assert!(sub.automated && sub.parent_key.as_deref() == Some("antigravity:c1"));

        let thread = Thread {
            id: "t".into(),
            harness: Harness::Antigravity,
            title: t.title.clone(),
            project_id: None,
            cwd: t.cwd.clone(),
            model: None,
            agent_id: None,
            parent_id: None,
            child_count: 0,
            assigned_by: None,
            status: emp_protocol::ThreadStatus::Idle,
            automated: false,
            session: t.session.clone(),
            message_count: None,
            created_at: 0,
            updated_at: 0,
            imported_from: None,
        };
        let items = imp.transcript(&thread).unwrap();
        assert!(matches!(&items[0], TranscriptItem::User { text, ts: Some(1_792_504_485_500) } if text == "Read a.txt"));
        assert!(matches!(&items[1], TranscriptItem::Thinking { text, .. } if text == "I should read it."));
        assert!(matches!(&items[2], TranscriptItem::ToolCall { name, input, call_id: Some(c), .. } if name == "read_file" && input.contains("/tmp/a.txt") && c == "call_1"));
        assert!(matches!(&items[3], TranscriptItem::ToolResult { output, is_error: false, .. } if output == "hello"));
        assert!(matches!(&items[4], TranscriptItem::Assistant { text, .. } if text == "hello"));
        assert_eq!(items.len(), 5);
        assert_eq!(imp.context(&thread).unwrap().unwrap().used, 6932 + 1 + 100);
    }

    #[test]
    fn slugs_model_labels() {
        assert_eq!(model_slug("Gemini 4 Argon (Medium)"), "gemini-4-argon-medium");
        assert_eq!(model_slug("Gemini 3.8 Flash (High)"), "gemini-3.8-flash-high");
    }

    #[test]
    fn bad_protobuf_is_ignored() {
        let mut out = Vec::new();
        step_items(14, &[0xff, 0xff, 0x01], &mut out);
        step_items(132, b"not protobuf at all", &mut out);
        assert!(out.is_empty());
        assert!(usage(&[0x08]).is_none());
    }
}
