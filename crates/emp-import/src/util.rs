//! Shared helpers for importers.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

/// A foreign SQLite store opened by [`open_db`]. Derefs to the connection.
pub struct Db {
    conn: Connection,
    /// Temp copy the connection reads; removed after the connection closes (field order).
    _snapshot: Option<Snapshot>,
}

impl Db {
    /// Temp directory of the copy being read, when [`open_db`] took a snapshot.
    pub fn snapshot_dir(&self) -> Option<&Path> {
        self._snapshot.as_ref().map(|s| s.0.as_path())
    }
}

impl std::ops::Deref for Db {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.conn
    }
}

struct Snapshot(PathBuf);

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Opens a harness's SQLite store (Hermes `state.db`, Codex `state_5.sqlite`, Orion
/// `orion.db`, all WAL mode) **without changing any file next to it**. The only way importers
/// may open a foreign database.
///
/// A plain read-only open of a WAL database is not hands-off: it creates missing
/// `-wal`/`-shm` sidecars, and the first connection to a dormant database rebuilds `-shm`.
/// So:
/// - **`-wal` missing or empty** (every writer closed cleanly, e.g. Orion while it's quit):
///   the main file holds everything; open with `immutable=1`, so SQLite creates, locks and
///   touches nothing.
/// - **`-wal` has content**: copy the database and its `-wal` into a temp dir (APFS clone,
///   near-instant) and read the copy. The copy is used only if neither source changed during
///   the copy, so the pair is consistent; WAL recovery drops uncommitted frames.
/// - **A writer raced the copy** (a live gateway): read the source with `mode=ro`. The writer
///   already owns `-shm`, so we only take shared locks and read marks. A read-only connection
///   never checkpoints or deletes the `-wal`.
pub fn open_db(path: &Path) -> Result<Db> {
    if !path.exists() {
        return Err(anyhow::anyhow!("{} not found", path.display()));
    }
    let wal = sidecar(path, "-wal");
    if fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
        match snapshot(path, &wal) {
            Ok(Some(db)) => return Ok(db),
            Ok(None) => tracing::debug!("{} changed while copying; reading live", path.display()),
            Err(e) => tracing::debug!("snapshot of {} failed: {e:#}; reading live", path.display()),
        }
        let conn = open_uri(path, "mode=ro")?;
        probe(&conn).with_context(|| format!("read {}", path.display()))?;
        return Ok(Db { conn, _snapshot: None });
    }
    let conn = open_uri(path, "mode=ro&immutable=1")?;
    probe(&conn).with_context(|| format!("read {}", path.display()))?;
    Ok(Db { conn, _snapshot: None })
}

fn open_uri(path: &Path, query: &str) -> Result<Connection> {
    let uri = format!("file:{}?{query}", uri_path(path));
    let conn = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open read-only {}", path.display()))?;
    conn.execute_batch("PRAGMA query_only = 1; PRAGMA busy_timeout = 2000;")?;
    Ok(conn)
}

/// Consistent temp copy of `path` + its `-wal`, or `None` if either changed while copying.
fn snapshot(path: &Path, wal: &Path) -> Result<Option<Db>> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = |p: &Path| -> Result<(std::time::SystemTime, u64)> {
        let m = fs::metadata(p)?;
        Ok((m.modified()?, m.len()))
    };
    let before = (stamp(path)?, stamp(wal)?);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("emp-import-{}-{seq}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let guard = Snapshot(dir);
    let copy = guard.0.join("db.sqlite");
    fs::copy(path, &copy)?;
    fs::copy(wal, sidecar(&copy, "-wal"))?;
    if (stamp(path)?, stamp(wal)?) != before {
        return Ok(None);
    }
    // Our own copy: opened read-write so WAL recovery can build its index next to it.
    let conn = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.execute_batch("PRAGMA query_only = 1; PRAGMA busy_timeout = 2000;")?;
    probe(&conn)?;
    Ok(Some(Db { conn, _snapshot: Some(guard) }))
}

fn probe(conn: &Connection) -> rusqlite::Result<()> {
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map(|_| ())
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Percent-encodes a filesystem path for an SQLite `file:` URI.
fn uri_path(path: &Path) -> String {
    let mut out = String::new();
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Column names of `table`. Harness schemas drift across versions; query with [`col`].
pub fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(cols)
}

/// `name` if the column exists, else `NULL`.
pub fn col<'a>(cols: &HashSet<String>, name: &'a str) -> &'a str {
    if cols.contains(name) { name } else { "NULL" }
}

/// `mtime_ms:size` — cheap change detector for one file.
pub fn file_fingerprint(path: &Path) -> Result<String> {
    let meta = fs::metadata(path)?;
    Ok(format!("{}:{}", mtime_ms(&meta).unwrap_or(0), meta.len()))
}

pub fn mtime_ms(meta: &fs::Metadata) -> Option<i64> {
    meta.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

pub fn ctime_ms(meta: &fs::Metadata) -> Option<i64> {
    meta.created().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

/// RFC 3339 / ISO 8601 timestamp → epoch ms.
pub fn parse_ts(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

/// Seconds (float, as in Python `time.time()`) → epoch ms.
pub fn secs_to_ms(secs: f64) -> i64 {
    (secs * 1000.0) as i64
}

/// Reads a JSONL file line by line, splitting on `\n` only (U+2028/U+2029 can appear inside
/// JSON strings). Invalid lines are passed to `on_bad` and skipped.
pub fn for_each_jsonl(
    path: &Path,
    mut on_value: impl FnMut(serde_json::Value) -> std::ops::ControlFlow<()>,
    mut on_bad: impl FnMut(usize),
) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut buf = Vec::new();
    let mut line_no = 0usize;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        line_no += 1;
        let line = trim_ascii(&buf);
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(v) => {
                if on_value(v).is_break() {
                    break;
                }
            }
            Err(_) => on_bad(line_no),
        }
    }
    Ok(())
}

/// Counts `\n`-terminated lines without parsing them.
pub fn count_lines(path: &Path) -> Result<i64> {
    let mut file = fs::File::open(path)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut count = 0i64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        count += buf[..n].iter().filter(|&&b| b == b'\n').count() as i64;
    }
    Ok(count)
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(start, |i| i + 1);
    &b[start..end]
}

/// One-line title from free text: first non-empty line, whitespace-collapsed, ≤ 80 chars.
pub fn title_from(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(truncate(&collapsed, 80))
}

/// Truncates on a char boundary, appending `…` when cut.
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Display-size cap for tool inputs/outputs in transcripts.
pub const MAX_TOOL_TEXT: usize = 4000;

/// Text content of a message `content` field that is either a string or an array of blocks
/// with `{type:"text", text}` entries.
pub fn content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                let ty = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                matches!(ty, "text" | "input_text" | "output_text")
                    .then(|| b.get("text").and_then(|t| t.as_str()))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Pretty JSON for display, truncated.
pub fn display_json(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    truncate(&s, MAX_TOOL_TEXT)
}

/// System temp locations. Threads run from these are grouped into one project per root.
pub const TEMP_ROOTS: &[&str] = &["/private/var/folders", "/private/tmp"];

/// True for paths under the system temp dirs (`/tmp`, `/var/folders`, and their `/private` forms).
pub fn is_temp_dir(p: &str) -> bool {
    temp_root(p).is_some()
}

fn temp_root(p: &str) -> Option<&'static str> {
    let p = p.strip_prefix("/private").unwrap_or(p);
    if p == "/var/folders" || p.starts_with("/var/folders/") {
        Some(TEMP_ROOTS[0])
    } else if p == "/tmp" || p.starts_with("/tmp/") {
        Some(TEMP_ROOTS[1])
    } else {
        None
    }
}

/// Groups a working directory under its git checkout root, falling back to the directory
/// itself. The *outermost* repository below home wins, so nested repos (a video's own repo
/// inside a studio, submodules) group under the workspace that holds them. Temp dirs collapse
/// into one [`TEMP_ROOTS`] entry.
/// Results are cached per process.
pub fn project_root_for(cwd: &str) -> String {
    if let Some(root) = temp_root(cwd) {
        return root.to_string();
    }
    static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(root) = cache.get(cwd) {
        return root.clone();
    }
    let home = emp_core::paths::home_dir();
    let mut dir = PathBuf::from(cwd);
    let mut root = cwd.trim_end_matches('/').to_string();
    if root.is_empty() {
        root = "/".into();
    }
    loop {
        if dir == home || dir.parent().is_none() {
            break;
        }
        if dir.join(".git").exists() {
            root = dir.to_string_lossy().into_owned();
        }
        if !dir.pop() {
            break;
        }
    }
    cache.insert(cwd.to_string(), root.clone());
    root
}

/// `~/…` → absolute path.
pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => emp_core::paths::home_dir().join(rest),
        None if p == "~" => emp_core::paths::home_dir(),
        None => PathBuf::from(p),
    }
}

/// The newest record in a JSONL file that `pick` accepts, scanning backwards. Only lines
/// containing `hint` are parsed. Reads the last 4 MiB first and the whole file only when the
/// tail has no match, so it stays cheap on long sessions.
pub fn last_jsonl<T>(path: &Path, hint: &str, mut pick: impl FnMut(&serde_json::Value) -> Option<T>) -> Result<Option<T>> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 4 << 20;
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut scan = |buf: &[u8], skip_first: bool| {
        let mut lines = buf.split(|b| *b == b'\n').collect::<Vec<_>>();
        if skip_first && !lines.is_empty() {
            lines.remove(0); // probably cut in half
        }
        lines.into_iter().rev().filter(|l| l.windows(hint.len()).any(|w| w == hint.as_bytes())).find_map(|l| {
            serde_json::from_slice::<serde_json::Value>(l.trim_ascii()).ok().and_then(|v| pick(&v))
        })
    };
    let mut buf = Vec::new();
    if len > TAIL {
        file.seek(SeekFrom::Start(len - TAIL))?;
        file.read_to_end(&mut buf)?;
        if let Some(found) = scan(&buf, true) {
            return Ok(Some(found));
        }
        buf.clear();
        file.seek(SeekFrom::Start(0))?;
    }
    file.read_to_end(&mut buf)?;
    Ok(scan(&buf, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_open_creates_no_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("with space?#.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(x); INSERT INTO t VALUES (1);").unwrap();
        }
        // Closing the last writer removes -wal/-shm.
        assert!(!dir.path().join("with space?#.db-wal").exists());
        let c = open_db(&p).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert!(c.execute("INSERT INTO t VALUES (2)", []).is_err());
        drop(c);
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "sidecars created: {names:?}");
    }

    #[test]
    fn nested_repos_group_under_the_outermost() {
        let home = emp_core::paths::home_dir();
        let base = tempfile::tempdir_in(&home).unwrap();
        let studio = base.path().join("studio");
        let video = studio.join("videos/01-x");
        fs::create_dir_all(video.join(".git")).unwrap();
        fs::create_dir_all(studio.join(".git")).unwrap();
        fs::create_dir_all(video.join("src")).unwrap();
        let got = project_root_for(video.join("src").to_str().unwrap());
        assert_eq!(got, studio.to_string_lossy());
    }

    #[test]
    fn temp_dirs_collapse_to_one_root() {
        assert_eq!(project_root_for("/private/var/folders/lf/abc/T/x"), "/private/var/folders");
        assert_eq!(project_root_for("/var/folders/zz/q"), "/private/var/folders");
        assert_eq!(project_root_for("/tmp/emp-test"), "/private/tmp");
        assert!(!is_temp_dir("/Users/me/tmp/project"));
    }

    #[test]
    fn titles_are_single_line_and_bounded() {
        assert_eq!(title_from("\n\n  hello   world \nsecond").as_deref(), Some("hello world"));
        assert_eq!(title_from("   ").as_deref(), None);
        let long = "x".repeat(200);
        assert_eq!(title_from(&long).unwrap().chars().count(), 80);
    }

    #[test]
    fn jsonl_splits_on_newline_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        fs::write(&p, "{\"t\":\"a\u{2028}b\"}\nnot json\n\n{\"t\":2}\n").unwrap();
        let mut vals = Vec::new();
        let mut bad = Vec::new();
        for_each_jsonl(&p, |v| { vals.push(v); std::ops::ControlFlow::Continue(()) }, |n| bad.push(n)).unwrap();
        assert_eq!(vals.len(), 2);
        assert_eq!(vals[0]["t"], "a\u{2028}b");
        assert_eq!(bad, vec![2]);
    }

    #[test]
    fn content_text_handles_strings_and_blocks() {
        let v: serde_json::Value = serde_json::json!([{"type":"text","text":"a"},{"type":"image"},{"type":"text","text":"b"}]);
        assert_eq!(content_text(&v), "a\nb");
        assert_eq!(content_text(&serde_json::json!("plain")), "plain");
    }

    #[test]
    fn last_jsonl_finds_the_newest_match() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        std::fs::write(&p, "{\"n\":1,\"usage\":1}\nnot json usage\n{\"n\":2,\"usage\":2}\n{\"n\":3}\n").unwrap();
        assert_eq!(last_jsonl(&p, "usage", |v| v["n"].as_i64()).unwrap(), Some(2));
        assert_eq!(last_jsonl(&p, "usage", |v| v["n"].as_i64().filter(|n| *n < 2)).unwrap(), Some(1));
        assert_eq!(last_jsonl(&p, "missing", |v| v["n"].as_i64()).unwrap(), None);
    }
}
