//! Orion Terminal importer (`~/Library/Application Support/com.lucaorion.orion-terminal/orion.db`).
//!
//! - **Agents:** Command Center profiles (`cc_profiles`) become pi agents. Their charter is
//!   the instructions, the wiki vault is the cwd, and the autonomy level carries over. The
//!   `commander` rank is the user, not an agent, so it's skipped.
//! - **Threads:** Orion chats (`chats`) are imported **only when their CLI session isn't
//!   already indexed from a harness store**. Orion runs Claude/Codex CLIs, whose sessions the
//!   Claude and Codex importers pick up (including Orion's private `CODEX_HOME`). Chats with
//!   no session, or whose session file is gone, become Orion-only threads. Those aren't
//!   resumable and their transcript is Orion's stored `messages_json`.
//!
//! Orion's own data is never written; see [`util::open_db`].

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use emp_core::{AgentUpsert, ThreadUpsert};
use emp_protocol::{Harness, ImportSourceKind, SessionRef, Thread, TranscriptItem};
use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::{Importer, ScanCtx, util};

pub struct OrionImporter {
    data_dir: PathBuf,
    claude_projects: PathBuf,
    codex_homes: Vec<PathBuf>,
}

impl Default for OrionImporter {
    fn default() -> Self {
        let home = emp_core::paths::home_dir();
        let data_dir = home.join("Library/Application Support/com.lucaorion.orion-terminal");
        let claude_root = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
        let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        Self {
            codex_homes: vec![codex_home, data_dir.join("cli-engines/codex-home")],
            claude_projects: claude_root.join("projects"),
            data_dir,
        }
    }
}

impl OrionImporter {
    fn db_path(&self) -> PathBuf {
        self.data_dir.join("orion.db")
    }

    /// Session ids that a harness importer already covers: Claude session file stems and
    /// Codex rollout ids (`rollout-<timestamp>-<uuid>.jsonl`).
    fn harness_session_ids(&self) -> HashSet<String> {
        let mut ids = HashSet::new();
        if let Ok(dirs) = std::fs::read_dir(&self.claude_projects) {
            for dir in dirs.flatten() {
                let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
                for f in files.flatten() {
                    let p = f.path();
                    if p.extension().is_some_and(|e| e == "jsonl")
                        && let Some(stem) = p.file_stem()
                    {
                        ids.insert(stem.to_string_lossy().into_owned());
                    }
                }
            }
        }
        for home in &self.codex_homes {
            for entry in walkdir::WalkDir::new(home.join("sessions")).into_iter().flatten() {
                let name = entry.file_name().to_string_lossy();
                if let Some(id) = name.strip_prefix("rollout-").and_then(|n| n.strip_suffix(".jsonl")) {
                    // `<YYYY-MM-DDThh-mm-ss>-<uuid>`: the uuid is the last 36 chars.
                    if id.len() >= 36 {
                        ids.insert(id[id.len() - 36..].to_string());
                    }
                }
            }
        }
        ids
    }
}

impl Importer for OrionImporter {
    fn kind(&self) -> ImportSourceKind {
        ImportSourceKind::Orion
    }

    fn paths(&self) -> Vec<PathBuf> {
        vec![self.db_path()]
    }

    fn scan(&self, ctx: &mut ScanCtx) -> Result<()> {
        let db_path = self.db_path();
        let conn = util::open_db(&db_path)?;
        let db_str = db_path.to_string_lossy().into_owned();

        if has_table(&conn, "cc_profiles")? {
            let mut stmt = conn.prepare(
                "SELECT id, name, rank, division, brain_model, wiki_root, charter, autonomy_level
                 FROM cc_profiles WHERE rank != 'commander' ORDER BY position",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?;
            for row in rows {
                let (id, name, rank, division, model, wiki_root, charter, autonomy) = match row {
                    Ok(row) => row,
                    Err(e) => {
                        ctx.skip(format!("cc_profiles row: {e}"));
                        continue;
                    }
                };
                let role = [rank.as_str(), division.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
                ctx.agent(AgentUpsert {
                    import_source: ImportSourceKind::Orion,
                    import_key: format!("orion:cc:{id}"),
                    import_path: db_str.clone(),
                    name: if name.is_empty() { "Command Center agent".into() } else { name },
                    harness: Harness::Pi,
                    model: non_empty(model),
                    effort: None,
                    role: Some(format!("Orion Command Center {role}")),
                    instructions: non_empty(util::truncate(&charter, 20_000)),
                    // Vault paths are stored relative to Orion's data dir.
                    cwd: non_empty(wiki_root).map(|p| {
                        let p = util::expand_home(&p);
                        if p.is_absolute() { p } else { self.data_dir.join(p) }.to_string_lossy().into_owned()
                    }),
                    autonomy: autonomy.clamp(0, 3) as u8,
                    harness_profile: None,
                });
            }
        }

        if !has_table(&conn, "chats")? {
            ctx.warn("orion.db has no chats table");
            return Ok(());
        }
        let covered = self.harness_session_ids();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.title, c.session_id, c.created_at, c.updated_at, length(c.messages_json),
                    c.origin, p.root_path
             FROM chats c LEFT JOIN projects p ON p.id = c.project_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?;
        for row in rows {
            let (id, title, session_id, created_at, updated_at, size, origin, root) = match row {
                Ok(row) => row,
                Err(e) => {
                    ctx.skip(format!("chats row: {e}"));
                    continue;
                }
            };
            if session_id.as_deref().is_some_and(|sid| covered.contains(sid)) {
                continue;
            }
            let key = format!("orion:chat:{id}");
            let fingerprint = format!("{updated_at}:{size}");
            if ctx.is_unchanged(&key, &fingerprint) {
                continue;
            }
            let surface = origin.filter(|o| !o.is_empty()).unwrap_or_else(|| "orion".into());
            ctx.thread(ThreadUpsert {
                import_source: ImportSourceKind::Orion,
                import_key: key,
                import_path: db_str.clone(),
                fingerprint,
                // Orion chats ran through the Claude CLI unless an owner record says otherwise;
                // either way they are not resumable from here (see module docs).
                harness: Harness::Claude,
                title: util::title_from(&title).unwrap_or_else(|| format!("Orion {surface} chat")),
                cwd: root.clone(),
                project_root: root.as_deref().map(util::project_root_for),
                model: None,
                session: SessionRef {
                    harness: Harness::Claude,
                    session_id: id,
                    path: Some(db_str.clone()),
                    home: None,
                    profile: Some(format!("orion:{surface}")),
                },
                message_count: None,
                created_at,
                updated_at,
                parent_key: None,
                automated: false,
            });
        }
        Ok(())
    }

    fn transcript(&self, thread: &Thread) -> Result<Vec<TranscriptItem>> {
        let conn = util::open_db(&self.db_path())?;
        let raw: Option<String> = conn
            .query_row("SELECT messages_json FROM chats WHERE id = ?1", [&thread.session.session_id], |r| r.get(0))
            .optional()?;
        let raw = raw.context("chat no longer exists in orion.db")?;
        Ok(chat_transcript(&raw))
    }
}

/// Orion threads are read from `orion.db` and can't be reopened in a harness TUI.
pub fn is_orion_only(thread: &Thread) -> bool {
    thread.session.profile.as_deref().is_some_and(|p| p.starts_with("orion:"))
}

fn chat_transcript(raw: &str) -> Vec<TranscriptItem> {
    let Ok(Value::Array(messages)) = serde_json::from_str::<Value>(raw) else { return Vec::new() };
    messages
        .iter()
        .filter_map(|m| {
            let mut text = m.get("content").and_then(Value::as_str).unwrap_or_default().to_string();
            if text.is_empty()
                && let Some(blocks) = m.get("blocks")
            {
                text = util::content_text(blocks);
            }
            if text.trim().is_empty() {
                return None;
            }
            let ts = m.get("createdAt").and_then(Value::as_i64);
            Some(match m.get("role").and_then(Value::as_str) {
                Some("user") => TranscriptItem::User { text, ts },
                Some("assistant") => TranscriptItem::Assistant { text, model: None, ts },
                _ => TranscriptItem::System { text, ts },
            })
        })
        .collect()
}

fn has_table(conn: &rusqlite::Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |_| Ok(()))
        .optional()?
        .is_some())
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn fixture(dir: &Path) -> OrionImporter {
        let data_dir = dir.join("orion");
        std::fs::create_dir_all(&data_dir).unwrap();
        let conn = rusqlite::Connection::open(data_dir.join("orion.db")).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT, root_path TEXT, last_opened_at INTEGER);
            CREATE TABLE chats (id TEXT PRIMARY KEY, title TEXT NOT NULL, messages_json TEXT NOT NULL,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, session_id TEXT, project_id TEXT,
                total_cost_usd REAL, searchable_text TEXT NOT NULL DEFAULT '', origin TEXT);
            CREATE TABLE cc_profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', rank TEXT NOT NULL,
                division TEXT NOT NULL DEFAULT '', accent TEXT NOT NULL DEFAULT '', brain_model TEXT NOT NULL DEFAULT '',
                skill_ids_json TEXT NOT NULL DEFAULT '[]', wiki_root TEXT NOT NULL DEFAULT '', charter TEXT NOT NULL DEFAULT '',
                autonomy_level INTEGER NOT NULL DEFAULT 1, position INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, avatar_path TEXT NOT NULL DEFAULT '');
            INSERT INTO projects VALUES ('p1', 'proj', '/tmp/proj', 0);
            INSERT INTO chats VALUES ('c1', 'Local chat', '[{"id":"m1","role":"user","content":"hi","createdAt":5},{"id":"m2","role":"assistant","blocks":[{"type":"text","text":"hello"}]}]', 1, 2, NULL, 'p1', 0, '', 'xdesign');
            INSERT INTO chats VALUES ('c2', 'Covered chat', '[]', 1, 2, 'sess-claude', NULL, 0, '', 'orion');
            INSERT INTO chats VALUES ('c3', 'Lost session', '[]', 1, 2, 'sess-gone', NULL, 0, '', NULL);
            INSERT INTO cc_profiles (id, name, rank, division, charter, wiki_root, autonomy_level, created_at, updated_at)
                VALUES ('u', 'Commander', 'commander', '', 'me', '', 0, 0, 0),
                       ('d', 'Dev Captain', 'captain', 'dev', 'Ship code.', '/v/dev', 2, 0, 0);
            "#,
        )
        .unwrap();
        drop(conn);
        let claude = dir.join("claude/projects/-tmp-x");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("sess-claude.jsonl"), "").unwrap();
        OrionImporter { data_dir, claude_projects: dir.join("claude/projects"), codex_homes: vec![dir.join("codex")] }
    }

    #[test]
    fn imports_uncovered_chats_and_captains() {
        let dir = tempfile::tempdir().unwrap();
        let imp = fixture(dir.path());
        let mut ctx = ScanCtx::new(HashMap::new());
        imp.scan(&mut ctx).unwrap();
        let keys: Vec<_> = ctx.threads.iter().map(|t| t.import_key.as_str()).collect();
        assert_eq!(keys, vec!["orion:chat:c1", "orion:chat:c3"]);
        assert_eq!(ctx.threads[0].cwd.as_deref(), Some("/tmp/proj"));
        assert_eq!(ctx.threads[0].session.profile.as_deref(), Some("orion:xdesign"));
        assert_eq!(ctx.agents.len(), 1);
        assert_eq!(ctx.agents[0].name, "Dev Captain");
        assert_eq!(ctx.agents[0].autonomy, 2);
        assert_eq!(ctx.agents[0].cwd.as_deref(), Some("/v/dev"));
    }

    #[test]
    fn relative_vault_paths_resolve_against_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let imp = fixture(dir.path());
        let conn = rusqlite::Connection::open(imp.db_path()).unwrap();
        conn.execute("UPDATE cc_profiles SET wiki_root = 'command-center/dev' WHERE id = 'd'", []).unwrap();
        drop(conn);
        let mut ctx = ScanCtx::new(HashMap::new());
        imp.scan(&mut ctx).unwrap();
        let expected = imp.data_dir.join("command-center/dev");
        assert_eq!(ctx.agents[0].cwd.as_deref(), Some(expected.to_str().unwrap()));
        assert_eq!(ctx.agents[0].harness, Harness::Pi);
    }

    #[test]
    fn unchanged_chats_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let imp = fixture(dir.path());
        let mut first = ScanCtx::new(HashMap::new());
        imp.scan(&mut first).unwrap();
        let known: HashMap<_, _> = first.threads.iter().map(|t| (t.import_key.clone(), t.fingerprint.clone())).collect();
        let mut second = ScanCtx::new(known);
        imp.scan(&mut second).unwrap();
        assert!(second.threads.is_empty());
        assert_eq!(second.stats.threads_unchanged, 2);
    }

    #[test]
    fn transcript_reads_content_and_blocks() {
        let items = chat_transcript(
            r#"[{"role":"user","content":"hi","createdAt":5},{"role":"assistant","blocks":[{"type":"text","text":"hello"}]},{"role":"assistant","content":""}]"#,
        );
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], TranscriptItem::User { text, ts: Some(5) } if text == "hi"));
        assert!(matches!(&items[1], TranscriptItem::Assistant { text, .. } if text == "hello"));
    }
}
