//! Slash commands. The chat drivers run a harness in a headless mode where only some commands
//! work (pi: prompt templates and skills; Claude: what its `init` lists; Hermes: what ACP
//! advertises). Everything else, `/login` included, needs the harness's own terminal UI:
//! `commands.run` opens it in an EMP terminal and types the command there.
//!
//! Sources: pi is asked directly (`pi --mode rpc --no-session` + `get_commands`, cached per
//! cwd for 10 minutes); Claude and Hermes lists are remembered from their drivers (kept in
//! `<data>/commands.json` across restarts); built-in terminal commands are listed below.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use emp_protocol::{
    CommandRun, CommandsListParams, CommandsRunParams, Harness, RpcError, SlashCommand, SpawnSpec, TerminalInfo, TerminalKind,
    ThreadTakeoverParams, codes,
};
use serde_json::{Value, json};

use crate::state::State;
use crate::terminals::{Launch, Terminals};

const PI_TTL: Duration = Duration::from_secs(600);
const PI_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Built-in commands that only work in each harness's terminal UI.
const PI_BUILTINS: &[(&str, &str)] = &[
    ("login", "Sign in to a provider"),
    ("logout", "Sign out of a provider"),
    ("model", "Select a model"),
    ("thinking", "Set the thinking level"),
    ("scoped-models", "Models used by interactive cycling"),
    ("settings", "Open settings"),
    ("session", "Session information and statistics"),
    ("name", "Set the session name"),
    ("compact", "Compact the context now"),
    ("tree", "Navigate the session tree"),
    ("fork", "New session from an earlier message"),
    ("clone", "Duplicate the session"),
    ("export", "Export the session as HTML or JSONL"),
    ("share", "Upload the session and get a link"),
    ("copy", "Copy the last reply"),
    ("reload", "Reload extensions, skills, templates"),
    ("trust", "Save a project trust decision"),
    ("hotkeys", "Keyboard shortcuts"),
    ("changelog", "What's new in pi"),
];
const CLAUDE_BUILTINS: &[(&str, &str)] = &[
    ("login", "Sign in to your Anthropic account"),
    ("logout", "Sign out"),
    ("status", "Version, model, account and connectivity"),
    ("permissions", "Allow and deny tool rules"),
    ("hooks", "Manage hooks"),
    ("memory", "Edit CLAUDE.md memory files"),
    ("resume", "Resume another conversation"),
    ("add-dir", "Let Claude use another folder: /add-dir <path>"),
    ("export", "Export the conversation"),
    ("ide", "Connect to an IDE"),
    ("terminal-setup", "Install the Shift+Enter key binding"),
    ("plugin", "Manage plugins"),
    ("help", "Help"),
];
const CODEX_BUILTINS: &[(&str, &str)] = &[
    ("goal", "Set a goal Codex keeps working on: /goal <objective> | pause | resume | clear"),
    ("model", "Choose the model"),
    ("status", "Model, usage limits and goal"),
    ("rename", "Rename this chat"),
    ("new", "Start a new chat"),
    ("copy", "Copy the last reply"),
    ("review", "Review the current changes"),
    ("diff", "Show the git diff"),
    ("init", "Create an AGENTS.md"),
    ("compact", "Summarize the conversation"),
    ("mcp", "List MCP tools"),
    ("logout", "Sign out of Codex"),
    ("login", "Sign in with ChatGPT"),
];

/// pi built-ins EMP runs in the chat: through pi's RPC (`threads.command`), or in the UI
/// (`/new` opens New thread, `/copy` copies the last reply).
const PI_NATIVE: &[&str] = &["compact", "model", "thinking", "name", "session", "export", "copy", "new"];

/// Claude built-ins EMP runs in the chat (`drivers/claude.rs`).
const CLAUDE_NATIVE: &[&str] = &["add-dir"];

/// Codex built-ins EMP runs in the chat through `codex app-server` (`drivers/codex.rs`).
const CODEX_NATIVE: &[&str] = &["goal", "compact", "model", "status", "rename", "copy", "new"];

#[derive(Default)]
pub struct Cache {
    /// `pi:<cwd>` → probed list.
    pi: Mutex<HashMap<String, (Instant, Vec<SlashCommand>)>>,
    /// `claude`, `hermes:<profile>` → what the driver last reported.
    reported: Mutex<Option<HashMap<String, Vec<SlashCommand>>>>,
    file: Mutex<Option<PathBuf>>,
}

impl Cache {
    pub fn new(data_dir: &Path) -> Self {
        let c = Self::default();
        *c.file.lock().unwrap_or_else(|e| e.into_inner()) = Some(data_dir.join("commands.json"));
        c
    }

    fn reported(&self, key: &str) -> Option<Vec<SlashCommand>> {
        let mut r = self.reported.lock().unwrap_or_else(|e| e.into_inner());
        if r.is_none() {
            let file = self.file.lock().unwrap_or_else(|e| e.into_inner()).clone();
            *r = Some(file.and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
        }
        r.as_ref().and_then(|m| m.get(key).cloned())
    }

    /// Remembers a driver's list (`DriverEvent::Commands`).
    pub fn record(&self, key: &str, commands: Vec<SlashCommand>) {
        if self.reported(key).as_ref() == Some(&commands) {
            return;
        }
        let mut r = self.reported.lock().unwrap_or_else(|e| e.into_inner());
        let map = r.get_or_insert_with(HashMap::new);
        map.insert(key.to_string(), commands);
        if let Some(f) = self.file.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
            && let Ok(bytes) = serde_json::to_vec(map)
        {
            let _ = std::fs::write(f, bytes);
        }
    }
}

fn builtin(list: &[(&str, &str)], native: &[&str]) -> Vec<SlashCommand> {
    list.iter()
        .map(|(n, d)| SlashCommand {
            name: (*n).into(),
            description: Some((*d).into()),
            run: if native.contains(n) { CommandRun::Native } else { CommandRun::Terminal },
            source: Some("built-in".into()),
        })
        .collect()
}

/// Commands from Claude's `system/init`: `slash_commands` run in chat except the ones it lists
/// as `terminal_slash_commands`.
pub fn from_claude_init(rec: &Value) -> Vec<SlashCommand> {
    let names = |k: &str| -> Vec<String> {
        rec[k].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    let terminal = names("terminal_slash_commands");
    let skills = names("skills");
    names("slash_commands")
        .into_iter()
        .filter(|n| !n.starts_with("__") && !n.starts_with("mcp__"))
        .map(|n| SlashCommand {
            run: if terminal.contains(&n) { CommandRun::Terminal } else { CommandRun::Chat },
            source: Some(if skills.contains(&n) { "skill" } else if n.contains(':') { "plugin" } else { "command" }.into()),
            description: None,
            name: n,
        })
        .collect()
}

/// ACP `available_commands_update` (Hermes): all run in chat.
pub fn from_acp(v: &Value) -> Vec<SlashCommand> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some(SlashCommand {
                        name: c["name"].as_str()?.trim_start_matches('/').to_string(),
                        description: c["description"].as_str().filter(|d| !d.is_empty()).map(String::from),
                        run: CommandRun::Chat,
                        source: Some("hermes".into()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// pi's `get_commands`: templates and skills run in chat; extension commands may open dialogs
/// the RPC driver can't show, so they go to the terminal.
fn from_pi(data: &Value) -> Vec<SlashCommand> {
    data["commands"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let source = c["source"].as_str().unwrap_or("extension");
                    Some(SlashCommand {
                        name: c["name"].as_str()?.to_string(),
                        description: c["description"].as_str().filter(|d| !d.is_empty()).map(String::from),
                        run: if source == "extension" { CommandRun::Terminal } else { CommandRun::Chat },
                        source: Some(source.into()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn probe_pi(cwd: &Path) -> anyhow::Result<Vec<SlashCommand>> {
    let mut p = crate::drivers::Proc::spawn("pi", &["--mode".into(), "rpc".into(), "--no-session".into()], cwd)?;
    p.send(&json!({"id": "emp-commands", "type": "get_commands"})).await?;
    let found = tokio::time::timeout(PI_PROBE_TIMEOUT, async {
        while let Some(rec) = p.next().await {
            if rec["id"] == "emp-commands" {
                return Some(rec);
            }
        }
        None
    })
    .await;
    p.shutdown().await;
    match found {
        Ok(Some(rec)) if rec["success"] == true => Ok(from_pi(&rec["data"])),
        Ok(Some(rec)) => anyhow::bail!("pi get_commands: {}", rec["error"]),
        _ => anyhow::bail!("pi didn't list its commands"),
    }
}

/// Appends `extra` entries whose names aren't taken yet.
fn merge(mut list: Vec<SlashCommand>, extra: Vec<SlashCommand>) -> Vec<SlashCommand> {
    for c in extra {
        if !list.iter().any(|x| x.name == c.name) {
            list.push(c);
        }
    }
    list
}

/// `commands.list`
pub async fn list(state: &State, p: CommandsListParams) -> Result<Vec<SlashCommand>, RpcError> {
    let agent = match p.agent_id.clone() {
        Some(id) => state.with_store(move |s| s.get_agent(&id)).await.map_err(internal)?,
        None => None,
    };
    let cwd = agent.as_ref().and_then(|a| a.cwd.as_deref()).map(emp_import::util::expand_home).filter(|d| d.is_dir());
    let cache = &state.commands;
    let mut list = match p.harness {
        Harness::Pi => {
            let cwd = cwd.unwrap_or_else(emp_core::paths::home_dir);
            let key = format!("pi:{}", cwd.display());
            let fresh = cache.pi.lock().unwrap_or_else(|e| e.into_inner()).get(&key).filter(|(t, _)| t.elapsed() < PI_TTL).map(|(_, l)| l.clone());
            let found = match fresh {
                Some(l) => l,
                None => match probe_pi(&cwd).await {
                    Ok(l) => {
                        cache.pi.lock().unwrap_or_else(|e| e.into_inner()).insert(key, (Instant::now(), l.clone()));
                        l
                    }
                    Err(e) => {
                        tracing::warn!("pi commands: {e:#}");
                        Vec::new()
                    }
                },
            };
            merge(builtin(PI_BUILTINS, PI_NATIVE), found)
        }
        Harness::Claude => merge(builtin(CLAUDE_BUILTINS, CLAUDE_NATIVE), cache.reported("claude").unwrap_or_default()),
        Harness::Hermes => {
            let profile = agent.as_ref().and_then(|a| a.harness_profile.clone()).unwrap_or_else(|| "default".into());
            cache.reported(&format!("hermes:{profile}")).unwrap_or_default()
        }
        Harness::Codex => builtin(CODEX_BUILTINS, CODEX_NATIVE),
        _ => Vec::new(),
    };
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

fn internal(e: anyhow::Error) -> RpcError {
    RpcError { code: codes::INTERNAL, message: format!("{e:#}"), data: None }
}

fn invalid(message: impl Into<String>) -> RpcError {
    RpcError { code: codes::INVALID_PARAMS, message: message.into(), data: None }
}

/// The harness TUI for a fresh session of `agent`.
fn tui_spec(agent: &emp_protocol::AgentProfile) -> Result<SpawnSpec, RpcError> {
    let (program, args): (&str, Vec<String>) = match agent.harness {
        Harness::Pi => ("pi", vec![]),
        Harness::Claude => ("claude", vec![]),
        Harness::Codex => ("codex", vec![]),
        Harness::Antigravity => ("agy", vec![]),
        Harness::Gemini => ("gemini", vec![]),
        Harness::Cursor => ("cursor-agent", vec![]),
        Harness::Hermes => match agent.harness_profile.as_deref().filter(|p| !p.is_empty() && *p != "default") {
            Some(p) => ("hermes", vec!["-p".into(), p.into()]),
            None => ("hermes", vec![]),
        },
        Harness::Shell => return Err(invalid("a shell has no slash commands")),
    };
    let cwd = agent
        .cwd
        .as_deref()
        .map(emp_import::util::expand_home)
        .filter(|d| d.is_dir())
        .map(|d| d.to_string_lossy().into_owned());
    let display = std::iter::once(program.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
    Ok(SpawnSpec { program: program.into(), args, cwd, env: Vec::new(), display })
}

/// `claude auth login` / `codex login`: they print a sign-in link, open the browser and wait.
fn login_spec(harness: Harness, cwd: Option<String>) -> Option<SpawnSpec> {
    let (program, args): (&str, &[&str]) = match harness {
        Harness::Claude => ("claude", &["auth", "login"]),
        Harness::Codex => ("codex", &["login"]),
        _ => return None,
    };
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let display = std::iter::once(program.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
    let cwd = cwd.map(|c| emp_import::util::expand_home(&c)).filter(|d| d.is_dir()).map(|d| d.to_string_lossy().into_owned());
    Some(SpawnSpec { program: program.into(), args, cwd, env: Vec::new(), display })
}

/// `commands.run`
pub async fn run(state: &State, p: CommandsRunParams) -> Result<TerminalInfo, RpcError> {
    let text = p.text.trim().to_string();
    if !text.starts_with('/') || text.len() < 2 || text.contains('\n') {
        return Err(invalid("a slash command is one line starting with /"));
    }
    // Claude and Codex sign in with their own login commands (a link and a browser), not
    // through their chat UIs.
    let name = text[1..].split_whitespace().next().unwrap_or("");
    if name == "login" {
        let (harness, cwd) = match (&p.thread_id, &p.agent_id) {
            (Some(t), _) => {
                let t = t.clone();
                let thread = state.with_store(move |s| s.get_thread(&t)).await.map_err(internal)?;
                (thread.as_ref().map(|t| t.harness), thread.and_then(|t| t.cwd))
            }
            (None, Some(a)) => {
                let a = a.clone();
                let agent = state.with_store(move |s| s.get_agent(&a)).await.map_err(internal)?;
                (agent.as_ref().map(|a| a.harness), agent.and_then(|a| a.cwd))
            }
            _ => (None, None),
        };
        if let Some(spec) = harness.and_then(|h| login_spec(h, cwd)) {
            let launch = Launch { kind: TerminalKind::Command, title: format!("{} sign-in", spec.display), thread_id: None, spec, cols: p.cols, rows: p.rows };
            let terminals = state.terminals.clone();
            return tokio::task::spawn_blocking(move || terminals.spawn(launch)).await.map_err(|e| internal(e.into()))?;
        }
    }
    let info = match (&p.thread_id, &p.agent_id) {
        (Some(thread_id), _) => {
            crate::live::takeover(state, ThreadTakeoverParams { thread_id: thread_id.clone(), cols: p.cols, rows: p.rows, force: None }).await?
        }
        (None, Some(agent_id)) => {
            let id = agent_id.clone();
            let agent = state
                .with_store(move |s| s.get_agent(&id))
                .await
                .map_err(internal)?
                .ok_or_else(|| RpcError { code: codes::NOT_FOUND, message: format!("no agent {agent_id}"), data: None })?;
            let spec = tui_spec(&agent)?;
            let launch = Launch {
                kind: TerminalKind::Command,
                title: format!("{} · {}", agent.name, emp_import::util::truncate(&text, 40)),
                thread_id: None,
                spec,
                cols: p.cols,
                rows: p.rows,
            };
            let terminals = state.terminals.clone();
            tokio::task::spawn_blocking(move || terminals.spawn(launch)).await.map_err(|e| internal(e.into()))??
        }
        (None, None) => return Err(invalid("commands.run needs a threadId or an agentId")),
    };
    type_when_ready(state.terminals.clone(), info.id.clone(), text);
    Ok(info)
}

/// Waits for the TUI to draw and settle, then types `text`, waits for it to be drawn, and
/// presses Return. TUIs keep drawing notices for a while after the first frame, and keys
/// that arrive mid-startup can be dropped, so "settled" means quiet for a good while.
fn type_when_ready(terminals: std::sync::Arc<Terminals>, id: String, text: String) {
    tokio::spawn(async move {
        if !settle(&terminals, &id, Duration::from_millis(1500), Duration::from_secs(3), Duration::from_secs(25)).await {
            return;
        }
        // No trailing space: after one, pi completes the argument and Return picks its first
        // choice (`/login` became `/login amazon-bedrock`).
        if terminals.write_bytes(&id, text.as_bytes()).is_err() {
            return;
        }
        // The echo and completion menu, then Return.
        if !settle(&terminals, &id, Duration::from_millis(500), Duration::from_millis(300), Duration::from_secs(5)).await {
            return;
        }
        let _ = terminals.write_bytes(&id, b"\r");
    });
}

/// Waits until the terminal has drawn something new and then been quiet for `quiet`, at least
/// `min` from now and at most `max`. `false` once the terminal is gone.
async fn settle(terminals: &Terminals, id: &str, quiet: Duration, min: Duration, max: Duration) -> bool {
    let started = Instant::now();
    let Some(first) = terminals.output_seq(id) else { return false };
    let (mut last, mut since) = (first, Instant::now());
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let Some(seq) = terminals.output_seq(id) else { return false };
        if seq != last {
            (last, since) = (seq, Instant::now());
        } else if seq > first && since.elapsed() >= quiet && started.elapsed() >= min {
            return true;
        }
        if started.elapsed() > max {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_init_splits_chat_and_terminal() {
        let rec = json!({
            "slash_commands": ["compact", "doctor", "frontend-design", "stripe:docs", "__remote", "mcp__x__y"],
            "terminal_slash_commands": ["doctor"],
            "skills": ["frontend-design"],
        });
        let l = from_claude_init(&rec);
        let by = |n: &str| l.iter().find(|c| c.name == n).cloned().unwrap();
        assert_eq!(l.len(), 4);
        assert_eq!(by("compact").run, CommandRun::Chat);
        assert_eq!(by("doctor").run, CommandRun::Terminal);
        assert_eq!(by("frontend-design").source.as_deref(), Some("skill"));
        assert_eq!(by("stripe:docs").source.as_deref(), Some("plugin"));
    }

    #[test]
    fn pi_extensions_go_to_the_terminal() {
        let data = json!({"commands": [
            {"name": "fix-tests", "description": "Fix failing tests", "source": "prompt"},
            {"name": "skill:wiki", "source": "skill"},
            {"name": "mcp", "description": "Manage MCP servers", "source": "extension"},
        ]});
        let l = from_pi(&data);
        assert_eq!(l.iter().map(|c| c.run).collect::<Vec<_>>(), vec![CommandRun::Chat, CommandRun::Chat, CommandRun::Terminal]);
        let merged = merge(builtin(PI_BUILTINS, PI_NATIVE), l);
        assert!(merged.iter().any(|c| c.name == "login" && c.run == CommandRun::Terminal));
        assert!(merged.iter().any(|c| c.name == "compact" && c.run == CommandRun::Native));
        assert!(merged.iter().any(|c| c.name == "fix-tests"));
    }

    #[test]
    fn codex_commands_run_in_chat_except_its_screens() {
        let l = builtin(CODEX_BUILTINS, CODEX_NATIVE);
        let run = |n: &str| l.iter().find(|c| c.name == n).map(|c| c.run);
        assert_eq!(run("goal"), Some(CommandRun::Native));
        assert_eq!(run("model"), Some(CommandRun::Native));
        assert_eq!(run("review"), Some(CommandRun::Terminal));
        assert_eq!(run("login"), Some(CommandRun::Terminal));
    }

    #[test]
    fn acp_commands_and_the_cache_file() {
        let l = from_acp(&json!([{"name": "compress", "description": "Compress context"}, {"name": "/model", "description": ""}]));
        assert_eq!(l[1].name, "model");
        assert_eq!(l[1].description, None);
        let dir = tempfile::tempdir().unwrap();
        Cache::new(dir.path()).record("hermes:default", l.clone());
        assert_eq!(Cache::new(dir.path()).reported("hermes:default"), Some(l));
    }
}
