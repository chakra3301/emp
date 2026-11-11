//! Shared daemon state.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use emp_core::{Store, paths};
use emp_protocol::{AgentProfile, Harness, RpcNotification, TranscriptItem};
use tokio::sync::broadcast;

use crate::live::Live;
use crate::terminals::Terminals;

#[derive(Clone)]
pub struct State(Arc<Inner>);

pub struct Inner {
    pub data_dir: PathBuf,
    pub token: String,
    pub store: Mutex<Store>,
    /// Serializes import runs.
    pub import_lock: tokio::sync::Mutex<()>,
    /// Notifications fanned out to every authenticated connection.
    pub notify: broadcast::Sender<RpcNotification>,
    /// Recently read transcripts, so paging doesn't re-read the source. `(thread id, updated_at)`.
    pub transcripts: Mutex<Vec<(TranscriptKey, Arc<Vec<TranscriptItem>>)>>,
    /// Daemon-owned PTYs.
    pub terminals: Arc<Terminals>,
    /// Chat drivers and session ownership.
    pub live: Live,
    /// Paired devices, pairing codes and the tailnet address.
    pub phone: crate::phone::Phone,
    /// Compaction limits, kept in memory so drivers read the current value at every turn.
    pub compaction: std::sync::RwLock<Compaction>,
    /// Slash command lists (`commands.list`).
    pub commands: crate::commands::Cache,
    /// Scheduled tasks in flight, and the loop's wake-up.
    pub schedules: crate::schedules::Scheduler,
}

#[derive(Default)]
pub struct Compaction {
    pub global: Option<u64>,
    pub per_agent: std::collections::HashMap<String, u64>,
}

/// Reads the compaction limit that applies to one agent, at the moment it's asked.
#[derive(Clone, Default)]
pub struct CompactAt(Limit);

#[derive(Clone, Default)]
enum Limit {
    #[default]
    None,
    #[cfg(test)]
    Fixed(u64),
    Live(State, Option<String>),
}

impl CompactAt {
    /// No limit.
    #[cfg(test)]
    pub fn none() -> Self {
        CompactAt(Limit::None)
    }

    #[cfg(test)]
    pub fn fixed(n: u64) -> Self {
        CompactAt(Limit::Fixed(n))
    }

    pub fn get(&self) -> Option<u64> {
        match &self.0 {
            Limit::None => None,
            #[cfg(test)]
            Limit::Fixed(n) => Some(*n),
            Limit::Live(state, agent) => {
                let c = state.compaction.read().unwrap_or_else(|e| e.into_inner());
                agent.as_ref().and_then(|a| c.per_agent.get(a).copied()).or(c.global)
            }
        }
    }
}

/// `183000` → `183k`, for compaction notes.
pub fn ktok(n: u64) -> String {
    match n {
        1_000_000.. => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
        1_000.. => format!("{}k", (n + 500) / 1000),
        _ => n.to_string(),
    }
}

impl std::fmt::Debug for CompactAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CompactAt({:?})", self.get())
    }
}

/// Thread id, its `updated_at`, and the size and modification time of the files it's read from:
/// a running session grows on disk before EMP's own record of the thread changes.
pub type TranscriptKey = (String, i64, Vec<(u64, u128)>);

impl std::ops::Deref for State {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

const TRANSCRIPT_CACHE: usize = 8;

impl State {
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        paths::ensure_data_dir(&data_dir)?;
        let store = Store::open(&paths::db_path(&data_dir))?;
        let token = load_or_create_token(&data_dir)?;
        // Large: terminal output shares this channel. A lagging client skips ahead and can
        // re-attach terminals for a fresh snapshot.
        let (notify, _) = broadcast::channel(4096);
        let terminals = Arc::new(Terminals::new(&data_dir, notify.clone()));
        let phone = crate::phone::Phone::new(store.device_token_hashes()?);
        let compaction = Compaction {
            global: store.get_settings()?.compact_at,
            per_agent: store.agent_compact_limits()?.into_iter().collect(),
        };
        let state = State(Arc::new(Inner {
            store: Mutex::new(store),
            token,
            import_lock: tokio::sync::Mutex::new(()),
            notify,
            transcripts: Mutex::new(Vec::new()),
            terminals,
            live: Live::default(),
            phone,
            compaction: std::sync::RwLock::new(compaction),
            commands: crate::commands::Cache::new(&data_dir),
            schedules: Default::default(),
            data_dir,
        }));
        state.ensure_coordinator()?;
        crate::attachments::sweep(&crate::attachments::root(&state.data_dir));
        Ok(state)
    }

    /// The compaction limit for an agent's chats (its own, else the global one), read live.
    pub fn compact_at(&self, agent_id: Option<String>) -> CompactAt {
        CompactAt(Limit::Live(self.clone(), agent_id))
    }

    /// Re-reads the limits after settings or an agent changed.
    pub async fn reload_compaction(&self) -> Result<()> {
        let (global, per_agent) =
            self.with_store(|s| Ok((s.get_settings()?.compact_at, s.agent_compact_limits()?))).await?;
        let mut c = self.compaction.write().unwrap_or_else(|e| e.into_inner());
        c.global = global;
        c.per_agent = per_agent.into_iter().collect();
        Ok(())
    }

    /// Runs `f` with the store on the blocking pool.
    pub async fn with_store<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let state = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut store)
        })
        .await?
    }

    pub fn notify(&self, n: RpcNotification) {
        let _ = self.notify.send(n);
    }

    pub fn cached_transcript(&self, key: &TranscriptKey) -> Option<Arc<Vec<TranscriptItem>>> {
        let cache = self.transcripts.lock().unwrap_or_else(|e| e.into_inner());
        cache.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    pub fn cache_transcript(&self, key: TranscriptKey, items: Arc<Vec<TranscriptItem>>) {
        let mut cache = self.transcripts.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(k, _)| k.0 != key.0);
        cache.insert(0, (key, items));
        cache.truncate(TRANSCRIPT_CACHE);
    }

    /// Suki is EMP's coordinator, like Amp's Puck (FOUNDATION §7a; it was called Desk): pi on
    /// GPT-6.1 Sol through the ChatGPT subscription, working from an EMP-owned directory, with
    /// the `emp` tools (`mcp.rs`) instead of file tools. pi 1.3 signs ChatGPT in as `openai`;
    /// `openai-codex` is gone and asks for an API key.
    fn ensure_coordinator(&self) -> Result<()> {
        let dir = self.coordinator_dir();
        std::fs::create_dir_all(&dir)?;
        // pi, Codex and Hermes read AGENTS.md and Claude reads CLAUDE.md, at every start, so the
        // instructions survive resumes (an appended system prompt lasts one process).
        std::fs::write(dir.join("AGENTS.md"), SUKI_INSTRUCTIONS)?;
        std::fs::write(dir.join("CLAUDE.md"), SUKI_INSTRUCTIONS)?;
        let suki = AgentProfile {
            id: String::new(),
            name: "Suki".into(),
            harness: Harness::Pi,
            model: Some(SUKI_MODEL.into()),
            effort: Some("high".into()),
            role: Some("Coordinator: starts, finds, steers and checks on your agents".into()),
            instructions: None,
            cwd: Some(dir.to_string_lossy().into_owned()),
            autonomy: emp_protocol::FULL_ACCESS,
            harness_profile: None,
            compact_at: None,
            imported_from: None,
            coordinator: true,
        };
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.upsert_builtin_agent(emp_core::COORDINATOR_KEY, &suki)?;
        // An edited profile keeps its settings, but not the old default model, which no longer exists.
        store.replace_builtin_model(emp_core::COORDINATOR_KEY, "openai-codex/gpt-6.1-sol", SUKI_MODEL)
    }

    /// Suki's working directory. Its `AGENTS.md` and `.pi/mcp.json` are EMP's.
    pub fn coordinator_dir(&self) -> PathBuf {
        self.data_dir.join("suki")
    }
}

const SUKI_MODEL: &str = "openai/gpt-6.1-sol";

/// Suki's standing instructions, written to its folder's `AGENTS.md` and `CLAUDE.md`.
const SUKI_INSTRUCTIONS: &str = include_str!("suki.md");

fn load_or_create_token(data_dir: &std::path::Path) -> Result<String> {
    let path = paths::token_path(data_dir);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let t = existing.trim();
        if t.len() >= 32 {
            return Ok(t.to_string());
        }
    }
    let bytes: [u8; 32] = rand::random();
    let token = hex::encode(bytes);
    paths::write_private(&path, token.as_bytes())?;
    Ok(token)
}
