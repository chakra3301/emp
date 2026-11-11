//! Codex driver: Codex's app-server protocol (JSON-RPC 2.0, codex-cli 0.163; the protocol is what
//! `codex app-server generate-ts` prints).
//!
//! Transport: Codex runs one shared app-server per home (`codex app-server --managed-daemon`,
//! WebSocket on `<CODEX_HOME>/app-server-control/app-server-control.sock`). Codex's TUI and app
//! are clients of it, and it keeps threads loaded after they disconnect. EMP joins it too, so a
//! thread open in a Terminal Codex is the same live thread in EMP: no second writer, and both see
//! every turn. Without that daemon (or for another `CODEX_HOME`), EMP runs its own
//! `codex app-server` over stdio, which can't open a thread someone else holds.
//!
//! Start: `initialize` → `initialized` → `thread/start` (new) or `thread/resume` (existing), then
//! `turn/start` per message, `turn/steer` mid-turn, `turn/interrupt` to stop. Notifications:
//! `turn/started|completed`, `item/started|completed`, streaming deltas, `thread/tokenUsage/updated`
//! and `thread/goal/updated`.
//!
//! Goals (`/goal`): while a goal is `active` Codex starts turns on its own until it marks the goal
//! complete, so `TurnStarted` arrives without a prompt. Codex keeps the goal active when a turn is
//! interrupted; EMP's Stop also pauses it, like Esc in Codex's TUI. Resuming a thread whose goal
//! is active continues it straight away.
//!
//! Codex refuses a second writer: `thread/resume` fails with "already has an active writer" while
//! the session is open in a Codex TUI or another app-server (`elsewhere.rs` finds which).
//!
//! Approvals follow agent autonomy. New threads: 0 → read-only, 1–2 → workspace-write, both
//! `on-request`; 3 (full access, the default) → no sandbox, never asks, set again on every
//! `turn/start` so a thread already loaded in Codex's shared app-server gets it too. Resumed
//! threads keep their own settings (from `~/.codex/config.toml`) except at autonomy 3. Requests
//! are question cards below 2 and allowed automatically from 2.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use emp_protocol::{GoalStatus, PartialKind, Question, QuestionKind, ThreadGoal, TranscriptItem};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Caps, DriverCmd, DriverEvent, DriverHandle, DriverSpec, Msg, Proc};

const CAPS: Caps = Caps { native_steer: true, native_follow_up: false, echoes_user: false };

/// Codex's own name for threads EMP starts, so they're never mistaken for automated runs.
const THREAD_SOURCE: &str = "emp";

/// Ids of questions EMP asks itself (answers stay in the driver).
const Q_MODEL: &str = "emp:model";
/// Prefix of question ids that answer a Codex server request.
const Q_CODEX: &str = "codex:";

const ALLOW: &str = "Allow";
const ALLOW_SESSION: &str = "Allow for this session";
const DENY: &str = "Deny";

pub fn args(spec: &DriverSpec) -> Vec<String> {
    let mut a = vec!["app-server".to_string()];
    // Read when the process starts: a changed limit applies from the next driver.
    if let Some(n) = spec.compact_at.get() {
        a.push("-c".into());
        a.push(format!("model_auto_compact_token_limit={n}"));
    }
    a
}

fn env(spec: &DriverSpec) -> Vec<(&'static str, String)> {
    spec.resume.as_ref().and_then(|s| s.home.clone()).map(|h| ("CODEX_HOME", h)).into_iter().collect()
}

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;

/// Where EMP speaks the protocol: the shared daemon, or its own process.
enum Link {
    Shared(Box<Socket>),
    Own(Box<Proc>),
}

impl Link {
    async fn send(&mut self, v: &Value) -> Result<()> {
        match self {
            Link::Own(p) => p.send(v).await,
            Link::Shared(ws) => {
                ws.send(tokio_tungstenite::tungstenite::Message::text(v.to_string())).await?;
                Ok(())
            }
        }
    }

    async fn next(&mut self) -> Option<Value> {
        match self {
            Link::Own(p) => p.next().await,
            Link::Shared(ws) => loop {
                match ws.next().await? {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => {
                        if let Ok(v) = serde_json::from_str(t.as_str()) {
                            return Some(v);
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => return None,
                    Ok(_) => {}
                }
            },
        }
    }
}

/// The shared app-server's socket for the default Codex home.
fn shared_socket() -> PathBuf {
    let home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| emp_core::paths::home_dir().join(".codex"));
    home.join("app-server-control").join("app-server-control.sock")
}

async fn connect_shared(path: &std::path::Path) -> Result<Socket> {
    let stream = tokio::time::timeout(std::time::Duration::from_secs(3), tokio::net::UnixStream::connect(path))
        .await
        .context("timed out")??;
    let (ws, _) = tokio::time::timeout(std::time::Duration::from_secs(5), tokio_tungstenite::client_async("ws://localhost/", stream))
        .await
        .context("handshake timed out")??;
    Ok(ws)
}

/// Ids of the threads Codex's shared app-server is running a turn on right now (a goal working,
/// or a turn started from Codex's TUI). Read-only: `thread/read` doesn't subscribe or resume.
/// Empty when the shared server isn't up.
pub async fn running_threads() -> Result<Vec<String>> {
    let path = shared_socket();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut ws = connect_shared(&path).await?;
    let found = async {
        let mut link = SharedRpc { ws: &mut ws, next: 0 };
        link.call("initialize", json!({"clientInfo": {"name": "emp", "title": "EMP", "version": env!("CARGO_PKG_VERSION")}, "capabilities": null})).await?;
        link.ws.send(tokio_tungstenite::tungstenite::Message::text(json!({"method": "initialized"}).to_string())).await?;
        let loaded = link.call("thread/loaded/list", json!({})).await?;
        let mut running = Vec::new();
        for id in loaded["data"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let r = link.call("thread/read", json!({"threadId": id})).await?;
            if r["thread"]["status"]["type"] == "active" {
                running.push(id.to_string());
            }
        }
        anyhow::Ok(running)
    };
    let found = tokio::time::timeout(std::time::Duration::from_secs(10), found).await.context("codex's app-server didn't answer")?;
    let _ = ws.close(None).await;
    found
}

/// Request/response over the shared socket, skipping notifications.
struct SharedRpc<'a> {
    ws: &'a mut Socket,
    next: u64,
}

impl SharedRpc<'_> {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.ws.send(tokio_tungstenite::tungstenite::Message::text(json!({"id": id, "method": method, "params": params}).to_string())).await?;
        loop {
            let msg = self.ws.next().await.context("the connection closed")??;
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else { continue };
            let v: Value = serde_json::from_str(t.as_str()).unwrap_or_default();
            if v["id"] == id && v.get("method").is_none() {
                if let Some(e) = v.get("error") {
                    anyhow::bail!("{method}: {}", e["message"].as_str().unwrap_or("failed"));
                }
                return Ok(v["result"].clone());
            }
        }
    }
}

pub fn spawn(spec: DriverSpec) -> Result<(DriverHandle, mpsc::UnboundedReceiver<DriverEvent>)> {
    // Threads of another CODEX_HOME (Orion's) and EMP-run limits need EMP's own process.
    let shared = spec.resume.as_ref().is_none_or(|s| s.home.is_none()) && shared_socket().exists();
    let own = if shared { None } else { Some(Proc::spawn_env("codex", &args(&spec), &spec.cwd, &env(&spec))?) };
    let pid = own.as_ref().and_then(|p| p.pid);
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let mut mapper = Mapper::new(&spec);
    tokio::spawn(async move {
        let mut link = match own {
            Some(p) => Link::Own(Box::new(p)),
            None => match connect_shared(&shared_socket()).await {
                Ok(ws) => Link::Shared(Box::new(ws)),
                Err(e) => {
                    tracing::info!("codex's shared app-server isn't reachable ({e:#}); running EMP's own");
                    match Proc::spawn_env("codex", &args(&spec), &spec.cwd, &env(&spec)) {
                        Ok(p) => Link::Own(Box::new(p)),
                        Err(e) => {
                            let _ = ev_tx.send(DriverEvent::Exited(Some(format!("{e:#}"))));
                            return;
                        }
                    }
                }
            },
        };
        if matches!(link, Link::Shared(_)) {
            mapper.shared = true;
        }
        for rec in mapper.start() {
            let _ = link.send(&rec).await;
        }
        let mut shutting_down = false;
        loop {
            let step = tokio::select! {
                cmd = cmd_rx.recv(), if !shutting_down => match cmd {
                    Some(DriverCmd::Shutdown) | None => {
                        shutting_down = true;
                        break;
                    }
                    Some(cmd) => mapper.cmd(cmd),
                },
                rec = link.next() => {
                    let Some(rec) = rec else { break };
                    mapper.handle(&rec)
                }
            };
            for rec in &step.send {
                let _ = link.send(rec).await;
            }
            for e in step.events {
                let _ = ev_tx.send(e);
            }
            if mapper.dead.is_some() {
                break;
            }
        }
        let error = match link {
            // Leaving the shared server leaves the thread loaded there, as Codex's TUI does.
            Link::Shared(mut ws) => {
                let _ = ws.as_mut().close(None).await;
                mapper.dead.take().or_else(|| (!shutting_down).then(|| "Codex's app-server closed the connection".to_string()))
            }
            Link::Own(mut proc) => {
                proc.stdin.take();
                proc.shutdown().await;
                let status = proc.child.wait().await.ok();
                mapper.dead.take().or_else(|| {
                    (!shutting_down).then(|| {
                        let code = status.and_then(|s| s.code()).map_or("signal".into(), |c| c.to_string());
                        format!("codex exited ({code}): {}", proc.stderr_summary())
                    })
                })
            }
        };
        let _ = ev_tx.send(DriverEvent::Exited(error));
    });
    Ok((DriverHandle { cmds: cmd_tx, pid, caps: CAPS }, ev_rx))
}

fn system(text: impl Into<String>) -> DriverEvent {
    DriverEvent::Item(TranscriptItem::System { text: text.into(), ts: Some(emp_core::now_ms()) })
}

/// What one input produced: events for the live layer and records to write to Codex.
#[derive(Default, Debug)]
pub struct Step {
    pub events: Vec<DriverEvent>,
    pub send: Vec<Value>,
}

impl Step {
    fn event(mut self, e: DriverEvent) -> Self {
        self.events.push(e);
        self
    }

    fn join(mut self, other: Step) -> Self {
        self.events.extend(other.events);
        self.send.extend(other.send);
        self
    }
}

/// What a request EMP sent was for, so its response can be handled.
#[derive(Debug)]
enum Pending {
    Init,
    Open,
    /// `turn/start`; ends the turn on failure.
    Turn,
    /// `turn/steer` with its input; a failed steer (the turn just ended) starts a turn instead.
    Steer(Vec<Value>),
    /// `thread/goal/set`; `from_prompt` when the line arrived as a message (New thread), which
    /// counts as a turn until the goal starts one.
    Goal { from_prompt: bool, starts: bool },
    /// `thread/goal/get`: `show` prints it, otherwise it only sets the live goal.
    GoalGet { show: bool, from_prompt: bool },
    GoalClear { from_prompt: bool },
    Compact,
    Models,
    Rename,
    Limits,
    /// `thread/turns/list` after joining a thread mid-turn: the turn to steer or interrupt.
    ActiveTurn,
    /// Nothing to do with the reply but report errors.
    Quiet,
}

/// A server request waiting on the owner.
#[derive(Debug)]
enum Waiting {
    /// Command, file change or permissions approval.
    Approval { rpc: Value, kind: ApprovalKind },
    /// One question of an `item/tool/requestUserInput`; the reply goes once all are answered.
    Input { rpc: Value, qid: String },
}

#[derive(Debug, Clone)]
enum ApprovalKind {
    Command,
    File,
    Permissions(Value),
}

/// Maps between EMP's driver commands and the app-server protocol. Pure, so tests drive it.
pub struct Mapper {
    autonomy: u8,
    /// Talking to Codex's shared app-server: process flags don't apply, so the compaction limit
    /// goes in the thread's config.
    pub shared: bool,
    compact_at: Option<u64>,
    open: Value,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    pub thread: Option<String>,
    opened: bool,
    /// Commands that arrived before the thread was open.
    early: Vec<DriverCmd>,
    turn: Option<String>,
    /// The last turn that completed: its late item events don't start a new one.
    ended: Option<String>,
    pub goal: Option<ThreadGoal>,
    /// The first `thread/goal/get` answered: until then, goal updates only record (an old,
    /// finished goal isn't news).
    goal_known: bool,
    model: Option<String>,
    /// `/model` choice for the next turn (Codex applies it to that turn and the ones after).
    next_model: Option<String>,
    /// Effort for the first turn of a new thread (later turns inherit it).
    effort: Option<String>,
    /// Streaming text per item id.
    texts: HashMap<String, String>,
    tool_out: String,
    waiting: HashMap<String, Waiting>,
    /// Answers gathered for multi-question input requests, by request id.
    inputs: HashMap<String, serde_json::Map<String, Value>>,
    /// A fatal error: the driver stops and reports it.
    pub dead: Option<String>,
}

fn rpc_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl Mapper {
    pub fn new(spec: &DriverSpec) -> Self {
        let full = spec.autonomy >= 3;
        let open = match &spec.resume {
            Some(s) => {
                let mut p = json!({"threadId": s.session_id, "excludeTurns": true});
                if full {
                    p["approvalPolicy"] = json!("never");
                    p["sandbox"] = json!("danger-full-access");
                }
                json!({"method": "thread/resume", "params": p})
            }
            None => {
                let sandbox = match spec.autonomy {
                    0 => "read-only",
                    1 | 2 => "workspace-write",
                    _ => "danger-full-access",
                };
                let mut p = json!({
                    "cwd": spec.cwd.to_string_lossy(),
                    "sandbox": sandbox,
                    "approvalPolicy": if full { "never" } else { "on-request" },
                    "threadSource": THREAD_SOURCE,
                });
                if let Some(m) = &spec.model {
                    p["model"] = json!(m);
                }
                if let Some(i) = spec.instructions.as_deref().filter(|i| !i.trim().is_empty()) {
                    p["developerInstructions"] = json!(i);
                }
                json!({"method": "thread/start", "params": p})
            }
        };
        Self {
            autonomy: spec.autonomy,
            shared: false,
            compact_at: spec.compact_at.get(),
            open,
            next_id: 0,
            pending: HashMap::new(),
            thread: None,
            opened: false,
            early: Vec::new(),
            turn: None,
            ended: None,
            goal: None,
            goal_known: false,
            model: spec.model.clone(),
            next_model: None,
            effort: spec.resume.is_none().then(|| spec.effort.clone()).flatten(),
            texts: HashMap::new(),
            tool_out: String::new(),
            waiting: HashMap::new(),
            inputs: HashMap::new(),
            dead: None,
        }
    }

    fn request(&mut self, method: &str, params: Value, why: Pending) -> Value {
        self.next_id += 1;
        self.pending.insert(self.next_id, why);
        json!({"id": self.next_id, "method": method, "params": params})
    }

    fn tid(&self) -> String {
        self.thread.clone().unwrap_or_default()
    }

    /// The first records: the handshake.
    pub fn start(&mut self) -> Vec<Value> {
        vec![self.request(
            "initialize",
            json!({"clientInfo": {"name": "emp", "title": "EMP", "version": env!("CARGO_PKG_VERSION")}, "capabilities": null}),
            Pending::Init,
        )]
    }

    pub fn cmd(&mut self, cmd: DriverCmd) -> Step {
        if !self.opened && !matches!(cmd, DriverCmd::Interrupt) {
            self.early.push(cmd);
            return Step::default();
        }
        match cmd {
            DriverCmd::Prompt(m) => match goal_line(&m.text) {
                Some(args) => self.goal_command(&args, true),
                None => self.turn_start(input(&m)),
            },
            DriverCmd::Steer(m) | DriverCmd::FollowUp(m) => {
                let input = input(&m);
                match self.turn.clone() {
                    Some(turn) => {
                        let tid = self.tid();
                        let rec = self.request(
                            "turn/steer",
                            json!({"threadId": tid, "input": input, "expectedTurnId": turn}),
                            Pending::Steer(input),
                        );
                        Step { send: vec![rec], ..Step::default() }
                    }
                    None => self.turn_start(input),
                }
            }
            DriverCmd::Interrupt => self.interrupt(),
            DriverCmd::SetModel(m) => {
                let note = system(format!("Model: {m}"));
                self.next_model = Some(m);
                Step::default().event(note)
            }
            DriverCmd::Command { name, args } => self.command(&name, args.trim()),
            DriverCmd::Answer { id, value, confirmed, cancelled } => self.answer(&id, value, confirmed, cancelled),
            DriverCmd::Shutdown => Step::default(),
        }
    }

    fn turn_start(&mut self, input: Vec<Value>) -> Step {
        let mut p = json!({"threadId": self.tid(), "input": input});
        if let Some(m) = self.next_model.take() {
            p["model"] = json!(m);
            self.model = Some(m);
        }
        if let Some(e) = self.effort.take() {
            p["effort"] = json!(e);
        }
        if self.autonomy >= emp_protocol::FULL_ACCESS {
            p["approvalPolicy"] = json!("never");
            p["sandboxPolicy"] = json!({"type": "dangerFullAccess"});
        }
        let rec = self.request("turn/start", p, Pending::Turn);
        Step { send: vec![rec], ..Step::default() }
    }

    fn interrupt(&mut self) -> Step {
        let mut step = Step::default();
        if let (Some(turn), Some(tid)) = (self.turn.clone(), self.thread.clone()) {
            step.send.push(self.request("turn/interrupt", json!({"threadId": tid, "turnId": turn}), Pending::Quiet));
        }
        // Stop means stop: an active goal would start the next turn by itself.
        if self.goal.as_ref().is_some_and(|g| g.status == GoalStatus::Active) {
            let tid = self.tid();
            step.send.push(self.request(
                "thread/goal/set",
                json!({"threadId": tid, "status": "paused", "origin": "user"}),
                Pending::Goal { from_prompt: false, starts: false },
            ));
        }
        step
    }

    /// `/goal [objective|pause|resume|clear]`, as in Codex's TUI.
    fn goal_command(&mut self, args: &str, from_prompt: bool) -> Step {
        let tid = self.tid();
        let rec = match args {
            "" => self.request("thread/goal/get", json!({"threadId": tid}), Pending::GoalGet { show: true, from_prompt }),
            "pause" => self.request(
                "thread/goal/set",
                json!({"threadId": tid, "status": "paused", "origin": "user"}),
                Pending::Goal { from_prompt, starts: false },
            ),
            // The goal may not be known yet (its `thread/goal/get` is still out); Codex says if
            // there is none.
            "resume" => self.request(
                "thread/goal/set",
                json!({"threadId": tid, "status": "active", "origin": "user"}),
                Pending::Goal { from_prompt, starts: true },
            ),
            "clear" => self.request("thread/goal/clear", json!({"threadId": tid, "origin": "user"}), Pending::GoalClear { from_prompt }),
            objective => self.request(
                "thread/goal/set",
                json!({"threadId": tid, "objective": objective, "status": "active", "origin": "user"}),
                Pending::Goal { from_prompt, starts: true },
            ),
        };
        Step { send: vec![rec], ..Step::default() }
    }

    /// Native slash commands (`CommandRun::Native` for Codex in `commands.rs`).
    pub fn command(&mut self, name: &str, args: &str) -> Step {
        let tid = self.tid();
        let rec = match name {
            "goal" => return self.goal_command(args, false),
            "compact" => self.request("thread/compact/start", json!({"threadId": tid}), Pending::Compact),
            "model" if args.is_empty() => self.request("model/list", json!({"limit": 100}), Pending::Models),
            "model" => {
                self.next_model = Some(args.to_string());
                return Step::default().event(system(format!("Model: {args}, from your next message")));
            }
            "rename" | "name" if args.is_empty() => {
                return Step::default().event(system(format!("/{name} takes the new name")));
            }
            "rename" | "name" => self.request("thread/name/set", json!({"threadId": tid, "name": args}), Pending::Rename),
            "status" => self.request("account/rateLimits/read", Value::Null, Pending::Limits),
            other => return Step::default().event(system(format!("/{other} doesn't run in chat; EMP opens it in a terminal"))),
        };
        Step { send: vec![rec], ..Step::default() }
    }

    pub fn answer(&mut self, id: &str, value: Option<String>, confirmed: Option<bool>, cancelled: bool) -> Step {
        if id == Q_MODEL {
            return match value.filter(|_| !cancelled) {
                Some(m) => self.command("model", &m),
                None => Step::default(),
            };
        }
        let Some(w) = self.waiting.remove(id) else { return Step::default() };
        match w {
            Waiting::Approval { rpc, kind } => {
                let choice = if cancelled { DENY.to_string() } else { value.unwrap_or_else(|| if confirmed == Some(true) { ALLOW.into() } else { DENY.into() }) };
                Step { send: vec![approval_reply(rpc, &kind, &choice)], ..Step::default() }
            }
            Waiting::Input { rpc, qid } => {
                let key = rpc_key(&rpc);
                let answers = self.inputs.entry(key.clone()).or_default();
                let given: Vec<String> = if cancelled { Vec::new() } else { value.into_iter().collect() };
                answers.insert(qid, json!({"answers": given}));
                let still = self.waiting.values().any(|w| matches!(w, Waiting::Input { rpc: r, .. } if rpc_key(r) == key));
                if still {
                    return Step::default();
                }
                let answers = self.inputs.remove(&key).unwrap_or_default();
                Step { send: vec![json!({"id": rpc, "result": {"answers": answers}})], ..Step::default() }
            }
        }
    }

    pub fn handle(&mut self, rec: &Value) -> Step {
        match (rec.get("id"), rec.get("method").and_then(Value::as_str)) {
            (Some(id), Some(method)) => self.server_request(id.clone(), method, &rec["params"]),
            (Some(id), None) => {
                let Some(why) = id.as_u64().and_then(|n| self.pending.remove(&n)) else { return Step::default() };
                match rec.get("error") {
                    Some(e) if !e.is_null() => self.failed(why, e["message"].as_str().unwrap_or("request failed")),
                    _ => self.replied(why, &rec["result"]),
                }
            }
            (None, Some(method)) => self.notification(method, &rec["params"]),
            _ => Step::default(),
        }
    }

    fn replied(&mut self, why: Pending, r: &Value) -> Step {
        match why {
            Pending::Init => {
                let mut open = self.open.clone();
                if let (true, Some(n)) = (self.shared, self.compact_at) {
                    open["params"]["config"] = json!({"model_auto_compact_token_limit": n});
                }
                let rec = self.request(open["method"].as_str().unwrap_or("thread/start"), open["params"].clone(), Pending::Open);
                Step { send: vec![json!({"method": "initialized"}), rec], ..Step::default() }
            }
            Pending::Open => {
                let Some(tid) = r["thread"]["id"].as_str() else {
                    self.dead = Some("codex didn't say which thread it opened".into());
                    return Step::default();
                };
                self.thread = Some(tid.to_string());
                self.opened = true;
                if let Some(m) = r["model"].as_str() {
                    self.model = Some(m.to_string());
                }
                let mut step = Step::default().event(DriverEvent::Ready { import_key: format!("codex:{tid}") });
                let goal = self.request("thread/goal/get", json!({"threadId": tid}), Pending::GoalGet { show: false, from_prompt: false });
                step.send.push(goal);
                // Joined mid-turn (a goal at work, or a turn from Codex's TUI): show it running,
                // and find the turn so Stop and steering work.
                if r["thread"]["status"]["type"] == "active" && self.turn.is_none() {
                    step = step.event(DriverEvent::TurnStarted);
                    let rec = self.request("thread/turns/list", json!({"threadId": tid, "limit": 1, "sortDirection": "desc"}), Pending::ActiveTurn);
                    step.send.push(rec);
                }
                for cmd in std::mem::take(&mut self.early) {
                    let s = self.cmd(cmd);
                    step = step.join(s);
                }
                step
            }
            Pending::Compact => Step::default().event(system("Compacting the conversation…")),
            Pending::ActiveTurn => {
                let turn = &r["data"][0];
                if self.turn.is_none() && turn["status"] == "inProgress" && turn["id"].as_str() != self.ended.as_deref() {
                    self.turn = turn["id"].as_str().map(str::to_owned);
                }
                Step::default()
            }
            Pending::Turn | Pending::Steer(_) | Pending::Quiet => Step::default(),
            Pending::Goal { from_prompt, starts } => {
                self.goal_known = true;
                let step = self.goal_changed(parse_goal(&r["goal"]));
                // A goal that won't start a turn ends the "turn" its message began; an active one
                // starts a real turn, which ends it.
                if from_prompt && !starts { step.event(DriverEvent::TurnEnded { aborted: false }) } else { step }
            }
            Pending::GoalGet { show, from_prompt } => {
                let goal = parse_goal(&r["goal"]);
                self.goal = goal.clone();
                self.goal_known = true;
                let mut step = Step::default().event(DriverEvent::Goal(goal.clone()));
                if show {
                    step = step.event(system(match &goal {
                        Some(g) => describe_goal(g),
                        None => "No goal. /goal <what to achieve> sets one; Codex then keeps working until it's done.".into(),
                    }));
                }
                if from_prompt {
                    step = step.event(DriverEvent::TurnEnded { aborted: false });
                }
                step
            }
            Pending::GoalClear { from_prompt } => {
                self.goal = None;
                let step = Step::default().event(DriverEvent::Goal(None)).event(system("Goal cleared."));
                if from_prompt { step.event(DriverEvent::TurnEnded { aborted: false }) } else { step }
            }
            Pending::Models => {
                let options: Vec<String> = r["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|m| m["hidden"] != true)
                    .filter_map(|m| m["id"].as_str().map(str::to_owned))
                    .collect();
                if options.is_empty() {
                    return Step::default().event(system("Codex didn't list any models"));
                }
                Step::default().event(DriverEvent::Ask(Question {
                    id: Q_MODEL.into(),
                    kind: QuestionKind::Select,
                    title: "Switch this chat to".into(),
                    message: self.model.as_ref().map(|m| format!("Now: {m}. The new model applies from your next message.")),
                    options,
                    placeholder: None,
                    prefill: None,
                }))
            }
            Pending::Rename => Step::default().event(system("Renamed.")).event(DriverEvent::SessionChanged),
            Pending::Limits => Step::default().event(system(self.status_line(&r["rateLimits"]))),
        }
    }

    fn failed(&mut self, why: Pending, message: &str) -> Step {
        let message = clean_error(message);
        match why {
            Pending::Init => {
                self.dead = Some(format!("codex app-server didn't start: {message}"));
                Step::default()
            }
            Pending::Open => {
                self.dead = Some(if message.contains("active writer") {
                    "This Codex session is open somewhere else (Codex in a terminal, or the Codex app). Close it there, then send again.".into()
                } else {
                    format!("codex couldn't open the session: {message}")
                });
                Step::default()
            }
            Pending::Turn => Step::default()
                .event(system(format!("codex: {message}")))
                .event(DriverEvent::TurnFailed(format!("codex: {message}")))
                .event(DriverEvent::TurnEnded { aborted: true }),
            // The turn ended between EMP's check and the steer: send it as a new turn.
            Pending::Steer(input) => self.turn_start(input),
            Pending::Goal { from_prompt, .. } | Pending::GoalGet { from_prompt, .. } | Pending::GoalClear { from_prompt } => {
                let step = Step::default().event(system(format!("/goal: {message}")));
                if from_prompt { step.event(DriverEvent::TurnEnded { aborted: true }) } else { step }
            }
            _ => Step::default().event(system(format!("codex: {message}"))),
        }
    }

    fn status_line(&self, limits: &Value) -> String {
        let mut parts = Vec::new();
        if let Some(m) = &self.model {
            parts.push(format!("Model {m}"));
        }
        for (key, fallback) in [("primary", "Usage"), ("secondary", "Second limit")] {
            let w = &limits[key];
            let Some(used) = w["usedPercent"].as_f64() else { continue };
            let name = match w["windowDurationMins"].as_u64() {
                Some(300) => "5-hour limit".to_string(),
                Some(10080) => "Weekly limit".to_string(),
                Some(m) if m % 1440 == 0 => format!("{}-day limit", m / 1440),
                Some(m) if m % 60 == 0 => format!("{}-hour limit", m / 60),
                _ => fallback.to_string(),
            };
            let resets = w["resetsAt"].as_i64().map(|t| format!(", resets in {}", duration(t - emp_core::now_ms() / 1000))).unwrap_or_default();
            parts.push(format!("{name} {used:.0}% used{resets}"));
        }
        if let Some(g) = &self.goal {
            parts.push(format!("Goal {}", status_word(g.status)));
        }
        if parts.is_empty() { "Codex didn't report its status".into() } else { parts.join(" · ") }
    }

    fn server_request(&mut self, rpc: Value, method: &str, p: &Value) -> Step {
        let key = format!("{Q_CODEX}{}", rpc_key(&rpc));
        let approval = |title: &str, message: String, kind: ApprovalKind, options: [&str; 3]| {
            (
                Question {
                    id: key.clone(),
                    kind: QuestionKind::Select,
                    title: title.into(),
                    message: Some(message).filter(|m| !m.trim().is_empty()),
                    options: options.iter().map(|o| o.to_string()).collect(),
                    placeholder: None,
                    prefill: None,
                },
                kind,
            )
        };
        let asked = match method {
            "item/commandExecution/requestApproval" => {
                let title = if p["kind"] == "writeStdin" { "Send input to the running command?" } else { "Run this command?" };
                let mut msg = p["command"].as_str().unwrap_or("").to_string();
                if let Some(r) = p["reason"].as_str() {
                    msg = format!("{msg}\n\n{r}");
                }
                Some(approval(title, msg.trim().to_string(), ApprovalKind::Command, [ALLOW, ALLOW_SESSION, DENY]))
            }
            "item/fileChange/requestApproval" => {
                let mut msg = p["reason"].as_str().unwrap_or("").to_string();
                if let Some(root) = p["grantRoot"].as_str() {
                    msg = format!("{msg}\n\nWrite access to {root}").trim().to_string();
                }
                Some(approval("Let Codex change these files?", msg, ApprovalKind::File, [ALLOW, ALLOW_SESSION, DENY]))
            }
            "item/permissions/requestApproval" => {
                let mut msg = p["reason"].as_str().unwrap_or("").to_string();
                msg = format!("{msg}\n\n{}", emp_import::util::display_json(&p["permissions"])).trim().to_string();
                Some(approval("Give Codex more access?", msg, ApprovalKind::Permissions(p["permissions"].clone()), ["Allow for this turn", ALLOW_SESSION, DENY]))
            }
            "item/tool/requestUserInput" => {
                let mut step = Step::default();
                for q in p["questions"].as_array().into_iter().flatten() {
                    let qid = q["id"].as_str().unwrap_or("q").to_string();
                    let options: Vec<String> =
                        q["options"].as_array().into_iter().flatten().filter_map(|o| o["label"].as_str().map(str::to_owned)).collect();
                    let id = format!("{key}:{qid}");
                    self.waiting.insert(id.clone(), Waiting::Input { rpc: rpc.clone(), qid });
                    step.events.push(DriverEvent::Ask(Question {
                        id,
                        kind: if options.is_empty() { QuestionKind::Input } else { QuestionKind::Select },
                        title: q["header"].as_str().filter(|h| !h.is_empty()).or(q["question"].as_str()).unwrap_or("Codex asks").into(),
                        message: q["question"].as_str().filter(|_| q["header"].as_str().is_some_and(|h| !h.is_empty())).map(str::to_owned),
                        options,
                        placeholder: q["isSecret"].as_bool().filter(|s| *s).map(|_| "Not shown in the chat afterwards".into()),
                        prefill: None,
                    }));
                }
                if step.events.is_empty() {
                    step.send.push(json!({"id": rpc, "result": {"answers": {}}}));
                }
                return step;
            }
            "mcpServer/elicitation/request" => {
                let server = p["serverName"].as_str().unwrap_or("an MCP server");
                let what = p["message"].as_str().unwrap_or("");
                let link = p["url"].as_str().map(|u| format!(" ({u})")).unwrap_or_default();
                return Step {
                    events: vec![system(format!("{server} asked for input EMP can't show yet, so it was declined: {what}{link}"))],
                    send: vec![json!({"id": rpc, "result": {"action": "decline", "content": null, "_meta": null}})],
                };
            }
            _ => None,
        };
        let Some((question, kind)) = asked else {
            return Step {
                send: vec![json!({"id": rpc, "error": {"code": -32601, "message": format!("EMP doesn't handle {method}")}})],
                ..Step::default()
            };
        };
        if self.autonomy >= 2 {
            let choice = if matches!(kind, ApprovalKind::Permissions(_)) { "Allow for this turn" } else { ALLOW };
            return Step { send: vec![approval_reply(rpc, &kind, choice)], ..Step::default() };
        }
        self.waiting.insert(key, Waiting::Approval { rpc, kind });
        Step::default().event(DriverEvent::Ask(question))
    }

    fn notification(&mut self, method: &str, p: &Value) -> Step {
        // Sub-agents' threads stream through the same process; they're imported on their own.
        if let (Some(t), Some(mine)) = (p["threadId"].as_str(), self.thread.as_deref())
            && t != mine
        {
            return Step::default();
        }
        let ts = Some(emp_core::now_ms());
        let mut step = Step::default();
        // Items of a turn EMP didn't see start (it joined mid-turn): that turn is running.
        if method.starts_with("item/")
            && self.turn.is_none()
            && let Some(turn) = p["turnId"].as_str().filter(|t| Some(*t) != self.ended.as_deref())
        {
            self.turn = Some(turn.to_string());
            step = step.event(DriverEvent::TurnStarted);
        }
        match method {
            "turn/started" => {
                self.turn = p["turn"]["id"].as_str().map(str::to_owned);
                step = step.event(DriverEvent::TurnStarted);
            }
            "turn/completed" => {
                self.turn = None;
                self.ended = p["turn"]["id"].as_str().map(str::to_owned);
                self.texts.clear();
                self.tool_out.clear();
                let turn = &p["turn"];
                match turn["status"].as_str().unwrap_or("completed") {
                    "interrupted" => step = step.event(system("Stopped.")).event(DriverEvent::TurnEnded { aborted: true }),
                    "failed" => {
                        let msg = turn["error"]["message"].as_str().map(clean_error).unwrap_or_else(|| "the turn failed".into());
                        step = step
                            .event(system(format!("codex: {msg}")))
                            .event(DriverEvent::TurnFailed(format!("codex: {msg}")))
                            .event(DriverEvent::TurnEnded { aborted: true });
                    }
                    _ => step = step.event(DriverEvent::TurnEnded { aborted: false }),
                }
            }
            "item/started" => {
                if let Some(call) = tool_call(&p["item"], ts) {
                    step = step.event(DriverEvent::Item(call));
                }
            }
            "item/completed" => {
                self.tool_out.clear();
                let item = &p["item"];
                if let Some(id) = item["id"].as_str() {
                    self.texts.remove(id);
                }
                for i in completed(item, ts) {
                    step = step.event(DriverEvent::Item(i));
                }
            }
            "item/agentMessage/delta" | "item/plan/delta" => {
                let text = self.stream(p);
                step = step.event(DriverEvent::Partial(PartialKind::Assistant, text));
            }
            "item/reasoning/summaryTextDelta" => {
                let text = self.stream(p);
                step = step.event(DriverEvent::Partial(PartialKind::Thinking, text));
            }
            "item/commandExecution/outputDelta" => {
                self.tool_out.push_str(p["delta"].as_str().unwrap_or(""));
                if self.tool_out.len() > 16_000 {
                    let mut cut = self.tool_out.len() - 8_000;
                    while !self.tool_out.is_char_boundary(cut) {
                        cut += 1;
                    }
                    self.tool_out.drain(..cut);
                }
                step = step.event(DriverEvent::Partial(PartialKind::Tool, self.tool_out.clone()));
            }
            "thread/goal/updated" => step = self.goal_changed(parse_goal(&p["goal"])),
            "thread/goal/cleared" => {
                self.goal = None;
                step = step.event(DriverEvent::Goal(None)).event(system("Goal cleared."));
            }
            "thread/tokenUsage/updated" => {
                let u = &p["tokenUsage"];
                if let Some(used) = u["last"]["totalTokens"].as_u64() {
                    step = step.event(DriverEvent::Context(emp_protocol::ContextUsage {
                        used,
                        window: u["modelContextWindow"].as_u64(),
                        model: self.model.clone(),
                    }));
                }
            }
            "thread/name/updated" => step = step.event(DriverEvent::SessionChanged),
            "thread/compacted" => step = step.event(system("Compacted the conversation.")),
            "serverRequest/resolved" => {
                // Answered or withdrawn by Codex itself (a timeout, the turn ending).
                let key = format!("{Q_CODEX}{}", rpc_key(&p["requestId"]));
                let gone: Vec<String> = self.waiting.keys().filter(|k| **k == key || k.starts_with(&format!("{key}:"))).cloned().collect();
                for k in gone {
                    self.waiting.remove(&k);
                    step = step.event(DriverEvent::Unask(k));
                }
            }
            "error" if p["willRetry"] != true && self.turn.is_none() => {
                if let Some(m) = p["error"]["message"].as_str() {
                    step = step.event(system(format!("codex: {}", clean_error(m))));
                }
            }
            _ => {}
        }
        step
    }

    fn stream(&mut self, p: &Value) -> String {
        let id = p["itemId"].as_str().unwrap_or("").to_string();
        let text = self.texts.entry(id).or_default();
        text.push_str(p["delta"].as_str().unwrap_or(""));
        text.clone()
    }

    /// Records a goal from Codex and says what changed.
    fn goal_changed(&mut self, goal: Option<ThreadGoal>) -> Step {
        let before = std::mem::replace(&mut self.goal, goal.clone());
        let mut step = Step::default();
        if before == goal {
            return step;
        }
        step = step.event(DriverEvent::Goal(goal.clone()));
        let Some(g) = goal.as_ref().filter(|_| self.goal_known) else { return step };
        let new = before.as_ref().is_none_or(|b| b.objective != g.objective);
        let changed = before.as_ref().is_none_or(|b| b.status != g.status);
        if !(new || changed) {
            return step;
        }
        let line = match g.status {
            GoalStatus::Active if new => format!("Goal set: {}", emp_import::util::truncate(&g.objective, 300)),
            GoalStatus::Active => "Goal resumed.".into(),
            GoalStatus::Paused => "Goal paused. /goal resume continues it.".into(),
            GoalStatus::Blocked => "Goal blocked: Codex needs your help to go on. Answer it, or /goal resume to try again.".into(),
            GoalStatus::UsageLimited => "Goal paused at the usage limit. /goal resume continues it once the limit resets.".into(),
            GoalStatus::BudgetLimited => "Goal stopped: its token budget is used up.".into(),
            GoalStatus::Complete => format!("Goal complete ({}).", usage(g)),
        };
        step.event(system(line))
    }
}

/// `/goal …` typed as a message (New thread starts Codex threads that way).
fn goal_line(text: &str) -> Option<String> {
    let t = text.trim();
    let rest = t.strip_prefix("/goal")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim().to_string())
}

fn input(m: &Msg) -> Vec<Value> {
    let mut v = vec![json!({"type": "text", "text": m.text, "text_elements": []})];
    v.extend(m.images.iter().filter(|p| super::image_mime(p).is_some()).map(|p: &PathBuf| json!({"type": "localImage", "path": p})));
    v
}

fn approval_reply(rpc: Value, kind: &ApprovalKind, choice: &str) -> Value {
    let allow = choice != DENY;
    let session = choice == ALLOW_SESSION;
    let result = match kind {
        ApprovalKind::Command | ApprovalKind::File => {
            json!({"decision": if !allow { "decline" } else if session { "acceptForSession" } else { "accept" }})
        }
        ApprovalKind::Permissions(asked) => {
            let mut granted = serde_json::Map::new();
            if allow {
                for k in ["network", "fileSystem"] {
                    if !asked[k].is_null() {
                        granted.insert(k.into(), asked[k].clone());
                    }
                }
            }
            json!({"permissions": granted, "scope": if session { "session" } else { "turn" }})
        }
    };
    json!({"id": rpc, "result": result})
}

pub fn parse_goal(g: &Value) -> Option<ThreadGoal> {
    let objective = g["objective"].as_str()?;
    Some(ThreadGoal {
        objective: objective.to_string(),
        status: GoalStatus::parse(g["status"].as_str()?)?,
        tokens_used: g["tokensUsed"].as_u64().unwrap_or(0),
        token_budget: g["tokenBudget"].as_u64(),
        time_used_seconds: g["timeUsedSeconds"].as_u64().unwrap_or(0),
    })
}

fn status_word(s: GoalStatus) -> &'static str {
    match s {
        GoalStatus::Active => "active",
        GoalStatus::Paused => "paused",
        GoalStatus::Blocked => "blocked",
        GoalStatus::UsageLimited => "paused at the usage limit",
        GoalStatus::BudgetLimited => "out of budget",
        GoalStatus::Complete => "complete",
    }
}

fn usage(g: &ThreadGoal) -> String {
    let tokens = match g.token_budget {
        Some(b) => format!("{} of {} tokens", crate::state::ktok(g.tokens_used), crate::state::ktok(b)),
        None => format!("{} tokens", crate::state::ktok(g.tokens_used)),
    };
    format!("{tokens}, {}", duration(g.time_used_seconds as i64))
}

fn describe_goal(g: &ThreadGoal) -> String {
    let next = match g.status {
        GoalStatus::Active => "/goal pause stops it",
        GoalStatus::Complete => "/goal <new objective> starts another",
        _ => "/goal resume continues it",
    };
    format!("Goal ({}, {}): {}\n{next}; /goal clear removes it.", status_word(g.status), usage(g), emp_import::util::truncate(&g.objective, 600))
}

/// `3d 4h`, `2h 5m`, `40s`.
fn duration(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
    }
}

/// Codex errors sometimes carry the API's JSON body; keep its message.
fn clean_error(m: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(m)
        && let Some(inner) = v["error"]["message"].as_str().or(v["message"].as_str())
    {
        return emp_import::util::truncate(inner, 500);
    }
    emp_import::util::truncate(m, 500)
}

/// The ToolCall item for tools that announce themselves on `item/started`.
fn tool_call(item: &Value, ts: Option<i64>) -> Option<TranscriptItem> {
    let call_id = item["id"].as_str().map(str::to_owned);
    let (name, input) = match item["type"].as_str()? {
        "commandExecution" => ("shell".to_string(), item["command"].as_str().unwrap_or("").to_string()),
        "mcpToolCall" => (
            format!("{}.{}", item["server"].as_str().unwrap_or("mcp"), item["tool"].as_str().unwrap_or("tool")),
            emp_import::util::display_json(&item["arguments"]),
        ),
        "dynamicToolCall" => (
            item["tool"].as_str().unwrap_or("tool").to_string(),
            emp_import::util::display_json(&item["arguments"]),
        ),
        "collabAgentToolCall" => (
            format!("agents.{}", item["tool"].as_str().unwrap_or("call")),
            item["prompt"].as_str().map(str::to_owned).unwrap_or_else(|| emp_import::util::display_json(&item["receiverThreadIds"])),
        ),
        "webSearch" => ("web_search".to_string(), item["query"].as_str().unwrap_or("").to_string()),
        _ => return None,
    };
    Some(TranscriptItem::ToolCall { call_id, name, input: emp_import::util::truncate(&input, emp_import::util::MAX_TOOL_TEXT), ts })
}

/// Transcript items for a finished item.
fn completed(item: &Value, ts: Option<i64>) -> Vec<TranscriptItem> {
    let call_id = item["id"].as_str().map(str::to_owned);
    let max = emp_import::util::MAX_TOOL_TEXT;
    let failed = matches!(item["status"].as_str(), Some("failed" | "declined"));
    match item["type"].as_str().unwrap_or("") {
        "agentMessage" | "plan" => item["text"]
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .map(|t| TranscriptItem::Assistant { text: t.trim_end().into(), model: None, ts })
            .into_iter()
            .collect(),
        "reasoning" => {
            let parts: Vec<&str> = item["summary"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let text = parts.join("\n\n");
            if text.trim().is_empty() { Vec::new() } else { vec![TranscriptItem::Thinking { text, ts }] }
        }
        "commandExecution" => vec![TranscriptItem::ToolResult {
            call_id,
            output: emp_import::util::truncate(item["aggregatedOutput"].as_str().unwrap_or(""), max),
            is_error: failed || item["exitCode"].as_i64().is_some_and(|c| c != 0),
            ts,
        }],
        "fileChange" => {
            let files: Vec<String> = item["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| format!("{} {}", c["kind"]["type"].as_str().unwrap_or("update"), c["path"].as_str().unwrap_or("?")))
                .collect();
            vec![
                TranscriptItem::ToolCall { call_id: call_id.clone(), name: "apply_patch".into(), input: files.join("\n"), ts },
                TranscriptItem::ToolResult {
                    call_id,
                    output: if failed { "Not applied".into() } else { "Applied".into() },
                    is_error: failed,
                    ts,
                },
            ]
        }
        "mcpToolCall" | "dynamicToolCall" => {
            let out = if item["error"].is_null() {
                if item["contentItems"].is_null() { &item["result"] } else { &item["contentItems"] }
            } else {
                &item["error"]
            };
            vec![TranscriptItem::ToolResult {
                call_id,
                output: emp_import::util::truncate(&emp_import::util::display_json(out), max),
                is_error: failed || !item["error"].is_null() || item["success"] == false,
                ts,
            }]
        }
        "collabAgentToolCall" => vec![TranscriptItem::ToolResult {
            call_id,
            output: item["status"].as_str().unwrap_or("done").into(),
            is_error: failed,
            ts,
        }],
        "webSearch" => vec![TranscriptItem::ToolResult { call_id, output: "Searched".into(), is_error: false, ts }],
        "contextCompaction" => vec![TranscriptItem::System { text: "Compacted the conversation.".into(), ts }],
        "enteredReviewMode" => vec![TranscriptItem::System { text: "Reviewing…".into(), ts }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(autonomy: u8, resume: Option<&str>) -> DriverSpec {
        DriverSpec {
            harness: emp_protocol::Harness::Codex,
            cwd: "/w".into(),
            resume: resume.map(|id| emp_protocol::SessionRef {
                harness: emp_protocol::Harness::Codex,
                session_id: id.into(),
                path: None,
                home: None,
                profile: None,
            }),
            model: Some("gpt-6.1-sol".into()),
            effort: Some("high".into()),
            instructions: None,
            profile: None,
            autonomy,
            compact_at: crate::state::CompactAt::none(),
            mcp: None,
            grants: Default::default(),
        }
    }

    /// Runs the handshake and opens thread `T`.
    fn opened(autonomy: u8, resume: Option<&str>) -> (Mapper, Vec<Value>) {
        let mut m = Mapper::new(&spec(autonomy, resume));
        let init = m.start();
        assert_eq!(init[0]["method"], "initialize");
        let s = m.handle(&json!({"id": 1, "result": {}}));
        assert_eq!(s.send[0]["method"], "initialized");
        let open = s.send[1].clone();
        let s = m.handle(&json!({"id": 2, "result": {"thread": {"id": "T"}, "model": "gpt-6.1-sol"}}));
        assert!(matches!(&s.events[0], DriverEvent::Ready { import_key } if import_key == "codex:T"));
        assert_eq!(s.send[0]["method"], "thread/goal/get");
        m.handle(&json!({"id": 3, "result": {"goal": null}}));
        (m, vec![open])
    }

    fn items(s: &Step) -> Vec<&TranscriptItem> {
        s.events.iter().filter_map(|e| if let DriverEvent::Item(i) = e { Some(i) } else { None }).collect()
    }

    fn texts(s: &Step) -> Vec<String> {
        items(s).iter().filter_map(|i| if let TranscriptItem::System { text, .. } = i { Some(text.clone()) } else { None }).collect()
    }

    #[test]
    fn joins_a_turn_already_running() {
        let mut m = Mapper::new(&spec(1, Some("abc")));
        m.start();
        m.handle(&json!({"id": 1, "result": {}}));
        let s = m.handle(&json!({"id": 2, "result": {"thread": {"id": "T", "status": {"type": "active", "activeFlags": []}}}}));
        assert!(s.events.iter().any(|e| matches!(e, DriverEvent::TurnStarted)));
        let list = s.send.iter().find(|r| r["method"] == "thread/turns/list").expect("asks for the running turn");
        m.handle(&json!({"id": list["id"], "result": {"data": [{"id": "U9", "status": "inProgress"}]}}));
        let s = m.cmd(DriverCmd::Steer(Msg { text: "also this".into(), images: vec![] }));
        assert_eq!((s.send[0]["method"].as_str(), s.send[0]["params"]["expectedTurnId"].as_str()), (Some("turn/steer"), Some("U9")));

        // Items of a turn EMP never saw start mean one is running; late ones of an ended turn don't.
        let (mut m, _) = opened(1, Some("abc"));
        let s = m.handle(&json!({"method": "item/started", "params": {"threadId": "T", "turnId": "U1", "item": {"type": "reasoning", "id": "r"}}}));
        assert!(matches!(s.events[0], DriverEvent::TurnStarted));
        m.handle(&json!({"method": "turn/completed", "params": {"threadId": "T", "turn": {"id": "U1", "status": "completed"}}}));
        let s = m.handle(&json!({"method": "item/completed", "params": {"threadId": "T", "turnId": "U1", "item": {"type": "reasoning", "id": "r"}}}));
        assert!(!s.events.iter().any(|e| matches!(e, DriverEvent::TurnStarted)));
    }

    #[test]
    fn opens_new_and_resumed_threads() {
        let (_, open) = opened(1, None);
        assert_eq!(open[0]["method"], "thread/start");
        let p = &open[0]["params"];
        assert_eq!((p["sandbox"].as_str(), p["approvalPolicy"].as_str(), p["threadSource"].as_str()), (Some("workspace-write"), Some("on-request"), Some("emp")));
        assert_eq!(p["model"], "gpt-6.1-sol");
        let (_, open) = opened(1, Some("abc"));
        assert_eq!(open[0]["method"], "thread/resume");
        assert_eq!(open[0]["params"], json!({"threadId": "abc", "excludeTurns": true}));
        let (_, open) = opened(3, Some("abc"));
        assert_eq!(open[0]["params"]["sandbox"], "danger-full-access");
        let mut limited = spec(1, None);
        limited.compact_at = crate::state::CompactAt::fixed(150_000);
        assert_eq!(args(&limited), ["app-server", "-c", "model_auto_compact_token_limit=150000"]);
    }

    #[test]
    fn a_turn_streams_and_ends() {
        let (mut m, _) = opened(1, None);
        let s = m.cmd(DriverCmd::Prompt("hi".into()));
        let p = &s.send[0]["params"];
        assert_eq!(s.send[0]["method"], "turn/start");
        assert_eq!(p["input"][0]["text"], "hi");
        assert_eq!(p["effort"], "high");
        assert!(p.get("approvalPolicy").is_none() && p.get("sandboxPolicy").is_none());
        assert!(matches!(m.handle(&json!({"method": "turn/started", "params": {"threadId": "T", "turn": {"id": "U1"}}})).events[0], DriverEvent::TurnStarted));
        let s = m.handle(&json!({"method": "item/started", "params": {"threadId": "T", "item": {"type": "commandExecution", "id": "c1", "command": "echo hi"}}}));
        assert!(matches!(items(&s)[0], TranscriptItem::ToolCall { name, input, .. } if name == "shell" && input == "echo hi"));
        let s = m.handle(&json!({"method": "item/completed", "params": {"threadId": "T", "item": {"type": "commandExecution", "id": "c1", "aggregatedOutput": "hi\n", "exitCode": 0, "status": "completed"}}}));
        assert!(matches!(items(&s)[0], TranscriptItem::ToolResult { output, is_error: false, .. } if output == "hi\n"));
        m.handle(&json!({"method": "item/agentMessage/delta", "params": {"threadId": "T", "itemId": "a", "delta": "po"}}));
        let s = m.handle(&json!({"method": "item/agentMessage/delta", "params": {"threadId": "T", "itemId": "a", "delta": "ng"}}));
        assert!(matches!(&s.events[0], DriverEvent::Partial(PartialKind::Assistant, t) if t == "pong"));
        // Another thread's events (a sub-agent) are ignored.
        assert!(m.handle(&json!({"method": "item/agentMessage/delta", "params": {"threadId": "SUB", "itemId": "b", "delta": "x"}})).events.is_empty());
        let s = m.handle(&json!({"method": "item/completed", "params": {"threadId": "T", "item": {"type": "agentMessage", "id": "a", "text": "pong"}}}));
        assert!(matches!(items(&s)[0], TranscriptItem::Assistant { text, .. } if text == "pong"));
        let s = m.handle(&json!({"method": "thread/tokenUsage/updated", "params": {"threadId": "T", "tokenUsage": {"last": {"totalTokens": 23057}, "modelContextWindow": 272000}}}));
        assert!(matches!(&s.events[0], DriverEvent::Context(c) if c.used == 23057 && c.window == Some(272000)));
        let s = m.handle(&json!({"method": "turn/completed", "params": {"threadId": "T", "turn": {"id": "U1", "status": "completed"}}}));
        assert!(matches!(s.events[0], DriverEvent::TurnEnded { aborted: false }));
        // Steering mid-turn goes to the running turn; between turns it starts one.
        m.handle(&json!({"method": "turn/started", "params": {"threadId": "T", "turn": {"id": "U2"}}}));
        let s = m.cmd(DriverCmd::Steer("also this".into()));
        assert_eq!((s.send[0]["method"].as_str(), s.send[0]["params"]["expectedTurnId"].as_str()), (Some("turn/steer"), Some("U2")));
        let retry = m.handle(&json!({"id": s.send[0]["id"], "error": {"code": -32600, "message": "no active turn"}}));
        assert_eq!(retry.send[0]["method"], "turn/start");
        let s = m.handle(&json!({"method": "turn/completed", "params": {"threadId": "T", "turn": {"id": "U2", "status": "failed", "error": {"message": "{\"type\":\"error\",\"error\":{\"message\":\"rate limited\"}}"}}}}));
        assert_eq!(texts(&s), ["codex: rate limited"]);
        assert!(matches!(s.events.last().unwrap(), DriverEvent::TurnEnded { aborted: true }));
    }

    #[test]
    fn commands_before_the_thread_opens_wait() {
        let mut m = Mapper::new(&spec(1, Some("abc")));
        m.start();
        assert!(m.cmd(DriverCmd::Prompt("early".into())).send.is_empty());
        m.handle(&json!({"id": 1, "result": {}}));
        let s = m.handle(&json!({"id": 2, "result": {"thread": {"id": "abc"}}}));
        assert!(s.send.iter().any(|r| r["method"] == "turn/start" && r["params"]["input"][0]["text"] == "early"));
        // A resumed thread keeps its own effort.
        assert!(s.send.iter().all(|r| r["params"].get("effort").is_none()));
    }

    #[test]
    fn open_elsewhere_is_fatal_and_says_so() {
        let mut m = Mapper::new(&spec(1, Some("abc")));
        m.start();
        m.handle(&json!({"id": 1, "result": {}}));
        m.handle(&json!({"id": 2, "error": {"code": -32600, "message": "thread abc already has an active writer"}}));
        assert!(m.dead.as_deref().unwrap().contains("open somewhere else"));
    }

    #[test]
    fn goals() {
        let (mut m, _) = opened(1, None);
        let goal = |status: &str| json!({"threadId": "T", "objective": "Ship it", "status": status, "tokenBudget": null, "tokensUsed": 2_500_000, "timeUsedSeconds": 7500, "createdAt": 1, "updatedAt": 2});
        let s = m.cmd(DriverCmd::Command { name: "goal".into(), args: "Ship it".into() });
        assert_eq!(s.send[0]["method"], "thread/goal/set");
        assert_eq!(s.send[0]["params"]["status"], "active");
        let s = m.handle(&json!({"id": s.send[0]["id"], "result": {"goal": goal("active")}}));
        assert_eq!(texts(&s), ["Goal set: Ship it"]);
        assert!(s.events.iter().any(|e| matches!(e, DriverEvent::Goal(Some(g)) if g.status == GoalStatus::Active)));
        // The same goal again says nothing.
        assert!(m.handle(&json!({"method": "thread/goal/updated", "params": {"threadId": "T", "goal": goal("active")}})).events.is_empty());
        // Stop interrupts the turn and pauses the goal.
        m.handle(&json!({"method": "turn/started", "params": {"threadId": "T", "turn": {"id": "U1"}}}));
        let s = m.cmd(DriverCmd::Interrupt);
        let methods: Vec<&str> = s.send.iter().filter_map(|r| r["method"].as_str()).collect();
        assert_eq!(methods, ["turn/interrupt", "thread/goal/set"]);
        assert_eq!(s.send[1]["params"]["status"], "paused");
        let s = m.handle(&json!({"method": "thread/goal/updated", "params": {"threadId": "T", "goal": goal("paused")}}));
        assert_eq!(texts(&s), ["Goal paused. /goal resume continues it."]);
        let s = m.cmd(DriverCmd::Command { name: "goal".into(), args: "resume".into() });
        assert_eq!((s.send[0]["params"]["status"].as_str(), s.send[0]["params"].get("objective")), (Some("active"), None));
        let s = m.handle(&json!({"method": "thread/goal/updated", "params": {"threadId": "T", "goal": goal("complete")}}));
        assert_eq!(texts(&s), ["Goal complete (2.5M tokens, 2h 5m)."]);
        let s = m.cmd(DriverCmd::Command { name: "goal".into(), args: String::new() });
        let s = m.handle(&json!({"id": s.send[0]["id"], "result": {"goal": goal("complete")}}));
        assert!(texts(&s)[0].starts_with("Goal (complete, 2.5M tokens, 2h 5m): Ship it"));
    }

    #[test]
    fn an_old_goal_seen_before_its_lookup_is_not_announced() {
        let mut m = Mapper::new(&spec(1, Some("abc")));
        m.start();
        m.handle(&json!({"id": 1, "result": {}}));
        m.handle(&json!({"id": 2, "result": {"thread": {"id": "abc"}}}));
        let s = m.handle(&json!({"method": "thread/goal/updated", "params": {"threadId": "abc", "goal": {"objective": "old", "status": "complete", "tokensUsed": 5}}}));
        assert!(texts(&s).is_empty());
        assert!(s.events.iter().any(|e| matches!(e, DriverEvent::Goal(Some(_)))));
    }

    #[test]
    fn a_goal_typed_as_the_first_message() {
        let (mut m, _) = opened(1, None);
        assert_eq!(goal_line("/goal build the app"), Some("build the app".into()));
        assert_eq!(goal_line("/goals are nice"), None);
        assert_eq!(goal_line("hello"), None);
        let s = m.cmd(DriverCmd::Prompt("/goal build the app".into()));
        assert_eq!(s.send[0]["params"]["objective"], "build the app");
        // Pausing through a message ends the "turn" that message began.
        let s = m.cmd(DriverCmd::Prompt("/goal pause".into()));
        let s = m.handle(&json!({"id": s.send[0]["id"], "result": {"goal": {"objective": "build the app", "status": "paused"}}}));
        assert!(s.events.iter().any(|e| matches!(e, DriverEvent::TurnEnded { .. })));
        // Resuming a thread that has no goal: Codex refuses, and the "turn" ends.
        let mut fresh = opened(1, None).0;
        let s = fresh.cmd(DriverCmd::Prompt("/goal resume".into()));
        let s = fresh.handle(&json!({"id": s.send[0]["id"], "error": {"code": -32600, "message": "thread has no goal"}}));
        assert_eq!(texts(&s), ["/goal: thread has no goal"]);
        assert!(s.events.iter().any(|e| matches!(e, DriverEvent::TurnEnded { aborted: true })));
    }

    #[test]
    fn full_access_is_set_on_every_turn() {
        let (mut m, _) = opened(emp_protocol::FULL_ACCESS, Some("T"));
        for text in ["one", "two"] {
            let s = m.cmd(DriverCmd::Prompt(text.into()));
            let p = &s.send[0]["params"];
            assert_eq!((p["approvalPolicy"].as_str(), &p["sandboxPolicy"]), (Some("never"), &json!({"type": "dangerFullAccess"})));
        }
    }

    #[test]
    fn approvals_become_questions_below_autonomy_two() {
        let (mut m, _) = opened(1, None);
        let s = m.handle(&json!({"id": 7, "method": "item/commandExecution/requestApproval", "params": {"threadId": "T", "kind": "command", "command": "rm -rf build", "reason": "clean the build"}}));
        let DriverEvent::Ask(q) = &s.events[0] else { panic!("no question") };
        assert_eq!((q.id.as_str(), q.title.as_str()), ("codex:7", "Run this command?"));
        assert_eq!(q.message.as_deref(), Some("rm -rf build\n\nclean the build"));
        let s = m.cmd(DriverCmd::Answer { id: "codex:7".into(), value: Some(ALLOW_SESSION.into()), confirmed: None, cancelled: false });
        assert_eq!(s.send[0], json!({"id": 7, "result": {"decision": "acceptForSession"}}));
        let s = m.handle(&json!({"id": 8, "method": "item/fileChange/requestApproval", "params": {"threadId": "T"}}));
        assert!(matches!(&s.events[0], DriverEvent::Ask(_)));
        let s = m.cmd(DriverCmd::Answer { id: "codex:8".into(), value: None, confirmed: None, cancelled: true });
        assert_eq!(s.send[0]["result"]["decision"], "decline");
        // Withdrawn by Codex: the card goes.
        m.handle(&json!({"id": 9, "method": "item/fileChange/requestApproval", "params": {"threadId": "T"}}));
        let s = m.handle(&json!({"method": "serverRequest/resolved", "params": {"threadId": "T", "requestId": 9}}));
        assert!(matches!(&s.events[0], DriverEvent::Unask(id) if id == "codex:9"));

        let (mut auto, _) = opened(2, None);
        let s = auto.handle(&json!({"id": 7, "method": "item/commandExecution/requestApproval", "params": {"threadId": "T", "command": "ls"}}));
        assert!(s.events.is_empty());
        assert_eq!(s.send[0]["result"]["decision"], "accept");
        let s = auto.handle(&json!({"id": 8, "method": "attestation/generate", "params": {}}));
        assert_eq!(s.send[0]["error"]["code"], -32601);
    }

    #[test]
    fn user_input_questions_answer_together() {
        let (mut m, _) = opened(1, None);
        let s = m.handle(&json!({"id": "r1", "method": "item/tool/requestUserInput", "params": {"threadId": "T", "questions": [
            {"id": "color", "header": "Colour", "question": "Which colour?", "isOther": false, "isSecret": false, "options": [{"label": "Red", "description": ""}, {"label": "Blue", "description": ""}]},
            {"id": "name", "header": "", "question": "Project name?", "isOther": false, "isSecret": false, "options": null}
        ]}}));
        assert_eq!(s.events.len(), 2);
        assert!(matches!(&s.events[1], DriverEvent::Ask(q) if q.kind == QuestionKind::Input && q.title == "Project name?"));
        assert!(m.cmd(DriverCmd::Answer { id: "codex:r1:color".into(), value: Some("Red".into()), confirmed: None, cancelled: false }).send.is_empty());
        let s = m.cmd(DriverCmd::Answer { id: "codex:r1:name".into(), value: Some("emp".into()), confirmed: None, cancelled: false });
        assert_eq!(s.send[0], json!({"id": "r1", "result": {"answers": {"color": {"answers": ["Red"]}, "name": {"answers": ["emp"]}}}}));
    }

    #[test]
    fn native_commands() {
        let (mut m, _) = opened(1, None);
        let s = m.cmd(DriverCmd::Command { name: "model".into(), args: String::new() });
        let s = m.handle(&json!({"id": s.send[0]["id"], "result": {"data": [{"id": "gpt-6.1-sol", "hidden": false}, {"id": "secret", "hidden": true}, {"id": "gpt-6-sol", "hidden": false}]}}));
        let DriverEvent::Ask(q) = &s.events[0] else { panic!("no picker") };
        assert_eq!(q.options, ["gpt-6.1-sol", "gpt-6-sol"]);
        m.cmd(DriverCmd::Answer { id: Q_MODEL.into(), value: Some("gpt-6-sol".into()), confirmed: None, cancelled: false });
        let s = m.cmd(DriverCmd::Prompt("next".into()));
        assert_eq!(s.send[0]["params"]["model"], "gpt-6-sol");
        // The composer's picker: silent until the next turn, then once.
        assert!(m.cmd(DriverCmd::SetModel("gpt-6-luna".into())).send.is_empty());
        m.handle(&json!({"method": "turn/completed", "params": {"threadId": "T", "turn": {"id": "x", "status": "completed"}}}));
        assert_eq!(m.cmd(DriverCmd::Prompt("on luna".into())).send[0]["params"]["model"], "gpt-6-luna");
        assert_eq!(m.cmd(DriverCmd::Command { name: "compact".into(), args: String::new() }).send[0]["method"], "thread/compact/start");
        assert_eq!(m.cmd(DriverCmd::Command { name: "rename".into(), args: "XDesign".into() }).send[0]["params"]["name"], "XDesign");
        let s = m.cmd(DriverCmd::Command { name: "status".into(), args: String::new() });
        let now = emp_core::now_ms() / 1000;
        let s = m.handle(&json!({"id": s.send[0]["id"], "result": {"rateLimits": {"primary": {"usedPercent": 30, "windowDurationMins": 10080, "resetsAt": now + 6 * 86_400 + 60}}}}));
        assert_eq!(texts(&s), ["Model gpt-6-luna · Weekly limit 30% used, resets in 6d 0h"]);
        assert_eq!(texts(&m.cmd(DriverCmd::Command { name: "review".into(), args: String::new() })), ["/review doesn't run in chat; EMP opens it in a terminal"]);
    }

    #[test]
    fn parses_goal_rows_from_both_spellings() {
        assert_eq!(GoalStatus::parse("usage_limited"), Some(GoalStatus::UsageLimited));
        assert_eq!(GoalStatus::parse("usageLimited"), Some(GoalStatus::UsageLimited));
        assert_eq!(duration(294_614), "3d 9h");
    }
}
