//! Live threads: which process owns each harness session, and the chat drivers' state.
//!
//! A thread's session has at most one owner (FOUNDATION §4, "single-owner handoff"):
//! - `Driver`: EMP's structured driver (chat view). Started on demand by `threads.send`.
//! - `Terminal`: the harness TUI in an EMP terminal (`threads.takeover`), until it exits or
//!   `threads.release`.
//!
//! Each driver's events are pumped into `thread.event` pushes. Steer/follow-up go to the
//! harness when it supports them natively, otherwise into EMP's queue, delivered when the turn
//! ends. After every turn the thread's source is re-imported, so title / counts / transcript
//! stay current, and clients get `TranscriptChanged`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use emp_protocol::{
    Harness, ImportSourceKind, LiveThread, PartialKind, RpcError, RpcNotification, SendMode, SessionOwner,
    TerminalInfo, TerminalState, Thread, ThreadEvent, ThreadSendParams, ThreadStartParams, ThreadStatus,
    ThreadTakeoverParams, TranscriptItem, codes, notifications,
};
use tokio::sync::{mpsc, watch};

use crate::drivers::{self, Caps, DriverCmd, DriverEvent, DriverHandle, DriverSpec, McpGrant, Msg};
use crate::state::State;
use crate::{importing, terminals};

/// Most drivers running at once.
const MAX_DRIVERS: usize = 8;
/// Idle drivers are shut down after this long without activity.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How often EMP looks for Codex threads working without it.
const CODEX_WATCH_EVERY: Duration = Duration::from_secs(20);
/// At most one `Partial` push per thread per this interval.
const PARTIAL_INTERVAL: Duration = Duration::from_millis(50);
/// A session file written this recently by something else is treated as in use.
const ACTIVE_ELSEWHERE: Duration = Duration::from_secs(30);
/// How long EMP's own writes keep the guard quiet after it last owned a session.
const OWN_WRITES: Duration = Duration::from_secs(120);

fn err(code: i32, message: impl Into<String>) -> RpcError {
    RpcError { code, message: message.into(), data: None }
}

fn internal(e: anyhow::Error) -> RpcError {
    err(codes::INTERNAL, format!("{e:#}"))
}

#[derive(Default)]
pub struct Live {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Last time EMP owned each thread's session (driver or takeover terminal).
    touched: Mutex<HashMap<String, Instant>>,
    /// MCP keys of coordinator driver processes, each tied to its session.
    grants: Mutex<HashMap<String, Weak<Session>>>,
}

struct Session {
    /// Changes once, when `threads.start` learns the real thread id.
    thread_id: Mutex<String>,
    source: ImportSourceKind,
    inner: Mutex<Inner>,
    /// `import_key` once the harness session exists.
    ready: watch::Sender<Option<String>>,
}

struct Inner {
    live: LiveThread,
    driver: Option<DriverHandle>,
    /// Flips to true when the current driver's process is gone.
    driver_exit: Option<watch::Receiver<bool>>,
    /// An idle driver is retiring; new messages must wait for its replacement.
    driver_stopping: bool,
    /// Messages EMP holds until the turn ends (harness has no native queue for them).
    queue: VecDeque<Msg>,
    /// The harness's own queue, as last reported.
    harness_queue: Vec<String>,
    running: bool,
    /// Turns finished since the driver started.
    turns_ended: u32,
    /// Most recent System item (errors), for reporting a failed `threads.start`.
    last_system: Option<String>,
    last_activity: Instant,
    last_partial: Option<Instant>,
    /// Latest context size the driver reported, if it reports one.
    context: Option<emp_protocol::ContextUsage>,
    /// How the current (or last) turn went.
    turn: TurnOutcome,
    /// The model EMP switched this driver process to (the owner's pick).
    model: Option<String>,
}

/// How a turn went, for a schedule following its run.
#[derive(Debug, Clone, Default)]
pub struct TurnOutcome {
    /// Why it failed (an API error, a refused prompt).
    pub error: Option<String>,
    /// The owner stopped it.
    pub stopped: bool,
    /// The agent's last message.
    pub reply: Option<String>,
    /// When it ended, on [`turn_clock`]'s scale; 0 while it runs.
    pub ended: u64,
}

static TURN_CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Counts turn endings across all threads: a turn whose `ended` is past a reading taken
/// earlier finished after it.
pub fn turn_clock() -> u64 {
    TURN_CLOCK.load(std::sync::atomic::Ordering::SeqCst)
}

impl Session {
    fn new(thread_id: String, source: ImportSourceKind) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                live: LiveThread {
                    thread_id: thread_id.clone(),
                    status: ThreadStatus::Idle,
                    owner: SessionOwner::None,
                    terminal_id: None,
                    queued: Vec::new(),
                    error: None,
                    questions: Vec::new(),
                    goal: None,
                },
                driver: None,
                driver_exit: None,
                driver_stopping: false,
                queue: VecDeque::new(),
                harness_queue: Vec::new(),
                running: false,
                turns_ended: 0,
                last_system: None,
                last_activity: Instant::now(),
                last_partial: None,
                context: None,
                turn: TurnOutcome::default(),
                model: None,
            }),
            thread_id: Mutex::new(thread_id),
            source,
            ready: watch::channel(None).0,
        })
    }

    fn id(&self) -> String {
        self.thread_id.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current LiveThread with the queue view filled in.
    fn snapshot(&self) -> LiveThread {
        let inner = self.lock();
        let mut live = inner.live.clone();
        live.thread_id = self.id();
        live.queued = inner.harness_queue.iter().cloned().chain(inner.queue.iter().map(|m| m.text.clone())).collect();
        live
    }
}

fn emit(state: &State, ev: ThreadEvent) {
    state.notify(RpcNotification::new(notifications::THREAD_EVENT, ev));
}

fn emit_state(state: &State, s: &Session) {
    emit(state, ThreadEvent::State { live: s.snapshot() });
}

fn source_for(h: Harness) -> Option<ImportSourceKind> {
    Some(match h {
        Harness::Pi => ImportSourceKind::Pi,
        Harness::Claude => ImportSourceKind::Claude,
        Harness::Hermes => ImportSourceKind::Hermes,
        Harness::Codex => ImportSourceKind::Codex,
        Harness::Antigravity => ImportSourceKind::Antigravity,
        _ => return None,
    })
}

impl Live {
    fn get(&self, thread_id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(thread_id).cloned()
    }

    fn get_or_create(&self, thread_id: &str, source: ImportSourceKind) -> Arc<Session> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(thread_id.to_string())
            .or_insert_with(|| Session::new(thread_id.to_string(), source))
            .clone()
    }

    fn remove(&self, thread_id: &str) {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(thread_id);
    }

    fn touch(&self, thread_id: &str) {
        self.touched.lock().unwrap_or_else(|e| e.into_inner()).insert(thread_id.to_string(), Instant::now());
    }

    fn recently_owned(&self, thread_id: &str) -> bool {
        self.touched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(thread_id)
            .is_some_and(|t| t.elapsed() < OWN_WRITES)
    }

    fn driver_count(&self) -> usize {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).values().filter(|s| s.lock().driver.is_some()).count()
    }

    /// The thread's status, its error, and how its latest turn went.
    pub fn turn_state(&self, thread_id: &str) -> Option<(ThreadStatus, Option<String>, TurnOutcome)> {
        self.get(thread_id).map(|s| {
            let inner = s.lock();
            (inner.live.status, inner.live.error.clone(), inner.turn.clone())
        })
    }

    /// Every session a driver or takeover terminal holds, plus failed ones still showing an error.
    pub fn list(&self) -> Vec<LiveThread> {
        let sessions: Vec<Arc<Session>> = self.sessions.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        sessions
            .iter()
            .filter(|s| !s.id().starts_with("pending:"))
            .map(|s| s.snapshot())
            .filter(|l| l.owner != SessionOwner::None || l.status == ThreadStatus::Failed)
            .collect()
    }

    /// A fresh `emp` MCP key for the next driver process of `session`; its older keys stop
    /// working, as do keys of sessions that are gone.
    fn grant(&self, session: &Arc<Session>) -> String {
        let token = hex::encode(rand::random::<[u8; 24]>());
        let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        grants.retain(|_, w| w.upgrade().is_some_and(|s| !Arc::ptr_eq(&s, session)));
        grants.insert(token.clone(), Arc::downgrade(session));
        token
    }

    /// The thread whose coordinator process holds this MCP key (`pending:…` during its first
    /// turn, until the session exists on disk).
    pub fn caller(&self, token: &str) -> Option<String> {
        self.grants.lock().unwrap_or_else(|e| e.into_inner()).get(token).and_then(Weak::upgrade).map(|s| s.id())
    }

    /// Who owns `thread_id`'s session right now.
    pub fn owner(&self, thread_id: &str) -> SessionOwner {
        self.get(thread_id).map_or(SessionOwner::None, |s| s.lock().live.owner)
    }

    /// Shuts every driver down (daemon exit): close stdin, then kill what's left after 2 s.
    pub async fn shutdown(&self) {
        let sessions: Vec<Arc<Session>> = self.sessions.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        let mut pids = Vec::new();
        let mut exits = Vec::new();
        for s in &sessions {
            let inner = s.lock();
            if let Some(d) = &inner.driver {
                let _ = d.cmds.send(DriverCmd::Shutdown);
                pids.extend(d.pid);
                exits.extend(inner.driver_exit.clone());
            }
        }
        let all = async {
            for mut e in exits {
                let _ = e.wait_for(|x| *x).await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), all).await;
        for pid in pids {
            drivers::kill_group(pid);
        }
    }

    /// Shuts down drivers idle for longer than [`IDLE_TIMEOUT`]. Runs every minute.
    pub fn spawn_reaper(state: &State) {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let sessions: Vec<Arc<Session>> =
                    state.live.sessions.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
                for s in sessions {
                    let mut inner = s.lock();
                    if let Some(d) = &inner.driver
                        && !inner.running
                        && !inner.driver_stopping
                        && inner.last_activity.elapsed() > IDLE_TIMEOUT
                    {
                        tracing::info!(thread = %s.id(), "shutting down idle driver");
                        let _ = d.cmds.send(DriverCmd::Shutdown);
                        inner.driver_stopping = true;
                    }
                }
            }
        });
    }
}

impl Live {
    /// Codex threads Codex's shared app-server is working on (a goal, a turn from its TUI) get a
    /// driver, so they stream into EMP without the owner sending anything: after an empd
    /// restart, or when the work began in a Terminal. Every 20 s.
    pub fn spawn_codex_watcher(state: &State) {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(CODEX_WATCH_EVERY);
            loop {
                tick.tick().await;
                let running = match drivers::codex::running_threads().await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::debug!("codex watch: {e:#}");
                        continue;
                    }
                };
                for id in running {
                    if let Err(e) = watch_codex(&state, &id).await {
                        tracing::warn!(codex = %id, "couldn't attach to a running codex thread: {}", e.message);
                    }
                }
            }
        });
    }
}

/// Attaches a driver to the EMP thread of Codex thread `codex_id` unless something holds it
/// already, it failed before, or it isn't a top-level thread of the default Codex home.
async fn watch_codex(state: &State, codex_id: &str) -> Result<bool, RpcError> {
    let key = format!("codex:{codex_id}");
    let Some(thread) = state.with_store(move |s| s.thread_by_import_key(&key)).await.map_err(internal)? else {
        return Ok(false);
    };
    if thread.harness != Harness::Codex || thread.session.home.is_some() || thread.parent_id.is_some() {
        return Ok(false);
    }
    if let Some(s) = state.live.get(&thread.id) {
        let inner = s.lock();
        if inner.live.owner != SessionOwner::None || inner.live.status == ThreadStatus::Failed {
            return Ok(false);
        }
    }
    // Leave room for the owner's own chats.
    if state.live.driver_count() + 2 >= MAX_DRIVERS {
        return Ok(false);
    }
    ensure_driver(state, &thread, ImportSourceKind::Codex, false).await?;
    tracing::info!(thread = %thread.id, codex = %codex_id, "watching a codex thread that's running");
    Ok(true)
}

/// `threads.watch`
pub async fn watch(state: &State, thread_id: &str) -> Result<bool, RpcError> {
    let thread = load_thread(state, thread_id).await?;
    if thread.harness != Harness::Codex || state.live.owner(thread_id) != SessionOwner::None {
        return Ok(false);
    }
    let running = drivers::codex::running_threads().await.map_err(internal)?;
    if !running.contains(&thread.session.session_id) {
        return Ok(false);
    }
    watch_codex(state, &thread.session.session_id).await
}

// ---- driver lifecycle ----------------------------------------------------------------------

/// Starts a driver for `session` and the task that pumps its events.
fn attach_driver(state: &State, session: &Arc<Session>, spec: DriverSpec) -> Result<Caps, RpcError> {
    if state.live.driver_count() >= MAX_DRIVERS {
        return Err(err(codes::BUSY, format!("at most {MAX_DRIVERS} agents can run at once; stop one first")));
    }
    let harness = spec.harness;
    let (handle, rx) = drivers::spawn(spec).map_err(|e| err(codes::HARNESS_ERROR, format!("{e:#}")))?;
    let caps = handle.caps;
    let (exit_tx, exit_rx) = watch::channel(false);
    {
        let mut inner = session.lock();
        inner.driver = Some(handle);
        inner.driver_exit = Some(exit_rx);
        inner.driver_stopping = false;
        inner.model = None;
        inner.live.owner = SessionOwner::Driver;
        inner.live.error = None;
        inner.live.status = ThreadStatus::Idle;
        inner.running = false;
        inner.last_activity = Instant::now();
    }
    tracing::info!(thread = %session.id(), harness = harness.as_str(), "driver started");
    tokio::spawn(pump(state.clone(), session.clone(), rx, exit_tx));
    Ok(caps)
}

/// Delivers `msg` as a new turn. The caller holds no lock.
fn start_turn(state: &State, session: &Session, msg: Msg) -> Result<(), RpcError> {
    let text = msg.text.clone();
    let echo = {
        let mut inner = session.lock();
        if inner.driver_stopping {
            return Err(err(codes::BUSY, "the idle driver is closing; try again shortly"));
        }
        let d = inner.driver.as_ref().ok_or_else(|| err(codes::HARNESS_ERROR, "the driver stopped; try again"))?;
        let echo = !d.caps.echoes_user;
        d.cmds.send(DriverCmd::Prompt(msg)).map_err(|_| err(codes::HARNESS_ERROR, "the driver stopped; try again"))?;
        inner.running = true;
        inner.live.status = ThreadStatus::Running;
        inner.last_activity = Instant::now();
        inner.turn = TurnOutcome::default();
        echo
    };
    if echo {
        emit(state, ThreadEvent::Item {
            thread_id: session.id(),
            item: TranscriptItem::User { text, ts: Some(emp_core::now_ms()) },
        });
    }
    Ok(())
}

async fn pump(state: State, session: Arc<Session>, mut rx: mpsc::UnboundedReceiver<DriverEvent>, exit: watch::Sender<bool>) {
    while let Some(ev) = rx.recv().await {
        let id = session.id();
        match ev {
            DriverEvent::Ready { import_key } => {
                session.ready.send_replace(Some(import_key));
            }
            DriverEvent::TurnStarted => {
                {
                    let mut inner = session.lock();
                    // A turn EMP didn't start (a Codex goal turn): `start_turn` resets the others.
                    if !inner.running {
                        inner.turn = TurnOutcome::default();
                    }
                    inner.running = true;
                    inner.live.status = ThreadStatus::Running;
                    inner.last_activity = Instant::now();
                }
                emit_state(&state, &session);
            }
            DriverEvent::Item(item) => {
                {
                    let mut inner = session.lock();
                    inner.last_activity = Instant::now();
                    match &item {
                        TranscriptItem::System { text, .. } => inner.last_system = Some(text.clone()),
                        // A reply after an error means the harness retried and got through.
                        TranscriptItem::Assistant { text, .. } => {
                            inner.turn.reply = Some(text.clone());
                            inner.turn.error = None;
                        }
                        _ => {}
                    }
                }
                emit(&state, ThreadEvent::Item { thread_id: id, item });
            }
            DriverEvent::Partial(kind, text) => {
                let due = {
                    let mut inner = session.lock();
                    let due = inner.last_partial.is_none_or(|t| t.elapsed() >= PARTIAL_INTERVAL);
                    if due {
                        inner.last_partial = Some(Instant::now());
                    }
                    due
                };
                if due {
                    emit(&state, ThreadEvent::Partial { thread_id: id, kind, text });
                }
            }
            DriverEvent::Context(c) => session.lock().context = Some(c),
            DriverEvent::TurnFailed(e) => session.lock().turn.error = Some(e),
            DriverEvent::Commands { key, commands } => state.commands.record(&key, commands),
            DriverEvent::Queue(q) => {
                session.lock().harness_queue = q;
                emit_state(&state, &session);
            }
            DriverEvent::Ask(q) => {
                {
                    let mut inner = session.lock();
                    inner.live.questions.retain(|x| x.id != q.id);
                    inner.live.questions.push(q);
                    inner.live.status = ThreadStatus::NeedsInput;
                    inner.last_activity = Instant::now();
                }
                emit_state(&state, &session);
            }
            DriverEvent::Unask(qid) => {
                {
                    let mut inner = session.lock();
                    inner.live.questions.retain(|q| q.id != qid);
                    if inner.live.questions.is_empty() && inner.live.status == ThreadStatus::NeedsInput {
                        inner.live.status = if inner.running { ThreadStatus::Running } else { ThreadStatus::Idle };
                    }
                }
                emit_state(&state, &session);
            }
            DriverEvent::Granted(grants) => {
                // Kept with the thread; a brand-new chat may still be waiting for its id.
                let (state, session) = (state.clone(), session.clone());
                tokio::spawn(async move {
                    for _ in 0..120 {
                        let id = session.id();
                        if !id.starts_with("pending:") {
                            if let Err(e) = state.with_store(move |s| s.add_thread_grants(&id, &grants)).await {
                                tracing::warn!("saving a chat's grants: {e:#}");
                            }
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                });
            }
            DriverEvent::Goal(goal) => {
                session.lock().live.goal = goal;
                emit_state(&state, &session);
            }
            DriverEvent::SessionChanged => refresh(&state, &id, session.source),
            DriverEvent::TurnEnded { aborted } => {
                let next = {
                    let mut inner = session.lock();
                    inner.running = false;
                    inner.turns_ended += 1;
                    inner.turn.ended = TURN_CLOCK.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    inner.live.status = ThreadStatus::Idle;
                    // pi resolves unanswered dialogs itself when the turn is aborted or times out.
                    inner.live.questions.retain(|q| q.id.starts_with("emp:"));
                    inner.last_activity = Instant::now();
                    inner.last_partial = None;
                    if aborted { None } else { inner.queue.pop_front() }
                };
                emit(&state, ThreadEvent::Partial { thread_id: id.clone(), kind: PartialKind::Assistant, text: String::new() });
                if let Some(text) = next
                    && let Err(e) = start_turn(&state, &session, text.clone())
                {
                    session.lock().queue.push_front(text);
                    emit(&state, ThreadEvent::Item {
                        thread_id: id.clone(),
                        item: TranscriptItem::System { text: e.message, ts: Some(emp_core::now_ms()) },
                    });
                }
                emit_state(&state, &session);
                refresh(&state, &id, session.source);
            }
            DriverEvent::Exited(error) => {
                if let Some(e) = &error {
                    tracing::warn!(thread = %id, "driver died: {e}");
                    let mut inner = session.lock();
                    inner.live.status = ThreadStatus::Failed;
                    inner.live.error = Some(e.clone());
                    drop(inner);
                    emit(&state, ThreadEvent::Item {
                        thread_id: id.clone(),
                        item: TranscriptItem::System { text: e.clone(), ts: Some(emp_core::now_ms()) },
                    });
                }
                break;
            }
        }
    }
    // The process is gone (or its event channel closed).
    let id = session.id();
    state.live.touch(&id);
    let owner = {
        let mut inner = session.lock();
        inner.driver = None;
        inner.driver_stopping = true;
        inner.running = false;
        inner.harness_queue.clear();
        inner.live.questions.clear();
        if inner.live.owner == SessionOwner::Driver {
            inner.live.owner = SessionOwner::None;
        }
        inner.live.owner
    };
    let failed = session.lock().live.status == ThreadStatus::Failed;
    if owner == SessionOwner::None && !failed {
        session.lock().live.status = ThreadStatus::Idle;
    }
    emit_state(&state, &session);
    if owner == SessionOwner::None && session.lock().queue.is_empty() && !failed {
        state.live.remove(&id);
    }
    refresh(&state, &id, session.source);
    {
        let mut inner = session.lock();
        inner.driver_exit = None;
        inner.driver_stopping = false;
    }
    let _ = exit.send(true);
}

/// Re-imports the thread's source in the background, then tells clients to refetch.
fn refresh(state: &State, thread_id: &str, source: ImportSourceKind) {
    if thread_id.starts_with("pending:") {
        return;
    }
    let state = state.clone();
    let thread_id = thread_id.to_string();
    tokio::spawn(async move {
        if let Err(e) = importing::run(&state, &[source]).await {
            tracing::warn!("re-import after turn failed: {e:#}");
        }
        emit(&state, ThreadEvent::TranscriptChanged { thread_id });
    });
}

// ---- guards --------------------------------------------------------------------------------

/// Refuses while the session is open outside EMP: a harness process names it on its command
/// line (`elsewhere.rs`), or its file was written very recently by something else. Two writers
/// corrupt a session. `force` skips only the file-age check; a holder must be closed first.
pub async fn guard_elsewhere(state: &State, thread: &Thread, force: bool) -> Result<(), RpcError> {
    // Codex settles it itself: EMP joins the app-server its TUI uses (one live thread, many
    // clients), and Codex refuses a second writer otherwise (`drivers/codex.rs` says so).
    if state.live.recently_owned(&thread.id) || thread.harness == Harness::Codex {
        return Ok(());
    }
    let session = thread.session.clone();
    let holders = tokio::task::spawn_blocking(move || crate::elsewhere::holders(&session)).await.unwrap_or_default();
    if let Some(h) = holders.first() {
        let place = h.app.as_deref().map_or("another terminal".to_string(), |a| format!("{a} ({})", h.command));
        return Err(RpcError {
            code: codes::BUSY,
            message: format!("This {} session is open in {place}. Close it there, or close it from here and continue.", harness_name(thread.harness)),
            data: Some(serde_json::json!({ "holders": holders })),
        });
    }
    if force {
        return Ok(());
    }
    let Some(path) = thread.session.path.as_deref() else { return Ok(()) };
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else { return Ok(()) };
    let age = modified.elapsed().unwrap_or_default();
    if age < ACTIVE_ELSEWHERE {
        return Err(err(
            codes::BUSY,
            format!(
                "This {} session was written {}s ago by another process; it's probably open in another terminal. \
                 Close it there first, or retry with force.",
                thread.harness.as_str(),
                age.as_secs()
            ),
        ));
    }
    Ok(())
}

fn harness_name(h: Harness) -> &'static str {
    match h {
        Harness::Pi => "pi",
        Harness::Claude => "Claude Code",
        Harness::Codex => "Codex",
        Harness::Hermes => "Hermes",
        Harness::Antigravity => "Antigravity",
        Harness::Gemini => "Gemini",
        Harness::Cursor => "Cursor",
        Harness::Shell => "shell",
    }
}

/// `threads.holders`
pub async fn holders(state: &State, thread_id: &str) -> Result<Vec<emp_protocol::SessionHolder>, RpcError> {
    let thread = load_thread(state, thread_id).await?;
    if state.live.owner(thread_id) == SessionOwner::Driver {
        // EMP has it; anything else would have been refused.
        return Ok(Vec::new());
    }
    let session = thread.session.clone();
    Ok(tokio::task::spawn_blocking(move || crate::elsewhere::holders(&session)).await.unwrap_or_default())
}

/// `threads.closeElsewhere`: the owner asked EMP to close the session's other window.
pub async fn close_elsewhere(state: &State, thread_id: &str) -> Result<emp_protocol::CloseElsewhereResult, RpcError> {
    let thread = load_thread(state, thread_id).await?;
    let (closed, remaining) = crate::elsewhere::close(&thread.session).await;
    if closed > 0 && remaining.is_empty() {
        // The other window's last writes are not a reason to refuse the owner now.
        state.live.touch(&thread.id);
    }
    tracing::info!(thread = %thread_id, closed, remaining = remaining.len(), "closed the session's other window");
    Ok(emp_protocol::CloseElsewhereResult { closed, remaining })
}

/// `threads.goal`: the live goal while a driver runs, else Codex's stored one.
pub async fn goal(state: &State, thread_id: &str) -> Result<Option<emp_protocol::ThreadGoal>, RpcError> {
    if let Some(s) = state.live.get(thread_id) {
        let inner = s.lock();
        if inner.driver.is_some() {
            return Ok(inner.live.goal.clone());
        }
    }
    let thread = load_thread(state, thread_id).await?;
    if thread.harness != Harness::Codex {
        return Ok(None);
    }
    let home = thread.session.home.clone().map(PathBuf::from);
    let id = thread.session.session_id.clone();
    tokio::task::spawn_blocking(move || emp_import::codex::goal(home.as_deref(), &id))
        .await
        .map_err(|e| internal(e.into()))?
        .map_err(internal)
}

fn resolve_cwd(candidates: &[Option<&str>]) -> PathBuf {
    candidates
        .iter()
        .flatten()
        .map(|c| emp_import::util::expand_home(c))
        .find(|p| p.is_dir())
        .unwrap_or_else(emp_core::paths::home_dir)
}

async fn load_thread(state: &State, id: &str) -> Result<Thread, RpcError> {
    let lookup = id.to_string();
    state
        .with_store(move |s| s.get_thread(&lookup))
        .await
        .map_err(internal)?
        .ok_or_else(|| err(codes::NOT_FOUND, format!("no thread {id}")))
}

// ---- methods -------------------------------------------------------------------------------

/// `threads.send`
pub async fn send(state: &State, p: ThreadSendParams) -> Result<LiveThread, RpcError> {
    let mut msg = message(state, &p.text, p.attachments.as_deref())?;
    let thread = load_thread(state, &p.thread_id).await?;
    if emp_import::orion::is_orion_only(&thread) {
        return Err(err(codes::UNSUPPORTED, "Orion-only chats have no harness session to continue"));
    }
    let source = source_for(thread.harness).filter(|_| drivers::supports(thread.harness)).ok_or_else(|| {
        err(codes::UNSUPPORTED, format!("chatting with {} threads arrives in a later phase; use the terminal", thread.harness.as_str()))
    })?;
    let session = ensure_driver(state, &thread, source, p.force.unwrap_or(false)).await?;
    switch_model(state, &session, &thread.id, p.model.as_deref()).await?;
    if is_coordinator_thread(state, &thread).await {
        msg.text = with_context(state, &msg.text, p.context.as_deref()).await;
    }

    let mode = p.mode.unwrap_or(SendMode::Prompt);
    let (running, caps) = {
        let inner = session.lock();
        (inner.running, inner.driver.as_ref().map(|d| d.caps))
    };
    let caps = caps.ok_or_else(|| err(codes::HARNESS_ERROR, "the driver stopped; try again"))?;
    if !running {
        start_turn(state, &session, msg)?;
    } else {
        let text = msg.text.clone();
        let native = match mode {
            SendMode::Steer => caps.native_steer.then_some(DriverCmd::Steer(msg.clone())),
            SendMode::Prompt | SendMode::FollowUp => caps.native_follow_up.then_some(DriverCmd::FollowUp(msg.clone())),
        };
        let mut inner = session.lock();
        match (native, &inner.driver) {
            (Some(cmd), Some(d)) => {
                let _ = d.cmds.send(cmd);
                if !caps.echoes_user {
                    drop(inner);
                    emit(state, ThreadEvent::Item {
                        thread_id: session.id(),
                        item: TranscriptItem::User { text, ts: Some(emp_core::now_ms()) },
                    });
                }
            }
            _ => inner.queue.push_back(msg),
        }
    }
    emit_state(state, &session);
    Ok(session.snapshot())
}

/// Puts the chat on the owner's model before a message: a new pick is saved (re-import keeps
/// it), and the saved one is applied once to each driver process, since some harnesses (Claude)
/// resume on their default model.
async fn switch_model(state: &State, session: &Arc<Session>, thread_id: &str, pick: Option<&str>) -> Result<(), RpcError> {
    let id = thread_id.to_string();
    let pick = match pick.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => {
            reject_copilot(m)?;
            let m = m.to_string();
            let saved = m.clone();
            state.with_store(move |s| s.set_thread_model_pick(&id, &saved)).await.map_err(internal)?;
            Some(m)
        }
        None => state.with_store(move |s| s.thread_model_pick(&id)).await.map_err(internal)?,
    };
    let Some(model) = pick else { return Ok(()) };
    reject_copilot(&model)?;
    let mut inner = session.lock();
    if inner.model.as_deref() != Some(model.as_str())
        && let Some(d) = &inner.driver
    {
        let _ = d.cmds.send(DriverCmd::SetModel(model.clone()));
        inner.model = Some(model);
    }
    Ok(())
}

fn reject_copilot(model: &str) -> Result<(), RpcError> {
    if crate::models::is_copilot_model(model) {
        return Err(err(codes::INVALID_PARAMS, "Copilot is disabled in EMP. Choose another model provider."));
    }
    Ok(())
}

/// The thread's session with a chat driver attached: starts one (resuming the session) when no
/// process holds it. Fails while it's open in a terminal.
async fn ensure_driver(state: &State, thread: &Thread, source: ImportSourceKind, force: bool) -> Result<Arc<Session>, RpcError> {
    let session = loop {
        let session = state.live.get_or_create(&thread.id, source);
        let retiring = {
            let inner = session.lock();
            inner.driver_stopping.then(|| inner.driver_exit.clone()).flatten()
        };
        let Some(mut exit) = retiring else { break session };
        if !matches!(tokio::time::timeout(Duration::from_secs(6), exit.wait_for(|done| *done)).await, Ok(Ok(_))) {
            return Err(err(codes::BUSY, "the idle driver is still closing; try again shortly"));
        }
        // The pump may have removed the old session. Look it up again before attaching.
    };
    let owner = session.lock().live.owner;
    match owner {
        SessionOwner::Terminal => {
            return Err(err(codes::BUSY, "this session is open in a terminal; return it to chat first"));
        }
        SessionOwner::None => {
            guard_elsewhere(state, thread, force).await?;
            let agent = match &thread.agent_id {
                Some(a) => {
                    let a = a.clone();
                    state.with_store(move |s| s.get_agent(&a)).await.map_err(internal)?
                }
                None => None,
            };
            let cwd = resolve_cwd(&[thread.cwd.as_deref()]);
            let mcp = match &agent {
                Some(a) if a.coordinator => coordinator_grant(state, &session, thread.harness, &cwd),
                _ => None,
            };
            let tid = thread.id.clone();
            let grants = state.with_store(move |s| s.thread_grants(&tid)).await.map_err(internal)?;
            let spec = DriverSpec {
                harness: thread.harness,
                cwd,
                resume: Some(thread.session.clone()),
                model: None,
                effort: None,
                instructions: None,
                profile: thread.session.profile.clone(),
                autonomy: agent.as_ref().map_or(emp_protocol::FULL_ACCESS, |a| a.autonomy),
                compact_at: state.compact_at(agent.as_ref().map(|a| a.id.clone())),
                mcp,
                grants,
            };
            if let Err(e) = attach_driver(state, &session, spec) {
                state.live.remove(&thread.id);
                return Err(e);
            }
            state.live.touch(&thread.id);
        }
        SessionOwner::Driver => {}
    }
    Ok(session)
}

/// The `emp` MCP server for a coordinator process: pi reads it from `.pi/mcp.json` in the
/// coordinator's own folder (rewritten here, so the port is current), Claude takes it as a flag.
/// Other harnesses get no tools yet.
fn coordinator_grant(state: &State, session: &Arc<Session>, harness: Harness, cwd: &std::path::Path) -> Option<McpGrant> {
    let port = state.phone.port();
    if port == 0 {
        return None;
    }
    let url = format!("http://127.0.0.1:{port}/mcp");
    match harness {
        Harness::Pi => {
            if cwd != state.coordinator_dir() {
                tracing::warn!("the coordinator runs outside its folder, so pi gets no emp tools");
                return None;
            }
            let dir = cwd.join(".pi");
            let body = serde_json::to_vec_pretty(&drivers::pi::mcp_config(&url)).ok()?;
            if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join("mcp.json"), body)) {
                tracing::warn!("couldn't write the coordinator's mcp.json: {e}");
                return None;
            }
        }
        Harness::Claude => {}
        _ => return None,
    }
    Some(McpGrant { url, token: state.live.grant(session) })
}

async fn is_coordinator_thread(state: &State, thread: &Thread) -> bool {
    let Some(aid) = thread.agent_id.clone() else { return false };
    state.with_store(move |s| s.coordinator_id()).await.ok().flatten().is_some_and(|c| c == aid)
}

/// The owner's message plus what EMP knows that they don't type: the page they're on and
/// what's running. It goes last so the first line still makes the thread's title.
async fn with_context(state: &State, text: &str, page: Option<&str>) -> String {
    let busy: Vec<(String, ThreadStatus)> = state
        .live
        .list()
        .into_iter()
        .filter(|l| matches!(l.status, ThreadStatus::Running | ThreadStatus::NeedsInput))
        .map(|l| (l.thread_id, l.status))
        .collect();
    let rows = state
        .with_store(move |s| {
            let coordinator = s.coordinator_id()?;
            let mut out = Vec::new();
            for (id, status) in busy {
                if let Some(t) = s.get_thread(&id)?
                    && (t.agent_id.is_none() || t.agent_id != coordinator)
                {
                    let what = if status == ThreadStatus::NeedsInput { "needs the owner" } else { "running" };
                    out.push(format!("- \"{}\" ({}, {what}) [{}]", t.title, t.harness.as_str(), t.id));
                }
            }
            Ok(out)
        })
        .await
        .unwrap_or_default();
    let mut lines = Vec::new();
    if let Some(p) = page.map(str::trim).filter(|p| !p.is_empty()) {
        lines.push(format!("Owner is viewing: {}", p.chars().take(600).collect::<String>()));
    }
    if rows.is_empty() {
        lines.push("Running: nothing".into());
    } else {
        lines.push("Running:".into());
        lines.extend(rows.into_iter().take(12));
    }
    format!("{text}\n\n<emp-context>\n{}\n</emp-context>", lines.join("\n"))
}

/// The owner's text plus attached files; refuses an empty message.
fn message(state: &State, text: &str, attachments: Option<&[String]>) -> Result<Msg, RpcError> {
    let files = match attachments {
        Some(ids) if !ids.is_empty() => crate::attachments::resolve(&crate::attachments::root(&state.data_dir), ids)
            .map_err(|e| err(codes::INVALID_PARAMS, format!("{e:#}")))?,
        _ => Vec::new(),
    };
    let msg = crate::attachments::message(text, &files);
    if msg.text.is_empty() {
        return Err(err(codes::INVALID_PARAMS, "empty message"));
    }
    Ok(msg)
}

/// `threads.interrupt`: aborts the running turn and drops EMP's queued messages.
pub async fn interrupt(state: &State, thread_id: &str) -> Result<LiveThread, RpcError> {
    let session = state.live.get(thread_id).ok_or_else(|| err(codes::NOT_FOUND, "this thread isn't running"))?;
    {
        let mut inner = session.lock();
        inner.queue.clear();
        if let Some(d) = &inner.driver {
            let _ = d.cmds.send(DriverCmd::Interrupt);
            inner.turn.stopped = true;
        }
    }
    emit_state(state, &session);
    Ok(session.snapshot())
}

/// `threads.answer`: hands the owner's answer to the driver and drops the card.
pub async fn answer(state: &State, p: emp_protocol::ThreadAnswerParams) -> Result<LiveThread, RpcError> {
    let session = state.live.get(&p.thread_id).ok_or_else(|| err(codes::NOT_FOUND, "this thread isn't running any more"))?;
    {
        let mut inner = session.lock();
        let pos = inner
            .live
            .questions
            .iter()
            .position(|q| q.id == p.question_id)
            .ok_or_else(|| err(codes::NOT_FOUND, "that question was already answered"))?;
        inner.live.questions.remove(pos);
        if inner.live.questions.is_empty() && inner.live.status == ThreadStatus::NeedsInput {
            inner.live.status = if inner.running { ThreadStatus::Running } else { ThreadStatus::Idle };
        }
        let d = inner.driver.as_ref().ok_or_else(|| err(codes::HARNESS_ERROR, "the driver stopped; try again"))?;
        let _ = d.cmds.send(DriverCmd::Answer {
            id: p.question_id,
            value: p.value,
            confirmed: p.confirmed,
            cancelled: p.cancelled.unwrap_or(false),
        });
    }
    emit_state(state, &session);
    Ok(session.snapshot())
}

/// `threads.command`: a slash command EMP runs in the chat through the harness's API (pi's RPC
/// mode, Codex's app-server); the result arrives as a system line or a question card.
pub async fn command(state: &State, p: emp_protocol::ThreadCommandParams) -> Result<LiveThread, RpcError> {
    let text = p.text.trim();
    let Some(line) = text.strip_prefix('/').filter(|l| !l.is_empty() && !l.contains('\n')) else {
        return Err(err(codes::INVALID_PARAMS, "a slash command is one line starting with /"));
    };
    let (name, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let mut args = args.trim().to_string();
    let thread = load_thread(state, &p.thread_id).await?;
    if thread.harness == Harness::Claude && name == "add-dir" {
        let dir = granted_dir(&thread, &args)?;
        let (tid, grant) = (thread.id.clone(), emp_core::Grants { dirs: vec![dir.clone()], rules: Vec::new() });
        state.with_store(move |s| s.add_thread_grants(&tid, &grant)).await.map_err(internal)?;
        args = dir;
    }
    let source = match thread.harness {
        Harness::Pi => ImportSourceKind::Pi,
        Harness::Codex => ImportSourceKind::Codex,
        Harness::Claude if name == "add-dir" => ImportSourceKind::Claude,
        other => return Err(err(codes::UNSUPPORTED, format!("/{name} doesn't run in chat for {} threads", other.as_str()))),
    };
    let session = ensure_driver(state, &thread, source, false).await?;
    {
        let inner = session.lock();
        let d = inner.driver.as_ref().ok_or_else(|| err(codes::HARNESS_ERROR, "the driver stopped; try again"))?;
        let _ = d.cmds.send(DriverCmd::Command { name: name.to_string(), args: args.clone() });
    }
    // Echo the command, so the reply below it makes sense (`/add-dir` answers in full itself).
    if name != "add-dir" {
        emit(state, ThreadEvent::Item { thread_id: thread.id.clone(), item: TranscriptItem::System { text: format!("/{line}"), ts: Some(emp_core::now_ms()) } });
    }
    Ok(session.snapshot())
}

/// `/add-dir <path>`: a folder that exists, as an absolute path (`~` and paths relative to the
/// chat's folder work; quotes and a trailing slash are dropped).
fn granted_dir(thread: &Thread, arg: &str) -> Result<String, RpcError> {
    let raw = arg.trim().trim_matches(|c| c == '"' || c == '\'').replace("\\ ", " ");
    if raw.is_empty() {
        return Err(err(codes::INVALID_PARAMS, "/add-dir takes a folder, like /add-dir ~/Desktop/survivor games"));
    }
    let mut path = emp_import::util::expand_home(&raw);
    if path.is_relative() {
        path = resolve_cwd(&[thread.cwd.as_deref()]).join(path);
    }
    let path = path.canonicalize().map_err(|_| err(codes::INVALID_PARAMS, format!("there's no folder at {raw}")))?;
    if !path.is_dir() {
        return Err(err(codes::INVALID_PARAMS, format!("{} is a file; /add-dir takes a folder", path.display())));
    }
    Ok(path.to_string_lossy().into_owned())
}

/// The context size a running driver last reported for this thread, if any.
pub fn reported_context(state: &State, thread_id: &str) -> Option<emp_protocol::ContextUsage> {
    state.live.get(thread_id).and_then(|s| s.lock().context.clone())
}

/// `threads.start`: a new session with an agent, first message included.
pub async fn start(state: &State, p: ThreadStartParams) -> Result<Thread, RpcError> {
    let mut msg = message(state, &p.text, p.attachments.as_deref())?;
    let agent_id = p.agent_id.clone();
    let agent = state
        .with_store(move |s| s.get_agent(&agent_id))
        .await
        .map_err(internal)?
        .ok_or_else(|| err(codes::NOT_FOUND, format!("no agent {}", p.agent_id)))?;
    let source = source_for(agent.harness).filter(|_| drivers::supports(agent.harness)).ok_or_else(|| {
        err(codes::UNSUPPORTED, format!("{} agents can't be started from chat yet", agent.harness.as_str()))
    })?;
    let project_root = match &p.project_id {
        Some(pid) => {
            let pid = pid.clone();
            state
                .with_store(move |s| Ok(s.list_projects()?.into_iter().find(|x| x.id == pid).map(|x| x.root_path)))
                .await
                .map_err(internal)?
        }
        None => None,
    };
    // The coordinator always works from its own folder, where its instructions and MCP config are.
    let own_dir = agent.coordinator.then(|| state.coordinator_dir());
    let cwd = match &own_dir {
        Some(d) => d.clone(),
        None => resolve_cwd(&[p.cwd.as_deref(), agent.cwd.as_deref(), project_root.as_deref()]),
    };
    if agent.coordinator {
        msg.text = with_context(state, &msg.text, p.context.as_deref()).await;
    }

    if let Some(model) = p.model.as_deref().or(agent.model.as_deref()) {
        reject_copilot(model)?;
    }
    let pending = format!("pending:{}", ulid::Ulid::new());
    let session = state.live.get_or_create(&pending, source);
    let mcp = if agent.coordinator { coordinator_grant(state, &session, agent.harness, &cwd) } else { None };
    // Hermes takes ACP model ids (`provider:model`): an agent's model picked in EMP is one; an
    // imported one is the profile config's own default, which Hermes uses anyway.
    let model = match agent.harness {
        Harness::Hermes => p.model.clone().or_else(|| agent.model.clone().filter(|m| drivers::hermes::is_acp_model(m))),
        _ => p.model.clone().or_else(|| agent.model.clone()),
    };
    let spec = DriverSpec {
        harness: agent.harness,
        cwd,
        resume: None,
        model: model.clone(),
        effort: p.effort.clone().or_else(|| agent.effort.clone()),
        instructions: agent.instructions.clone(),
        profile: agent.harness_profile.clone(),
        autonomy: agent.autonomy,
        compact_at: state.compact_at(Some(agent.id.clone())),
        mcp,
        grants: Default::default(),
    };
    if let Err(e) = attach_driver(state, &session, spec) {
        state.live.remove(&pending);
        return Err(e);
    }
    start_turn(state, &session, msg)?;

    let fail = |state: &State, msg: String| {
        if let Some(d) = &session.lock().driver {
            let _ = d.cmds.send(DriverCmd::Shutdown);
        }
        state.live.remove(&pending);
        err(codes::HARNESS_ERROR, msg)
    };

    // 1. The harness creates its session.
    let mut ready = session.ready.subscribe();
    let exit = session.lock().driver_exit.clone();
    let key = tokio::select! {
        r = tokio::time::timeout(Duration::from_secs(90), ready.wait_for(|k| k.is_some())) => match r {
            Ok(Ok(k)) => k.clone().unwrap_or_default(),
            _ => return Err(fail(state, format!("{} didn't open a session within 90 s", agent.harness.as_str()))),
        },
        _ = async { if let Some(mut e) = exit { let _ = e.wait_for(|x| *x).await; } else { std::future::pending::<()>().await } } => {
            return Err(fail(state, format!("{} exited before opening a session; see Import & Doctor", agent.harness.as_str())));
        }
    };

    // 2. Its session file / row appears; import it so the thread exists in EMP.
    let deadline = Instant::now() + Duration::from_secs(90);
    let thread = loop {
        importing::run(state, &[source]).await.map_err(internal)?;
        let k = key.clone();
        if let Some(t) = state.with_store(move |s| s.thread_by_import_key(&k)).await.map_err(internal)? {
            break t;
        }
        // The first turn ended (or the driver died) and still no session on disk: it failed.
        let (ended, gone, why) = {
            let inner = session.lock();
            (inner.turns_ended > 0, inner.driver.is_none(), inner.last_system.clone())
        };
        if (ended || gone) && let Some(why) = why {
            return Err(fail(state, why));
        }
        if Instant::now() > deadline {
            return Err(fail(state, format!("the new {} session ({key}) never appeared on disk", agent.harness.as_str())));
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    };

    // 3. Re-key the live session under the real thread id and link the agent.
    {
        let mut map = state.live.sessions.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(&pending);
        map.insert(thread.id.clone(), session.clone());
    }
    *session.thread_id.lock().unwrap_or_else(|e| e.into_inner()) = thread.id.clone();
    session.lock().live.thread_id = thread.id.clone();
    state.live.touch(&thread.id);
    let (tid, aid) = (thread.id.clone(), agent.id.clone());
    state.with_store(move |s| {
        s.set_thread_agent(&tid, &aid)?;
        if let Some(model) = model {
            s.set_thread_model_pick(&tid, &model)?;
        }
        Ok(())
    }).await.map_err(internal)?;
    emit_state(state, &session);
    emit(state, ThreadEvent::TranscriptChanged { thread_id: thread.id.clone() });
    load_thread(state, &thread.id).await
}

/// `threads.takeover`: stop the driver and open the harness TUI on the same session.
pub async fn takeover(state: &State, p: ThreadTakeoverParams) -> Result<TerminalInfo, RpcError> {
    let thread = load_thread(state, &p.thread_id).await?;
    let source = source_for(thread.harness).unwrap_or(ImportSourceKind::Pi);
    let session = state.live.get_or_create(&thread.id, source);
    let (owner, running, exit) = {
        let inner = session.lock();
        (inner.live.owner, inner.running, inner.driver_exit.clone())
    };
    match owner {
        SessionOwner::Terminal => {
            let tid = session.lock().live.terminal_id.clone();
            return tid
                .and_then(|id| state.terminals.info(&id))
                .ok_or_else(|| err(codes::BUSY, "already open in a terminal"));
        }
        SessionOwner::Driver if running => {
            return Err(err(codes::BUSY, "a turn is running; wait for it or interrupt it first"));
        }
        SessionOwner::Driver => {
            if let Some(d) = &session.lock().driver {
                let _ = d.cmds.send(DriverCmd::Shutdown);
            }
            if let Some(mut e) = exit {
                let _ = tokio::time::timeout(Duration::from_secs(6), e.wait_for(|x| *x)).await;
            }
        }
        SessionOwner::None => guard_elsewhere(state, &thread, p.force.unwrap_or(false)).await?,
    }
    let launch = terminals::resume_launch(&thread, p.cols, p.rows)?;
    let terminals = state.terminals.clone();
    let info = tokio::task::spawn_blocking(move || terminals.spawn(launch))
        .await
        .map_err(|e| err(codes::INTERNAL, e.to_string()))??;

    // The pump may have dropped the session after the driver exited; make sure it's mapped.
    let session = state.live.get_or_create(&thread.id, source);
    {
        let mut inner = session.lock();
        inner.live.owner = SessionOwner::Terminal;
        inner.live.terminal_id = Some(info.id.clone());
        inner.live.status = ThreadStatus::Idle;
        inner.live.error = None;
    }
    state.live.touch(&thread.id);
    emit_state(state, &session);

    // Hand the session back to chat when the TUI exits.
    if let Some(mut w) = state.terminals.exit_watch(&info.id) {
        let state = state.clone();
        let thread_id = thread.id.clone();
        let term_id = info.id.clone();
        tokio::spawn(async move {
            let _ = w.wait_for(|s| *s == TerminalState::Exited).await;
            if state.live.get(&thread_id).is_some_and(|s| s.lock().live.terminal_id.as_deref() == Some(term_id.as_str())) {
                let _ = release(&state, &thread_id).await;
            }
        });
    }
    Ok(info)
}

/// `threads.release`: close the takeover terminal and return the session to chat.
pub async fn release(state: &State, thread_id: &str) -> Result<LiveThread, RpcError> {
    let session = state.live.get(thread_id).ok_or_else(|| err(codes::NOT_FOUND, "this thread isn't held"))?;
    let term = {
        let mut inner = session.lock();
        if inner.live.owner != SessionOwner::Terminal {
            return Ok(session.snapshot());
        }
        inner.live.owner = SessionOwner::None;
        inner.live.terminal_id.take()
    };
    if let Some(id) = term
        && state.terminals.info(&id).is_some_and(|i| i.state == TerminalState::Running)
    {
        let _ = state.terminals.kill(&id);
        if let Some(mut w) = state.terminals.exit_watch(&id) {
            let _ = tokio::time::timeout(Duration::from_secs(3), w.wait_for(|s| *s == TerminalState::Exited)).await;
        }
    }
    state.live.touch(thread_id);
    let live = session.snapshot();
    emit_state(state, &session);
    state.live.remove(thread_id);
    refresh(state, thread_id, session.source);
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_driver(session: &Session) -> mpsc::UnboundedReceiver<DriverCmd> {
        let (cmds, rx) = mpsc::unbounded_channel();
        let mut inner = session.lock();
        inner.driver = Some(DriverHandle {
            cmds,
            pid: None,
            caps: Caps { native_steer: true, native_follow_up: false, echoes_user: false },
        });
        inner.live.owner = SessionOwner::Driver;
        rx
    }

    #[test]
    fn prompts_are_not_accepted_by_a_retiring_or_closed_driver() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path().into()).unwrap();
        let session = Session::new("test".into(), ImportSourceKind::Claude);
        let mut commands = fake_driver(&session);
        let mut notes = state.notify.subscribe();
        session.lock().driver_stopping = true;
        assert!(start_turn(&state, &session, "keep my prompt".into()).is_err());
        assert!(commands.try_recv().is_err());
        assert!(notes.try_recv().is_err(), "a rejected prompt must not be echoed as sent");
        session.lock().driver_stopping = false;
        drop(commands);
        assert!(start_turn(&state, &session, "keep my prompt".into()).is_err());
        assert!(!session.lock().running);
        assert_eq!(session.snapshot().status, ThreadStatus::Idle);
    }

    #[tokio::test]
    async fn a_message_waits_for_retirement_and_uses_the_current_session() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path().into()).unwrap();
        let old = state.live.get_or_create("test", ImportSourceKind::Claude);
        let _old_commands = fake_driver(&old);
        let (exit, waiting) = watch::channel(false);
        {
            let mut inner = old.lock();
            inner.driver_stopping = true;
            inner.driver_exit = Some(waiting);
        }
        let thread: Thread = serde_json::from_value(serde_json::json!({
            "id": "test", "harness": "claude", "title": "Test", "childCount": 0,
            "status": "idle", "automated": false, "createdAt": 0, "updatedAt": 0,
            "session": {"harness": "claude", "sessionId": "test"}
        })).unwrap();
        let ready = ensure_driver(&state, &thread, ImportSourceKind::Claude, false);
        tokio::pin!(ready);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut ready).await.is_err());
        state.live.remove("test");
        let replacement = state.live.get_or_create("test", ImportSourceKind::Claude);
        let mut commands = fake_driver(&replacement);
        exit.send(true).unwrap();
        let session = tokio::time::timeout(Duration::from_secs(1), ready).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&session, &replacement));
        start_turn(&state, &session, "deliver once".into()).unwrap();
        assert!(matches!(commands.try_recv(), Ok(DriverCmd::Prompt(msg)) if msg.text == "deliver once"));
        assert!(commands.try_recv().is_err());
    }
}
