//! SQLite store. Migrations are append-only: never edit a shipped entry in `MIGRATIONS`,
//! add a new one.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use emp_protocol::{
    AgentProfile, Device, Harness, ImportOrigin, ImportSourceKind, Page, Project, Schedule,
    ScheduleRunStatus, ScheduleWhen, SessionRef, Thread, ThreadStatus, ThreadsListParams,
};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value};

use crate::now_ms;

const MIGRATIONS: &[&str] = &[
    // 1 — initial schema
    r#"
    CREATE TABLE projects (
        id          TEXT PRIMARY KEY,
        name        TEXT NOT NULL,
        root_path   TEXT NOT NULL UNIQUE,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    CREATE TABLE agents (
        id               TEXT PRIMARY KEY,
        name             TEXT NOT NULL,
        harness          TEXT NOT NULL,
        model            TEXT,
        effort           TEXT,
        role             TEXT,
        instructions     TEXT,
        cwd              TEXT,
        autonomy         INTEGER NOT NULL DEFAULT 1,
        harness_profile  TEXT,
        import_source    TEXT,
        import_key       TEXT UNIQUE,
        import_path      TEXT,
        created_at       INTEGER NOT NULL,
        updated_at       INTEGER NOT NULL
    );
    CREATE TABLE threads (
        id               TEXT PRIMARY KEY,
        harness          TEXT NOT NULL,
        title            TEXT NOT NULL,
        project_id       TEXT REFERENCES projects(id),
        cwd              TEXT,
        model            TEXT,
        agent_id         TEXT REFERENCES agents(id),
        parent_id        TEXT REFERENCES threads(id),
        status           TEXT NOT NULL DEFAULT 'idle',
        session_id       TEXT NOT NULL,
        session_path     TEXT,
        session_home     TEXT,
        session_profile  TEXT,
        message_count    INTEGER,
        created_at       INTEGER NOT NULL,
        updated_at       INTEGER NOT NULL,
        import_source    TEXT,
        import_key       TEXT UNIQUE,
        import_path      TEXT,
        parent_key       TEXT,
        fingerprint      TEXT
    );
    CREATE INDEX threads_by_project ON threads(project_id, updated_at DESC);
    CREATE INDEX threads_by_updated ON threads(updated_at DESC);
    CREATE INDEX threads_by_source ON threads(import_source);
    CREATE TABLE import_runs (
        id           INTEGER PRIMARY KEY,
        source       TEXT NOT NULL,
        started_at   INTEGER NOT NULL,
        finished_at  INTEGER,
        stats_json   TEXT,
        error        TEXT
    );
    "#,
    // 2 — machine-started runs are kept but hidden by default
    r#"
    ALTER TABLE threads ADD COLUMN automated INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX threads_by_automated ON threads(automated, updated_at DESC);
    "#,
    // 3 — agents edited in EMP are no longer overwritten by re-import
    r#"
    ALTER TABLE agents ADD COLUMN user_edited INTEGER NOT NULL DEFAULT 0;
    "#,
    // 4 — removed imported agents stay hidden across re-imports; projects regroup under the
    //     outermost git repo, so every thread is re-indexed once
    r#"
    ALTER TABLE agents ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
    UPDATE threads SET fingerprint = NULL;
    "#,
    // 5 — projects the user added in EMP show even before they have threads
    r#"
    ALTER TABLE projects ADD COLUMN user_added INTEGER NOT NULL DEFAULT 0;
    "#,
    // 6 — paired phones/browsers; only a SHA-256 of each device token is stored
    r#"
    CREATE TABLE devices (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        token_hash TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL,
        last_seen_at INTEGER
    );
    "#,
    // 7 — compaction limits: per agent, plus daemon-wide settings as JSON values
    r#"
    ALTER TABLE agents ADD COLUMN compact_at INTEGER;
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    // 8 — the owner's thread groups; a thread is in at most one
    r#"
    CREATE TABLE thread_groups (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        pinned INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE thread_group_members (
        thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
        group_id TEXT NOT NULL REFERENCES thread_groups(id) ON DELETE CASCADE,
        added_at INTEGER NOT NULL
    );
    CREATE INDEX thread_group_members_group ON thread_group_members(group_id);
    "#,
    // 9 — subagent threads are listed under their parent
    r#"
    CREATE INDEX threads_by_parent ON threads(parent_id);
    "#,
    // 10 — threads the coordinator (Suki) started remember which of its chats did it
    r#"
    ALTER TABLE threads ADD COLUMN assigned_by TEXT REFERENCES threads(id) ON DELETE SET NULL;
    "#,
    // 11 — scheduled tasks. Agent and thread ids aren't foreign keys: a run whose agent or
    // thread is gone fails and pauses, rather than quietly changing what it does.
    r#"
    CREATE TABLE schedules (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        prompt TEXT NOT NULL,
        when_json TEXT NOT NULL,
        agent_id TEXT,
        cwd TEXT,
        project_id TEXT,
        thread_id TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        next_run_at INTEGER,
        last_run_at INTEGER,
        last_status TEXT,
        last_error TEXT,
        last_thread_id TEXT,
        run_count INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE INDEX schedules_due ON schedules(enabled, next_run_at);
    "#,
    // 12 — schedules can end (a date, a number of runs, a condition the agent checks), and
    // pause after failing three times in a row
    r#"
    ALTER TABLE schedules ADD COLUMN ends_at INTEGER;
    ALTER TABLE schedules ADD COLUMN max_runs INTEGER;
    ALTER TABLE schedules ADD COLUMN stop_when TEXT;
    ALTER TABLE schedules ADD COLUMN ended TEXT;
    ALTER TABLE schedules ADD COLUMN fail_streak INTEGER NOT NULL DEFAULT 0;
    "#,
    // 13 — the model the owner switched a chat to; re-import leaves it alone (`model` is what
    // the harness recorded, often the model the session started with)
    r#"
    ALTER TABLE threads ADD COLUMN model_pick TEXT;
    "#,
    // 14 — access the owner gave a chat ("Allow for this chat", folders): JSON `Grants`
    r#"
    ALTER TABLE threads ADD COLUMN grants TEXT;
    "#,
    // 15 — the owner: nothing waits on their permission; every agent gets full access
    r#"
    UPDATE agents SET autonomy = 3;
    "#,
];

/// Access the owner gave one chat beyond its agent's defaults, re-applied each time EMP starts
/// the harness for it. Claude: `dirs` become working directories, `rules` allow rules
/// (`Bash(npm test)`, `Read(//path/**)`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Grants {
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default)]
    pub rules: Vec<String>,
}

impl Grants {
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && self.rules.is_empty()
    }
}

pub struct Store {
    conn: Connection,
}

/// A thread as produced by an importer. Matched to an existing row by `import_key`.
#[derive(Debug, Clone)]
pub struct ThreadUpsert {
    pub import_source: ImportSourceKind,
    pub import_key: String,
    pub import_path: String,
    /// Changes whenever the underlying record changes (e.g. `mtime:size`). Equal → no update.
    pub fingerprint: String,
    pub harness: Harness,
    pub title: String,
    pub cwd: Option<String>,
    /// Project root to group under; created on demand.
    pub project_root: Option<String>,
    pub model: Option<String>,
    pub session: SessionRef,
    pub message_count: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// `import_key` of the parent thread (subagents, sidechains), resolved after the run.
    pub parent_key: Option<String>,
    /// Machine-started run; hidden from lists unless asked for.
    pub automated: bool,
}

/// An agent profile as produced by an importer.
#[derive(Debug, Clone)]
pub struct AgentUpsert {
    pub import_source: ImportSourceKind,
    pub import_key: String,
    pub import_path: String,
    pub name: String,
    pub harness: Harness,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub role: Option<String>,
    pub instructions: Option<String>,
    pub cwd: Option<String>,
    pub autonomy: u8,
    pub harness_profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Added,
    Updated,
    Unchanged,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let target = MIGRATIONS.len() as i64;
        if current > target {
            bail!("emp.db schema v{current} is newer than this build (v{target})");
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("migration {}", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    // ---- import -----------------------------------------------------------------------------

    /// `import_key → fingerprint` for one source, so importers can skip unchanged records
    /// without parsing them.
    pub fn fingerprints(&self, source: ImportSourceKind) -> Result<HashMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT import_key, fingerprint FROM threads WHERE import_source = ?1 AND import_key IS NOT NULL",
        )?;
        let rows = stmt.query_map([source.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Applies one importer's output in a single transaction.
    pub fn apply_import(
        &mut self,
        threads: &[ThreadUpsert],
        agents: &[AgentUpsert],
    ) -> Result<Vec<UpsertOutcome>> {
        let tx = self.conn.transaction()?;
        let mut outcomes = Vec::with_capacity(threads.len());
        let mut project_ids: HashMap<String, String> = HashMap::new();
        for t in threads {
            let project_id = match &t.project_root {
                Some(root) => Some(match project_ids.get(root) {
                    Some(id) => id.clone(),
                    None => {
                        let id = ensure_project(&tx, root)?;
                        project_ids.insert(root.clone(), id.clone());
                        id
                    }
                }),
                None => None,
            };
            outcomes.push(upsert_thread(&tx, t, project_id.as_deref())?);
        }
        for a in agents {
            upsert_agent(&tx, a)?;
        }
        // Link parents now that every thread of this batch exists.
        tx.execute(
            "UPDATE threads SET parent_id = (SELECT p.id FROM threads p WHERE p.import_key = threads.parent_key)
             WHERE parent_key IS NOT NULL AND parent_id IS NULL",
            [],
        )?;
        tx.commit()?;
        Ok(outcomes)
    }

    /// Creates or updates an agent that EMP itself defines (e.g. Desk). `key` is stored as the
    /// import key with no import source, so it never shows as imported.
    pub fn upsert_builtin_agent(&self, key: &str, a: &AgentProfile) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO agents (id, name, harness, model, effort, role, instructions, cwd, autonomy,
                 harness_profile, import_source, import_key, import_path, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, NULL, ?12, ?12)
             ON CONFLICT(import_key) DO UPDATE SET
                 name = excluded.name, harness = excluded.harness, model = excluded.model,
                 effort = excluded.effort, role = excluded.role, instructions = excluded.instructions,
                 cwd = excluded.cwd, autonomy = excluded.autonomy, updated_at = excluded.updated_at
             WHERE agents.user_edited = 0",
            params![
                ulid::Ulid::new().to_string(),
                a.name,
                a.harness.as_str(),
                a.model,
                a.effort,
                a.role,
                a.instructions,
                a.cwd,
                a.autonomy,
                a.harness_profile,
                key,
                now,
            ],
        )?;
        Ok(())
    }

    /// Swaps a builtin agent's model when it is still `old`, even if the owner edited the profile.
    pub fn replace_builtin_model(&self, key: &str, old: &str, new: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agents SET model = ?3, updated_at = ?4 WHERE import_key = ?1 AND model = ?2",
            params![key, old, new, now_ms()],
        )?;
        Ok(())
    }

    pub fn begin_import_run(&self, source: ImportSourceKind) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO import_runs (source, started_at) VALUES (?1, ?2)",
            params![source.as_str(), now_ms()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_import_run(&self, run_id: i64, stats_json: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE import_runs SET finished_at = ?2, stats_json = ?3, error = ?4 WHERE id = ?1",
            params![run_id, now_ms(), stats_json, error],
        )?;
        Ok(())
    }

    pub fn last_import_at(&self, source: ImportSourceKind) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT MAX(finished_at) FROM import_runs WHERE source = ?1 AND error IS NULL",
                [source.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn source_counts(&self, source: ImportSourceKind) -> Result<(u32, u32)> {
        let threads: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM threads WHERE import_source = ?1",
            [source.as_str()],
            |r| r.get(0),
        )?;
        let agents: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM agents WHERE import_source = ?1",
            [source.as_str()],
            |r| r.get(0),
        )?;
        Ok((threads, agents))
    }

    // ---- queries ----------------------------------------------------------------------------

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, p.root_path,
                    COUNT(t.id) FILTER (WHERE t.automated = 0),
                    COUNT(t.id) FILTER (WHERE t.automated = 1),
                    MAX(t.updated_at)
             FROM projects p LEFT JOIN threads t ON t.project_id = p.id
             GROUP BY p.id
             HAVING COUNT(t.id) > 0 OR p.user_added = 1
             ORDER BY COUNT(t.id) FILTER (WHERE t.automated = 0) = 0 AND p.user_added = 0,
                      COALESCE(MAX(t.updated_at), p.created_at) DESC, p.name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Project {
                id: r.get(0)?,
                name: r.get(1)?,
                root_path: r.get(2)?,
                thread_count: r.get(3)?,
                automated_count: r.get(4)?,
                last_activity_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Registers `root` as a project the user added (renaming it if `name` is given).
    pub fn create_project(&self, root: &str, name: Option<&str>) -> Result<Project> {
        let id = ensure_project(&self.conn, root)?;
        self.conn.execute(
            "UPDATE projects SET user_added = 1, name = COALESCE(?2, name), updated_at = ?3 WHERE id = ?1",
            params![id, name.map(str::trim).filter(|n| !n.is_empty()), now_ms()],
        )?;
        self.list_projects()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| anyhow::anyhow!("project vanished"))
    }

    pub fn project_root(&self, id: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT root_path FROM projects WHERE id = ?1", [id], |r| r.get(0)).optional()?)
    }

    // ---- thread groups ------------------------------------------------------------------------

    /// Pinned groups first, then oldest first; each with its threads, newest first.
    pub fn list_groups(&self) -> Result<Vec<emp_protocol::ThreadGroup>> {
        let mut stmt = self.conn.prepare("SELECT id, name, pinned, created_at FROM thread_groups ORDER BY pinned DESC, created_at")?;
        let mut groups: Vec<emp_protocol::ThreadGroup> = stmt
            .query_map([], |r| {
                Ok(emp_protocol::ThreadGroup { id: r.get(0)?, name: r.get(1)?, pinned: r.get(2)?, created_at: r.get(3)?, threads: Vec::new() })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let cols = thread_cols("t");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT m.group_id, {cols} FROM thread_group_members m JOIN threads t ON t.id = m.thread_id ORDER BY t.updated_at DESC"
        ))?;
        let rows = stmt.query_map([], |r| {
            let group: String = r.get(0)?;
            Ok((group, thread_from_row_at(r, 1)?))
        })?;
        for row in rows {
            let (group, thread) = row?;
            if let Some(g) = groups.iter_mut().find(|g| g.id == group) {
                g.threads.push(thread);
            }
        }
        Ok(groups)
    }

    pub fn create_group(&self, name: &str, pinned: bool) -> Result<String> {
        let id = ulid::Ulid::new().to_string();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO thread_groups (id, name, pinned, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, clean_group_name(name)?, pinned, now],
        )?;
        Ok(id)
    }

    /// Returns false when there's no such group.
    pub fn update_group(&self, id: &str, name: Option<&str>, pinned: Option<bool>) -> Result<bool> {
        let name = name.map(clean_group_name).transpose()?;
        Ok(self.conn.execute(
            "UPDATE thread_groups SET name = COALESCE(?2, name), pinned = COALESCE(?3, pinned), updated_at = ?4 WHERE id = ?1",
            params![id, name, pinned, now_ms()],
        )? > 0)
    }

    /// Deletes the group; its threads go back to the main list.
    pub fn delete_group(&self, id: &str) -> Result<bool> {
        self.conn.execute("DELETE FROM thread_group_members WHERE group_id = ?1", [id])?;
        Ok(self.conn.execute("DELETE FROM thread_groups WHERE id = ?1", [id])? > 0)
    }

    /// Moves threads into a group (out of any other). Unknown thread ids are skipped.
    pub fn add_to_group(&self, group: &str, thread_ids: &[String]) -> Result<bool> {
        let exists: bool = self.conn.query_row("SELECT EXISTS (SELECT 1 FROM thread_groups WHERE id = ?1)", [group], |r| r.get(0))?;
        if !exists {
            return Ok(false);
        }
        let now = now_ms();
        for t in thread_ids {
            self.conn.execute(
                "INSERT INTO thread_group_members (thread_id, group_id, added_at)
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM threads WHERE id = ?1)
                 ON CONFLICT(thread_id) DO UPDATE SET group_id = excluded.group_id, added_at = excluded.added_at",
                params![t, group, now],
            )?;
        }
        Ok(true)
    }

    pub fn remove_from_group(&self, thread_id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM thread_group_members WHERE thread_id = ?1", [thread_id])?;
        Ok(())
    }

    // ---- scheduled tasks ----------------------------------------------------------------------

    /// Every schedule, soonest next run first (paused and finished ones last). `summary` is
    /// left empty for the caller to fill in.
    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SCHEDULE_COLS} FROM schedules ORDER BY enabled DESC, next_run_at IS NULL, next_run_at, created_at"
        ))?;
        let rows = stmt.query_map([], schedule_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_schedule(&self, id: &str) -> Result<Option<Schedule>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {SCHEDULE_COLS} FROM schedules WHERE id = ?1"), [id], schedule_from_row)
            .optional()?)
    }

    /// Enabled schedules due at `now`.
    pub fn due_schedules(&self, now: i64) -> Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SCHEDULE_COLS} FROM schedules WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 ORDER BY next_run_at"
        ))?;
        let rows = stmt.query_map([now], schedule_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Inserts or replaces what a schedule does and when. Run history is kept on replace.
    pub fn save_schedule(&self, s: &Schedule) -> Result<()> {
        self.conn.execute(
            "INSERT INTO schedules (id, title, prompt, when_json, agent_id, cwd, project_id, thread_id, enabled, next_run_at, created_at, updated_at,
               ends_at, max_runs, stop_when, ended, fail_streak)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, prompt = excluded.prompt, when_json = excluded.when_json,
               agent_id = excluded.agent_id, cwd = excluded.cwd, project_id = excluded.project_id, thread_id = excluded.thread_id,
               enabled = excluded.enabled, next_run_at = excluded.next_run_at, updated_at = excluded.updated_at,
               ends_at = excluded.ends_at, max_runs = excluded.max_runs, stop_when = excluded.stop_when,
               ended = excluded.ended, fail_streak = excluded.fail_streak",
            params![
                s.id,
                s.title,
                s.prompt,
                serde_json::to_string(&s.when)?,
                s.agent_id,
                s.cwd,
                s.project_id,
                s.thread_id,
                s.enabled,
                s.next_run_at,
                s.created_at,
                now_ms(),
                s.ends_at,
                s.max_runs,
                s.stop_when,
                s.ended,
                s.fail_streak
            ],
        )?;
        Ok(())
    }

    pub fn delete_schedule(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM schedules WHERE id = ?1", [id])? > 0)
    }

    /// Sets when it runs next, and whether it's on.
    pub fn set_schedule_next(&self, id: &str, next_run_at: Option<i64>, enabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE schedules SET next_run_at = ?2, enabled = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, next_run_at, enabled, now_ms()],
        )?;
        Ok(())
    }

    /// Resumes it from `next_run_at`, forgetting why it ended and its failures.
    pub fn resume_schedule(&self, id: &str, next_run_at: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE schedules SET next_run_at = ?2, enabled = 1, ended = NULL, fail_streak = 0, updated_at = ?3 WHERE id = ?1",
            params![id, next_run_at, now_ms()],
        )?;
        Ok(())
    }

    /// It finished on its own; `why` is shown with it.
    pub fn end_schedule(&self, id: &str, why: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE schedules SET next_run_at = NULL, enabled = 0, ended = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, why, now_ms()],
        )?;
        Ok(())
    }

    /// A run started (`started`: counts it and stamps the time) or changed status. A failed
    /// run adds to the failure streak; a finished one clears it.
    pub fn set_schedule_run(
        &self,
        id: &str,
        started: bool,
        status: ScheduleRunStatus,
        error: Option<&str>,
        thread_id: Option<&str>,
    ) -> Result<()> {
        let status = serde_json::to_value(status)?.as_str().unwrap_or_default().to_string();
        self.conn.execute(
            "UPDATE schedules SET last_status = ?2, last_error = ?3,
               last_thread_id = COALESCE(?4, last_thread_id),
               last_run_at = CASE WHEN ?5 THEN ?6 ELSE last_run_at END,
               run_count = run_count + CASE WHEN ?5 THEN 1 ELSE 0 END,
               fail_streak = CASE ?2 WHEN 'failed' THEN fail_streak + 1 WHEN 'done' THEN 0 ELSE fail_streak END
             WHERE id = ?1",
            params![id, status, error, thread_id, started, now_ms()],
        )?;
        Ok(())
    }

    // ---- daemon settings ----------------------------------------------------------------------

    pub fn get_settings(&self) -> Result<emp_protocol::DaemonSettings> {
        let raw: Option<String> =
            self.conn.query_row("SELECT value FROM settings WHERE key = 'daemon'", [], |r| r.get(0)).optional()?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default())
    }

    pub fn set_settings(&self, s: &emp_protocol::DaemonSettings) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES ('daemon', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [serde_json::to_string(s)?],
        )?;
        Ok(())
    }

    /// `(agent id, limit)` for agents with their own compaction limit.
    pub fn agent_compact_limits(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare("SELECT id, compact_at FROM agents WHERE compact_at IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).filter_map(|(id, n)| u64::try_from(n).ok().map(|n| (id, n))).collect())
    }

    // ---- paired devices -----------------------------------------------------------------------

    pub fn create_device(&self, name: &str, token_hash: &str) -> Result<Device> {
        let id = ulid::Ulid::new().to_string();
        let now = now_ms();
        let name = name.trim().chars().take(60).collect::<String>();
        let name = if name.is_empty() { "Device".to_string() } else { name };
        self.conn.execute(
            "INSERT INTO devices (id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, token_hash, now],
        )?;
        Ok(Device { id, name, created_at: now, last_seen_at: None })
    }

    pub fn list_devices(&self) -> Result<Vec<Device>> {
        let mut stmt = self.conn.prepare("SELECT id, name, created_at, last_seen_at FROM devices ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |r| Ok(Device { id: r.get(0)?, name: r.get(1)?, created_at: r.get(2)?, last_seen_at: r.get(3)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `(token hash, device id)` for every paired device.
    pub fn device_token_hashes(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT token_hash, id FROM devices")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Returns false when there was no such device.
    pub fn revoke_device(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM devices WHERE id = ?1", [id])? > 0)
    }

    pub fn touch_device(&self, id: &str) -> Result<()> {
        self.conn.execute("UPDATE devices SET last_seen_at = ?2 WHERE id = ?1", params![id, now_ms()])?;
        Ok(())
    }

    pub fn list_agents(&self) -> Result<Vec<AgentProfile>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {AGENT_COLS} FROM agents WHERE hidden = 0 ORDER BY name"))?;
        let rows = stmt.query_map([], agent_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn list_threads(&self, p: &ThreadsListParams) -> Result<Page<Thread>> {
        let mut clauses = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        if let Some(project) = &p.project_id {
            clauses.push("project_id = ?");
            args.push(Value::Text(project.clone()));
        }
        if let Some(parent) = &p.parent_id {
            clauses.push("parent_id = ?");
            args.push(Value::Text(parent.clone()));
        } else if !p.include_automated.unwrap_or(false) {
            clauses.push("automated = 0");
        }
        if p.top_level.unwrap_or(false) {
            clauses.push("parent_id IS NULL");
        }
        if let Some(h) = p.harness {
            clauses.push("harness = ?");
            args.push(Value::Text(h.as_str().into()));
        }
        match p.coordinator {
            Some(true) => {
                clauses.push("agent_id IN (SELECT id FROM agents WHERE import_key = ?)");
                args.push(Value::Text(COORDINATOR_KEY.into()));
            }
            Some(false) => {
                clauses.push("(agent_id IS NULL OR agent_id NOT IN (SELECT id FROM agents WHERE import_key = ?))");
                args.push(Value::Text(COORDINATOR_KEY.into()));
            }
            None => {}
        }
        if p.ungrouped.unwrap_or(false) {
            clauses.push("NOT EXISTS (SELECT 1 FROM thread_group_members m WHERE m.thread_id = threads.id)");
        }
        if let Some(q) = p.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            clauses.push("(title LIKE ? ESCAPE '\\' OR cwd LIKE ? ESCAPE '\\')");
            let pattern = format!("%{}%", escape_like(q));
            args.push(Value::Text(pattern.clone()));
            args.push(Value::Text(pattern));
        }
        let filter = if clauses.is_empty() { String::new() } else { format!("WHERE {}", clauses.join(" AND ")) };
        let total: u32 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM threads {filter}"),
            params_from_iter(args.iter()),
            |r| r.get(0),
        )?;
        let limit = p.limit.unwrap_or(200).min(1000);
        let offset = p.offset.unwrap_or(0);
        args.push(Value::Integer(limit as i64));
        args.push(Value::Integer(offset as i64));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM threads {filter} ORDER BY updated_at DESC LIMIT ? OFFSET ?",
            thread_cols("threads")
        ))?;
        let items = stmt
            .query_map(params_from_iter(args.iter()), thread_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Page { items, total, offset })
    }

    /// Thread imported under `import_key` (e.g. `pi:<session id>`), if any.
    pub fn thread_by_import_key(&self, key: &str) -> Result<Option<Thread>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {} FROM threads WHERE import_key = ?1", thread_cols("threads")), [key], thread_from_row)
            .optional()?)
    }

    /// Links a thread to the agent profile that started it in EMP.
    pub fn set_thread_assigned_by(&self, thread_id: &str, by: &str) -> Result<()> {
        self.conn.execute("UPDATE threads SET assigned_by = ?2 WHERE id = ?1", params![thread_id, by])?;
        Ok(())
    }

    /// The coordinator profile's id.
    pub fn coordinator_id(&self) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT id FROM agents WHERE import_key = ?1", [COORDINATOR_KEY], |r| r.get(0)).optional()?)
    }

    pub fn thread_grants(&self, thread_id: &str) -> Result<Grants> {
        let raw: Option<String> = self.conn.query_row("SELECT grants FROM threads WHERE id = ?1", [thread_id], |r| r.get(0)).optional()?.flatten();
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default())
    }

    /// Adds to a chat's grants (no duplicates); returns them all.
    pub fn add_thread_grants(&self, thread_id: &str, add: &Grants) -> Result<Grants> {
        let mut g = self.thread_grants(thread_id)?;
        for d in &add.dirs {
            if !g.dirs.contains(d) {
                g.dirs.push(d.clone());
            }
        }
        for r in &add.rules {
            if !g.rules.contains(r) {
                g.rules.push(r.clone());
            }
        }
        self.conn.execute("UPDATE threads SET grants = ?2 WHERE id = ?1", params![thread_id, serde_json::to_string(&g)?])?;
        Ok(g)
    }

    /// The model the owner switched this chat to, if they did.
    pub fn thread_model_pick(&self, thread_id: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT model_pick FROM threads WHERE id = ?1", [thread_id], |r| r.get(0)).optional()?.flatten())
    }

    pub fn set_thread_model_pick(&self, thread_id: &str, model: &str) -> Result<()> {
        self.conn.execute("UPDATE threads SET model_pick = ?2 WHERE id = ?1", params![thread_id, model])?;
        Ok(())
    }

    pub fn set_thread_agent(&self, thread_id: &str, agent_id: &str) -> Result<()> {
        self.conn.execute("UPDATE threads SET agent_id = ?2 WHERE id = ?1", params![thread_id, agent_id])?;
        Ok(())
    }

    pub fn get_agent(&self, id: &str) -> Result<Option<AgentProfile>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {AGENT_COLS} FROM agents WHERE id = ?1"), [id], agent_from_row)
            .optional()?)
    }

    /// Creates or updates a user-defined agent. Updating marks it `user_edited`, so re-import
    /// leaves it alone. Returns the saved profile.
    pub fn save_agent(&self, p: &emp_protocol::AgentSaveParams) -> Result<AgentProfile> {
        let now = now_ms();
        let id = match &p.id {
            Some(id) => {
                let n = self.conn.execute(
                    "UPDATE agents SET name = ?2, harness = ?3, model = ?4, effort = ?5, role = ?6, instructions = ?7,
                         cwd = ?8, autonomy = ?9, harness_profile = ?10, user_edited = 1, updated_at = ?11, compact_at = ?12
                     WHERE id = ?1",
                    params![
                        id, p.name, p.harness.as_str(), p.model, p.effort, p.role, p.instructions, p.cwd,
                        p.autonomy.min(3), p.harness_profile, now, p.compact_at.map(|n| n as i64)
                    ],
                )?;
                if n == 0 {
                    bail!("no agent {id}");
                }
                id.clone()
            }
            None => {
                let id = ulid::Ulid::new().to_string();
                self.conn.execute(
                    "INSERT INTO agents (id, name, harness, model, effort, role, instructions, cwd, autonomy,
                         harness_profile, user_edited, created_at, updated_at, compact_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?11, ?12)",
                    params![
                        id, p.name, p.harness.as_str(), p.model, p.effort, p.role, p.instructions, p.cwd,
                        p.autonomy.min(3), p.harness_profile, now, p.compact_at.map(|n| n as i64)
                    ],
                )?;
                id
            }
        };
        self.get_agent(&id)?.ok_or_else(|| anyhow::anyhow!("agent vanished"))
    }

    /// Removes an agent. Imported (and built-in) agents are hidden instead of deleted, so
    /// re-import doesn't bring them back. Threads keep their history and their agent link.
    pub fn delete_agent(&self, id: &str) -> Result<bool> {
        let keyed: Option<Option<String>> = self
            .conn
            .query_row("SELECT import_key FROM agents WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        match keyed {
            None => Ok(false),
            Some(Some(_)) => Ok(self.conn.execute("UPDATE agents SET hidden = 1 WHERE id = ?1", [id])? > 0),
            Some(None) => {
                self.conn.execute("UPDATE threads SET agent_id = NULL WHERE agent_id = ?1", [id])?;
                Ok(self.conn.execute("DELETE FROM agents WHERE id = ?1", [id])? > 0)
            }
        }
    }

    pub fn get_thread(&self, id: &str) -> Result<Option<Thread>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {} FROM threads WHERE id = ?1", thread_cols("threads")), [id], thread_from_row)
            .optional()?)
    }
}

const THREAD_COLS: &str = "id, harness, title, project_id, cwd, model, agent_id, parent_id, status, \
     session_id, session_path, session_home, session_profile, message_count, created_at, updated_at, \
     import_source, import_key, import_path, automated, assigned_by";

/// `THREAD_COLS` of table `alias`, plus its subagent count. `model` is the owner's pick when
/// there is one.
fn thread_cols(alias: &str) -> String {
    let cols = THREAD_COLS
        .split(", ")
        .map(|c| match c.trim() {
            "model" => format!("COALESCE({alias}.model_pick, {alias}.model)"),
            c => format!("{alias}.{c}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{cols}, (SELECT COUNT(*) FROM threads c WHERE c.parent_id = {alias}.id)")
}

/// `import_key` of the coordinator profile EMP defines (it was called Desk at first).
pub const COORDINATOR_KEY: &str = "emp:desk";

const AGENT_COLS: &str = "id, name, harness, model, effort, role, instructions, cwd, autonomy, \
     harness_profile, import_source, import_key, import_path, compact_at";

fn clean_group_name(name: &str) -> Result<String> {
    let n: String = name.trim().chars().take(60).collect();
    if n.is_empty() {
        bail!("a group needs a name");
    }
    Ok(n)
}

fn thread_from_row(r: &Row) -> rusqlite::Result<Thread> {
    thread_from_row_at(r, 0)
}

/// A thread whose columns start at index `o` (`THREAD_COLS` order).
fn thread_from_row_at(r: &Row, o: usize) -> rusqlite::Result<Thread> {
    let harness = parse_harness(r, o + 1)?;
    Ok(Thread {
        id: r.get(o)?,
        harness,
        title: r.get(o + 2)?,
        project_id: r.get(o + 3)?,
        cwd: r.get(o + 4)?,
        model: r.get(o + 5)?,
        agent_id: r.get(o + 6)?,
        parent_id: r.get(o + 7)?,
        child_count: r.get(o + 21)?,
        assigned_by: r.get(o + 20)?,
        status: parse_status(&r.get::<_, String>(o + 8)?),
        automated: r.get(o + 19)?,
        session: SessionRef {
            harness,
            session_id: r.get(o + 9)?,
            path: r.get(o + 10)?,
            home: r.get(o + 11)?,
            profile: r.get(o + 12)?,
        },
        message_count: r.get(o + 13)?,
        created_at: r.get(o + 14)?,
        updated_at: r.get(o + 15)?,
        imported_from: origin_from_row(r, o + 16)?,
    })
}

fn agent_from_row(r: &Row) -> rusqlite::Result<AgentProfile> {
    Ok(AgentProfile {
        id: r.get(0)?,
        name: r.get(1)?,
        harness: parse_harness(r, 2)?,
        model: r.get(3)?,
        effort: r.get(4)?,
        role: r.get(5)?,
        instructions: r.get(6)?,
        cwd: r.get(7)?,
        autonomy: r.get(8)?,
        harness_profile: r.get(9)?,
        compact_at: r.get::<_, Option<i64>>(13)?.and_then(|n| u64::try_from(n).ok()),
        imported_from: origin_from_row(r, 10)?,
        coordinator: r.get::<_, Option<String>>(11)?.as_deref() == Some(COORDINATOR_KEY),
    })
}

fn origin_from_row(r: &Row, start: usize) -> rusqlite::Result<Option<ImportOrigin>> {
    let source: Option<String> = r.get(start)?;
    let key: Option<String> = r.get(start + 1)?;
    let path: Option<String> = r.get(start + 2)?;
    Ok(match (source.as_deref().and_then(parse_source), key) {
        (Some(source), Some(key)) => Some(ImportOrigin { source, key, path: path.unwrap_or_default() }),
        _ => None,
    })
}

fn parse_harness(r: &Row, idx: usize) -> rusqlite::Result<Harness> {
    let s: String = r.get(idx)?;
    Harness::parse(&s).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, format!("unknown harness {s}").into())
    })
}

fn parse_source(s: &str) -> Option<ImportSourceKind> {
    ImportSourceKind::ALL.into_iter().find(|k| k.as_str() == s)
}

fn parse_status(s: &str) -> ThreadStatus {
    match s {
        "running" => ThreadStatus::Running,
        "needs-input" => ThreadStatus::NeedsInput,
        "done" => ThreadStatus::Done,
        "failed" => ThreadStatus::Failed,
        _ => ThreadStatus::Idle,
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

fn ensure_project(conn: &Connection, root: &str) -> Result<String> {
    if let Some(id) = conn
        .query_row("SELECT id FROM projects WHERE root_path = ?1", [root], |r| r.get::<_, String>(0))
        .optional()?
    {
        return Ok(id);
    }
    let id = ulid::Ulid::new().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![id, project_name(root), root, now],
    )?;
    Ok(id)
}

const SCHEDULE_COLS: &str = "id, title, prompt, when_json, agent_id, cwd, project_id, thread_id, enabled, next_run_at, \
     last_run_at, last_status, last_error, last_thread_id, run_count, created_at, ends_at, max_runs, stop_when, ended, fail_streak";

fn schedule_from_row(r: &Row) -> rusqlite::Result<Schedule> {
    let when: String = r.get(3)?;
    let status: Option<String> = r.get(11)?;
    Ok(Schedule {
        id: r.get(0)?,
        title: r.get(1)?,
        prompt: r.get(2)?,
        when: serde_json::from_str(&when).unwrap_or(ScheduleWhen::Every { minutes: 60 }),
        summary: String::new(),
        agent_id: r.get(4)?,
        cwd: r.get(5)?,
        project_id: r.get(6)?,
        thread_id: r.get(7)?,
        thread_title: None,
        enabled: r.get(8)?,
        next_run_at: r.get(9)?,
        last_run_at: r.get(10)?,
        last_status: status.and_then(|s| serde_json::from_value(serde_json::Value::String(s)).ok()),
        last_error: r.get(12)?,
        last_thread_id: r.get(13)?,
        run_count: r.get(14)?,
        created_at: r.get(15)?,
        ends_at: r.get(16)?,
        max_runs: r.get(17)?,
        stop_when: r.get(18)?,
        ended: r.get(19)?,
        fail_streak: r.get(20)?,
    })
}

/// Display name for a project root: its last path component, or `~` for the home dir.
pub fn project_name(root: &str) -> String {
    match root {
        "/private/var/folders" => return "Temporary (system)".into(),
        "/private/tmp" => return "Temporary (/tmp)".into(),
        _ => {}
    }
    let home = crate::paths::home_dir();
    let path = Path::new(root);
    if path == home {
        return "~".into();
    }
    // App bundles: `/Applications/FL Studio 2025.app/Contents` → `FL Studio 2025`.
    if let Some(app) = path.ancestors().find_map(|p| {
        let name = p.file_name()?.to_str()?;
        name.strip_suffix(".app").map(str::to_string)
    }) {
        return app;
    }
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.to_string())
}

fn upsert_thread(conn: &Connection, t: &ThreadUpsert, project_id: Option<&str>) -> Result<UpsertOutcome> {
    let existing: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT id, fingerprint FROM threads WHERE import_key = ?1",
            [&t.import_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match existing {
        Some((_, Some(fp))) if fp == t.fingerprint => Ok(UpsertOutcome::Unchanged),
        Some((id, _)) => {
            conn.execute(
                "UPDATE threads SET harness = ?2, title = ?3, project_id = ?4, cwd = ?5, model = ?6,
                     session_id = ?7, session_path = ?8, session_home = ?9, session_profile = ?10,
                     message_count = ?11, created_at = ?12, updated_at = ?13, import_path = ?14,
                     parent_key = ?15, fingerprint = ?16, automated = ?17
                 WHERE id = ?1",
                params![
                    id,
                    t.harness.as_str(),
                    t.title,
                    project_id,
                    t.cwd,
                    t.model,
                    t.session.session_id,
                    t.session.path,
                    t.session.home,
                    t.session.profile,
                    t.message_count,
                    t.created_at,
                    t.updated_at,
                    t.import_path,
                    t.parent_key,
                    t.fingerprint,
                    t.automated,
                ],
            )?;
            Ok(UpsertOutcome::Updated)
        }
        None => {
            conn.execute(
                "INSERT INTO threads (id, harness, title, project_id, cwd, model, status, session_id,
                     session_path, session_home, session_profile, message_count, created_at, updated_at,
                     import_source, import_key, import_path, parent_key, fingerprint, automated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                params![
                    ulid::Ulid::new().to_string(),
                    t.harness.as_str(),
                    t.title,
                    project_id,
                    t.cwd,
                    t.model,
                    t.session.session_id,
                    t.session.path,
                    t.session.home,
                    t.session.profile,
                    t.message_count,
                    t.created_at,
                    t.updated_at,
                    t.import_source.as_str(),
                    t.import_key,
                    t.import_path,
                    t.parent_key,
                    t.fingerprint,
                    t.automated,
                ],
            )?;
            Ok(UpsertOutcome::Added)
        }
    }
}

fn upsert_agent(conn: &Connection, a: &AgentUpsert) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO agents (id, name, harness, model, effort, role, instructions, cwd, autonomy,
             harness_profile, import_source, import_key, import_path, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
         ON CONFLICT(import_key) DO UPDATE SET
             name = excluded.name, harness = excluded.harness, model = excluded.model,
             effort = excluded.effort, role = excluded.role, instructions = excluded.instructions,
             cwd = excluded.cwd, autonomy = excluded.autonomy, harness_profile = excluded.harness_profile,
             import_path = excluded.import_path, updated_at = excluded.updated_at
         WHERE agents.user_edited = 0",
        params![
            ulid::Ulid::new().to_string(),
            a.name,
            a.harness.as_str(),
            a.model,
            a.effort,
            a.role,
            a.instructions,
            a.cwd,
            a.autonomy,
            a.harness_profile,
            a.import_source.as_str(),
            a.import_key,
            a.import_path,
            now,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(key: &str, fp: &str, root: Option<&str>) -> ThreadUpsert {
        ThreadUpsert {
            import_source: ImportSourceKind::Pi,
            import_key: key.into(),
            import_path: "/tmp/x.jsonl".into(),
            fingerprint: fp.into(),
            harness: Harness::Pi,
            title: format!("title {key}"),
            cwd: root.map(Into::into),
            project_root: root.map(Into::into),
            model: None,
            session: SessionRef { harness: Harness::Pi, session_id: key.into(), path: None, home: None, profile: None },
            message_count: Some(3),
            created_at: 1,
            updated_at: 2,
            parent_key: None,
            automated: false,
        }
    }

    #[test]
    fn import_is_idempotent_and_detects_changes() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.apply_import(&[thread("pi:a", "1", Some("/r/one")), thread("pi:b", "1", Some("/r/one"))], &[]).unwrap();
        assert_eq!(a, vec![UpsertOutcome::Added, UpsertOutcome::Added]);
        let b = s.apply_import(&[thread("pi:a", "1", Some("/r/one")), thread("pi:b", "2", Some("/r/one"))], &[]).unwrap();
        assert_eq!(b, vec![UpsertOutcome::Unchanged, UpsertOutcome::Updated]);
        let projects = s.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "one");
        assert_eq!(projects[0].thread_count, 2);
    }

    #[test]
    fn a_switched_model_survives_reimport() {
        let mut s = Store::open_in_memory().unwrap();
        let mut t = thread("pi:m", "1", None);
        t.model = Some("openai/gpt-6.1-sol".into());
        s.apply_import(std::slice::from_ref(&t), &[]).unwrap();
        let id = s.thread_by_import_key("pi:m").unwrap().unwrap().id;
        assert_eq!(s.thread_model_pick(&id).unwrap(), None);
        s.set_thread_model_pick(&id, "openai/gpt-6-luna").unwrap();
        t.fingerprint = "2".into();
        s.apply_import(&[t], &[]).unwrap();
        assert_eq!(s.get_thread(&id).unwrap().unwrap().model.as_deref(), Some("openai/gpt-6-luna"));
        assert_eq!(s.thread_by_import_key("pi:m").unwrap().unwrap().model.as_deref(), Some("openai/gpt-6-luna"));
        assert_eq!(s.thread_model_pick(&id).unwrap().as_deref(), Some("openai/gpt-6-luna"));

        assert!(s.thread_grants(&id).unwrap().is_empty());
        s.add_thread_grants(&id, &Grants { dirs: vec!["/a".into()], rules: vec!["Bash(ls)".into()] }).unwrap();
        let g = s.add_thread_grants(&id, &Grants { dirs: vec!["/a".into(), "/b".into()], rules: vec![] }).unwrap();
        assert_eq!(g, Grants { dirs: vec!["/a".into(), "/b".into()], rules: vec!["Bash(ls)".into()] });
        assert_eq!(s.thread_grants(&id).unwrap(), g);
    }

    #[test]
    fn parents_resolve_by_import_key_across_batches() {
        let mut s = Store::open_in_memory().unwrap();
        let mut child = thread("pi:child", "1", None);
        child.parent_key = Some("pi:parent".into());
        s.apply_import(&[child], &[]).unwrap();
        s.apply_import(&[thread("pi:parent", "1", None)], &[]).unwrap();
        let page = s.list_threads(&ThreadsListParams { query: Some("child".into()), ..Default::default() }).unwrap();
        let parent = s.list_threads(&ThreadsListParams { query: Some("parent".into()), ..Default::default() }).unwrap();
        assert_eq!(page.items[0].parent_id.as_deref(), Some(parent.items[0].id.as_str()));
        assert_eq!(parent.items[0].child_count, 1);

        // Subagents leave the top-level list and are listed under their parent, even when
        // machine-started.
        let mut auto = thread("pi:auto-child", "1", None);
        auto.parent_key = Some("pi:parent".into());
        auto.automated = true;
        s.apply_import(&[auto], &[]).unwrap();
        let top = s.list_threads(&ThreadsListParams { top_level: Some(true), ..Default::default() }).unwrap();
        assert_eq!(top.items.iter().map(|t| t.child_count).collect::<Vec<_>>(), vec![2]);
        let kids = s.list_threads(&ThreadsListParams { parent_id: Some(parent.items[0].id.clone()), ..Default::default() }).unwrap();
        assert_eq!(kids.total, 2);
    }

    #[test]
    fn schedules_round_trip_and_come_due() {
        let store = Store::open_in_memory().unwrap();
        let mut s = Schedule {
            id: "s1".into(),
            title: "Morning check".into(),
            prompt: "Look at the slow queries".into(),
            when: ScheduleWhen::Cron { expr: "0 9 * * 1-5".into() },
            summary: String::new(),
            agent_id: Some("a1".into()),
            cwd: None,
            project_id: None,
            thread_id: None,
            thread_title: None,
            enabled: true,
            next_run_at: Some(1_000),
            last_run_at: None,
            last_status: None,
            last_error: None,
            last_thread_id: None,
            run_count: 0,
            ends_at: None,
            max_runs: Some(3),
            stop_when: Some("the deploy is live".into()),
            ended: None,
            fail_streak: 0,
            created_at: 1,
        };
        store.save_schedule(&s).unwrap();
        assert_eq!(store.due_schedules(999).unwrap().len(), 0);
        assert_eq!(store.due_schedules(1_000).unwrap()[0].when, s.when);

        store.set_schedule_run("s1", true, ScheduleRunStatus::Running, None, Some("t9")).unwrap();
        store.set_schedule_run("s1", false, ScheduleRunStatus::Failed, Some("boom"), None).unwrap();
        // Editing it keeps the run history.
        s.title = "Renamed".into();
        store.save_schedule(&s).unwrap();
        let got = store.get_schedule("s1").unwrap().unwrap();
        assert_eq!((got.title.as_str(), got.run_count, got.last_status), ("Renamed", 1, Some(ScheduleRunStatus::Failed)));
        assert_eq!((got.last_error.as_deref(), got.last_thread_id.as_deref()), (Some("boom"), Some("t9")));
        // An edit saves the streak it's given (the daemon starts it over).
        assert_eq!((got.max_runs, got.stop_when.as_deref(), got.fail_streak), (Some(3), Some("the deploy is live"), 0));
        store.set_schedule_run("s1", false, ScheduleRunStatus::Failed, Some("again"), None).unwrap();
        store.set_schedule_run("s1", false, ScheduleRunStatus::Failed, Some("again"), None).unwrap();
        assert_eq!(store.get_schedule("s1").unwrap().unwrap().fail_streak, 2);
        store.set_schedule_run("s1", false, ScheduleRunStatus::Done, None, None).unwrap();
        assert_eq!(store.get_schedule("s1").unwrap().unwrap().fail_streak, 0);
        store.end_schedule("s1", "Condition met").unwrap();
        let got = store.get_schedule("s1").unwrap().unwrap();
        assert_eq!((got.enabled, got.next_run_at, got.ended.as_deref()), (false, None, Some("Condition met")));
        store.resume_schedule("s1", Some(5_000)).unwrap();
        let got = store.get_schedule("s1").unwrap().unwrap();
        assert_eq!((got.enabled, got.next_run_at, got.ended), (true, Some(5_000), None));

        store.set_schedule_next("s1", None, false).unwrap();
        assert!(store.due_schedules(i64::MAX).unwrap().is_empty());
        assert!(store.delete_schedule("s1").unwrap());
        assert!(store.list_schedules().unwrap().is_empty());
    }

    #[test]
    fn groups_hold_threads_once_and_hide_them_from_the_main_list() {
        let mut s = Store::open_in_memory().unwrap();
        s.apply_import(&[thread("pi:one", "1", None), thread("pi:two", "1", None)], &[]).unwrap();
        let id = |k: &str| s.thread_by_import_key(k).unwrap().unwrap().id;
        let (t1, t2) = (id("pi:one"), id("pi:two"));
        let a = s.create_group("Survivor", false).unwrap();
        let b = s.create_group("  Pinned work ", true).unwrap();
        assert!(s.create_group("   ", false).is_err());
        assert!(s.add_to_group(&a, &[t1.clone(), t2.clone(), "missing".into()]).unwrap());
        assert!(!s.add_to_group("nope", std::slice::from_ref(&t1)).unwrap());
        // Moving a thread takes it out of its old group.
        s.add_to_group(&b, std::slice::from_ref(&t2)).unwrap();
        let g = s.list_groups().unwrap();
        assert_eq!(g.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["Pinned work", "Survivor"], "pinned first");
        assert_eq!(g[0].threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), [t2.as_str()]);
        assert_eq!(g[1].threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), [t1.as_str()]);
        let ungrouped = ThreadsListParams { ungrouped: Some(true), ..Default::default() };
        assert_eq!(s.list_threads(&ungrouped).unwrap().total, 0);
        assert_eq!(s.list_threads(&ThreadsListParams::default()).unwrap().total, 2);
        s.remove_from_group(&t1).unwrap();
        assert_eq!(s.list_threads(&ungrouped).unwrap().total, 1);
        assert!(s.update_group(&a, Some("Games"), Some(true)).unwrap());
        assert!(s.list_groups().unwrap().iter().all(|g| g.pinned));
        assert!(s.delete_group(&b).unwrap());
        assert_eq!(s.list_threads(&ungrouped).unwrap().total, 2, "a deleted group's threads come back");
    }

    #[test]
    fn compaction_settings_and_agent_limits() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.get_settings().unwrap().compact_at, None);
        s.set_settings(&emp_protocol::DaemonSettings { compact_at: Some(150_000) }).unwrap();
        s.set_settings(&emp_protocol::DaemonSettings { compact_at: Some(200_000) }).unwrap();
        assert_eq!(s.get_settings().unwrap().compact_at, Some(200_000));
        let p = |compact_at| emp_protocol::AgentSaveParams {
            id: None,
            name: "Builder".into(),
            harness: Harness::Claude,
            model: None,
            effort: None,
            role: None,
            instructions: None,
            cwd: None,
            autonomy: 1,
            harness_profile: None,
            compact_at,
        };
        let a = s.save_agent(&p(Some(120_000))).unwrap();
        assert_eq!(a.compact_at, Some(120_000));
        assert_eq!(s.agent_compact_limits().unwrap(), vec![(a.id.clone(), 120_000)]);
        let b = s.save_agent(&emp_protocol::AgentSaveParams { id: Some(a.id.clone()), ..p(None) }).unwrap();
        assert_eq!(b.compact_at, None);
        assert!(s.agent_compact_limits().unwrap().is_empty());
    }

    #[test]
    fn devices_pair_list_touch_and_revoke() {
        let s = Store::open_in_memory().unwrap();
        let d = s.create_device("  Demo's iPhone  ", "hash-1").unwrap();
        assert_eq!(d.name, "Demo's iPhone");
        assert!(s.create_device("dup", "hash-1").is_err(), "token hashes are unique");
        s.touch_device(&d.id).unwrap();
        assert!(s.list_devices().unwrap()[0].last_seen_at.is_some());
        assert_eq!(s.device_token_hashes().unwrap(), vec![("hash-1".to_string(), d.id.clone())]);
        assert!(s.revoke_device(&d.id).unwrap());
        assert!(!s.revoke_device(&d.id).unwrap());
        assert!(s.list_devices().unwrap().is_empty());
    }

    #[test]
    fn user_added_projects_show_without_threads() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.list_projects().unwrap().is_empty());
        let p = s.create_project("/work/new-game", Some("New game")).unwrap();
        assert_eq!(p.name, "New game");
        assert_eq!(p.thread_count, 0);
        assert_eq!(s.list_projects().unwrap().len(), 1);
        // Idempotent on the same root.
        assert_eq!(s.create_project("/work/new-game", None).unwrap().id, p.id);
    }

    #[test]
    fn project_names() {
        assert_eq!(project_name("/Applications/FL Studio 2025.app/Contents"), "FL Studio 2025");
        assert_eq!(project_name("/Users/me/code/emp"), "emp");
        assert_eq!(project_name("/private/var/folders"), "Temporary (system)");
    }

    #[test]
    fn edited_agents_survive_reimport() {
        let mut s = Store::open_in_memory().unwrap();
        let mut a = AgentUpsert {
            import_source: ImportSourceKind::Hermes,
            import_key: "hermes:profile:studio".into(),
            import_path: "/x".into(),
            name: "studio".into(),
            harness: Harness::Hermes,
            model: Some("m1".into()),
            effort: None,
            role: None,
            instructions: None,
            cwd: None,
            autonomy: 1,
            harness_profile: Some("studio".into()),
        };
        s.apply_import(&[], std::slice::from_ref(&a)).unwrap();
        let id = s.list_agents().unwrap()[0].id.clone();
        let saved = s
            .save_agent(&emp_protocol::AgentSaveParams {
                id: Some(id.clone()),
                name: "Studio (mine)".into(),
                harness: Harness::Hermes,
                model: Some("m-edit".into()),
                effort: None,
                role: Some("design".into()),
                instructions: None,
                cwd: None,
                autonomy: 9,
                harness_profile: Some("studio".into()),
                compact_at: None,
            })
            .unwrap();
        assert_eq!(saved.autonomy, 3);
        a.model = Some("m2".into());
        s.apply_import(&[], &[a]).unwrap();
        let agent = s.get_agent(&id).unwrap().unwrap();
        assert_eq!(agent.name, "Studio (mine)");
        assert_eq!(agent.model.as_deref(), Some("m-edit"));
        assert!(s.delete_agent(&id).unwrap());
        assert!(s.list_agents().unwrap().is_empty());
        // Removing an imported agent hides it; re-import doesn't bring it back.
        let again = AgentUpsert {
            import_source: ImportSourceKind::Hermes,
            import_key: "hermes:profile:studio".into(),
            import_path: "/x".into(),
            name: "studio".into(),
            harness: Harness::Hermes,
            model: None,
            effort: None,
            role: None,
            instructions: None,
            cwd: None,
            autonomy: 1,
            harness_profile: Some("studio".into()),
        };
        s.apply_import(&[], &[again]).unwrap();
        assert!(s.list_agents().unwrap().is_empty());
        // Agents made in EMP are really deleted.
        let mine = s
            .save_agent(&emp_protocol::AgentSaveParams {
                id: None,
                name: "Scratch".into(),
                harness: Harness::Pi,
                model: None,
                effort: None,
                role: None,
                instructions: None,
                cwd: None,
                autonomy: 1,
                harness_profile: None,
                compact_at: None,
            })
            .unwrap();
        assert!(s.delete_agent(&mine.id).unwrap());
        assert!(s.get_agent(&mine.id).unwrap().is_none());
    }

    #[test]
    fn query_escapes_like_wildcards() {
        let mut s = Store::open_in_memory().unwrap();
        s.apply_import(&[thread("pi:a", "1", None)], &[]).unwrap();
        let none = s.list_threads(&ThreadsListParams { query: Some("%".into()), ..Default::default() }).unwrap();
        assert_eq!(none.total, 0);
    }

    #[test]
    fn agent_reimport_updates_in_place() {
        let mut s = Store::open_in_memory().unwrap();
        let mut a = AgentUpsert {
            import_source: ImportSourceKind::Hermes,
            import_key: "hermes:profile:studio".into(),
            import_path: "/x".into(),
            name: "studio".into(),
            harness: Harness::Hermes,
            model: Some("m1".into()),
            effort: None,
            role: None,
            instructions: None,
            cwd: None,
            autonomy: 1,
            harness_profile: Some("studio".into()),
        };
        s.apply_import(&[], std::slice::from_ref(&a)).unwrap();
        a.model = Some("m2".into());
        s.apply_import(&[], &[a]).unwrap();
        let agents = s.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].model.as_deref(), Some("m2"));
    }
}
