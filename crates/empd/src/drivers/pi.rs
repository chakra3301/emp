//! pi driver: `pi --mode rpc` (pi 1.3; docs/rpc.md, rpc-commands.md, json.md in the pi package).

use anyhow::Result;
use emp_protocol::{PartialKind, TranscriptItem};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Caps, DriverCmd, DriverEvent, DriverHandle, DriverSpec, Partials, Proc, text_of};

const CAPS: Caps = Caps { native_steer: true, native_follow_up: true, echoes_user: true };

const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn args(spec: &DriverSpec) -> Vec<String> {
    let mut args = vec!["--mode".to_string(), "rpc".to_string()];
    match &spec.resume {
        Some(s) => {
            args.push("--session".into());
            args.push(s.path.clone().unwrap_or_else(|| s.session_id.clone()));
        }
        None => {
            if let Some(m) = &spec.model {
                args.push("--model".into());
                args.push(m.clone());
            }
            if let Some(e) = spec.effort.as_deref().filter(|e| THINKING_LEVELS.contains(e)) {
                args.push("--thinking".into());
                args.push(e.into());
            }
            if let Some(i) = spec.instructions.as_deref().filter(|i| !i.trim().is_empty()) {
                args.push("--append-system-prompt".into());
                args.push(i.into());
            }
        }
    }
    if spec.mcp.is_some() {
        // The coordinator: read-only file tools plus the `emp` server from its EMP-owned
        // folder's `.pi/mcp.json`, trusted for this process only (`-a`), never in trust.json.
        args.extend(["-a", "--tools", "read,grep,find,ls,mcp__emp__*"].map(String::from));
    }
    args
}

fn env(spec: &DriverSpec) -> Vec<(&'static str, String)> {
    spec.mcp.iter().map(|g| (super::MCP_TOKEN_ENV, g.token.clone())).collect()
}

/// `.pi/mcp.json` for the coordinator's folder: the `emp` server with direct tools, a long
/// timeout for `wait_for_threads`, and the token taken from the environment.
pub fn mcp_config(url: &str) -> Value {
    json!({
        "mcpServers": {
            "emp": {
                "url": url,
                "headers": { "Authorization": format!("Bearer ${{{}}}", super::MCP_TOKEN_ENV) },
                "exposure": "direct",
                "timeout": 900,
                "description": "EMP: the owner's agents, threads, projects and dev servers"
            }
        }
    })
}

pub fn spawn(spec: DriverSpec) -> Result<(DriverHandle, mpsc::UnboundedReceiver<DriverEvent>)> {
    let mut proc = Proc::spawn_env("pi", &args(&spec), &spec.cwd, &env(&spec))?;
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let pid = proc.pid;
    let compact_at = spec.compact_at.clone();
    tokio::spawn(async move {
        let mut mapper = Mapper { compact_at, ..Mapper::default() };
        let mut seq = 0u64;
        let _ = proc.send(&json!({"id": "emp-state", "type": "get_state"})).await;
        let mut shutting_down = false;
        loop {
            tokio::select! {
                cmd = cmd_rx.recv(), if !shutting_down => {
                    let (kind, message) = match cmd {
                        Some(DriverCmd::Prompt(t)) => ("prompt", Some(t)),
                        Some(DriverCmd::Steer(t)) => ("steer", Some(t)),
                        Some(DriverCmd::FollowUp(t)) => ("follow_up", Some(t)),
                        Some(DriverCmd::Interrupt) => ("abort", None),
                        Some(DriverCmd::Command { name, args }) => {
                            let (events, rec) = mapper.command(&name, &args);
                            for e in events {
                                let _ = ev_tx.send(e);
                            }
                            if let Some(rec) = rec {
                                let _ = proc.send(&rec).await;
                            }
                            continue;
                        }
                        // pi answers with "Model: provider/id" (or why not) and records it in the session.
                        Some(DriverCmd::SetModel(m)) => {
                            let (events, rec) = mapper.command("model", &m);
                            for e in events {
                                let _ = ev_tx.send(e);
                            }
                            if let Some(rec) = rec {
                                let _ = proc.send(&rec).await;
                            }
                            continue;
                        }
                        Some(DriverCmd::Answer { id, value, confirmed, cancelled }) => {
                            if let Some(rec) = mapper.answer(&id, value, confirmed, cancelled) {
                                let _ = proc.send(&rec).await;
                            }
                            continue;
                        }
                        Some(DriverCmd::Shutdown) | None => {
                            shutting_down = true;
                            break;
                        }
                    };
                    seq += 1;
                    let mut rec = json!({"id": format!("emp-{seq}"), "type": kind});
                    if let Some(m) = message {
                        rec["message"] = Value::String(m.text);
                        let images = super::inline_images(&m.images);
                        if !images.is_empty() {
                            rec["images"] = images.into_iter().map(|(mime, data)| json!({"type": "image", "data": data, "mimeType": mime})).collect();
                        }
                    }
                    if let Err(e) = proc.send(&rec).await {
                        let _ = ev_tx.send(DriverEvent::Item(system(format!("pi: could not send {kind}: {e}"))));
                    }
                }
                rec = proc.next() => {
                    let Some(rec) = rec else { break };
                    let (events, reply) = mapper.handle(&rec);
                    if let Some(reply) = reply {
                        let _ = proc.send(&reply).await;
                    }
                    for e in events {
                        let _ = ev_tx.send(e);
                    }
                }
            }
        }
        cmd_rx.close();
        proc.shutdown().await;
        let status = proc.child.wait().await.ok();
        let error = (!shutting_down).then(|| {
            let code = status.and_then(|s| s.code()).map_or("signal".into(), |c| c.to_string());
            format!("pi exited ({code}): {}", proc.stderr_summary())
        });
        let _ = ev_tx.send(DriverEvent::Exited(error));
    });
    Ok((DriverHandle { cmds: cmd_tx, pid, caps: CAPS }, ev_rx))
}

fn system(text: String) -> TranscriptItem {
    TranscriptItem::System { text, ts: Some(emp_core::now_ms()) }
}

const ID_STATS: &str = "emp-stats";
const ID_COMPACT: &str = "emp-compact";

/// Maps pi RPC stdout records to driver events, plus an optional record to write back.
///
/// Compaction limit: when a turn settles and a limit is set, the turn's end is held back while
/// EMP asks pi for the context size (`get_session_stats`) and, if it's over, sends `compact`.
/// pi's own auto-compaction (near the full window) stays on as a backstop.
#[derive(Default)]
pub struct Mapper {
    partials: Partials,
    pub compact_at: crate::state::CompactAt,
    /// A settled turn waiting on the compaction check: `(limit, tokens before)`.
    held: Option<(u64, Option<u64>)>,
}

/// Prefix of the ids of records EMP sends for native slash commands.
const ID_CMD: &str = "emp-cmd-";
/// Ids of questions EMP itself asks (answers go to [`Mapper::answer`], not to pi).
const Q_MODEL: &str = "emp:model";
const Q_THINKING: &str = "emp:thinking";

impl Mapper {
    /// The RPC record for a native slash command (`CommandRun::Native`), or a message saying
    /// why there is none.
    pub fn command(&mut self, name: &str, args: &str) -> (Vec<DriverEvent>, Option<Value>) {
        let args = args.trim();
        let id = |what: &str| format!("{ID_CMD}{what}");
        let rec = match name {
            "compact" if args.is_empty() => json!({"id": id("compact"), "type": "compact"}),
            "compact" => json!({"id": id("compact"), "type": "compact", "customInstructions": args}),
            "model" if args.is_empty() => json!({"id": id("models"), "type": "get_available_models"}),
            "model" => match set_model(args, &id("model")) {
                Some(rec) => rec,
                None => return (vec![DriverEvent::Item(system(format!("/model takes provider/model, like anthropic/claude-sonnet-5-5 (got {args})")))], None),
            },
            "thinking" if args.is_empty() => json!({"id": id("levels"), "type": "get_available_thinking_levels"}),
            "thinking" => json!({"id": id("thinking"), "type": "set_thinking_level", "level": args}),
            "name" if args.is_empty() => json!({"id": id("name-get"), "type": "get_state"}),
            "name" => json!({"id": id("name"), "type": "set_session_name", "name": args}),
            "session" => json!({"id": id("session"), "type": "get_session_stats"}),
            "export" if args.is_empty() => json!({"id": id("export"), "type": "export_html"}),
            "export" => json!({"id": id("export"), "type": "export_html", "outputPath": args}),
            other => return (vec![DriverEvent::Item(system(format!("/{other} doesn't run in chat; EMP opens it in a terminal")))], None),
        };
        (Vec::new(), Some(rec))
    }

    /// The record that answers question `id`: EMP's own pickers turn into commands, the rest
    /// go back to pi as `extension_ui_response`.
    pub fn answer(&mut self, id: &str, value: Option<String>, confirmed: Option<bool>, cancelled: bool) -> Option<Value> {
        match id {
            Q_MODEL => value.filter(|_| !cancelled).and_then(|v| set_model(&v, &format!("{ID_CMD}model"))),
            Q_THINKING => value.filter(|_| !cancelled).map(|v| json!({"id": format!("{ID_CMD}thinking"), "type": "set_thinking_level", "level": v})),
            _ if cancelled => Some(json!({"type": "extension_ui_response", "id": id, "cancelled": true})),
            _ => Some(match (confirmed, value) {
                (Some(c), _) => json!({"type": "extension_ui_response", "id": id, "confirmed": c}),
                (None, Some(v)) => json!({"type": "extension_ui_response", "id": id, "value": v}),
                (None, None) => json!({"type": "extension_ui_response", "id": id, "cancelled": true}),
            }),
        }
    }

    /// Replies to [`Mapper::command`] records.
    fn command_reply(&mut self, what: &str, rec: &Value, out: &mut Vec<DriverEvent>) {
        let data = &rec["data"];
        if rec["success"] != true {
            let err = rec["error"].as_str().unwrap_or("pi refused");
            out.push(DriverEvent::Item(system(format!("/{}: {}", what.trim_end_matches("-get"), err.lines().next().unwrap_or(err)))));
            return;
        }
        let text = match what {
            "compact" => {
                let before = data["tokensBefore"].as_u64().unwrap_or(0);
                match data["estimatedTokensAfter"].as_u64() {
                    Some(after) => format!("Compacted: {} → about {} tokens.", crate::state::ktok(before), crate::state::ktok(after)),
                    None => format!("Compacted from {} tokens.", crate::state::ktok(before)),
                }
            }
            "models" => {
                let options: Vec<String> = data["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| Some(format!("{}/{}", m["provider"].as_str()?, m["id"].as_str()?)))
                    .collect();
                out.push(DriverEvent::Ask(question(Q_MODEL, emp_protocol::QuestionKind::Select, "Switch this chat to", options)));
                return;
            }
            "levels" => {
                let options = strings(&data["levels"]);
                out.push(DriverEvent::Ask(question(Q_THINKING, emp_protocol::QuestionKind::Select, "Thinking level", options)));
                return;
            }
            "model" => {
                out.push(DriverEvent::SessionChanged);
                format!("Model: {}/{}", data["provider"].as_str().unwrap_or("?"), data["id"].as_str().unwrap_or("?"))
            }
            "thinking" => "Thinking level changed.".into(),
            "name-get" => match data["sessionName"].as_str() {
                Some(n) => format!("Session name: {n}"),
                None => "This session has no name yet; /name <name> sets one.".into(),
            },
            "name" => {
                out.push(DriverEvent::SessionChanged);
                "Renamed.".into()
            }
            "session" => {
                let t = &data["tokens"];
                let mut s = format!(
                    "{} messages ({} from you), {} tool calls. Tokens: {} in, {} out, {} total",
                    data["totalMessages"].as_u64().unwrap_or(0),
                    data["userMessages"].as_u64().unwrap_or(0),
                    data["toolCalls"].as_u64().unwrap_or(0),
                    crate::state::ktok(t["input"].as_u64().unwrap_or(0)),
                    crate::state::ktok(t["output"].as_u64().unwrap_or(0)),
                    crate::state::ktok(t["total"].as_u64().unwrap_or(0)),
                );
                if let Some(c) = data["cost"].as_f64().filter(|c| *c > 0.0) {
                    s.push_str(&format!(", ${c:.2}"));
                }
                if let (Some(used), Some(window)) = (data["contextUsage"]["tokens"].as_u64(), data["contextUsage"]["contextWindow"].as_u64()) {
                    s.push_str(&format!(". Context {} of {}", crate::state::ktok(used), crate::state::ktok(window)));
                }
                s.push('.');
                s
            }
            "export" => format!("Exported to {}", data["path"].as_str().unwrap_or("?")),
            _ => return,
        };
        out.push(DriverEvent::Item(system(text)));
    }

    pub fn handle(&mut self, rec: &Value) -> (Vec<DriverEvent>, Option<Value>) {
        let mut out = Vec::new();
        let mut reply = None;
        if rec["type"] == "response"
            && let Some(what) = rec["id"].as_str().and_then(|id| id.strip_prefix(ID_CMD))
        {
            let what = what.to_string();
            self.command_reply(&what, rec, &mut out);
            return (out, None);
        }
        match rec["type"].as_str().unwrap_or("") {
            "response" if rec["id"] == ID_STATS && self.held.is_some() => {
                let (limit, _) = self.held.expect("held");
                let tokens = rec["data"]["contextUsage"]["tokens"].as_u64();
                match tokens {
                    Some(t) if rec["success"] == true && t > limit => {
                        self.held = Some((limit, Some(t)));
                        out.push(DriverEvent::Item(system(format!(
                            "Context is at {} tokens, past the {} limit. Compacting…",
                            crate::state::ktok(t),
                            crate::state::ktok(limit)
                        ))));
                        reply = Some(json!({"id": ID_COMPACT, "type": "compact"}));
                    }
                    _ => {
                        self.held = None;
                        out.push(DriverEvent::TurnEnded { aborted: false });
                    }
                }
            }
            "response" if rec["id"] == ID_COMPACT && self.held.is_some() => {
                let (limit, before) = self.held.take().expect("held");
                out.push(DriverEvent::Item(system(if rec["success"] == true {
                    let b = rec["data"]["tokensBefore"].as_u64().or(before).unwrap_or(0);
                    let a = rec["data"]["estimatedTokensAfter"].as_u64();
                    match a {
                        Some(a) => format!("Compacted: {} → about {} tokens (limit {}).", crate::state::ktok(b), crate::state::ktok(a), crate::state::ktok(limit)),
                        None => format!("Compacted from {} tokens (limit {}).", crate::state::ktok(b), crate::state::ktok(limit)),
                    }
                } else {
                    format!("Couldn't compact: {}", rec["error"].as_str().unwrap_or("pi refused"))
                })));
                out.push(DriverEvent::TurnEnded { aborted: false });
            }
            "response" => {
                if rec["command"] == "get_state" && rec["success"] == true {
                    if let Some(id) = rec["data"]["sessionId"].as_str() {
                        out.push(DriverEvent::Ready { import_key: format!("pi:{id}") });
                    }
                } else if rec["success"] == false {
                    let cmd = rec["command"].as_str().unwrap_or("command");
                    let err = rec["error"].as_str().unwrap_or("failed");
                    out.push(DriverEvent::Item(system(format!("pi {cmd}: {}", err.lines().next().unwrap_or(err)))));
                    // A rejected prompt never starts a run, so no `agent_settled` will follow.
                    if cmd == "prompt" {
                        out.push(DriverEvent::TurnFailed(format!("pi: {}", err.lines().next().unwrap_or(err))));
                        out.push(DriverEvent::TurnEnded { aborted: true });
                    }
                }
            }
            "agent_start" => out.push(DriverEvent::TurnStarted),
            "agent_settled" => {
                self.partials.clear();
                let aborted = rec["aborted"] == true;
                match self.compact_at.get() {
                    Some(limit) if !aborted && self.held.is_none() => {
                        self.held = Some((limit, None));
                        reply = Some(json!({"id": ID_STATS, "type": "get_session_stats"}));
                    }
                    _ => out.push(DriverEvent::TurnEnded { aborted }),
                }
            }
            "message_start" => self.partials.clear(),
            "message_update" => {
                let ev = &rec["assistantMessageEvent"];
                let index = ev["contentIndex"].as_i64().unwrap_or(0);
                let delta = ev["delta"].as_str().unwrap_or("");
                match ev["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        out.push(DriverEvent::Partial(PartialKind::Assistant, self.partials.push(index, PartialKind::Assistant, delta)))
                    }
                    "thinking_delta" => {
                        out.push(DriverEvent::Partial(PartialKind::Thinking, self.partials.push(index, PartialKind::Thinking, delta)))
                    }
                    _ => {}
                }
            }
            "message_end" => {
                self.partials.clear();
                out.extend(emp_import::pi::message_items(&rec["message"], None, None).into_iter().map(DriverEvent::Item));
                // The model call failed for good (pi retries first, with `auto_retry_start`).
                if rec["message"]["stopReason"] == "error" {
                    let e = rec["message"]["errorMessage"].as_str().unwrap_or("the model call failed");
                    out.push(DriverEvent::TurnFailed(emp_import::util::truncate(e, 500)));
                }
            }
            "tool_execution_update" => {
                let text = text_of(&rec["partialResult"]["content"]);
                if !text.is_empty() {
                    out.push(DriverEvent::Partial(PartialKind::Tool, emp_import::util::truncate(&text, 8000)));
                }
            }
            "queue_update" => {
                let mut queued = strings(&rec["steering"]);
                queued.extend(strings(&rec["followUp"]));
                out.push(DriverEvent::Queue(queued));
            }
            "auto_retry_start" => out.push(DriverEvent::Item(system(format!(
                "Retrying after error (attempt {}): {}",
                rec["attempt"].as_i64().unwrap_or(1),
                emp_import::util::truncate(rec["errorMessage"].as_str().unwrap_or(""), 300)
            )))),
            "compaction_start" if self.held.is_none() => out.push(DriverEvent::Item(system("Compacting context…".into()))),
            "extension_error" => out.push(DriverEvent::Item(system(format!(
                "pi extension error: {}",
                emp_import::util::truncate(rec["error"].as_str().unwrap_or(""), 300)
            )))),
            "extension_ui_request" => {
                // Dialogs block pi until answered: they become question cards in the chat.
                use emp_protocol::QuestionKind as K;
                let kind = match rec["method"].as_str().unwrap_or("") {
                    "select" => Some(K::Select),
                    "confirm" => Some(K::Confirm),
                    "input" => Some(K::Input),
                    "editor" => Some(K::Editor),
                    "notify" => {
                        if let Some(m) = rec["message"].as_str() {
                            out.push(DriverEvent::Item(system(emp_import::util::truncate(m, 500))));
                        }
                        None
                    }
                    _ => None,
                };
                if let (Some(kind), Some(id)) = (kind, rec["id"].as_str()) {
                    let mut q = question(id, kind, rec["title"].as_str().unwrap_or("pi has a question"), strings(&rec["options"]));
                    q.message = rec["message"].as_str().map(String::from);
                    q.placeholder = rec["placeholder"].as_str().map(String::from);
                    q.prefill = rec["prefill"].as_str().map(String::from);
                    out.push(DriverEvent::Ask(q));
                }
            }
            _ => {}
        }
        (out, reply)
    }
}

fn question(id: &str, kind: emp_protocol::QuestionKind, title: &str, options: Vec<String>) -> emp_protocol::Question {
    emp_protocol::Question { id: id.into(), kind, title: title.into(), message: None, options, placeholder: None, prefill: None }
}

/// `provider/model` (the model id may itself contain slashes) → `set_model`.
fn set_model(spec: &str, id: &str) -> Option<Value> {
    let (provider, model) = spec.trim().split_once('/')?;
    (!provider.is_empty() && !model.is_empty()).then(|| json!({"id": id, "type": "set_model", "provider": provider, "modelId": model}))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| match x {
            Value::String(s) => Some(s.clone()),
            other => {
                let t = text_of(other.get("content").or(other.get("message")).unwrap_or(other));
                (!t.is_empty()).then_some(t)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `pi --mode rpc --no-session` run (pi 1.3.0).
    const RUN: &str = include_str!("fixtures/pi-rpc.jsonl");

    fn run_all(text: &str) -> Vec<DriverEvent> {
        let mut m = Mapper::default();
        text.lines().filter(|l| !l.trim().is_empty()).flat_map(|l| m.handle(&serde_json::from_str(l).unwrap()).0).collect()
    }

    #[test]
    fn maps_a_real_run() {
        let events = run_all(RUN);
        assert!(matches!(&events[0], DriverEvent::Ready { import_key } if import_key.starts_with("pi:")));
        assert!(events.iter().any(|e| matches!(e, DriverEvent::TurnStarted)));
        let items: Vec<&TranscriptItem> =
            events.iter().filter_map(|e| if let DriverEvent::Item(i) = e { Some(i) } else { None }).collect();
        assert!(matches!(items[0], TranscriptItem::User { text, .. } if text.contains("echo hi")));
        assert!(items.iter().any(|i| matches!(i, TranscriptItem::ToolCall { name, .. } if name == "bash")));
        assert!(items.iter().any(|i| matches!(i, TranscriptItem::ToolResult { output, is_error: false, .. } if output.contains("hi"))));
        assert!(matches!(items.last().unwrap(), TranscriptItem::Assistant { text, .. } if text == "pong"));
        assert!(events.iter().any(|e| matches!(e, DriverEvent::Partial(PartialKind::Thinking, _))));
        assert!(matches!(events.last().unwrap(), DriverEvent::TurnEnded { aborted: false }));
    }

    #[test]
    fn a_rejected_prompt_ends_the_turn() {
        let mut m = Mapper::default();
        let (ev, _) = m.handle(&json!({"id":"emp-1","type":"response","command":"prompt","success":false,
            "error":"No API key found for openai-codex.\n\nUse /login"}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text == "pi prompt: No API key found for openai-codex."));
        assert!(matches!(&ev[1], DriverEvent::TurnFailed(e) if e == "pi: No API key found for openai-codex."));
        assert!(matches!(&ev[2], DriverEvent::TurnEnded { aborted: true }));
        // A model call that failed for good (after pi's own retries) fails the turn too.
        let (ev, _) = m.handle(&json!({"type":"message_end","message":{"role":"assistant","content":[],
            "stopReason":"error","errorMessage":"429 rate limited"}}));
        assert!(ev.iter().any(|e| matches!(e, DriverEvent::TurnFailed(e) if e == "429 rate limited")));
    }

    #[test]
    fn partials_carry_the_whole_block() {
        let mut m = Mapper::default();
        let upd = |d: &str| json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":d}});
        m.handle(&upd("Hel"));
        let (ev, _) = m.handle(&upd("lo"));
        assert!(matches!(&ev[0], DriverEvent::Partial(PartialKind::Assistant, t) if t == "Hello"));
    }

    #[test]
    fn dialogs_become_questions_and_status_pings_are_ignored() {
        let mut m = Mapper::default();
        let (ev, reply) = m.handle(&json!({"type":"extension_ui_request","id":"x1","method":"select","title":"Allow?","options":["Allow","Block"]}));
        assert!(reply.is_none());
        match &ev[..] {
            [DriverEvent::Ask(q)] => {
                assert_eq!((q.id.as_str(), q.kind, q.title.as_str()), ("x1", emp_protocol::QuestionKind::Select, "Allow?"));
                assert_eq!(q.options, ["Allow", "Block"]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(m.answer("x1", Some("Block".into()), None, false).unwrap(), json!({"type":"extension_ui_response","id":"x1","value":"Block"}));
        assert_eq!(m.answer("x2", None, Some(true), false).unwrap(), json!({"type":"extension_ui_response","id":"x2","confirmed":true}));
        assert_eq!(m.answer("x3", Some("x".into()), None, true).unwrap(), json!({"type":"extension_ui_response","id":"x3","cancelled":true}));
        let (ev, reply) = m.handle(&json!({"type":"extension_ui_request","id":"x4","method":"setStatus","statusText":"hi"}));
        assert!(ev.is_empty() && reply.is_none());
        let (ev, _) = m.handle(&json!({"type":"extension_ui_request","id":"x5","method":"notify","message":"Saved"}));
        assert!(matches!(&ev[..], [DriverEvent::Item(TranscriptItem::System { text, .. })] if text == "Saved"));
    }

    #[test]
    fn native_commands() {
        let mut m = Mapper::default();
        assert_eq!(m.command("compact", " focus on the API ").1.unwrap(), json!({"id":"emp-cmd-compact","type":"compact","customInstructions":"focus on the API"}));
        assert_eq!(
            m.command("model", "openrouter/meta/llama-5").1.unwrap(),
            json!({"id":"emp-cmd-model","type":"set_model","provider":"openrouter","modelId":"meta/llama-5"})
        );
        assert!(m.command("model", "nope").1.is_none());
        assert!(m.command("tree", "").1.is_none());

        // `/model` alone asks which one, and the answer switches.
        assert_eq!(m.command("model", "").1.unwrap()["type"], "get_available_models");
        let (ev, _) = m.handle(&json!({"type":"response","id":"emp-cmd-models","command":"get_available_models","success":true,
            "data":{"models":[{"provider":"anthropic","id":"claude-sonnet-5-5"},{"provider":"openai-codex","id":"gpt-6.1-sol"}]}}));
        let [DriverEvent::Ask(q)] = &ev[..] else { panic!("{ev:?}") };
        assert_eq!(q.options, ["anthropic/claude-sonnet-5-5", "openai-codex/gpt-6.1-sol"]);
        assert_eq!(m.answer(&q.id, Some("openai-codex/gpt-6.1-sol".into()), None, false).unwrap()["modelId"], "gpt-6.1-sol");
        assert!(m.answer(&q.id, None, None, true).is_none());

        let (ev, _) = m.handle(&json!({"type":"response","id":"emp-cmd-model","command":"set_model","success":true,"data":{"provider":"anthropic","id":"x"}}));
        assert!(matches!(&ev[..], [DriverEvent::SessionChanged, DriverEvent::Item(_)]));
        let (ev, _) = m.handle(&json!({"type":"response","id":"emp-cmd-compact","command":"compact","success":false,"error":"Nothing to compact\nstack"}));
        assert!(matches!(&ev[..], [DriverEvent::Item(TranscriptItem::System { text, .. })] if text == "/compact: Nothing to compact"));
        let (ev, _) = m.handle(&json!({"type":"response","id":"emp-cmd-session","command":"get_session_stats","success":true,
            "data":{"totalMessages":4,"userMessages":2,"toolCalls":1,"tokens":{"input":1000,"output":200,"total":1200},"cost":0.01,"contextUsage":{"tokens":900,"contextWindow":200000}}}));
        let [DriverEvent::Item(TranscriptItem::System { text, .. })] = &ev[..] else { panic!("{ev:?}") };
        assert!(text.starts_with("4 messages (2 from you), 1 tool calls."), "{text}");
    }

    #[test]
    fn new_sessions_get_agent_settings_but_resumes_keep_theirs() {
        let mut spec = DriverSpec {
            harness: emp_protocol::Harness::Pi,
            cwd: "/tmp".into(),
            resume: None,
            model: Some("openai-codex/gpt-6.1-sol".into()),
            effort: Some("high".into()),
            instructions: Some("Be Desk.".into()),
            profile: None,
            autonomy: 1,
            compact_at: crate::state::CompactAt::none(),
            mcp: None,
            grants: Default::default(),
        };
        assert_eq!(
            args(&spec),
            ["--mode", "rpc", "--model", "openai-codex/gpt-6.1-sol", "--thinking", "high", "--append-system-prompt", "Be Desk."]
        );
        spec.resume = Some(emp_protocol::SessionRef {
            harness: emp_protocol::Harness::Pi,
            session_id: "abc".into(),
            path: Some("/s/a.jsonl".into()),
            home: None,
            profile: None,
        });
        assert_eq!(args(&spec), ["--mode", "rpc", "--session", "/s/a.jsonl"]);
    }

    #[test]
    fn compacts_over_the_limit_before_ending_the_turn() {
        let mut m = Mapper { compact_at: crate::state::CompactAt::fixed(150_000), ..Mapper::default() };
        // The turn settles: EMP asks for stats instead of ending it.
        let (ev, reply) = m.handle(&json!({"type": "agent_settled", "aborted": false}));
        assert!(ev.is_empty());
        assert_eq!(reply.unwrap()["type"], "get_session_stats");
        // Over the limit: compact.
        let (ev, reply) = m.handle(&json!({"type": "response", "id": "emp-stats", "command": "get_session_stats", "success": true,
            "data": {"contextUsage": {"tokens": 183_000, "contextWindow": 1_000_000}}}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text.contains("183k") && text.contains("150k")));
        assert_eq!(reply.unwrap()["type"], "compact");
        // pi's own "Compacting…" is suppressed; the result ends the turn.
        assert!(m.handle(&json!({"type": "compaction_start"})).0.is_empty());
        let (ev, _) = m.handle(&json!({"type": "response", "id": "emp-compact", "command": "compact", "success": true,
            "data": {"tokensBefore": 183_000, "estimatedTokensAfter": 31_000}}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text.contains("183k → about 31k")));
        assert!(matches!(ev[1], DriverEvent::TurnEnded { aborted: false }));

        // Under the limit: the turn just ends.
        m.handle(&json!({"type": "agent_settled"}));
        let (ev, reply) = m.handle(&json!({"type": "response", "id": "emp-stats", "success": true, "data": {"contextUsage": {"tokens": 90_000}}}));
        assert!(reply.is_none());
        assert!(matches!(ev[..], [DriverEvent::TurnEnded { aborted: false }]));

        // Aborted turns and no limit skip the check.
        assert!(matches!(m.handle(&json!({"type": "agent_settled", "aborted": true})).0[..], [DriverEvent::TurnEnded { aborted: true }]));
        let mut plain = Mapper::default();
        assert!(matches!(plain.handle(&json!({"type": "agent_settled"})).0[..], [DriverEvent::TurnEnded { aborted: false }]));
    }
}
