//! Daemon-owned terminals (PTYs).
//!
//! Terminals outlive the clients watching them: a UI attaches, gets the recent scrollback
//! ([`Terminals::attach`]), then follows `terminal.output` pushes. Output is also recorded to
//! `<data_dir>/terminals/<id>.log` (raw bytes, capped) with a `<id>.json` sidecar holding the
//! [`TerminalInfo`], so a session stays inspectable after the daemon restarts.
//!
//! API used by other modules (e.g. thread takeover in `live.rs`):
//! - [`open`] resolves `terminals.open` params (thread cwd, resume command) and spawns.
//! - [`Terminals::spawn`] spawns an already-resolved [`Launch`].
//! - [`Terminals::info`], [`Terminals::kill`], [`Terminals::exit_watch`] (await exit).
//! - [`Terminals::shutdown`] kills every running terminal when the daemon stops.
//!
//! Threads: one blocking reader per PTY feeds a flusher that coalesces chunks (≤ 8 ms or
//! 64 KiB) before broadcasting, so `cat bigfile` doesn't turn into 100k notifications.
//! One blocking waiter per PTY reaps the child.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use emp_protocol::{
    RpcError, RpcNotification, SpawnSpec, TerminalInfo, TerminalKind, TerminalOpenParams, TerminalOutput,
    TerminalSnapshot, TerminalState, codes, notifications,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{broadcast, watch};

use crate::doctor;
use crate::state::State;

/// Scrollback kept in memory per terminal for `attach`.
const RING_BYTES: usize = 1024 * 1024;
/// Recording cap per terminal on disk.
const LOG_BYTES: u64 = 32 * 1024 * 1024;
/// Most terminals running at once.
const MAX_RUNNING: usize = 32;
/// Output coalescing window and size.
const FLUSH_WINDOW: Duration = Duration::from_millis(8);
const FLUSH_BYTES: usize = 64 * 1024;

/// A fully resolved program to run in a new terminal.
pub struct Launch {
    pub kind: TerminalKind,
    pub title: String,
    pub thread_id: Option<String>,
    pub spec: SpawnSpec,
    pub cols: u16,
    pub rows: u16,
}

pub struct Terminals {
    dir: PathBuf,
    notify: broadcast::Sender<RpcNotification>,
    map: Mutex<HashMap<String, Arc<Term>>>,
}

struct Term {
    info: Mutex<TerminalInfo>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    ring: Mutex<Ring>,
    exit: watch::Sender<TerminalState>,
}

#[derive(Default)]
struct Ring {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    /// Seq of the newest chunk in `chunks` (0 before any output).
    seq: u64,
}

impl Ring {
    fn push(&mut self, chunk: Vec<u8>) -> u64 {
        self.seq += 1;
        self.bytes += chunk.len();
        self.chunks.push_back(chunk);
        while self.bytes > RING_BYTES && self.chunks.len() > 1 {
            if let Some(old) = self.chunks.pop_front() {
                self.bytes -= old.len();
            }
        }
        self.seq
    }

    fn contents(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes);
        for c in &self.chunks {
            out.extend_from_slice(c);
        }
        // A single oversized chunk can exceed the cap; keep its tail.
        if out.len() > RING_BYTES {
            out.drain(..out.len() - RING_BYTES);
        }
        out
    }
}

fn err(code: i32, message: impl Into<String>) -> RpcError {
    RpcError { code, message: message.into(), data: None }
}

impl Terminals {
    pub fn new(data_dir: &Path, notify: broadcast::Sender<RpcNotification>) -> Self {
        Self { dir: data_dir.join("terminals"), notify, map: Mutex::new(HashMap::new()) }
    }

    fn get(&self, id: &str) -> Result<Arc<Term>, RpcError> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| err(codes::NOT_FOUND, format!("no terminal {id}")))
    }

    pub fn info(&self, id: &str) -> Option<TerminalInfo> {
        self.get(id).ok().map(|t| t.info.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    /// Newest first.
    pub fn list(&self) -> Vec<TerminalInfo> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let mut all: Vec<TerminalInfo> =
            map.values().map(|t| t.info.lock().unwrap_or_else(|e| e.into_inner()).clone()).collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
        all
    }

    /// Resolves when the terminal exits (immediately if it already has).
    pub fn exit_watch(&self, id: &str) -> Option<watch::Receiver<TerminalState>> {
        self.get(id).ok().map(|t| t.exit.subscribe())
    }

    pub fn attach(&self, id: &str) -> Result<TerminalSnapshot, RpcError> {
        let t = self.get(id)?;
        // Hold the ring lock while reading info so seq and contents match.
        let ring = t.ring.lock().unwrap_or_else(|e| e.into_inner());
        let info = t.info.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Ok(TerminalSnapshot { info, scrollback: B64.encode(ring.contents()), seq: ring.seq })
    }

    pub fn write(&self, id: &str, data_b64: &str) -> Result<(), RpcError> {
        let t = self.get(id)?;
        if t.info.lock().unwrap_or_else(|e| e.into_inner()).state == TerminalState::Exited {
            return Err(err(codes::INVALID_REQUEST, "terminal has exited"));
        }
        drop(t);
        let bytes = B64.decode(data_b64).map_err(|e| err(codes::INVALID_PARAMS, format!("data: {e}")))?;
        self.write_bytes(id, &bytes)
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), RpcError> {
        let t = self.get(id)?;
        let (cols, rows) = (clamp(cols), clamp(rows));
        t.master
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| err(codes::HARNESS_ERROR, e.to_string()))?;
        let mut info = t.info.lock().unwrap_or_else(|e| e.into_inner());
        info.cols = cols;
        info.rows = rows;
        Ok(())
    }

    /// Running: SIGHUP the process group, SIGKILL after 2 s. Exited: forget it.
    pub fn kill(&self, id: &str) -> Result<(), RpcError> {
        let t = self.get(id)?;
        let (state, pid) = {
            let info = t.info.lock().unwrap_or_else(|e| e.into_inner());
            (info.state, info.pid)
        };
        if state == TerminalState::Exited {
            self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
            return Ok(());
        }
        match pid {
            Some(pid) => {
                signal_group(pid, libc::SIGHUP);
                let mut exit = t.exit.subscribe();
                tokio::spawn(async move {
                    let exited = tokio::time::timeout(Duration::from_secs(2), exit.wait_for(|s| *s == TerminalState::Exited));
                    if exited.await.is_err() {
                        signal_group(pid, libc::SIGKILL);
                    }
                });
            }
            None => {
                let _ = t.killer.lock().unwrap_or_else(|e| e.into_inner()).kill();
            }
        }
        Ok(())
    }

    /// Kills every running terminal's process group (daemon shutdown). Synchronous.
    pub fn shutdown(&self) {
        let pids: Vec<u32> = self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter_map(|t| {
                let info = t.info.lock().unwrap_or_else(|e| e.into_inner());
                (info.state == TerminalState::Running).then_some(info.pid).flatten()
            })
            .collect();
        for &pid in &pids {
            signal_group(pid, libc::SIGHUP);
        }
        if !pids.is_empty() {
            std::thread::sleep(Duration::from_millis(300));
            for pid in pids {
                signal_group(pid, libc::SIGKILL);
            }
        }
    }

    fn running(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|t| t.info.lock().unwrap_or_else(|e| e.into_inner()).state == TerminalState::Running)
            .count()
    }

    /// Spawns `launch` in a new PTY and starts streaming its output.
    /// Raw bytes to a running terminal's input.
    pub fn write_bytes(&self, id: &str, bytes: &[u8]) -> Result<(), RpcError> {
        let t = self.get(id)?;
        if t.info.lock().unwrap_or_else(|e| e.into_inner()).state == TerminalState::Exited {
            return Err(err(codes::INVALID_REQUEST, "terminal has exited"));
        }
        let mut w = t.writer.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(bytes).and_then(|_| w.flush()).map_err(|e| err(codes::HARNESS_ERROR, e.to_string()))
    }

    /// Output chunks so far (grows while the program draws); `None` once it has exited.
    pub fn output_seq(&self, id: &str) -> Option<u64> {
        let t = self.get(id).ok()?;
        if t.info.lock().unwrap_or_else(|e| e.into_inner()).state == TerminalState::Exited {
            return None;
        }
        Some(t.ring.lock().unwrap_or_else(|e| e.into_inner()).seq)
    }

    pub fn spawn(&self, launch: Launch) -> Result<TerminalInfo, RpcError> {
        if self.running() >= MAX_RUNNING {
            return Err(err(codes::BUSY, format!("at most {MAX_RUNNING} terminals can run at once")));
        }
        let (cols, rows) = (clamp(launch.cols), clamp(launch.rows));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| err(codes::HARNESS_ERROR, format!("openpty: {e}")))?;

        let cmd = command_for(&launch.spec)?;
        let mut child =
            pair.slave.spawn_command(cmd).map_err(|e| err(codes::HARNESS_ERROR, format!("spawn {}: {e}", launch.spec.program)))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().map_err(|e| err(codes::HARNESS_ERROR, e.to_string()))?;
        let writer = pair.master.take_writer().map_err(|e| err(codes::HARNESS_ERROR, e.to_string()))?;

        let info = TerminalInfo {
            id: ulid::Ulid::new().to_string(),
            kind: launch.kind,
            title: launch.title,
            thread_id: launch.thread_id,
            cwd: launch.spec.cwd.clone(),
            command: launch.spec.display.clone(),
            pid: child.process_id(),
            state: TerminalState::Running,
            exit_code: None,
            cols,
            rows,
            created_at: emp_core::now_ms(),
        };
        let (exit, _) = watch::channel(TerminalState::Running);
        let term = Arc::new(Term {
            info: Mutex::new(info.clone()),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            ring: Mutex::new(Ring::default()),
            exit,
        });
        self.map.lock().unwrap_or_else(|e| e.into_inner()).insert(info.id.clone(), term.clone());

        let _ = fs::create_dir_all(&self.dir);
        write_sidecar(&self.dir, &info);
        let log = fs::OpenOptions::new().create(true).append(true).open(self.dir.join(format!("{}.log", info.id))).ok();
        self.push_state(&info);

        // Reader → flusher → ring/log/broadcast.
        let (tx, rx) = std_mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name(format!("pty-read-{}", info.id))
            .spawn(move || read_loop(reader, tx))
            .map_err(|e| err(codes::INTERNAL, e.to_string()))?;
        let flusher = Flusher { id: info.id.clone(), term: term.clone(), notify: self.notify.clone(), log, logged: 0 };
        let flush_handle = std::thread::Builder::new()
            .name(format!("pty-flush-{}", info.id))
            .spawn(move || flusher.run(rx))
            .map_err(|e| err(codes::INTERNAL, e.to_string()))?;

        // Waiter: reap, let the last output drain, then publish the exit.
        let notify = self.notify.clone();
        let dir = self.dir.clone();
        std::thread::Builder::new()
            .name(format!("pty-wait-{}", info.id))
            .spawn(move || {
                let code = child.wait().ok().map(|s| s.exit_code() as i32);
                // The reader sees EOF once every process holding the slave has exited; don't
                // block forever on a background job that keeps it open.
                let deadline = std::time::Instant::now() + Duration::from_millis(500);
                while !flush_handle.is_finished() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let info = {
                    let mut info = term.info.lock().unwrap_or_else(|e| e.into_inner());
                    info.state = TerminalState::Exited;
                    info.exit_code = code;
                    info.clone()
                };
                write_sidecar(&dir, &info);
                let _ = notify.send(RpcNotification::new(notifications::TERMINAL_STATE, &info));
                let _ = term.exit.send(TerminalState::Exited);
            })
            .map_err(|e| err(codes::INTERNAL, e.to_string()))?;

        Ok(info)
    }

    fn push_state(&self, info: &TerminalInfo) {
        let _ = self.notify.send(RpcNotification::new(notifications::TERMINAL_STATE, info));
    }
}

/// `terminals.open`: resolves the thread's cwd / resume command, then spawns.
pub async fn open(state: &State, p: TerminalOpenParams) -> Result<TerminalInfo, RpcError> {
    let thread = match &p.thread_id {
        Some(id) => {
            let lookup = id.clone();
            let found = state
                .with_store(move |s| s.get_thread(&lookup))
                .await
                .map_err(|e| err(codes::INTERNAL, format!("{e:#}")))?;
            Some(found.ok_or_else(|| err(codes::NOT_FOUND, format!("no thread {id}")))?)
        }
        None => None,
    };
    let launch = match p.kind {
        TerminalKind::Shell => {
            let cwd = p
                .cwd
                .clone()
                .or_else(|| thread.as_ref().and_then(|t| t.cwd.clone()))
                .filter(|d| Path::new(d).is_dir())
                .unwrap_or_else(|| emp_core::paths::home_dir().to_string_lossy().into_owned());
            let shell = login_shell();
            let title = format!("Shell · {}", emp_core::store::project_name(&cwd));
            Launch {
                kind: TerminalKind::Shell,
                title,
                thread_id: p.thread_id.clone(),
                spec: SpawnSpec {
                    display: format!("{shell} -l"),
                    program: shell,
                    args: vec!["-l".into()],
                    cwd: Some(cwd),
                    env: Vec::new(),
                },
                cols: p.cols,
                rows: p.rows,
            }
        }
        TerminalKind::Resume => {
            let thread = thread.ok_or_else(|| err(codes::INVALID_PARAMS, "resume needs threadId"))?;
            if state.live.owner(&thread.id) != emp_protocol::SessionOwner::None {
                return Err(err(codes::BUSY, "EMP already holds this session; use takeover from the thread instead"));
            }
            crate::live::guard_elsewhere(state, &thread, p.force.unwrap_or(false)).await?;
            resume_launch(&thread, p.cols, p.rows)?
        }
        TerminalKind::Command => {
            let spec = p.spec.clone().ok_or_else(|| err(codes::INVALID_PARAMS, "command needs spec"))?;
            Launch {
                kind: TerminalKind::Command,
                title: spec.display.clone(),
                thread_id: p.thread_id.clone(),
                spec,
                cols: p.cols,
                rows: p.rows,
            }
        }
    };
    let terminals = state.terminals.clone();
    tokio::task::spawn_blocking(move || terminals.spawn(launch))
        .await
        .map_err(|e| err(codes::INTERNAL, e.to_string()))?
}

/// The harness TUI resuming `thread`'s session (same command as `threads.resumeCommand`).
pub fn resume_launch(thread: &emp_protocol::Thread, cols: u16, rows: u16) -> Result<Launch, RpcError> {
    if emp_import::orion::is_orion_only(thread) {
        return Err(err(codes::UNSUPPORTED, "Orion-only chats have no harness session to resume"));
    }
    let mut spec = emp_import::resume::resume_command(thread)
        .ok_or_else(|| err(codes::UNSUPPORTED, format!("{} sessions can't be resumed by id", thread.harness.as_str())))?;
    if spec.cwd.as_deref().is_some_and(|d| !Path::new(d).is_dir()) {
        spec.cwd = None;
    }
    Ok(Launch {
        kind: TerminalKind::Resume,
        title: format!("{} · {}", thread.harness.as_str(), emp_import::util::truncate(&thread.title, 60)),
        thread_id: Some(thread.id.clone()),
        spec,
        cols,
        rows,
    })
}

fn login_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| Path::new(s).exists()).unwrap_or_else(|| "/bin/zsh".into())
}

/// Builds the PTY command. Harness binaries are resolved with the same search path the doctor
/// uses, because a launchd-started daemon has a minimal `PATH`.
fn command_for(spec: &SpawnSpec) -> Result<CommandBuilder, RpcError> {
    let program = if spec.program.contains('/') {
        PathBuf::from(&spec.program)
    } else {
        doctor::which(&spec.program).ok_or_else(|| err(codes::HARNESS_ERROR, format!("{} is not installed", spec.program)))?
    };
    let mut cmd = CommandBuilder::new(program);
    cmd.args(&spec.args);
    let cwd = spec.cwd.clone().unwrap_or_else(|| emp_core::paths::home_dir().to_string_lossy().into_owned());
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "EMP");
    if let Ok(path) = std::env::join_paths(doctor::search_dirs()) {
        cmd.env("PATH", path);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    Ok(cmd)
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: std_mpsc::Sender<Vec<u8>>) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

struct Flusher {
    id: String,
    term: Arc<Term>,
    notify: broadcast::Sender<RpcNotification>,
    log: Option<fs::File>,
    logged: u64,
}

impl Flusher {
    fn run(mut self, rx: std_mpsc::Receiver<Vec<u8>>) {
        while let Ok(first) = rx.recv() {
            let mut batch = first;
            let deadline = std::time::Instant::now() + FLUSH_WINDOW;
            while batch.len() < FLUSH_BYTES {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match rx.recv_timeout(left) {
                    Ok(more) => batch.extend_from_slice(&more),
                    Err(_) => break,
                }
            }
            self.flush(batch);
        }
    }

    fn flush(&mut self, batch: Vec<u8>) {
        self.record(&batch);
        let data = B64.encode(&batch);
        // Push under the ring lock so `attach` never sees a seq whose push is still pending
        // out of order.
        let mut ring = self.term.ring.lock().unwrap_or_else(|e| e.into_inner());
        let seq = ring.push(batch);
        let _ = self.notify.send(RpcNotification::new(
            notifications::TERMINAL_OUTPUT,
            TerminalOutput { terminal_id: self.id.clone(), data, seq },
        ));
    }

    fn record(&mut self, bytes: &[u8]) {
        let Some(log) = self.log.as_mut() else { return };
        if self.logged >= LOG_BYTES {
            return;
        }
        let room = (LOG_BYTES - self.logged) as usize;
        let take = bytes.len().min(room);
        if log.write_all(&bytes[..take]).is_err() {
            self.log = None;
            return;
        }
        self.logged += take as u64;
        if self.logged >= LOG_BYTES {
            let _ = log.write_all(b"\r\n[EMP: recording stopped at 32 MiB]\r\n");
        }
    }
}

fn write_sidecar(dir: &Path, info: &TerminalInfo) {
    if let Ok(json) = serde_json::to_vec_pretty(info) {
        let _ = fs::write(dir.join(format!("{}.json", info.id)), json);
    }
}

fn clamp(n: u16) -> u16 {
    n.clamp(2, 1000)
}

fn signal_group(pid: u32, sig: libc::c_int) {
    // The child is a session leader (portable-pty calls setsid), so its pgid is its pid.
    unsafe {
        libc::killpg(pid as libc::pid_t, sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminals() -> (tempfile::TempDir, Terminals, broadcast::Receiver<RpcNotification>) {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = broadcast::channel(4096);
        let t = Terminals::new(dir.path(), tx);
        (dir, t, rx)
    }

    fn command(program: &str, args: &[&str], cwd: Option<&str>) -> Launch {
        Launch {
            kind: TerminalKind::Command,
            title: program.into(),
            thread_id: None,
            spec: SpawnSpec {
                program: program.into(),
                args: args.iter().map(|s| s.to_string()).collect(),
                cwd: cwd.map(Into::into),
                env: Vec::new(),
                display: program.into(),
            },
            cols: 80,
            rows: 24,
        }
    }

    async fn wait_exit(t: &Terminals, id: &str) {
        let mut w = t.exit_watch(id).unwrap();
        tokio::time::timeout(Duration::from_secs(10), w.wait_for(|s| *s == TerminalState::Exited))
            .await
            .expect("terminal exits")
            .unwrap();
    }

    fn output(rx: &mut broadcast::Receiver<RpcNotification>, id: &str) -> String {
        let mut out = Vec::new();
        while let Ok(n) = rx.try_recv() {
            if n.method == notifications::TERMINAL_OUTPUT && n.params["terminalId"] == id {
                out.extend(B64.decode(n.params["data"].as_str().unwrap()).unwrap());
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn command_output_and_exit_code() {
        let (dir, t, mut rx) = terminals();
        let info = t.spawn(command("/bin/echo", &["hello"], None)).unwrap();
        wait_exit(&t, &info.id).await;
        assert!(output(&mut rx, &info.id).contains("hello"));
        let done = t.info(&info.id).unwrap();
        assert_eq!(done.state, TerminalState::Exited);
        assert_eq!(done.exit_code, Some(0));
        let log = fs::read_to_string(dir.path().join(format!("terminals/{}.log", info.id))).unwrap();
        assert!(log.contains("hello"));
        assert!(dir.path().join(format!("terminals/{}.json", info.id)).exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shell_runs_in_cwd_and_accepts_input() {
        let (dir, t, mut rx) = terminals();
        let cwd = dir.path().canonicalize().unwrap();
        let info = t.spawn(command("/bin/sh", &[], Some(cwd.to_str().unwrap()))).unwrap();
        t.write(&info.id, &B64.encode("echo \"pwd=$PWD\"; exit 3\n")).unwrap();
        wait_exit(&t, &info.id).await;
        let out = output(&mut rx, &info.id);
        assert!(out.contains(&format!("pwd={}", cwd.display())), "{out}");
        assert_eq!(t.info(&info.id).unwrap().exit_code, Some(3));
        assert!(t.write(&info.id, &B64.encode("x")).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn kill_ends_a_running_process_and_then_forgets_it() {
        let (_dir, t, _rx) = terminals();
        let info = t.spawn(command("/bin/sleep", &["100"], None)).unwrap();
        t.resize(&info.id, 120, 40).unwrap();
        assert_eq!(t.info(&info.id).unwrap().cols, 120);
        t.kill(&info.id).unwrap();
        wait_exit(&t, &info.id).await;
        assert_eq!(t.list().len(), 1);
        t.kill(&info.id).unwrap();
        assert!(t.list().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_snapshot_matches_pushed_output() {
        let (_dir, t, mut rx) = terminals();
        let info = t.spawn(command("/bin/sh", &["-c", "for i in 1 2 3 4 5; do echo line$i; sleep 0.02; done"], None)).unwrap();
        wait_exit(&t, &info.id).await;
        let snap = t.attach(&info.id).unwrap();
        let replay = String::from_utf8(B64.decode(&snap.scrollback).unwrap()).unwrap();
        assert_eq!(replay, output(&mut rx, &info.id));
        assert!(snap.seq >= 1);
        assert!(replay.contains("line5"));
    }

    #[test]
    fn ring_caps_bytes_and_keeps_seq() {
        let mut ring = Ring::default();
        for _ in 0..5 {
            ring.push(vec![b'a'; 400 * 1024]);
        }
        assert_eq!(ring.seq, 5);
        assert!(ring.bytes <= RING_BYTES);
        assert_eq!(ring.contents().len(), ring.bytes);
        ring.push(vec![b'b'; 3 * RING_BYTES]);
        assert_eq!(ring.contents().len(), RING_BYTES);
    }
}
