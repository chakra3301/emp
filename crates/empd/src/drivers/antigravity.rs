//! Antigravity driver: one `agy --print=<message> --output-format stream-json` process per
//! turn; later turns add `--conversation <id>`. (agy 1.4.3.)
//!
//! Events: `init {conversation_id}`, then `step_update {step_index, state (ACTIVE | DONE |
//! ERROR), step_type (user_input | agent_response | tool | system_message), text_delta,
//! tool_name, tool_info {parameters, output, error}, usage}`, and finally `result {status,
//! usage, denied_actions}`. Agent text streams as `text_delta`s on ACTIVE updates of one step.
//!
//! Permissions follow agent autonomy. Headless agy can't ask, so whatever needs a prompt is
//! denied: 0 → default mode (reads only), 1–2 → `--mode accept-edits` (file edits, no shell
//! commands, like Claude Code's acceptEdits), 3 → `--dangerously-skip-permissions`.
//! agy has no system-prompt flag, so agent instructions aren't applied, and it compacts on its
//! own (no limit flag). Steer and follow-up are queued by EMP; interrupt kills the turn.

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::{Context, Result};
use emp_protocol::{PartialKind, TranscriptItem};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::{Caps, DriverCmd, DriverEvent, DriverHandle, DriverSpec, kill_group};
use crate::doctor;

const CAPS: Caps = Caps { native_steer: false, native_follow_up: false, echoes_user: false };

/// Arguments for one turn. `conversation` is the agy conversation id once known.
pub fn args(spec: &DriverSpec, conversation: Option<&str>, text: &str) -> Vec<String> {
    // `--print=…` keeps a message that starts with `-` from being read as a flag.
    let mut a: Vec<String> = vec![format!("--print={text}"), "--output-format".into(), "stream-json".into()];
    if let Some(id) = conversation {
        a.push("--conversation".into());
        a.push(id.into());
    }
    if let Some(m) = &spec.model {
        a.push("--model".into());
        a.push(m.clone());
    }
    if let Some(e) = &spec.effort {
        a.push("--effort".into());
        a.push(e.clone());
    }
    match spec.autonomy {
        0 => {}
        3 => a.push("--dangerously-skip-permissions".into()),
        _ => a.extend(["--mode", "accept-edits"].map(String::from)),
    }
    a
}

pub fn spawn(mut spec: DriverSpec) -> Result<(DriverHandle, mpsc::UnboundedReceiver<DriverEvent>)> {
    let program = doctor::which("agy").context("agy (Antigravity CLI) is not installed")?;
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let mut conversation: Option<String> = spec.resume.as_ref().map(|s| s.session_id.clone());
    if let Some(id) = &conversation {
        let _ = ev_tx.send(DriverEvent::Ready { import_key: format!("antigravity:{id}") });
    }
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            let text = match cmd {
                DriverCmd::Prompt(m) | DriverCmd::Steer(m) | DriverCmd::FollowUp(m) => m.text,
                // Nothing running between turns; no native commands or questions.
                DriverCmd::Interrupt | DriverCmd::Command { .. } | DriverCmd::Answer { .. } => continue,
                // Each turn is its own `agy` run, so the next one just takes the flag.
                DriverCmd::SetModel(m) => {
                    let _ = ev_tx.send(DriverEvent::Item(system(format!("Model: {m}"))));
                    spec.model = Some(m);
                    continue;
                }
                DriverCmd::Shutdown => break,
            };
            let _ = ev_tx.send(DriverEvent::TurnStarted);
            let mut c = Command::new(&program);
            c.args(args(&spec, conversation.as_deref(), &text))
                .current_dir(&spec.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .kill_on_drop(true);
            if let Ok(p) = std::env::join_paths(doctor::search_dirs()) {
                c.env("PATH", p);
            }
            let mut child = match c.spawn() {
                Ok(ch) => ch,
                Err(e) => {
                    let _ = ev_tx.send(DriverEvent::Item(system(format!("agy: {e}"))));
                    let _ = ev_tx.send(DriverEvent::TurnEnded { aborted: true });
                    continue;
                }
            };
            let pid = child.id();
            let stderr = child.stderr.take();
            let stderr_task = tokio::spawn(async move {
                let mut tail = String::new();
                if let Some(err) = stderr {
                    let mut lines = BufReader::new(err).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        tail.push_str(&l);
                        tail.push('\n');
                        if tail.len() > 4000 {
                            tail.drain(..tail.len() - 4000);
                        }
                    }
                }
                tail
            });
            let mut mapper = Mapper::default();
            let mut ended = false;
            let mut stdout = BufReader::new(child.stdout.take().expect("piped")).lines();
            loop {
                tokio::select! {
                    line = stdout.next_line() => {
                        let Ok(Some(line)) = line else { break };
                        let Ok(rec) = serde_json::from_str::<Value>(&line) else { continue };
                        for ev in mapper.handle(&rec) {
                            match &ev {
                                DriverEvent::Ready { import_key } => {
                                    conversation = import_key.strip_prefix("antigravity:").map(String::from);
                                }
                                DriverEvent::TurnEnded { .. } => ended = true,
                                _ => {}
                            }
                            let _ = ev_tx.send(ev);
                        }
                        if ended {
                            break;
                        }
                    }
                    cmd = cmd_rx.recv() => match cmd {
                        Some(DriverCmd::Interrupt) | Some(DriverCmd::Shutdown) | None => {
                            if let Some(pid) = pid {
                                unsafe { libc::killpg(pid as libc::pid_t, libc::SIGINT) };
                                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                                kill_group(pid);
                            }
                            let _ = ev_tx.send(DriverEvent::Item(system("Stopped.".into())));
                            let _ = ev_tx.send(DriverEvent::TurnEnded { aborted: true });
                            ended = true;
                            break;
                        }
                        // Steer / follow-up while running are queued by the live layer.
                        Some(_) => {}
                    },
                }
            }
            let status = match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                Ok(s) => s.ok(),
                Err(_) => {
                    if let Some(pid) = pid {
                        kill_group(pid);
                    }
                    None
                }
            };
            if ended {
                stderr_task.abort();
            } else {
                let tail = match tokio::time::timeout(std::time::Duration::from_secs(1), stderr_task).await {
                    Ok(Ok(t)) => t,
                    _ => String::new(),
                };
                let ok = status.is_some_and(|s| s.success());
                if !ok {
                    let last = tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("agy exited");
                    let _ = ev_tx.send(DriverEvent::Item(system(format!("agy: {}", emp_import::util::truncate(last, 400)))));
                    let _ = ev_tx.send(DriverEvent::TurnFailed(format!("agy: {}", emp_import::util::truncate(last, 400))));
                }
                let _ = ev_tx.send(DriverEvent::TurnEnded { aborted: !ok });
            }
        }
        let _ = ev_tx.send(DriverEvent::Exited(None));
    });
    Ok((DriverHandle { cmds: cmd_tx, pid: None, caps: CAPS }, ev_rx))
}

fn system(text: String) -> TranscriptItem {
    TranscriptItem::System { text, ts: Some(emp_core::now_ms()) }
}

/// Maps `agy --output-format stream-json` records to driver events.
#[derive(Default)]
pub struct Mapper {
    ready: bool,
    /// Text so far per agent-response step (and thinking, should agy stream it).
    text: HashMap<i64, String>,
    thinking: HashMap<i64, String>,
    /// Tool steps already announced.
    calls: Vec<i64>,
}

impl Mapper {
    pub fn handle(&mut self, rec: &Value) -> Vec<DriverEvent> {
        let mut out = Vec::new();
        let ts = Some(emp_core::now_ms());
        match rec["event"].as_str().unwrap_or("") {
            "init" => {
                if let Some(id) = rec["conversation_id"].as_str()
                    && !self.ready
                {
                    self.ready = true;
                    out.push(DriverEvent::Ready { import_key: format!("antigravity:{id}") });
                }
            }
            "step_update" => {
                let s = &rec["step_update"];
                let idx = s["step_index"].as_i64().unwrap_or(-1);
                let state = s["state"].as_str().unwrap_or("");
                let done = state != "ACTIVE";
                match s["step_type"].as_str().unwrap_or("") {
                    "agent_response" => {
                        if let Some(d) = s["thinking_delta"].as_str().filter(|d| !d.is_empty()) {
                            let t = self.thinking.entry(idx).or_default();
                            t.push_str(d);
                            out.push(DriverEvent::Partial(PartialKind::Thinking, t.clone()));
                        }
                        if let Some(d) = s["text_delta"].as_str().filter(|d| !d.is_empty()) {
                            let t = self.text.entry(idx).or_default();
                            t.push_str(d);
                            if !done {
                                out.push(DriverEvent::Partial(PartialKind::Assistant, t.clone()));
                            }
                        }
                        if done {
                            if let Some(u) = s["usage"].as_object() {
                                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                                out.push(DriverEvent::Context(emp_protocol::ContextUsage {
                                    used: n("input_tokens") + n("cache_read_tokens") + n("output_tokens"),
                                    window: None,
                                    model: None,
                                }));
                            }
                            if let Some(t) = self.thinking.remove(&idx).filter(|t| !t.trim().is_empty()) {
                                out.push(DriverEvent::Item(TranscriptItem::Thinking { text: t, ts }));
                            }
                            if let Some(t) = self.text.remove(&idx).filter(|t| !t.trim().is_empty()) {
                                out.push(DriverEvent::Item(TranscriptItem::Assistant { text: t.trim_end().to_string(), model: None, ts }));
                            }
                            if state == "ERROR"
                                && let Some(msg) = s["error"]["message"].as_str()
                            {
                                out.push(DriverEvent::Item(system(format!("agy: {}", emp_import::util::truncate(msg, 500)))));
                    out.push(DriverEvent::TurnFailed(format!("agy: {}", emp_import::util::truncate(msg, 500))));
                            }
                        }
                    }
                    "tool" => {
                        let info = &s["tool_info"];
                        let call_id = Some(format!("step-{idx}"));
                        if !self.calls.contains(&idx) {
                            self.calls.push(idx);
                            let name = s["tool_name"].as_str().or(info["name"].as_str()).unwrap_or("tool").to_string();
                            let input = emp_import::util::display_json(&info["parameters"]);
                            out.push(DriverEvent::Item(TranscriptItem::ToolCall {
                                call_id: call_id.clone(),
                                name,
                                input: emp_import::util::truncate(&input, emp_import::util::MAX_TOOL_TEXT),
                                ts,
                            }));
                        }
                        if done {
                            let error = info["error"]["message"].as_str();
                            let output = error.or(info["output"].as_str()).unwrap_or("");
                            out.push(DriverEvent::Item(TranscriptItem::ToolResult {
                                call_id,
                                output: emp_import::util::truncate(output, emp_import::util::MAX_TOOL_TEXT),
                                is_error: state == "ERROR" || error.is_some(),
                                ts,
                            }));
                        }
                    }
                    _ => {}
                }
            }
            "result" => {
                let r = &rec["result"];
                let ok = r["status"] == "SUCCESS";
                if !ok {
                    let msg = r["error"]["message"].as_str().or(r["error"].as_str()).or(r["status"].as_str()).unwrap_or("turn failed");
                    out.push(DriverEvent::Item(system(format!("agy: {}", emp_import::util::truncate(msg, 500)))));
                }
                if r["denied_actions"].as_array().is_some_and(|a| !a.is_empty()) {
                    out.push(DriverEvent::Item(system(
                        "Antigravity wanted to run something it needs permission for, and EMP can't ask in headless mode, \
                         so it was refused. Set this agent's autonomy to 3 to allow shell commands."
                            .into(),
                    )));
                }
                out.push(DriverEvent::TurnEnded { aborted: !ok });
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real two-turn run (agy 1.4.3), conversation ids and tool list trimmed.
    const RUN: &str = r#"{"event":"init","conversation_id":"c1","init":{"model":"gemini-3.8-flash-low","cwd":"/tmp/agy-t","tools":["read_file"]}}
{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"conversation_id":"c1","step_index":3,"state":"DONE","step_type":"system_message","duration_seconds":0.000172}}
{"event":"step_update","step_update":{"conversation_id":"c1","step_index":4,"state":"DONE","step_type":"agent_response","duration_seconds":2.004911,"usage":{"input_tokens":6821,"output_tokens":25,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":6846}}}
{"event":"step_update","step_update":{"conversation_id":"c1","step_index":5,"state":"ACTIVE","step_type":"tool","tool_name":"read_file","tool_info":{"name":"read_file","parameters":{"Path":"/tmp/agy-t/a.txt"}}}}
{"event":"step_update","step_update":{"conversation_id":"c1","step_index":5,"state":"DONE","step_type":"tool","tool_name":"read_file","duration_seconds":0.051653,"tool_info":{"name":"read_file","parameters":{"Path":"/tmp/agy-t/a.txt"},"output":"1 lines, 6 bytes"}}}
{"event":"step_update","step_update":{"conversation_id":"c1","step_index":6,"state":"ACTIVE","step_type":"agent_response","text_delta":"hello"}}
{"event":"step_update","step_update":{"conversation_id":"c1","step_index":6,"state":"DONE","step_type":"agent_response","text_delta":"\n","duration_seconds":2.70,"usage":{"input_tokens":6932,"output_tokens":1,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":6933}}}
{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"hello\n","duration_seconds":17.53,"num_turns":2,"usage":{"input_tokens":20354,"output_tokens":27,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":20381}}}"#;

    fn spec(autonomy: u8) -> DriverSpec {
        DriverSpec {
            harness: emp_protocol::Harness::Antigravity,
            cwd: "/w".into(),
            resume: None,
            model: Some("gemini-4-argon-medium".into()),
            effort: None,
            instructions: None,
            profile: None,
            autonomy,
            compact_at: crate::state::CompactAt::none(),
            mcp: None,
            grants: Default::default(),
        }
    }

    #[test]
    fn maps_a_real_run() {
        let mut m = Mapper::default();
        let events: Vec<DriverEvent> = RUN.lines().flat_map(|l| m.handle(&serde_json::from_str(l).unwrap())).collect();
        assert!(matches!(&events[0], DriverEvent::Ready { import_key } if import_key == "antigravity:c1"));
        let items: Vec<&TranscriptItem> =
            events.iter().filter_map(|e| if let DriverEvent::Item(i) = e { Some(i) } else { None }).collect();
        assert!(matches!(items[0], TranscriptItem::ToolCall { name, input, .. } if name == "read_file" && input.contains("a.txt")));
        assert!(matches!(items[1], TranscriptItem::ToolResult { output, is_error: false, .. } if output == "1 lines, 6 bytes"));
        assert!(matches!(items[2], TranscriptItem::Assistant { text, .. } if text == "hello"));
        assert_eq!(items.len(), 3);
        assert!(events.iter().any(|e| matches!(e, DriverEvent::Partial(PartialKind::Assistant, t) if t == "hello")));
        assert!(matches!(events.last().unwrap(), DriverEvent::TurnEnded { aborted: false }));
    }

    #[test]
    fn denied_commands_explain_themselves() {
        let mut m = Mapper::default();
        let ev = m.handle(&serde_json::json!({"event":"step_update","step_update":{"step_index":2,"state":"ERROR","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"},"error":{"type":"TOOL_ERROR","message":"permission check failed"}}}}));
        assert!(matches!(&ev[1], DriverEvent::Item(TranscriptItem::ToolResult { is_error: true, output, .. }) if output.contains("permission")));
        let ev = m.handle(&serde_json::json!({"event":"result","result":{"status":"SUCCESS","denied_actions":[{"action":"command"}]}}));
        assert!(matches!(&ev[0], DriverEvent::Item(TranscriptItem::System { text, .. }) if text.contains("autonomy to 3")));
        assert!(matches!(&ev[1], DriverEvent::TurnEnded { aborted: false }));
    }

    #[test]
    fn args_follow_autonomy() {
        let a = args(&spec(1), None, "-hi");
        assert_eq!(a, ["--print=-hi", "--output-format", "stream-json", "--model", "gemini-4-argon-medium", "--mode", "accept-edits"]);
        let a = args(&spec(3), Some("c1"), "x");
        assert!(a.windows(2).any(|w| w == ["--conversation", "c1"]));
        assert!(a.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(!args(&spec(0), None, "x").iter().any(|a| a == "--mode" || a.starts_with("--danger")));
    }
}
