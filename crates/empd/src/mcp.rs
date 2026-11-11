//! The `emp` MCP server: the coordinator's (Suki's) tools. Streamable HTTP at `POST /mcp`,
//! answered with plain JSON (no SSE stream, no MCP session id). Each call carries the caller's
//! own key (`live::Live::grant`), so the daemon knows which coordinator chat is asking and the
//! daemon token never reaches a model. Tools are thin wrappers over the JSON-RPC methods.

use std::time::{Duration, Instant};

use axum::extract::State as AxState;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use emp_protocol::{RpcError, RpcNotification, ServersKillParams, methods, notifications};
use serde_json::{Value, json};

use crate::state::State;

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const WAIT_DEFAULT: u64 = 240;
const WAIT_MAX: u64 = 840;
const TEXT_MAX: usize = 4000;

pub async fn handle(AxState(state): AxState<State>, headers: HeaderMap, body: String) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let Some(caller) = state.live.caller(token) else {
        return (StatusCode::UNAUTHORIZED, "unknown or expired EMP tool key").into_response();
    };
    let Ok(msg) = serde_json::from_str::<Value>(&body) else {
        return json_response(rpc_error(Value::Null, -32700, "parse error"));
    };
    let ctx = Caller { state, token: token.to_string(), first_id: caller };
    match msg {
        Value::Array(batch) => {
            let mut out = Vec::new();
            for m in batch {
                if let Some(r) = ctx.message(m).await {
                    out.push(r);
                }
            }
            if out.is_empty() { StatusCode::ACCEPTED.into_response() } else { json_response(Value::Array(out)) }
        }
        m => match ctx.message(m).await {
            Some(r) => json_response(r),
            None => StatusCode::ACCEPTED.into_response(),
        },
    }
}

/// GET opens a server-to-client stream in streamable HTTP; EMP has nothing to push.
pub async fn no_stream() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "EMP's MCP server answers POST requests only").into_response()
}

fn json_response(v: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

struct Caller {
    state: State,
    token: String,
    /// The calling thread when the request arrived (maybe still `pending:…`).
    first_id: String,
}

impl Caller {
    /// Answers one JSON-RPC message; `None` for notifications.
    async fn message(&self, m: Value) -> Option<Value> {
        let id = m.get("id").cloned()?;
        let method = m.get("method").and_then(Value::as_str).unwrap_or("");
        let params = m.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "emp", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "EMP runs the owner's coding agents as threads. Use these tools to find agents, projects and threads, start and steer agents, wait for their results, schedule tasks, and manage dev servers."
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": tool_list()}),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                match self.call(&name, &args).await {
                    Ok(v) => {
                        let text = match &v {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string_pretty(other).unwrap_or_default(),
                        };
                        json!({"content": [{"type": "text", "text": text}], "isError": false})
                    }
                    Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                }
            }
            _ => return Some(rpc_error(id, -32601, &format!("unknown method {method}"))),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    /// The calling chat's thread id, waiting briefly while its first turn is still creating it.
    async fn thread_id(&self) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let id = self.state.live.caller(&self.token).unwrap_or_else(|| self.first_id.clone());
            if !id.starts_with("pending:") {
                return Some(id);
            }
            if Instant::now() > deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        crate::rpc::dispatch(&self.state, method, params).await.map_err(|e: RpcError| e.message)
    }

    async fn call(&self, name: &str, a: &Value) -> Result<Value, String> {
        match name {
            "list_agents" => self.list_agents().await,
            "list_projects" => self.list_projects().await,
            "list_threads" => self.list_threads(a).await,
            "read_thread" => self.read_thread(&req_str(a, "thread_id")?, a.get("last").and_then(Value::as_u64).unwrap_or(10)).await,
            "start_thread" => self.start_thread(a).await,
            "send_message" => {
                let mode = match a.get("mode").and_then(Value::as_str).unwrap_or("prompt") {
                    "steer" => "steer",
                    "follow_up" | "follow-up" => "follow-up",
                    _ => "prompt",
                };
                let id = req_str(a, "thread_id")?;
                self.not_self(&id).await?;
                let live = self.rpc(methods::THREADS_SEND, json!({"threadId": id, "text": req_str(a, "text")?, "mode": mode})).await?;
                Ok(json!({"sent": true, "status": live["status"], "queued": live["queued"]}))
            }
            "interrupt_thread" => {
                let id = req_str(a, "thread_id")?;
                self.not_self(&id).await?;
                self.rpc(methods::THREADS_INTERRUPT, json!({"threadId": id})).await?;
                Ok(json!("Interrupted."))
            }
            "wait_for_threads" => self.wait(a).await,
            "open_thread" => {
                let id = req_str(a, "thread_id")?;
                let t = self.rpc(methods::THREADS_GET, json!({"threadId": id})).await?;
                self.state.notify(RpcNotification::new(notifications::UI_OPEN_THREAD, json!({"threadId": id})));
                Ok(json!(format!("Opened \"{}\" in the owner's EMP window.", t["title"].as_str().unwrap_or(""))))
            }
            "list_dev_servers" => self.list_servers(a.get("all").and_then(Value::as_bool).unwrap_or(false)).await,
            "stop_dev_server" => {
                let pid = a.get("pid").and_then(Value::as_u64).ok_or("pid is required")? as u32;
                let force = a.get("force").and_then(Value::as_bool);
                let r = crate::servers::kill(&self.state, ServersKillParams { pid, force }).await.map_err(|e| e.message)?;
                Ok(json!(if r.stopped { "Stopped." } else { "It's still running 3 s after SIGTERM; call again with force: true to kill it." }))
            }
            "schedule_task" => self.schedule(a, None).await,
            "update_schedule" => {
                let id = req_str(a, "schedule_id")?;
                self.schedule(a, Some(id)).await
            }
            "list_schedules" => {
                let now = emp_core::now_ms();
                let all = crate::schedules::list(&self.state).await.map_err(|e| e.message)?;
                let rows: Vec<Value> = all.iter().map(|s| schedule_row(s, now)).collect();
                Ok(json!({"now": crate::cron::moment(chrono::Local::now()), "schedules": rows}))
            }
            "delete_schedule" => {
                crate::schedules::delete(&self.state, &req_str(a, "schedule_id")?).await.map_err(|e| e.message)?;
                Ok(json!("Deleted."))
            }
            "run_schedule_now" => {
                let s = crate::schedules::run_now(&self.state, &req_str(a, "schedule_id")?).await.map_err(|e| e.message)?;
                Ok(json!(format!("Started a run of \"{}\".", s.title)))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }

    /// `schedule_task` (`id` None) and `update_schedule`: fields left out keep their value.
    async fn schedule(&self, a: &Value, id: Option<String>) -> Result<Value, String> {
        use emp_protocol::{ScheduleSaveParams, ScheduleWhen};
        let old = match &id {
            Some(id) => Some(
                crate::schedules::list(&self.state)
                    .await
                    .map_err(|e| e.message)?
                    .into_iter()
                    .find(|s| &s.id == id)
                    .ok_or_else(|| format!("no schedule {id}; list_schedules shows them"))?,
            ),
            None => None,
        };
        let text = |k: &str| a.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let when = if let Some(expr) = text("cron") {
            Some(ScheduleWhen::Cron { expr })
        } else if let Some(m) = a.get("every_minutes").and_then(Value::as_u64) {
            Some(ScheduleWhen::Every { minutes: m.min(u64::from(u32::MAX)) as u32 })
        } else if let Some(m) = a.get("in_minutes").and_then(Value::as_u64) {
            Some(ScheduleWhen::Once { at: emp_core::now_ms() + m as i64 * 60_000 })
        } else if let Some(at) = text("at") {
            Some(ScheduleWhen::Once { at: parse_local(&at)? })
        } else {
            None
        };
        let when = when.or_else(|| old.as_ref().map(|o| o.when.clone())).ok_or("say when: cron, every_minutes, at or in_minutes")?;
        // Where it runs: this conversation, another thread, or an agent's new thread each time.
        let (mut agent_id, mut thread_id) = old.as_ref().map(|o| (o.agent_id.clone(), o.thread_id.clone())).unwrap_or_default();
        if let Some(t) = text("thread_id") {
            thread_id = Some(if t == "this" { self.thread_id().await.ok_or("this conversation has no thread yet")? } else { t });
            agent_id = None;
        }
        if let Some(wanted) = text("agent") {
            let w = wanted.to_lowercase();
            let agents = self.agents().await?;
            let agent = agents
                .iter()
                .find(|x| x["id"] == wanted.as_str() || x["name"].as_str().is_some_and(|n| n.to_lowercase() == w))
                .ok_or_else(|| format!("no agent called {wanted}; list_agents shows them"))?;
            agent_id = agent["id"].as_str().map(String::from);
            thread_id = None;
        }
        let project_id = match text("project") {
            Some(p) => Some(self.project_id(&p).await?),
            None => old.as_ref().and_then(|o| o.project_id.clone()),
        };
        let paused = a.get("paused").and_then(Value::as_bool);
        // Ends: left out keeps the old value; "" or 0 clears it.
        let raw = |k: &str| a.get(k).filter(|v| !v.is_null());
        let ends_at = match raw("until").and_then(Value::as_str).map(str::trim) {
            Some("") => None,
            Some(t) => Some(parse_local(t)?),
            None => old.as_ref().and_then(|o| o.ends_at),
        };
        let max_runs = match raw("max_runs").and_then(Value::as_u64) {
            Some(0) => None,
            Some(n) => Some(n.min(u64::from(u32::MAX)) as u32),
            None => old.as_ref().and_then(|o| o.max_runs),
        };
        let stop_when = match raw("stop_when").and_then(Value::as_str) {
            Some(c) => Some(c.trim().to_string()).filter(|c| !c.is_empty()),
            None => old.as_ref().and_then(|o| o.stop_when.clone()),
        };
        let p = ScheduleSaveParams {
            id: id.clone(),
            title: text("title").or_else(|| old.as_ref().map(|o| o.title.clone())).unwrap_or_default(),
            prompt: text("prompt").or_else(|| old.as_ref().map(|o| o.prompt.clone())).ok_or("prompt is required")?,
            when,
            agent_id,
            cwd: text("folder").or_else(|| old.as_ref().and_then(|o| o.cwd.clone())),
            project_id,
            thread_id,
            enabled: Some(paused.map(|p| !p).unwrap_or_else(|| old.as_ref().is_none_or(|o| o.enabled))),
            ends_at,
            max_runs,
            stop_when,
        };
        let s = crate::schedules::save(&self.state, p).await.map_err(|e| e.message)?;
        Ok(schedule_row(&s, emp_core::now_ms()))
    }

    /// The coordinator steering or waiting on itself would deadlock its own turn.
    async fn not_self(&self, id: &str) -> Result<(), String> {
        if self.thread_id().await.as_deref() == Some(id) { Err("that's this conversation".into()) } else { Ok(()) }
    }

    async fn agents(&self) -> Result<Vec<Value>, String> {
        let v = self.rpc(methods::AGENTS_LIST, Value::Null).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    async fn list_agents(&self) -> Result<Value, String> {
        let out: Vec<Value> = self
            .agents()
            .await?
            .into_iter()
            .filter(|a| !a["coordinator"].as_bool().unwrap_or(false))
            .map(|a| json!({"id": a["id"], "name": a["name"], "harness": a["harness"], "model": a["model"], "role": a["role"], "folder": a["cwd"]}))
            .collect();
        Ok(Value::Array(out))
    }

    async fn list_projects(&self) -> Result<Value, String> {
        let now = emp_core::now_ms();
        let v = self.rpc(methods::PROJECTS_LIST, Value::Null).await?;
        let out: Vec<Value> = v
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|p| json!({"id": p["id"], "name": p["name"], "folder": p["rootPath"], "threads": p["threadCount"], "last_activity": p["lastActivityAt"].as_i64().map(|t| ago(now, t))}))
            .collect();
        Ok(Value::Array(out))
    }

    async fn list_threads(&self, a: &Value) -> Result<Value, String> {
        let now = emp_core::now_ms();
        let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 50);
        let live = self.rpc(methods::THREADS_LIVE, Value::Null).await?;
        let live: Vec<Value> = live.as_array().cloned().unwrap_or_default();
        let status_of = |id: &str| live.iter().find(|l| l["threadId"] == id).map(|l| l["status"].clone());
        let names: Vec<(String, String)> =
            self.agents().await?.iter().map(|a| (a["id"].as_str().unwrap_or("").to_string(), a["name"].as_str().unwrap_or("").to_string())).collect();
        let agent_name = |id: &Value| id.as_str().and_then(|id| names.iter().find(|(i, _)| i == id)).map(|(_, n)| n.clone());

        let active_only = a.get("active_only").and_then(Value::as_bool).unwrap_or(false);
        let mine = a.get("started_by_me").and_then(Value::as_bool).unwrap_or(false);
        let me = if mine { self.thread_id().await } else { None };
        let mut threads: Vec<Value> = if active_only {
            let mut out = Vec::new();
            for l in &live {
                if matches!(l["status"].as_str(), Some("running" | "needs-input"))
                    && let Ok(t) = self.rpc(methods::THREADS_GET, json!({"threadId": l["threadId"]})).await
                {
                    out.push(t);
                }
            }
            out
        } else {
            let mut p = json!({"limit": if mine { 200 } else { limit }, "coordinator": false});
            if let Some(q) = a.get("query").and_then(Value::as_str) {
                p["query"] = json!(q);
            }
            if let Some(proj) = a.get("project").and_then(Value::as_str) {
                p["projectId"] = json!(self.project_id(proj).await?);
            }
            let page = self.rpc(methods::THREADS_LIST, p).await?;
            page["items"].as_array().cloned().unwrap_or_default()
        };
        if let Some(me) = &me {
            threads.retain(|t| t["assignedBy"].as_str() == Some(me.as_str()));
        }
        let out: Vec<Value> = threads
            .into_iter()
            .take(limit as usize)
            .map(|t| {
                let id = t["id"].as_str().unwrap_or("").to_string();
                json!({
                    "thread_id": id,
                    "title": t["title"],
                    "agent": agent_name(&t["agentId"]),
                    "harness": t["harness"],
                    "status": status_of(&id).unwrap_or_else(|| t["status"].clone()),
                    "folder": t["cwd"],
                    "updated": t["updatedAt"].as_i64().map(|u| ago(now, u)),
                    "started_by_suki": !t["assignedBy"].is_null(),
                })
            })
            .collect();
        Ok(Value::Array(out))
    }

    async fn project_id(&self, wanted: &str) -> Result<String, String> {
        let v = self.rpc(methods::PROJECTS_LIST, Value::Null).await?;
        let list = v.as_array().cloned().unwrap_or_default();
        let w = wanted.trim().to_lowercase();
        list.iter()
            .find(|p| p["id"] == wanted || p["name"].as_str().is_some_and(|n| n.to_lowercase() == w) || p["rootPath"].as_str().is_some_and(|r| r == wanted))
            .and_then(|p| p["id"].as_str().map(String::from))
            .ok_or_else(|| format!("no project called {wanted}; list_projects shows them"))
    }

    async fn read_thread(&self, id: &str, last: u64) -> Result<Value, String> {
        let last = last.clamp(1, 40) as u32;
        let t = self.rpc(methods::THREADS_GET, json!({"threadId": id})).await?;
        let first = self.rpc(methods::THREADS_TRANSCRIPT, json!({"threadId": id, "limit": last})).await?;
        let total = first["total"].as_u64().unwrap_or(0) as u32;
        let page = if total > last { self.rpc(methods::THREADS_TRANSCRIPT, json!({"threadId": id, "offset": total - last, "limit": last})).await? } else { first };
        let live = self.rpc(methods::THREADS_LIVE, Value::Null).await?;
        let status = live.as_array().and_then(|l| l.iter().find(|x| x["threadId"] == id)).map_or_else(|| t["status"].clone(), |l| l["status"].clone());
        let mut lines = vec![format!("# {}", t["title"].as_str().unwrap_or("")), format!("status: {status}, folder: {}, {total} items", t["cwd"].as_str().unwrap_or("?"))];
        for item in page["items"].as_array().cloned().unwrap_or_default() {
            if let Some(line) = render_item(&item) {
                lines.push(line);
            }
        }
        Ok(Value::String(lines.join("\n\n")))
    }

    async fn start_thread(&self, a: &Value) -> Result<Value, String> {
        let wanted = req_str(a, "agent")?;
        let agents = self.agents().await?;
        let w = wanted.trim().to_lowercase();
        let agent = agents
            .iter()
            .find(|x| x["id"] == wanted.as_str() || x["name"].as_str().is_some_and(|n| n.to_lowercase() == w))
            .ok_or_else(|| format!("no agent called {wanted}; list_agents shows them"))?;
        if agent["coordinator"].as_bool().unwrap_or(false) {
            return Err("start a different agent; this conversation is the coordinator".into());
        }
        let mut p = json!({"agentId": agent["id"], "text": req_str(a, "prompt")?});
        if let Some(proj) = a.get("project").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            p["projectId"] = json!(self.project_id(proj).await?);
        }
        if let Some(cwd) = a.get("folder").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            p["cwd"] = json!(cwd);
        }
        if let Some(m) = a.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            p["model"] = json!(m);
        }
        let t = self.rpc(methods::THREADS_START, p).await?;
        let tid = t["id"].as_str().unwrap_or("").to_string();
        if let Some(me) = self.thread_id().await {
            let (tid, me) = (tid.clone(), me);
            let _ = self.state.with_store(move |s| s.set_thread_assigned_by(&tid, &me)).await;
        }
        Ok(json!({"thread_id": tid, "title": t["title"], "agent": agent["name"], "folder": t["cwd"], "status": "running"}))
    }

    async fn wait(&self, a: &Value) -> Result<Value, String> {
        let ids: Vec<String> = a.get("thread_ids").and_then(Value::as_array).map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if ids.is_empty() {
            return Err("thread_ids is required".into());
        }
        for id in &ids {
            self.not_self(id).await?;
        }
        let timeout = a.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(WAIT_DEFAULT).clamp(5, WAIT_MAX);
        let deadline = Instant::now() + Duration::from_secs(timeout);
        loop {
            let busy = self.state.live.list().iter().any(|l| ids.contains(&l.thread_id) && l.status == emp_protocol::ThreadStatus::Running);
            if !busy || Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let live = self.state.live.list();
        let mut out = Vec::new();
        for id in &ids {
            let status = live.iter().find(|l| &l.thread_id == id).map(|l| l.status);
            let label = match status {
                Some(emp_protocol::ThreadStatus::Running) => "still running",
                Some(emp_protocol::ThreadStatus::NeedsInput) => "needs the owner",
                Some(emp_protocol::ThreadStatus::Failed) => "failed",
                _ => "finished",
            };
            let error = live.iter().find(|l| &l.thread_id == id).and_then(|l| l.error.clone());
            let (title, reply) = self.last_reply(id).await;
            out.push(json!({"thread_id": id, "title": title, "status": label, "error": error, "last_reply": reply}));
        }
        Ok(Value::Array(out))
    }

    async fn last_reply(&self, id: &str) -> (Value, Option<String>) {
        let title = self.rpc(methods::THREADS_GET, json!({"threadId": id})).await.map(|t| t["title"].clone()).unwrap_or(Value::Null);
        let Ok(first) = self.rpc(methods::THREADS_TRANSCRIPT, json!({"threadId": id, "limit": 1})).await else { return (title, None) };
        let total = first["total"].as_u64().unwrap_or(0) as u32;
        let page = self.rpc(methods::THREADS_TRANSCRIPT, json!({"threadId": id, "offset": total.saturating_sub(12), "limit": 12})).await.unwrap_or(first);
        let reply = page["items"].as_array().and_then(|items| items.iter().rev().find(|i| i["kind"] == "assistant")).and_then(|i| i["text"].as_str()).map(|s| clip(s, TEXT_MAX));
        (title, reply)
    }

    async fn list_servers(&self, all: bool) -> Result<Value, String> {
        let now = emp_core::now_ms();
        let list = crate::servers::list(&self.state).await.map_err(|e| e.message)?;
        let out: Vec<Value> = list
            .into_iter()
            .filter(|s| all || s.kind == emp_protocol::ServerKind::Dev)
            .map(|s| {
                json!({
                    "pid": s.pid, "name": s.name, "folder": s.cwd, "ports": s.ports, "kind": s.kind,
                    "in_use": s.connections > 0, "reachable_from_network": s.exposed,
                    "running_for": ago(now, s.started_at).trim_end_matches(" ago").to_string(),
                    "memory_mb": s.memory / 1_048_576, "protected": s.protected,
                })
            })
            .collect();
        Ok(Value::Array(out))
    }
}

fn req_str(a: &Value, key: &str) -> Result<String, String> {
    a.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from).ok_or_else(|| format!("{key} is required"))
}

/// One transcript item as a short text line for the model; thinking is left out.
fn render_item(i: &Value) -> Option<String> {
    let text = |k: &str| i[k].as_str().unwrap_or("").to_string();
    Some(match i["kind"].as_str()? {
        "user" => format!("**Owner:** {}", clip(strip_context(&text("text")), 1500)),
        "assistant" => format!("**Agent:** {}", clip(&text("text"), TEXT_MAX)),
        "toolCall" => format!("→ {} {}", text("name"), clip(&text("input"), 200)),
        "toolResult" => format!("← {}{}", if i["isError"].as_bool().unwrap_or(false) { "error: " } else { "" }, clip(&text("output"), 300)),
        "system" => format!("[{}]", clip(&text("text"), 300)),
        _ => return None,
    })
}

/// Drops the `<emp-context>` block EMP appends to coordinator messages.
fn strip_context(s: &str) -> &str {
    s.find("<emp-context>").map_or(s, |i| s[..i].trim_end())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push_str(" …");
    t
}

fn ago(now: i64, ts: i64) -> String {
    let s = (now - ts).max(0) / 1000;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// A schedule as the model sees it.
fn schedule_row(s: &emp_protocol::Schedule, now: i64) -> Value {
    use chrono::TimeZone;
    let when = |t: Option<i64>| t.and_then(|t| chrono::Local.timestamp_millis_opt(t).single()).map(crate::cron::moment);
    json!({
        "schedule_id": s.id,
        "title": s.title,
        "when": s.summary,
        "paused": !s.enabled,
        "next_run": when(s.next_run_at),
        "runs_in": if s.thread_id.is_some() { "the same thread each time" } else { "a new thread each time" },
        "thread_id": s.thread_id,
        "agent_id": s.agent_id,
        "folder": s.cwd,
        "last_run": s.last_run_at.map(|t| ago(now, t)),
        "last_status": s.last_status,
        "last_error": s.last_error,
        "last_thread_id": s.last_thread_id,
        "until": when(s.ends_at),
        "max_runs": s.max_runs,
        "runs_so_far": s.run_count,
        "stop_when": s.stop_when,
        "finished": s.ended,
        "failures_in_a_row": s.fail_streak,
        "prompt": clip(&s.prompt, 600),
    })
}

/// "2026-10-21 09:00", "2026-10-21T09:00:00" (the Mac's time) or RFC 3339 with an offset.
fn parse_local(at: &str) -> Result<i64, String> {
    use chrono::{NaiveDateTime, TimeZone};
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(at) {
        return Ok(t.timestamp_millis());
    }
    let naive = ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(at, f).ok())
        .ok_or_else(|| format!("couldn't read the time \"{at}\"; use YYYY-MM-DD HH:MM"))?;
    chrono::Local.from_local_datetime(&naive).earliest().map(|t| t.timestamp_millis()).ok_or_else(|| format!("{at} doesn't exist here (a daylight-saving jump)"))
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": description, "inputSchema": {"type": "object", "properties": properties, "required": required}})
}

fn schedule_props(update: bool) -> Value {
    let mut p = json!({
        "title": {"type": "string", "description": "Short name for the list (defaults to the prompt's first line)"},
        "prompt": {"type": "string", "description": "What the agent does each run"},
        "agent": {"type": "string", "description": "Agent name or id: a new thread with it each run"},
        "thread_id": {"type": "string", "description": "Continue this thread each run instead (\"this\" = this conversation)"},
        "project": {"type": "string", "description": "Project to work in (new-thread runs)"},
        "folder": {"type": "string", "description": "Absolute folder to work in (new-thread runs)"},
        "cron": {"type": "string", "description": "Five fields: minute hour day month weekday. Weekdays at 9 AM = \"0 9 * * 1-5\""},
        "every_minutes": {"type": "integer", "minimum": 1, "description": "Repeat every N minutes from now"},
        "at": {"type": "string", "description": "Run once at YYYY-MM-DD HH:MM"},
        "in_minutes": {"type": "integer", "minimum": 1, "description": "Run once, N minutes from now"},
        "paused": {"type": "boolean"},
        "until": {"type": "string", "description": "No runs after YYYY-MM-DD HH:MM (repeating schedules; \"\" clears)"},
        "max_runs": {"type": "integer", "minimum": 0, "description": "Finish after this many runs (repeating schedules; 0 clears)"},
        "stop_when": {"type": "string", "description": "Finish once this is true, e.g. \"the deploy is live\". The agent checks it each run (\"\" clears)"}
    });
    if update {
        p["schedule_id"] = json!({"type": "string"});
    }
    p
}

fn tool_list() -> Vec<Value> {
    let id = json!({"type": "string", "description": "A thread_id from list_threads or start_thread"});
    vec![
        tool("list_agents", "The owner's agent profiles: name, harness (pi, claude, codex, hermes, antigravity), model, role and default folder.", json!({}), &[]),
        tool("list_projects", "The owner's projects (folders with threads).", json!({}), &[]),
        tool(
            "list_threads",
            "Find threads, newest first. Each has a status (running, needs-input, idle, done, failed).",
            json!({
                "query": {"type": "string", "description": "Words in the title or folder"},
                "project": {"type": "string", "description": "Project name, id or folder"},
                "active_only": {"type": "boolean", "description": "Only threads running or waiting for the owner right now"},
                "started_by_me": {"type": "boolean", "description": "Only threads this conversation started"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50}
            }),
            &[],
        ),
        tool("read_thread", "The latest messages of a thread, oldest first.", json!({"thread_id": id, "last": {"type": "integer", "minimum": 1, "maximum": 40, "description": "How many items (default 10)"}}), &["thread_id"]),
        tool(
            "start_thread",
            "Start an agent on a task. Returns once its session exists; the agent keeps working in the background. The agent can't see this conversation, so the prompt must stand on its own.",
            json!({
                "agent": {"type": "string", "description": "Agent name or id from list_agents"},
                "prompt": {"type": "string", "description": "The full brief: goal, context, what done looks like"},
                "project": {"type": "string", "description": "Project name, id or folder to work in"},
                "folder": {"type": "string", "description": "An absolute folder instead of a project"},
                "model": {"type": "string", "description": "Override the agent's model (rarely needed)"}
            }),
            &["agent", "prompt"],
        ),
        tool(
            "send_message",
            "Send a thread more instructions. prompt: a new turn when it's idle (queued if busy); steer: change course during the current turn; follow_up: run after the current turn.",
            json!({"thread_id": id, "text": {"type": "string"}, "mode": {"type": "string", "enum": ["prompt", "steer", "follow_up"]}}),
            &["thread_id", "text"],
        ),
        tool("interrupt_thread", "Stop a thread's running turn and drop its queued messages.", json!({"thread_id": id}), &["thread_id"]),
        tool(
            "wait_for_threads",
            "Wait until the given threads stop running (or the timeout passes), then return each one's status and last reply.",
            json!({"thread_ids": {"type": "array", "items": {"type": "string"}}, "timeout_seconds": {"type": "integer", "minimum": 5, "maximum": WAIT_MAX, "description": "Default 240"}}),
            &["thread_ids"],
        ),
        tool("open_thread", "Show a thread in the owner's EMP window.", json!({"thread_id": id}), &["thread_id"]),
        tool(
            "list_dev_servers",
            "Processes listening on ports on the owner's Mac. in_use means something (usually a browser tab) is connected right now.",
            json!({"all": {"type": "boolean", "description": "Include databases, agents and apps, not just dev servers"}}),
            &[],
        ),
        tool(
            "stop_dev_server",
            "Stop a listening process by pid (SIGTERM, plus a wrapper like npm run dev). Ask the owner first if it's in use.",
            json!({"pid": {"type": "integer"}, "force": {"type": "boolean", "description": "SIGKILL, for one that ignored a normal stop"}}),
            &["pid"],
        ),
        tool(
            "schedule_task",
            "Schedule a prompt to run later or repeatedly (EMP's cron jobs; they show on the Scheduled page). Each run either starts a new thread with an agent, or continues one thread so the agent keeps its history (thread_id; \"this\" means this conversation, for reminders and recurring reports from you). Give exactly one of cron, every_minutes, at or in_minutes. Times are the Mac's local time; list_schedules tells you the time now. The prompt must stand on its own and say what to report, including when nothing changed.",
            json!(schedule_props(false)),
            &["prompt"],
        ),
        tool(
            "update_schedule",
            "Change, pause or resume a schedule. Fields you leave out stay as they are.",
            json!(schedule_props(true)),
            &["schedule_id"],
        ),
        tool("list_schedules", "The owner's scheduled tasks with their timetable, next run and how the last run went; plus the time now.", json!({}), &[]),
        tool("delete_schedule", "Delete a scheduled task.", json!({"schedule_id": {"type": "string"}}), &["schedule_id"]),
        tool("run_schedule_now", "Run a scheduled task once now; its timetable carries on as before.", json!({"schedule_id": {"type": "string"}}), &["schedule_id"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_well_formed() {
        let tools = tool_list();
        assert_eq!(tools.len(), 16);
        for t in &tools {
            assert!(t["name"].as_str().unwrap().chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            assert_eq!(t["inputSchema"]["type"], "object");
            for r in t["inputSchema"]["required"].as_array().unwrap() {
                assert!(t["inputSchema"]["properties"].get(r.as_str().unwrap()).is_some(), "{} requires an undeclared {r}", t["name"]);
            }
        }
    }

    #[test]
    fn renders_transcript_items() {
        let user = json!({"kind": "user", "text": "fix the build\n\n<emp-context>\nRunning: nothing\n</emp-context>"});
        assert_eq!(render_item(&user).unwrap(), "**Owner:** fix the build");
        assert_eq!(render_item(&json!({"kind": "thinking", "text": "hm"})), None);
        assert_eq!(render_item(&json!({"kind": "toolResult", "output": "boom", "isError": true})).unwrap(), "← error: boom");
        assert_eq!(clip("abcdef", 3), "abc …");
        assert_eq!(ago(10_000_000, 10_000_000 - 7_200_000), "2h ago");
        assert_eq!(parse_local("2026-10-21T09:00:00Z").unwrap(), 1_792_573_200_000);
        assert!(parse_local("tomorrow").is_err());
    }
}
