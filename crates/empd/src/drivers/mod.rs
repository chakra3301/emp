//! Structured drivers: one long-lived harness process per live thread, spoken to over stdio.
//!
//! Each driver runs as a tokio task that owns its child process. The live layer (`live.rs`)
//! sends [`DriverCmd`]s and receives harness-independent [`DriverEvent`]s; it never sees the
//! harness protocol. Transcript items come from the same mappers the importers use, so a live
//! turn renders exactly like the same turn re-read from disk later.
//!
//! | Harness | Process | Steer | Follow-up | Interrupt |
//! |---|---|---|---|---|
//! | pi | `pi --mode rpc` | native `steer` | native `follow_up` | `abort` |
//! | Claude Code | `claude -p --input-format stream-json …` | message sent mid-turn | EMP queue | `control_request` interrupt |
//! | Hermes | `hermes [-p profile] acp` (ACP) | EMP queue | EMP queue | `session/cancel` |
//! | Codex | `codex app-server` (JSON-RPC) | native `turn/steer` | EMP queue | `turn/interrupt` (+ pauses an active goal) |
//! | Antigravity | `agy --print=… --output-format stream-json` per turn (`--conversation <id>` after) | EMP queue | EMP queue | kill the turn |

mod antigravity;
mod claude;
pub mod codex;
pub mod hermes;
pub(crate) mod pi;

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use emp_protocol::{Harness, PartialKind, SessionRef, TranscriptItem};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

use crate::doctor;

/// A user message. Attached files are already listed (by path) in `text`; `images` are the
/// attached images, which harnesses that take images inline also get as pictures.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Msg {
    pub text: String,
    pub images: Vec<PathBuf>,
}

impl From<&str> for Msg {
    fn from(text: &str) -> Self {
        Self { text: text.into(), images: Vec::new() }
    }
}

/// Model APIs refuse images over ~5 MB of base64; bigger ones go by path only.
pub(crate) const INLINE_IMAGE_MAX: u64 = 3_750_000;

/// `(mime, base64)` for each image small enough to send inline.
pub(crate) fn inline_images(paths: &[PathBuf]) -> Vec<(String, String)> {
    use base64::Engine;
    paths
        .iter()
        .filter_map(|p| {
            let mime = image_mime(p)?;
            if std::fs::metadata(p).ok()?.len() > INLINE_IMAGE_MAX {
                return None;
            }
            Some((mime.to_string(), base64::engine::general_purpose::STANDARD.encode(std::fs::read(p).ok()?)))
        })
        .collect()
}

/// Image types the model APIs accept, by extension.
pub fn image_mime(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Commands from the live layer to a driver.
#[derive(Debug, Clone)]
pub enum DriverCmd {
    /// Start a turn. Only sent while idle.
    Prompt(Msg),
    /// Inject into the running turn. Only sent when [`Caps::native_steer`].
    Steer(Msg),
    /// Run after the current turn. Only sent when [`Caps::native_follow_up`].
    FollowUp(Msg),
    Interrupt,
    /// Switch the session's model (an id from `models.list`) from its next turn on.
    SetModel(String),
    /// A slash command EMP runs through the harness's API (pi, Codex): name without the slash.
    Command { name: String, args: String },
    /// The owner's answer to a [`DriverEvent::Ask`].
    Answer { id: String, value: Option<String>, confirmed: Option<bool>, cancelled: bool },
    /// Close stdin and let the harness exit; the driver force-kills after a grace period.
    Shutdown,
}

/// Harness-independent events from a driver.
#[derive(Debug, Clone)]
pub enum DriverEvent {
    /// The harness session exists. `import_key` matches what the importer for this harness
    /// produces, so the live layer can find (or import) the thread.
    Ready { import_key: String },
    TurnStarted,
    Item(TranscriptItem),
    /// Whole in-progress block text so far (not a delta).
    Partial(PartialKind, String),
    /// The harness's own pending queue (pi), oldest first.
    Queue(Vec<String>),
    TurnEnded { aborted: bool },
    /// The turn failed (an API error, a refused prompt), as opposed to being stopped. Comes
    /// before its `TurnEnded`; a later reply in the same turn means it recovered.
    TurnFailed(String),
    /// Context size as the harness reports it live (Hermes, Antigravity); others are read
    /// from the session on disk by `threads.context`.
    Context(emp_protocol::ContextUsage),
    /// Slash commands the harness says it runs in this mode (Claude `init`, Hermes ACP).
    Commands { key: String, commands: Vec<emp_protocol::SlashCommand> },
    /// A question for the owner; the harness waits for [`DriverCmd::Answer`].
    Ask(emp_protocol::Question),
    /// The owner gave this chat lasting access ("Allow for this chat"); the live layer keeps it
    /// with the thread so a restarted process gets it too.
    Granted(emp_core::Grants),
    /// The harness withdrew question `id` (it timed out, or the turn ended).
    Unask(String),
    /// The thread's goal changed (Codex `/goal`); `None` when it was cleared.
    Goal(Option<emp_protocol::ThreadGoal>),
    /// The session changed outside a turn (renamed, model switched): re-import it.
    SessionChanged,
    /// The process is gone. `Some` when it died unexpectedly.
    Exited(Option<String>),
}

/// What a driver can do natively; the live layer queues the rest.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub native_steer: bool,
    pub native_follow_up: bool,
    /// The harness echoes the user's message back as a transcript item.
    pub echoes_user: bool,
}

/// Everything needed to start a driver.
#[derive(Debug, Clone)]
pub struct DriverSpec {
    pub harness: Harness,
    pub cwd: PathBuf,
    /// Continue this session; `None` starts a new one.
    pub resume: Option<SessionRef>,
    /// Applied to new sessions only: a thread keeps the model it started with.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub instructions: Option<String>,
    /// Hermes profile (`default` = `~/.hermes`).
    pub profile: Option<String>,
    /// Agent autonomy (0–3); decides automatic permission answers.
    pub autonomy: u8,
    /// Compact once the context passes this many tokens (read at each use, so changes apply to
    /// running chats). Claude and Codex enforce it natively; for pi and Hermes the driver checks
    /// after each turn and compacts before reporting the turn as ended.
    pub compact_at: crate::state::CompactAt,
    /// The `emp` tools, for the coordinator only (`mcp.rs`).
    pub mcp: Option<McpGrant>,
    /// Access the owner already gave this chat (Claude: extra folders and allow rules).
    pub grants: emp_core::Grants,
}

/// Access to EMP's MCP server for one driver process. The token is that session's own key:
/// it identifies the caller and is never the daemon token.
#[derive(Debug, Clone)]
pub struct McpGrant {
    pub url: String,
    pub token: String,
}

/// Environment variable the harness reads the grant token from (`.pi/mcp.json` headers).
pub const MCP_TOKEN_ENV: &str = "EMP_MCP_TOKEN";

pub struct DriverHandle {
    pub cmds: mpsc::UnboundedSender<DriverCmd>,
    pub pid: Option<u32>,
    pub caps: Caps,
}

pub fn supports(harness: Harness) -> bool {
    matches!(harness, Harness::Pi | Harness::Claude | Harness::Hermes | Harness::Codex | Harness::Antigravity)
}

/// Spawns the driver for `spec.harness`.
pub fn spawn(spec: DriverSpec) -> Result<(DriverHandle, mpsc::UnboundedReceiver<DriverEvent>)> {
    match spec.harness {
        Harness::Pi => pi::spawn(spec),
        Harness::Claude => claude::spawn(spec),
        Harness::Hermes => hermes::spawn(spec),
        Harness::Codex => codex::spawn(spec),
        Harness::Antigravity => antigravity::spawn(spec),
        other => bail!("{} has no structured driver yet", other.as_str()),
    }
}

/// A harness child process with piped stdio, in its own process group.
pub(crate) struct Proc {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: BufReader<ChildStdout>,
    pub pid: Option<u32>,
    /// Last few KiB of stderr, for error messages when the process dies.
    pub stderr_tail: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

const STDERR_TAIL: usize = 4096;

impl Proc {
    pub fn spawn(program: &str, args: &[String], cwd: &std::path::Path) -> Result<Self> {
        Self::spawn_env(program, args, cwd, &[])
    }

    /// [`Proc::spawn`] with extra environment variables.
    pub fn spawn_env(program: &str, args: &[String], cwd: &std::path::Path, env: &[(&str, String)]) -> Result<Self> {
        let path = doctor::which(program).with_context(|| format!("{program} is not installed"))?;
        let mut cmd = Command::new(path);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        if let Ok(p) = std::env::join_paths(doctor::search_dirs()) {
            cmd.env("PATH", p);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().with_context(|| format!("spawn {program}"))?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = BufReader::with_capacity(256 * 1024, child.stdout.take().context("stdout")?);
        let stderr_tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        if let Some(mut err) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                while let Ok(n) = err.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                    t.extend_from_slice(&buf[..n]);
                    if t.len() > STDERR_TAIL {
                        let cut = t.len() - STDERR_TAIL;
                        t.drain(..cut);
                    }
                }
            });
        }
        Ok(Self { child, stdin, stdout, pid, stderr_tail })
    }

    /// Writes one JSON record followed by LF.
    pub async fn send(&mut self, v: &serde_json::Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("stdin closed")?;
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Next JSON record; `None` at EOF. Splits on LF only and skips non-JSON lines.
    pub async fn next(&mut self) -> Option<serde_json::Value> {
        let mut line = Vec::new();
        loop {
            line.clear();
            match self.stdout.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => return None,
                Ok(_) => {
                    if let Ok(v) = serde_json::from_slice(line.trim_ascii()) {
                        return Some(v);
                    }
                }
            }
        }
    }

    pub fn stderr_summary(&self) -> String {
        let t = self.stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
        let text = String::from_utf8_lossy(&t);
        let last: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let start = last.len().saturating_sub(3);
        emp_import::util::truncate(&last[start..].join(" | "), 400)
    }

    /// Closes stdin, waits up to 3 s, then kills the process group.
    pub async fn shutdown(&mut self) {
        self.stdin.take();
        if tokio::time::timeout(std::time::Duration::from_secs(3), self.child.wait()).await.is_err() {
            if let Some(pid) = self.pid {
                kill_group(pid);
            }
            let _ = self.child.wait().await;
        }
    }
}

/// SIGKILL to a driver's process group (it was spawned with `process_group(0)`).
pub fn kill_group(pid: u32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Accumulates streaming text per content block and reports the newest block's whole text.
#[derive(Default)]
pub(crate) struct Partials {
    blocks: Vec<(i64, PartialKind, String)>,
}

impl Partials {
    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    /// Appends `delta` to block `index` and returns the block's text so far.
    pub fn push(&mut self, index: i64, kind: PartialKind, delta: &str) -> String {
        match self.blocks.iter_mut().find(|(i, k, _)| *i == index && *k == kind) {
            Some((_, _, text)) => {
                text.push_str(delta);
                text.clone()
            }
            None => {
                self.blocks.push((index, kind, delta.to_string()));
                delta.to_string()
            }
        }
    }
}

/// Text of a message or tool result `content` (string, or blocks with `text`).
pub(crate) fn text_of(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(_) => emp_import::util::content_text(v),
        serde_json::Value::Object(o) => o.get("text").and_then(|t| t.as_str()).unwrap_or_default().to_string(),
        _ => String::new(),
    }
}
