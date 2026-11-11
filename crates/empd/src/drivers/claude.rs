//! Claude Code driver: `claude -p --input-format stream-json --output-format stream-json`.
//!
//! One process serves many turns: each user message on stdin starts a turn and a `result`
//! record ends it. Messages sent mid-turn are queued by Claude itself, so steer means "send
//! now". Interrupt is the stream-json `control_request` `{subtype: "interrupt"}`.
//!
//! Permissions: agents at full access (autonomy 3, the default) run with
//! `--permission-mode bypassPermissions`, so nothing waits on the owner. Below that,
//! `acceptEdits` allows file edits in the working folders and anything else Claude would ask
//! about comes to EMP (`--permission-prompt-tool stdio`, a `control_request {subtype:
//! "can_use_tool"}`) as a question card: Allow, Allow for this chat (Claude's own suggestions,
//! kept in the session and in the thread's grants), Deny. Requests that still arrive at full
//! access are allowed without asking. Folders the owner gives a chat (`/add-dir`,
//! a dropped folder) are applied live with `apply_flag_settings`; grants come back as
//! `--settings` when EMP starts the process again.

use std::collections::HashMap;

use anyhow::Result;
use emp_protocol::{PartialKind, Question, QuestionKind, TranscriptItem};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Caps, DriverCmd, DriverEvent, DriverHandle, DriverSpec, Partials, Proc};

const CAPS: Caps = Caps { native_steer: true, native_follow_up: false, echoes_user: false };

pub fn args(spec: &DriverSpec) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        if spec.autonomy >= emp_protocol::FULL_ACCESS { "bypassPermissions" } else { "acceptEdits" },
        "--permission-prompt-tool",
        "stdio",
    ]
    .map(String::from)
    .to_vec();
    if !spec.grants.is_empty() {
        let settings = json!({"permissions": {"allow": spec.grants.rules, "additionalDirectories": spec.grants.dirs}});
        args.extend(["--settings".to_string(), settings.to_string()]);
    }
    match &spec.resume {
        Some(s) => {
            args.push("--resume".into());
            args.push(s.session_id.clone());
        }
        None => {
            if let Some(m) = &spec.model {
                args.push("--model".into());
                args.push(m.clone());
            }
            if let Some(i) = spec.instructions.as_deref().filter(|i| !i.trim().is_empty()) {
                args.push("--append-system-prompt".into());
                args.push(i.into());
            }
        }
    }
    if let Some(g) = &spec.mcp {
        // The coordinator: only the `emp` server, its tools pre-approved, no shell or edits.
        let config = json!({"mcpServers": {"emp": {"type": "http", "url": g.url, "headers": {"Authorization": format!("Bearer {}", g.token)}}}});
        args.extend(["--mcp-config".to_string(), config.to_string(), "--strict-mcp-config".into(), "--allowedTools".into(), "mcp__emp".into()]);
        args.extend(["--disallowedTools", "Bash", "Edit", "Write", "NotebookEdit"].map(String::from));
    }
    args
}

fn user_message(m: &super::Msg) -> Value {
    let mut content: Vec<Value> = super::inline_images(&m.images)
        .into_iter()
        .map(|(mime, data)| json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}}))
        .collect();
    content.push(json!({"type": "text", "text": m.text}));
    json!({"type": "user", "message": {"role": "user", "content": content}})
}

/// Claude Code's own auto-compact window (it accepts 100k–1M). Applies for the life of this
/// process, i.e. from the next time EMP starts the session.
pub fn env(spec: &DriverSpec) -> Vec<(&'static str, String)> {
    let mut env: Vec<(&'static str, String)> =
        spec.compact_at.get().map(|n| vec![("CLAUDE_CODE_AUTO_COMPACT_WINDOW", n.clamp(100_000, 1_000_000).to_string())]).unwrap_or_default();
    if spec.mcp.is_some() {
        // `wait_for_threads` can take minutes.
        env.push(("MCP_TOOL_TIMEOUT", "900000".into()));
    }
    env
}

pub fn spawn(spec: DriverSpec) -> Result<(DriverHandle, mpsc::UnboundedReceiver<DriverEvent>)> {
    let proc = Proc::spawn_env("claude", &args(&spec), &spec.cwd, &env(&spec))?;
    Ok(spawn_proc(spec, proc))
}

fn spawn_proc(spec: DriverSpec, mut proc: Proc) -> (DriverHandle, mpsc::UnboundedReceiver<DriverEvent>) {
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let pid = proc.pid;
    if let Some(s) = &spec.resume {
        let _ = ev_tx.send(DriverEvent::Ready { import_key: format!("claude:{}", s.session_id) });
    }
    tokio::spawn(async move {
        let mut mapper = Mapper { autonomy: spec.autonomy, dirs: spec.grants.dirs.clone(), ..Mapper::default() };
        let mut seq = 0u64;
        let mut shutting_down = false;
        loop {
            tokio::select! {
                cmd = cmd_rx.recv(), if !shutting_down => {
                    let rec = match cmd {
                        Some(DriverCmd::Prompt(t)) => {
                            let _ = ev_tx.send(DriverEvent::TurnStarted);
                            user_message(&t)
                        }
                        Some(DriverCmd::Steer(t) | DriverCmd::FollowUp(t)) => user_message(&t),
                        Some(DriverCmd::Interrupt) => {
                            seq += 1;
                            json!({"type": "control_request", "request_id": format!("emp-int-{seq}"), "request": {"subtype": "interrupt"}})
                        }
                        Some(DriverCmd::SetModel(m)) => {
                            seq += 1;
                            let _ = ev_tx.send(DriverEvent::Item(system(format!("Model: {m}"))));
                            json!({"type": "control_request", "request_id": format!("emp-model-{seq}"), "request": {"subtype": "set_model", "model": m}})
                        }
                        Some(DriverCmd::Answer { id, value, cancelled, .. }) => {
                            let (events, reply) = mapper.answer(&id, value.as_deref(), cancelled);
                            for e in events {
                                let _ = ev_tx.send(e);
                            }
                            match reply {
                                Some(r) => r,
                                None => continue,
                            }
                        }
                        Some(DriverCmd::Command { name, args }) => {
                            seq += 1;
                            let (events, rec) = mapper.command(&name, &args, seq);
                            for e in events {
                                let _ = ev_tx.send(e);
                            }
                            match rec {
                                Some(r) => r,
                                None => continue,
                            }
                        }
                        Some(DriverCmd::Shutdown) | None => {
                            shutting_down = true;
                            break;
                        }
                    };
                    if let Err(e) = proc.send(&rec).await {
                        let _ = ev_tx.send(DriverEvent::Item(system(format!("claude: could not write: {e}"))));
                    }
                }
                rec = proc.next() => {
                    let Some(rec) = rec else { break };
                    for e in mapper.handle(&rec) {
                        let _ = ev_tx.send(e);
                    }
                    for reply in std::mem::take(&mut mapper.replies) {
                        let _ = proc.send(&reply).await;
                    }
                }
            }
        }
        cmd_rx.close();
        proc.shutdown().await;
        let status = proc.child.wait().await.ok();
        let error = (!shutting_down).then(|| {
            let code = status.and_then(|s| s.code()).map_or("signal".into(), |c| c.to_string());
            format!("claude exited ({code}): {}", proc.stderr_summary())
        });
        let _ = ev_tx.send(DriverEvent::Exited(error));
    });
    (DriverHandle { cmds: cmd_tx, pid, caps: CAPS }, ev_rx)
}

fn permission_reply(rid: &str, response: Value) -> Value {
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": rid, "response": response}})
}

/// The folders and allow rules in one of Claude's permission suggestions.
fn collect_grants(s: &Value, g: &mut emp_core::Grants) {
    match s["type"].as_str() {
        Some("addDirectories") => {
            g.dirs.extend(s["directories"].as_array().into_iter().flatten().filter_map(|d| d.as_str().map(String::from)));
        }
        Some("addRules" | "replaceRules") if s["behavior"] == "allow" => {
            for r in s["rules"].as_array().into_iter().flatten() {
                let Some(tool) = r["toolName"].as_str() else { continue };
                g.rules.push(match r["ruleContent"].as_str() {
                    Some(c) => format!("{tool}({c})"),
                    None => tool.to_string(),
                });
            }
        }
        _ => {}
    }
}

fn system(text: String) -> TranscriptItem {
    TranscriptItem::System { text, ts: Some(emp_core::now_ms()) }
}

/// Question ids for Claude's permission requests: `claude:<request_id>`.
const Q_CLAUDE: &str = "claude:";
const ALLOW: &str = "Allow";
const ALLOW_CHAT: &str = "Allow for this chat";
const DENY: &str = "Deny";

#[derive(Default)]
pub struct Mapper {
    partials: Partials,
    ready: bool,
    pub autonomy: u8,
    /// Permission requests waiting on the owner, by question id.
    asks: HashMap<String, Value>,
    /// Folders given to this process (sent whole: `apply_flag_settings` replaces the list).
    pub dirs: Vec<String>,
    /// Records to write back to Claude after `handle`.
    pub replies: Vec<Value>,
}

impl Mapper {
    /// `can_use_tool`: a card, or an automatic yes at autonomy 3.
    fn permission(&mut self, rid: &str, req: &Value, out: &mut Vec<DriverEvent>) {
        if self.autonomy >= 3 {
            self.replies.push(permission_reply(rid, json!({"behavior": "allow", "updatedInput": req["input"]})));
            return;
        }
        let id = format!("{Q_CLAUDE}{rid}");
        let tool = req["tool_name"].as_str().unwrap_or("a tool");
        let input = &req["input"];
        let (title, mut message) = match tool {
            "Bash" => ("Run this command?", input["command"].as_str().unwrap_or("").to_string()),
            "Read" | "Glob" | "Grep" | "LS" | "NotebookRead" => (
                "Let Claude read this?",
                [input["file_path"].as_str(), input["pattern"].as_str(), input["path"].as_str()].into_iter().flatten().collect::<Vec<_>>().join("\n"),
            ),
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
                ("Let Claude change this file?", input["file_path"].as_str().or(input["notebook_path"].as_str()).unwrap_or("").to_string())
            }
            "WebFetch" | "WebSearch" => ("Let Claude go online?", input["url"].as_str().or(input["query"].as_str()).unwrap_or("").to_string()),
            _ => ("", emp_import::util::truncate(&emp_import::util::display_json(input), 600)),
        };
        let title = if title.is_empty() {
            format!("Let Claude use {}?", req["display_name"].as_str().unwrap_or(tool))
        } else {
            title.to_string()
        };
        if let Some(d) = req["description"].as_str().filter(|d| !d.is_empty() && !message.contains(d)) {
            message = format!("{message}\n\n{d}");
        }
        if let Some(p) = req["blocked_path"].as_str() {
            message = format!("{message}\n\nOutside this chat's folders: {p}");
        }
        if let Some(r) = req["decision_reason"].as_str().filter(|r| !r.is_empty()) {
            message = format!("{message}\n\n{r}");
        }
        let lasting = req["permission_suggestions"].as_array().is_some_and(|s| !s.is_empty());
        let options = if lasting { vec![ALLOW, ALLOW_CHAT, DENY] } else { vec![ALLOW, DENY] };
        self.asks.insert(id.clone(), req.clone());
        out.push(DriverEvent::Ask(Question {
            id,
            kind: QuestionKind::Select,
            title,
            message: Some(message.trim().to_string()).filter(|m| !m.is_empty()),
            options: options.into_iter().map(String::from).collect(),
            placeholder: None,
            prefill: None,
        }));
    }

    /// The owner's answer to a permission card: the reply to Claude, and the lasting grants
    /// when they chose "Allow for this chat".
    pub fn answer(&mut self, id: &str, value: Option<&str>, cancelled: bool) -> (Vec<DriverEvent>, Option<Value>) {
        let Some(req) = self.asks.remove(id) else { return (Vec::new(), None) };
        let rid = id.strip_prefix(Q_CLAUDE).unwrap_or(id);
        let mut events = vec![DriverEvent::Unask(id.to_string())];
        let choice = if cancelled { DENY } else { value.unwrap_or(DENY) };
        let response = match choice {
            ALLOW => json!({"behavior": "allow", "updatedInput": req["input"]}),
            ALLOW_CHAT => {
                // Claude suggests where to keep a rule (`localSettings` writes the project's
                // .claude/settings.local.json); EMP keeps them in the session and the thread.
                let mut grants = emp_core::Grants::default();
                let suggestions: Vec<Value> = req["permission_suggestions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        collect_grants(s, &mut grants);
                        let mut s = s.clone();
                        s["destination"] = json!("session");
                        s
                    })
                    .collect();
                if !grants.is_empty() {
                    events.push(DriverEvent::Granted(grants));
                }
                json!({"behavior": "allow", "updatedInput": req["input"], "updatedPermissions": suggestions})
            }
            _ => json!({"behavior": "deny", "message": "The owner declined this in EMP."}),
        };
        (events, Some(permission_reply(rid, response)))
    }

    /// Native commands: `/add-dir <absolute path>` (the live layer resolves and checks it).
    pub fn command(&mut self, name: &str, args: &str, seq: u64) -> (Vec<DriverEvent>, Option<Value>) {
        match name {
            "add-dir" if !args.is_empty() => {
                if !self.dirs.iter().any(|d| d == args) {
                    self.dirs.push(args.to_string());
                }
                let rec = json!({"type": "control_request", "request_id": format!("emp-dir-{seq}"), "request": {
                    "subtype": "apply_flag_settings",
                    "settings": {"permissions": {"additionalDirectories": self.dirs}},
                }});
                (vec![DriverEvent::Item(system(format!("Claude can now use {args}")))], Some(rec))
            }
            other => (vec![DriverEvent::Item(system(format!("/{other} doesn't run in chat; EMP opens it in a terminal")))], None),
        }
    }

    pub fn handle(&mut self, rec: &Value) -> Vec<DriverEvent> {
        let mut out = Vec::new();
        match rec["type"].as_str().unwrap_or("") {
            "system" if rec["subtype"] == "init" && !self.ready => {
                if let Some(id) = rec["session_id"].as_str() {
                    self.ready = true;
                    out.push(DriverEvent::Ready { import_key: format!("claude:{id}") });
                }
                if rec["slash_commands"].is_array() {
                    out.push(DriverEvent::Commands { key: "claude".into(), commands: crate::commands::from_claude_init(rec) });
                }
            }
            "stream_event" => {
                let ev = &rec["event"];
                match ev["type"].as_str().unwrap_or("") {
                    "message_start" => self.partials.clear(),
                    "content_block_delta" => {
                        let index = ev["index"].as_i64().unwrap_or(0);
                        let d = &ev["delta"];
                        match d["type"].as_str().unwrap_or("") {
                            "text_delta" => out.push(DriverEvent::Partial(
                                PartialKind::Assistant,
                                self.partials.push(index, PartialKind::Assistant, d["text"].as_str().unwrap_or("")),
                            )),
                            "thinking_delta" => out.push(DriverEvent::Partial(
                                PartialKind::Thinking,
                                self.partials.push(index, PartialKind::Thinking, d["thinking"].as_str().unwrap_or("")),
                            )),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            "assistant" | "user" => {
                // Subagent traffic (Task tool) carries a parent tool id; the main thread shows
                // only the Task call and its result.
                if rec["parent_tool_use_id"].is_string() {
                    return out;
                }
                self.partials.clear();
                let mut items = Vec::new();
                emp_import::claude::record_items(rec, &mut items);
                out.extend(items.into_iter().map(DriverEvent::Item));
            }
            "control_request" if rec["request"]["subtype"] == "can_use_tool" => {
                let rid = rec["request_id"].as_str().unwrap_or("").to_string();
                self.permission(&rid, &rec["request"], &mut out);
            }
            "control_request" => {
                // Nothing else is expected from Claude; answer so it doesn't wait.
                let rid = rec["request_id"].as_str().unwrap_or("");
                self.replies.push(json!({"type": "control_response", "response": {"subtype": "error", "request_id": rid, "error": "EMP doesn't handle this request"}}));
            }
            "control_cancel_request" => {
                let id = format!("{Q_CLAUDE}{}", rec["request_id"].as_str().unwrap_or(""));
                if self.asks.remove(&id).is_some() {
                    out.push(DriverEvent::Unask(id));
                }
            }
            "result" => {
                self.partials.clear();
                // A finished turn asks nothing more.
                for (id, _) in self.asks.drain() {
                    out.push(DriverEvent::Unask(id));
                }
                let aborted = rec["subtype"] != "success" && rec["stop_reason"] != "end_turn";
                if rec["is_error"] == true {
                    let msg = rec["result"].as_str().or(rec["subtype"].as_str()).unwrap_or("error");
                    out.push(DriverEvent::Item(system(format!("Claude: {}", emp_import::util::truncate(msg, 500)))));
                    out.push(DriverEvent::TurnFailed(format!("Claude: {}", emp_import::util::truncate(msg, 500))));
                }
                out.push(DriverEvent::TurnEnded { aborted });
            }
            "control_response" if rec["response"]["subtype"] == "error" => {
                let msg = rec["response"]["error"].as_str().unwrap_or("refused");
                out.push(DriverEvent::Item(system(format!("Claude: {}", emp_import::util::truncate(msg, 300)))));
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `claude -p --input-format stream-json …` run (Claude Code 2.1.303).
    const RUN: &str = include_str!("fixtures/claude-stream.jsonl");

    #[tokio::test]
    async fn shutdown_finishes_when_the_harness_stays_alive_after_stdin_eof() {
        let dir = tempfile::tempdir().unwrap();
        let spec = DriverSpec {
            harness: emp_protocol::Harness::Claude,
            cwd: dir.path().into(),
            resume: None,
            model: None,
            effort: None,
            instructions: None,
            profile: None,
            autonomy: 1,
            compact_at: crate::state::CompactAt::none(),
            mcp: None,
            grants: Default::default(),
        };
        // Like Claude with a background task: reads stdin, then keeps stdout open after EOF.
        let script = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"shutdown-test"}'; cat >/dev/null; while :; do sleep 1; done"#;
        let proc = Proc::spawn("/bin/sh", &["-c".into(), script.into()], dir.path()).unwrap();
        let (handle, mut events) = spawn_proc(spec, proc);
        let result = tokio::time::timeout(std::time::Duration::from_secs(6), async {
            assert!(matches!(events.recv().await, Some(DriverEvent::Ready { .. })));
            handle.cmds.send(DriverCmd::Shutdown).unwrap();
            loop {
                match events.recv().await {
                    Some(DriverEvent::Exited(error)) => break error,
                    Some(_) => {}
                    None => panic!("driver closed without reporting exit"),
                }
            }
        }).await;
        if result.is_err() {
            super::super::kill_group(handle.pid.unwrap());
        }
        assert!(result.unwrap().is_none(), "requested shutdown is not a harness failure");
        assert!(handle.cmds.is_closed(), "retired drivers must reject later prompts");
    }

    #[test]
    fn maps_a_real_three_turn_run() {
        let mut m = Mapper::default();
        let events: Vec<DriverEvent> =
            RUN.lines().filter(|l| !l.trim().is_empty()).flat_map(|l| m.handle(&serde_json::from_str(l).unwrap())).collect();
        assert!(matches!(&events[0], DriverEvent::Ready { import_key } if import_key.starts_with("claude:")));
        assert_eq!(events.iter().filter(|e| matches!(e, DriverEvent::Ready { .. })).count(), 1);
        assert_eq!(events.iter().filter(|e| matches!(e, DriverEvent::TurnEnded { aborted: false })).count(), 3);
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                DriverEvent::Item(TranscriptItem::Assistant { text, .. }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts.first(), Some(&"pong"));
        assert_eq!(texts.last(), Some(&"ok"));
        assert!(events.iter().any(|e| matches!(e, DriverEvent::Partial(PartialKind::Assistant, _))));
    }

    #[test]
    fn errors_surface_and_subagent_traffic_is_hidden() {
        let mut m = Mapper::default();
        let ev = m.handle(&json!({"type":"result","subtype":"error_during_execution","is_error":true,"result":"boom"}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text.contains("boom")));
        assert!(matches!(&ev[1], DriverEvent::TurnFailed(e) if e == "Claude: boom"));
        assert!(matches!(&ev[2], DriverEvent::TurnEnded { aborted: true }));
        let ev = m.handle(&json!({"type":"assistant","parent_tool_use_id":"toolu_1","message":{"content":[{"type":"text","text":"sub"}]}}));
        assert!(ev.is_empty());
        // A model switch Claude refused.
        let ev = m.handle(&json!({"type":"control_response","response":{"subtype":"error","request_id":"emp-model-1","error":"unknown model"}}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text == "Claude: unknown model"));
        assert!(m.handle(&json!({"type":"control_response","response":{"subtype":"success","request_id":"emp-model-2"}})).is_empty());
    }

    fn can_use_tool(rid: &str, input: Value, suggestions: Value) -> Value {
        json!({"type": "control_request", "request_id": rid, "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "display_name": "Bash", "input": input,
            "description": "List the game folder", "permission_suggestions": suggestions,
            "blocked_path": "/Users/demo/Projects/sample project",
        }})
    }

    #[test]
    fn permission_requests_become_cards() {
        let mut m = Mapper { autonomy: 1, ..Mapper::default() };
        let input = json!({"command": "ls \"/Users/demo/Projects/sample project\""});
        let suggestions = json!([
            {"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "ls:*"}], "behavior": "allow", "destination": "localSettings"},
            {"type": "addDirectories", "directories": ["/Users/demo/Projects/sample project"], "destination": "session"}
        ]);
        let ev = m.handle(&can_use_tool("r1", input.clone(), suggestions));
        let DriverEvent::Ask(q) = &ev[0] else { panic!("no card") };
        assert_eq!((q.id.as_str(), q.title.as_str()), ("claude:r1", "Run this command?"));
        assert_eq!(q.options, [ALLOW, ALLOW_CHAT, DENY]);
        assert!(q.message.as_deref().unwrap().ends_with("Outside this chat's folders: /Users/demo/Projects/sample project"));
        assert!(m.replies.is_empty());

        // Allow for this chat: Claude keeps the rules for the session, EMP for the thread.
        let (ev, reply) = m.answer("claude:r1", Some(ALLOW_CHAT), false);
        let reply = reply.unwrap();
        assert_eq!(reply["response"]["request_id"], "r1");
        let r = &reply["response"]["response"];
        assert_eq!((r["behavior"].as_str(), &r["updatedInput"]), (Some("allow"), &input));
        assert!(r["updatedPermissions"].as_array().unwrap().iter().all(|s| s["destination"] == "session"));
        assert!(matches!(&ev[0], DriverEvent::Unask(id) if id == "claude:r1"));
        let DriverEvent::Granted(g) = &ev[1] else { panic!("no grant") };
        assert_eq!(g, &emp_core::Grants { dirs: vec!["/Users/demo/Projects/sample project".into()], rules: vec!["Bash(ls:*)".into()] });
        // Answered once only.
        assert!(m.answer("claude:r1", Some(ALLOW), false).1.is_none());

        // Deny, and dismissing the card, both say no.
        m.handle(&can_use_tool("r2", json!({"command": "rm -rf x"}), json!([])));
        let (_, reply) = m.answer("claude:r2", None, true);
        assert_eq!(reply.unwrap()["response"]["response"]["behavior"], "deny");

        // Withdrawn by Claude, or the turn ended: the card goes.
        m.handle(&can_use_tool("r3", json!({}), json!([])));
        assert!(matches!(&m.handle(&json!({"type": "control_cancel_request", "request_id": "r3"}))[0], DriverEvent::Unask(id) if id == "claude:r3"));
        let DriverEvent::Ask(q) = &m.handle(&can_use_tool("r4", json!({}), json!([])))[0] else { panic!() };
        assert_eq!(q.options, [ALLOW, DENY], "nothing to keep without suggestions");
        assert!(matches!(&m.handle(&json!({"type": "result", "subtype": "success"}))[0], DriverEvent::Unask(id) if id == "claude:r4"));

        // Full autonomy: allowed without a card.
        let mut auto = Mapper { autonomy: 3, ..Mapper::default() };
        assert!(auto.handle(&can_use_tool("r5", json!({"command": "ls"}), json!([]))).is_empty());
        assert_eq!(auto.replies[0]["response"]["response"]["behavior"], "allow");
    }

    #[test]
    fn folders_are_given_live_and_on_start() {
        let mut m = Mapper { dirs: vec!["/a".into()], ..Mapper::default() };
        let (ev, rec) = m.command("add-dir", "/b", 4);
        let rec = rec.unwrap();
        assert_eq!(rec["request"]["subtype"], "apply_flag_settings");
        assert_eq!(rec["request"]["settings"]["permissions"]["additionalDirectories"], json!(["/a", "/b"]));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text == "Claude can now use /b"));

        let spec = DriverSpec {
            harness: emp_protocol::Harness::Claude,
            cwd: "/tmp".into(),
            resume: None,
            model: None,
            effort: None,
            instructions: None,
            profile: None,
            autonomy: 1,
            compact_at: crate::state::CompactAt::none(),
            mcp: None,
            grants: emp_core::Grants { dirs: vec!["/a".into()], rules: vec!["Bash(ls:*)".into()] },
        };
        let a = args(&spec);
        let i = a.iter().position(|x| x == "--settings").unwrap();
        let s: Value = serde_json::from_str(&a[i + 1]).unwrap();
        assert_eq!(s, json!({"permissions": {"allow": ["Bash(ls:*)"], "additionalDirectories": ["/a"]}}));
        assert!(a.windows(2).any(|w| w == ["--permission-prompt-tool", "stdio"]));
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "acceptEdits"]));
        // Full access: nothing is asked.
        let spec = DriverSpec { autonomy: emp_protocol::FULL_ACCESS, ..spec };
        assert!(args(&spec).windows(2).any(|w| w == ["--permission-mode", "bypassPermissions"]));
    }

    #[test]
    fn resume_keeps_the_session_model() {
        let spec = DriverSpec {
            harness: emp_protocol::Harness::Claude,
            cwd: "/tmp".into(),
            resume: Some(emp_protocol::SessionRef {
                harness: emp_protocol::Harness::Claude,
                session_id: "sid".into(),
                path: None,
                home: None,
                profile: None,
            }),
            model: Some("opus".into()),
            effort: None,
            instructions: None,
            profile: None,
            autonomy: 1,
            compact_at: crate::state::CompactAt::none(),
            mcp: None,
            grants: Default::default(),
        };
        let a = args(&spec);
        assert!(a.ends_with(&["--resume".to_string(), "sid".to_string()]));
        assert!(!a.contains(&"--model".to_string()));
    }

    #[test]
    fn compaction_limit_becomes_claudes_window_env() {
        let mut spec = DriverSpec {
            harness: emp_protocol::Harness::Claude,
            cwd: "/tmp".into(),
            resume: None,
            model: None,
            effort: None,
            instructions: None,
            profile: None,
            autonomy: 1,
            compact_at: crate::state::CompactAt::fixed(150_000),
            mcp: None,
            grants: Default::default(),
        };
        assert_eq!(env(&spec), [("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "150000".to_string())]);
        spec.compact_at = crate::state::CompactAt::fixed(60_000);
        assert_eq!(env(&spec)[0].1, "100000", "raised to Claude's minimum");
        spec.compact_at = crate::state::CompactAt::none();
        assert!(env(&spec).is_empty());
    }
}
