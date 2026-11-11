//! Hermes driver: `hermes [-p <profile>] acp`, the Agent Client Protocol over stdio
//! (JSON-RPC 2.0 both ways).
//!
//! Sequence: `initialize` → `session/new` (or `session/load` to continue; its history replay
//! is ignored) → one `session/prompt` per turn, whose response ends the turn. Streaming comes
//! as `session/update` notifications. Hermes may call back with `session/request_permission`:
//! EMP answers "allow once" for agents with autonomy ≥ 2 and rejects otherwise.
//!
//! The model comes from the Hermes profile's own config unless EMP picked one (an ACP id,
//! `provider:model`): `session/set_model`. Hermes refuses that while a prompt runs ("Session is
//! busy"), so a switch waits for the chat to be idle, and the next prompt waits for its answer.
//! Never run `hermes resume` from here: that subcommand lifts the owner's emergency stop.

use std::collections::VecDeque;

use anyhow::Result;
use emp_protocol::{PartialKind, TranscriptItem};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Caps, DriverCmd, DriverEvent, DriverHandle, DriverSpec, Proc, text_of};

const CAPS: Caps = Caps { native_steer: false, native_follow_up: false, echoes_user: false };

const DEFAULT_PROFILE: &str = "default";

pub fn args(spec: &DriverSpec) -> Vec<String> {
    match profile(spec) {
        DEFAULT_PROFILE => vec!["acp".into()],
        p => vec!["-p".into(), p.into(), "acp".into()],
    }
}

fn profile(spec: &DriverSpec) -> &str {
    spec.resume
        .as_ref()
        .and_then(|s| s.profile.as_deref())
        .or(spec.profile.as_deref())
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_PROFILE)
}

const ID_INIT: i64 = 1;
const ID_SESSION: i64 = 2;
/// EMP's `/compress` prompt (compaction limit).
const ID_COMPRESS: i64 = 3;

pub fn spawn(spec: DriverSpec) -> Result<(DriverHandle, mpsc::UnboundedReceiver<DriverEvent>)> {
    let mut proc = Proc::spawn("hermes", &args(&spec), &spec.cwd)?;
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let pid = proc.pid;
    let profile = profile(&spec).to_string();
    let cwd = spec.cwd.to_string_lossy().into_owned();
    let resume_id = spec.resume.as_ref().map(|s| s.session_id.clone());
    let autonomy = spec.autonomy;
    tokio::spawn(async move {
        let mut m = Mapper::new(profile, autonomy);
        m.compact_at = spec.compact_at.clone();
        let _ = proc
            .send(&json!({"jsonrpc": "2.0", "id": ID_INIT, "method": "initialize", "params": {
                "protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            }}))
            .await;
        // Commands that arrive before the session is open wait here.
        let mut pending: VecDeque<DriverCmd> = VecDeque::new();
        let mut next_id = 10i64;
        let mut shutting_down = false;
        let mut model_set = spec.model.is_none() || resume_id.is_some();
        let model = spec.model.clone();
        loop {
            // A model picked in EMP applies to new sessions, before the first prompt.
            if !model_set && let (Some(_), Some(model)) = (m.session.clone(), model.as_ref()) {
                model_set = true;
                if let Some(rec) = m.set_model(model.clone(), &mut next_id, true) {
                    let _ = proc.send(&rec).await;
                }
            }
            // Drain buffered commands once the session exists; nothing passes a model switch in
            // flight, and a switch waits for the running turn.
            while m.session.is_some() && model_set && m.model_req.is_none() && !pending.is_empty() {
                if matches!(pending.front(), Some(DriverCmd::SetModel(_))) && m.busy() {
                    break;
                }
                let cmd = pending.pop_front().expect("non-empty");
                if let Some(rec) = m.command(cmd, &mut next_id, &ev_tx) {
                    let _ = proc.send(&rec).await;
                }
            }
            tokio::select! {
                cmd = cmd_rx.recv(), if !shutting_down => match cmd {
                    Some(DriverCmd::Shutdown) | None => {
                        shutting_down = true;
                        break;
                    }
                    Some(cmd) => pending.push_back(cmd),
                },
                rec = proc.next() => {
                    let Some(rec) = rec else { break };
                    // Open the session as soon as initialize answers.
                    if rec["id"] == ID_INIT && rec.get("method").is_none() {
                        m.takes_images = rec["result"]["agentCapabilities"]["promptCapabilities"]["image"] == true;
                        let (method, params) = match &resume_id {
                            Some(id) => ("session/load", json!({"sessionId": id, "cwd": cwd, "mcpServers": []})),
                            None => ("session/new", json!({"cwd": cwd, "mcpServers": []})),
                        };
                        m.loading = resume_id.clone();
                        let _ = proc.send(&json!({"jsonrpc": "2.0", "id": ID_SESSION, "method": method, "params": params})).await;
                        if let Some(err) = rec.get("error") {
                            let _ = ev_tx.send(DriverEvent::Item(system(format!("Hermes initialize failed: {}", err["message"]))));
                        }
                        continue;
                    }
                    if rec["id"].as_i64().is_some_and(|id| Some(id) == m.prompt_id)
                        && let Some(sid) = m.session.clone()
                    {
                        let profile = m.profile.clone();
                        let started_at = m.prompt_started_at;
                        m.turn_failure = tokio::task::spawn_blocking(move || emp_import::hermes::latest_turn_failure(&profile, &sid, started_at))
                            .await.ok().and_then(Result::ok).flatten();
                    }
                    let (events, reply) = m.handle(&rec);
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
            format!("hermes exited ({code}): {}", proc.stderr_summary())
        });
        let _ = ev_tx.send(DriverEvent::Exited(error));
    });
    Ok((DriverHandle { cmds: cmd_tx, pid, caps: CAPS }, ev_rx))
}

fn system(text: String) -> TranscriptItem {
    TranscriptItem::System { text, ts: Some(emp_core::now_ms()) }
}

/// ACP state + mapping from Hermes records to driver events.
pub struct Mapper {
    profile: String,
    autonomy: u8,
    pub session: Option<String>,
    /// `session/load` in progress for this id: its history replay is ignored.
    loading: Option<String>,
    prompt_id: Option<i64>,
    prompt_started_at: i64,
    assistant: String,
    thinking: String,
    /// Redacted display metadata for a failed-turn boundary streamed as an ordinary reply.
    turn_failure: Option<String>,
    pub compact_at: crate::state::CompactAt,
    /// Context tokens from Hermes's last `usage_update` (sent before each turn's response).
    used: Option<u64>,
    /// EMP's `/compress` is running: its reply text is collected here, not shown as a message.
    compress: Option<(u64, u64, String)>,
    /// The agent advertised `promptCapabilities.image`.
    pub takes_images: bool,
    /// `session/set_model` in flight: request id, model, and whether it was the owner's switch
    /// (said in the chat) or the agent's model on a new session.
    pub model_req: Option<(i64, String, bool)>,
}

impl Mapper {
    pub fn new(profile: String, autonomy: u8) -> Self {
        Self {
            profile,
            autonomy,
            session: None,
            loading: None,
            prompt_id: None,
            prompt_started_at: 0,
            assistant: String::new(),
            thinking: String::new(),
            turn_failure: None,
            compact_at: Default::default(),
            used: None,
            compress: None,
            takes_images: false,
            model_req: None,
        }
    }

    /// A prompt (or EMP's `/compress`) is running.
    pub fn busy(&self) -> bool {
        self.prompt_id.is_some() || self.compress.is_some()
    }

    fn set_model(&mut self, model: String, next_id: &mut i64, quiet: bool) -> Option<Value> {
        let session = self.session.clone()?;
        *next_id += 1;
        let rec = json!({"jsonrpc": "2.0", "id": *next_id, "method": "session/set_model", "params": {"sessionId": session, "modelId": model}});
        self.model_req = Some((*next_id, model, !quiet));
        Some(rec)
    }

    /// JSON-RPC record for a command, once the session is open.
    fn command(&mut self, cmd: DriverCmd, next_id: &mut i64, ev: &mpsc::UnboundedSender<DriverEvent>) -> Option<Value> {
        let session = self.session.clone()?;
        match cmd {
            DriverCmd::Prompt(m) | DriverCmd::Steer(m) | DriverCmd::FollowUp(m) => {
                *next_id += 1;
                self.prompt_id = Some(*next_id);
                self.prompt_started_at = emp_core::now_ms();
                let _ = ev.send(DriverEvent::TurnStarted);
                let mut prompt: Vec<Value> = if self.takes_images {
                    super::inline_images(&m.images).into_iter().map(|(mime, data)| json!({"type": "image", "mimeType": mime, "data": data})).collect()
                } else {
                    Vec::new()
                };
                prompt.push(json!({"type": "text", "text": m.text}));
                Some(json!({"jsonrpc": "2.0", "id": *next_id, "method": "session/prompt", "params": {
                    "sessionId": session, "prompt": prompt,
                }}))
            }
            DriverCmd::Interrupt => {
                Some(json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}}))
            }
            DriverCmd::SetModel(model) => self.set_model(model, next_id, false),
            DriverCmd::Command { .. } | DriverCmd::Answer { .. } | DriverCmd::Shutdown => None,
        }
    }

    /// Emits buffered thinking, then assistant text, as finished items.
    fn flush(&mut self, out: &mut Vec<DriverEvent>) {
        let ts = Some(emp_core::now_ms());
        if !self.thinking.trim().is_empty() {
            out.push(DriverEvent::Item(TranscriptItem::Thinking { text: std::mem::take(&mut self.thinking), ts }));
        }
        self.thinking.clear();
        if !self.assistant.trim().is_empty() {
            out.push(DriverEvent::Item(TranscriptItem::Assistant { text: std::mem::take(&mut self.assistant), model: None, ts }));
        }
        self.assistant.clear();
    }

    pub fn handle(&mut self, rec: &Value) -> (Vec<DriverEvent>, Option<Value>) {
        let mut out = Vec::new();
        // Agent → client request.
        if let (Some(method), Some(id)) = (rec["method"].as_str(), rec.get("id")) {
            if method == "session/request_permission" {
                return (out_permission(self.autonomy, &rec["params"], &mut out), Some(json!({
                    "jsonrpc": "2.0", "id": id, "result": {"outcome": permission_outcome(self.autonomy, &rec["params"])},
                })));
            }
            return (out, Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("EMP does not provide {method}")}})));
        }
        // Notification.
        if rec["method"] == "session/update" {
            if self.loading.is_some() {
                return (out, None);
            }
            let u = &rec["params"]["update"];
            let ts = Some(emp_core::now_ms());
            match u["sessionUpdate"].as_str().unwrap_or("") {
                "agent_message_chunk" if self.compress.is_some() => {
                    self.compress.as_mut().expect("compressing").2.push_str(&text_of(&u["content"]));
                }
                "agent_thought_chunk" if self.compress.is_some() => {}
                "usage_update" => {
                    self.used = u["used"].as_u64();
                    if let Some(used) = self.used {
                        out.push(DriverEvent::Context(emp_protocol::ContextUsage { used, window: u["size"].as_u64(), model: None }));
                    }
                }
                "available_commands_update" => {
                    out.push(DriverEvent::Commands {
                        key: format!("hermes:{}", self.profile),
                        commands: crate::commands::from_acp(&u["availableCommands"]),
                    });
                }
                "agent_message_chunk" => {
                    self.assistant.push_str(&text_of(&u["content"]));
                    out.push(DriverEvent::Partial(PartialKind::Assistant, self.assistant.clone()));
                }
                "agent_thought_chunk" => {
                    self.thinking.push_str(&text_of(&u["content"]));
                    out.push(DriverEvent::Partial(PartialKind::Thinking, self.thinking.clone()));
                }
                "tool_call" => {
                    self.flush(&mut out);
                    let name = u["title"].as_str().or(u["kind"].as_str()).unwrap_or("tool").to_string();
                    let input = if u["rawInput"].is_null() { String::new() } else { emp_import::util::display_json(&u["rawInput"]) };
                    out.push(DriverEvent::Item(TranscriptItem::ToolCall { call_id: u["toolCallId"].as_str().map(Into::into), name, input, ts }));
                }
                "tool_call_update" => {
                    let status = u["status"].as_str().unwrap_or("");
                    if status == "completed" || status == "failed" {
                        let mut output = tool_content(&u["content"]);
                        if output.is_empty() && !u["rawOutput"].is_null() {
                            output = emp_import::util::display_json(&u["rawOutput"]);
                        }
                        out.push(DriverEvent::Item(TranscriptItem::ToolResult {
                            call_id: u["toolCallId"].as_str().map(Into::into),
                            output: emp_import::util::truncate(&output, emp_import::util::MAX_TOOL_TEXT),
                            is_error: status == "failed",
                            ts,
                        }));
                    }
                }
                _ => {}
            }
            return (out, None);
        }
        // Response to one of our requests.
        let id = rec["id"].as_i64();
        if let Some((_, model, said)) = self.model_req.take_if(|(req, _, _)| id == Some(*req)) {
            match rec.get("error") {
                Some(e) => out.push(DriverEvent::Item(system(format!(
                    "Hermes didn't switch to {model}: {}",
                    e["message"].as_str().unwrap_or("unknown error")
                )))),
                None if said => out.push(DriverEvent::Item(system(format!("Model: {model}")))),
                None => {}
            }
            return (out, None);
        }
        if id == Some(ID_SESSION) {
            // `session/load` answers without echoing the id; it's the one we asked for.
            let load_id = self.loading.take();
            let sid = rec["result"]["sessionId"].as_str().map(String::from).or(load_id.filter(|_| !rec["result"].is_null()));
            match sid {
                Some(sid) if rec.get("error").is_none() => {
                    out.push(DriverEvent::Ready { import_key: format!("hermes:{}:{sid}", self.profile) });
                    self.session = Some(sid);
                }
                _ => out.push(DriverEvent::Item(system(format!(
                    "Hermes could not open the session: {}",
                    rec["error"]["message"].as_str().unwrap_or("session not found")
                )))),
            }
        } else if id.is_some() && id == self.prompt_id {
            self.prompt_id = None;
            let failure = if self.turn_failure.is_some() || emp_import::hermes::is_failed_turn_notice(&self.assistant) {
                let notice = std::mem::take(&mut self.assistant);
                Some(
                    self.turn_failure
                        .take()
                        .unwrap_or_else(|| format!("Hermes request failed. {notice}")),
                )
            } else {
                self.turn_failure = None;
                None
            };
            self.flush(&mut out);
            if let Some(msg) = failure.as_ref() {
                out.push(DriverEvent::Item(system(msg.clone())));
                out.push(DriverEvent::TurnFailed(msg.clone()));
            } else if let Some(err) = rec.get("error") {
                let msg = format!(
                    "Hermes: {}",
                    err["message"].as_str().unwrap_or("prompt failed")
                );
                out.push(DriverEvent::Item(system(msg.clone())));
                out.push(DriverEvent::TurnFailed(msg));
            }
            let aborted = failure.is_some()
                || rec["result"]["stopReason"] == "cancelled"
                || rec.get("error").is_some();
            // Over the compaction limit: run Hermes's /compress before reporting the turn as ended.
            if let (false, Some(limit), Some(used), Some(sid)) = (aborted, self.compact_at.get(), self.used, self.session.clone())
                && used > limit
            {
                self.compress = Some((limit, used, String::new()));
                out.push(DriverEvent::Item(system(format!(
                    "Context is at {} tokens, past the {} limit. Compacting…",
                    crate::state::ktok(used),
                    crate::state::ktok(limit)
                ))));
                return (out, Some(json!({"jsonrpc": "2.0", "id": ID_COMPRESS, "method": "session/prompt", "params": {
                    "sessionId": sid, "prompt": [{"type": "text", "text": "/compress"}],
                }})));
            }
            out.push(DriverEvent::TurnEnded { aborted });
        } else if id == Some(ID_COMPRESS) && self.compress.is_some() {
            let (limit, before, text) = self.compress.take().expect("compressing");
            let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
            let note = match (rec.get("error"), self.used.filter(|u| *u < before)) {
                (Some(err), _) => format!("Couldn't compact: {}", err["message"].as_str().unwrap_or("Hermes refused")),
                (None, Some(after)) => {
                    format!("Compacted: {} → about {} tokens (limit {}).", crate::state::ktok(before), crate::state::ktok(after), crate::state::ktok(limit))
                }
                (None, None) if first.is_empty() => format!("Compacted from {} tokens (limit {}).", crate::state::ktok(before), crate::state::ktok(limit)),
                (None, None) => format!("Hermes: {}", emp_import::util::truncate(&first, 300)),
            };
            out.push(DriverEvent::Item(system(note)));
            out.push(DriverEvent::TurnEnded { aborted: false });
        }
        (out, None)
    }
}

fn tool_content(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| text_of(c.get("content").unwrap_or(c)))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A model id Hermes's ACP `session/set_model` takes: `provider:model`, as listed by
/// `models.list` (`nous:anthropic/claude-opus-5.5`). Imported agents carry the profile config's
/// own form (`upstage/solar-mini4:free`, `nous/x`), which the profile already uses by itself.
pub fn is_acp_model(id: &str) -> bool {
    id.split_once(':').is_some_and(|(provider, model)| !provider.is_empty() && !provider.contains('/') && !model.is_empty())
}

/// Picks an `allow_once` option for autonomy ≥ 2, otherwise `reject_once` (or cancels).
fn permission_outcome(autonomy: u8, params: &Value) -> Value {
    let options = params["options"].as_array().cloned().unwrap_or_default();
    let want = if autonomy >= 2 { "allow_once" } else { "reject_once" };
    match options.iter().find(|o| o["kind"] == want).and_then(|o| o["optionId"].as_str()) {
        Some(id) => json!({"outcome": "selected", "optionId": id}),
        None => json!({"outcome": "cancelled"}),
    }
}

fn out_permission(autonomy: u8, params: &Value, out: &mut Vec<DriverEvent>) -> Vec<DriverEvent> {
    let what = params["toolCall"]["title"].as_str().unwrap_or("a tool call");
    let verdict = if autonomy >= 2 { "allowed once (agent autonomy ≥ 2)" } else { "denied (agent autonomy < 2; approvals arrive in P2)" };
    out.push(DriverEvent::Item(system(format!("Hermes asked to run {what}: {verdict}"))));
    std::mem::take(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(u: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": u}})
    }

    #[test]
    fn new_session_then_a_turn_with_a_tool() {
        let mut m = Mapper::new("studio".into(), 1);
        let (ev, _) = m.handle(&json!({"jsonrpc": "2.0", "id": ID_SESSION, "result": {"sessionId": "s1", "models": {}}}));
        assert!(matches!(&ev[0], DriverEvent::Ready { import_key } if import_key == "hermes:studio:s1"));
        m.prompt_id = Some(11);
        m.handle(&update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}})));
        let (ev, _) = m.handle(&update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Let me look."}})));
        assert!(matches!(&ev[0], DriverEvent::Partial(PartialKind::Assistant, t) if t == "Let me look."));
        let (ev, _) = m.handle(&update(json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "read_file", "rawInput": {"path": "a"}})));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::Thinking { .. })));
        assert!(matches!(&ev[1], DriverEvent::Item(TranscriptItem::Assistant { text, .. }) if text == "Let me look."));
        assert!(matches!(&ev[2], DriverEvent::Item(TranscriptItem::ToolCall { name, .. }) if name == "read_file"));
        let (ev, _) = m.handle(&update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "file body"}}]})));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::ToolResult { output, is_error: false, .. }) if output == "file body"));
        m.handle(&update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Done."}})));
        let (ev, _) = m.handle(&json!({"jsonrpc": "2.0", "id": 11, "result": {"stopReason": "end_turn"}}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::Assistant { text, .. }) if text == "Done."));
        assert!(matches!(&ev[1], DriverEvent::TurnEnded { aborted: false }));
    }

    #[test]
    fn load_replay_is_ignored_and_ready_uses_the_requested_id() {
        let mut m = Mapper::new("default".into(), 1);
        m.loading = Some("old".into());
        let (ev, _) = m.handle(&update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "history"}})));
        assert!(ev.is_empty());
        let (ev, _) = m.handle(&json!({"jsonrpc": "2.0", "id": ID_SESSION, "result": {"models": {}}}));
        assert!(matches!(&ev[0], DriverEvent::Ready { import_key } if import_key == "hermes:default:old"));
        assert!(m.assistant.is_empty());
    }

    #[test]
    fn model_switches_wait_for_hermes() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut m = Mapper::new("studio".into(), 3);
        m.session = Some("s1".into());
        let mut next = 10;
        m.prompt_id = Some(9);
        assert!(m.busy(), "a running turn holds a switch back");
        m.prompt_id = None;
        let rec = m.command(DriverCmd::SetModel("anthropic:claude-opus-5-5".into()), &mut next, &tx).unwrap();
        assert_eq!((rec["method"].as_str(), rec["params"]["modelId"].as_str()), (Some("session/set_model"), Some("anthropic:claude-opus-5-5")));
        assert!(m.model_req.is_some(), "prompts wait for the answer");
        // Refused: said in the chat, and the next switch can go.
        let (ev, _) = m.handle(&json!({"jsonrpc": "2.0", "id": 11, "error": {"code": -32603, "message": "Session is busy"}}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text == "Hermes didn't switch to anthropic:claude-opus-5-5: Session is busy"));
        assert!(m.model_req.is_none());
        m.command(DriverCmd::SetModel("anthropic:claude-opus-5-5".into()), &mut next, &tx).unwrap();
        let (ev, _) = m.handle(&json!({"jsonrpc": "2.0", "id": 12, "result": {}}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text == "Model: anthropic:claude-opus-5-5"));
        // The agent's own model on a new session is applied quietly.
        m.set_model("anthropic:claude-opus-5-5".into(), &mut next, true).unwrap();
        assert!(m.handle(&json!({"jsonrpc": "2.0", "id": 13, "result": {}})).0.is_empty());
        assert!(rx.try_recv().is_err(), "no note before Hermes agrees");
    }

    #[test]
    fn only_acp_model_ids_are_passed() {
        assert!(is_acp_model("claude-subscription-directsdk-experimental:claude-opus-5-5[1m]"));
        assert!(is_acp_model("nous:anthropic/claude-opus-5.5"));
        assert!(!is_acp_model("upstage/solar-mini4:free"));
        assert!(!is_acp_model("stealth/space-bunny-alpha"));
        assert!(!is_acp_model("claude-x"));
    }

    #[test]
    fn permissions_follow_autonomy() {
        let params = json!({"toolCall": {"title": "rm -rf build"}, "options": [
            {"optionId": "a", "kind": "allow_once"}, {"optionId": "r", "kind": "reject_once"}]});
        let req = json!({"jsonrpc": "2.0", "id": 99, "method": "session/request_permission", "params": params});
        let (ev, reply) = Mapper::new("p".into(), 1).handle(&req);
        assert_eq!(reply.unwrap()["result"]["outcome"], json!({"outcome": "selected", "optionId": "r"}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text.contains("denied")));
        let (_, reply) = Mapper::new("p".into(), 2).handle(&req);
        assert_eq!(reply.unwrap()["result"]["outcome"]["optionId"], "a");
        let (_, reply) = Mapper::new("p".into(), 1).handle(&json!({"jsonrpc": "2.0", "id": 5, "method": "fs/read_text_file", "params": {}}));
        assert_eq!(reply.unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn default_profile_omits_the_flag() {
        let spec = |p: Option<&str>| DriverSpec {
            harness: emp_protocol::Harness::Hermes,
            cwd: "/tmp".into(),
            resume: None,
            model: None,
            effort: None,
            instructions: None,
            profile: p.map(Into::into),
            autonomy: 1,
            compact_at: crate::state::CompactAt::none(),
            mcp: None,
            grants: Default::default(),
        };
        assert_eq!(args(&spec(None)), ["acp"]);
        assert_eq!(args(&spec(Some("default"))), ["acp"]);
        assert_eq!(args(&spec(Some("studio"))), ["-p", "studio", "acp"]);
    }

    #[test]
    fn failed_turn_boundary_is_a_failure_even_when_acp_says_end_turn() {
        let mut m = Mapper::new("studio".into(), 3);
        m.session = Some("s1".into());
        m.prompt_id = Some(11);
        m.compact_at = crate::state::CompactAt::fixed(1);
        m.used = Some(100);
        m.assistant =
            "Your request was not processed. Send it again if you still want me to carry it out."
                .into();
        let detail = "Hermes request failed (GitHub Copilot · gpt-6.1-sol): HTTP 400: The requested model is not supported.";
        m.turn_failure = Some(detail.into());
        let (ev, reply) = m.handle(&json!({"id":11,"result":{"stopReason":"end_turn"}}));
        assert!(reply.is_none(), "a failed turn must not trigger compaction");
        assert!(
            matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text == detail)
        );
        assert!(matches!(&ev[1], DriverEvent::TurnFailed(text) if text == detail));
        assert!(matches!(&ev[2], DriverEvent::TurnEnded { aborted: true }));
        assert!(m.assistant.is_empty());
        // ACP may stream the raw error instead of the durable boundary text.
        m.prompt_id = Some(12);
        m.assistant = "HTTP 400: The requested model is not supported.".into();
        m.turn_failure = Some(detail.into());
        let (ev, _) = m.handle(&json!({"id":12,"result":{"stopReason":"end_turn"}}));
        assert!(matches!(&ev[1], DriverEvent::TurnFailed(text) if text == detail));
        assert!(matches!(ev.last(), Some(DriverEvent::TurnEnded { aborted: true })));
        // The failure detail belongs to this turn only.
        m.prompt_id = Some(13);
        m.used = None;
        m.assistant = "Recovered".into();
        let (ev, _) = m.handle(&json!({"id":13,"result":{"stopReason":"end_turn"}}));
        assert!(
            matches!(&ev[0], DriverEvent::Item(TranscriptItem::Assistant { text, .. }) if text == "Recovered")
        );
        assert!(matches!(
            ev.last(),
            Some(DriverEvent::TurnEnded { aborted: false })
        ));
    }

    #[test]
    fn compresses_over_the_limit_and_hides_the_command_reply() {
        let mut m = Mapper::new("default".into(), 1);
        m.compact_at = crate::state::CompactAt::fixed(150_000);
        m.session = Some("s1".into());
        m.prompt_id = Some(11);
        let upd = |u: Value| json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": u}});
        m.handle(&upd(json!({"sessionUpdate": "usage_update", "used": 210_000, "size": 1_000_000})));
        let (ev, reply) = m.handle(&json!({"jsonrpc": "2.0", "id": 11, "result": {"stopReason": "end_turn"}}));
        assert!(ev.iter().all(|e| !matches!(e, DriverEvent::TurnEnded { .. })));
        let reply = reply.unwrap();
        assert_eq!(reply["id"], ID_COMPRESS);
        assert_eq!(reply["params"]["prompt"][0]["text"], "/compress");
        // The command's own reply text isn't shown as an agent message.
        assert!(m.handle(&upd(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Compressed 40 messages"}}))).0.is_empty());
        m.handle(&upd(json!({"sessionUpdate": "usage_update", "used": 40_000, "size": 1_000_000})));
        let (ev, _) = m.handle(&json!({"jsonrpc": "2.0", "id": ID_COMPRESS, "result": {"stopReason": "end_turn"}}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text.contains("210k → about 40k")));
        assert!(matches!(ev[1], DriverEvent::TurnEnded { aborted: false }));

        // Under the limit: no compression.
        m.prompt_id = Some(12);
        let (ev, reply) = m.handle(&json!({"jsonrpc": "2.0", "id": 12, "result": {"stopReason": "end_turn"}}));
        assert!(reply.is_none());
        assert!(matches!(ev.last(), Some(DriverEvent::TurnEnded { aborted: false })));
    }
}
