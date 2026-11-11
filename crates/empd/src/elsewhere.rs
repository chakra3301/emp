//! Sessions open outside EMP: a harness TUI in Terminal, iTerm or another app that has the same
//! session loaded. Two writers corrupt a session (Codex refuses the second itself), so EMP finds
//! the holder, names it, and can close it for the owner, as closing its window would.
//!
//! A holder is a harness process (not one EMP started) whose command line names the session:
//! `codex resume <id>`, `claude --resume <id>`, `pi --session <path>`, … A TUI started fresh
//! (`codex` with no id) can't be matched this way; the mtime guard in `live.rs` and Codex's own
//! single-writer check cover those.

use std::collections::HashMap;

use emp_protocol::{SessionHolder, SessionRef};

use crate::servers::{self, Proc};

/// Programs that hold sessions: the harness binaries, or an interpreter running one.
const HARNESSES: &[&str] = &["codex", "claude", "pi", "hermes", "agy", "gemini", "cursor-agent"];

/// Processes outside EMP with this session open, outermost first (a `node` wrapper before the
/// binary it runs counts once).
pub fn holders(session: &SessionRef) -> Vec<SessionHolder> {
    let Ok(table) = servers::process_table() else { return Vec::new() };
    find(&table, session, std::process::id())
}

fn find(table: &HashMap<u32, Proc>, session: &SessionRef, me: u32) -> Vec<SessionHolder> {
    let keys: Vec<&str> = [Some(session.session_id.as_str()), session.path.as_deref()]
        .into_iter()
        .flatten()
        .filter(|k| k.len() >= 8)
        .collect();
    if keys.is_empty() {
        return Vec::new();
    }
    let matches: Vec<u32> = table
        .iter()
        .filter(|(pid, p)| **pid != me && is_harness(p) && keys.iter().any(|k| p.args.contains(k)))
        .filter(|(pid, _)| !servers::ancestors(**pid, table).contains(&me))
        .map(|(pid, _)| *pid)
        .collect();
    let mut out: Vec<SessionHolder> = matches
        .iter()
        .filter(|pid| table.get(pid).is_none_or(|p| !matches.contains(&p.ppid)))
        .map(|&pid| SessionHolder { pid, app: app_of(pid, table), command: display(&table[&pid].args) })
        .collect();
    out.sort_by_key(|h| h.pid);
    out
}

fn is_harness(p: &Proc) -> bool {
    let mut words = p.args.split_whitespace();
    let Some(first) = words.next() else { return false };
    let first = servers::base(first);
    if HARNESSES.contains(&first) {
        return true;
    }
    // `node /opt/homebrew/bin/codex resume …`
    servers::is_interpreter(first) && words.next().is_some_and(|w| HARNESSES.contains(&servers::base(w)))
}

/// The `.app` the process runs under (`Terminal`, `iTerm2`, `Orion Terminal`), if any.
fn app_of(pid: u32, table: &HashMap<u32, Proc>) -> Option<String> {
    servers::ancestors(pid, table).into_iter().find_map(|a| {
        let exe = &table.get(&a)?.exe;
        let app = exe.split(".app/Contents/MacOS/").next().filter(|_| exe.contains(".app/Contents/MacOS/"))?;
        Some(servers::base(app).to_string())
    })
}

/// `codex resume 01a1…` for `node /opt/homebrew/bin/codex resume 01a1…`.
fn display(args: &str) -> String {
    let mut words: Vec<&str> = args.split_whitespace().collect();
    if words.len() > 1 && servers::is_interpreter(servers::base(words[0])) {
        words.remove(0);
    }
    if let Some(w) = words.first_mut() {
        *w = servers::base(w);
    }
    let line = words.join(" ");
    if line.chars().count() > 120 { format!("{}…", line.chars().take(119).collect::<String>()) } else { line }
}

/// Closes every holder the way closing its window would (SIGHUP), then SIGTERM for any that
/// ignore it. Returns how many went away and who's still there.
pub async fn close(session: &SessionRef) -> (u32, Vec<SessionHolder>) {
    let before = {
        let s = session.clone();
        tokio::task::spawn_blocking(move || holders(&s)).await.unwrap_or_default()
    };
    if before.is_empty() {
        return (0, Vec::new());
    }
    // The wrapper and the binary it runs both get the signal: a wrapper doesn't always forward it.
    let pids: Vec<u32> = {
        let s = session.clone();
        let tops: Vec<u32> = before.iter().map(|h| h.pid).collect();
        tokio::task::spawn_blocking(move || with_children(&tops, &s)).await.unwrap_or_default()
    };
    for sig in [libc::SIGHUP, libc::SIGTERM] {
        for &pid in &pids {
            if servers::alive(pid) {
                servers::signal(pid, sig);
            }
        }
        for _ in 0..15 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if pids.iter().all(|&p| !servers::alive(p)) {
                break;
            }
        }
        if pids.iter().all(|&p| !servers::alive(p)) {
            break;
        }
    }
    let after = {
        let s = session.clone();
        tokio::task::spawn_blocking(move || holders(&s)).await.unwrap_or_default()
    };
    let closed = before.iter().filter(|h| !after.iter().any(|a| a.pid == h.pid)).count() as u32;
    (closed, after)
}

/// `tops` plus their descendants that also hold the session.
fn with_children(tops: &[u32], session: &SessionRef) -> Vec<u32> {
    let Ok(table) = servers::process_table() else { return tops.to_vec() };
    let mut out = tops.to_vec();
    let keys = [session.session_id.as_str()];
    for (pid, p) in &table {
        if !out.contains(pid) && is_harness(p) && keys.iter().any(|k| p.args.contains(k)) && servers::ancestors(*pid, &table).iter().any(|a| tops.contains(a)) {
            out.push(*pid);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(ppid: u32, exe: &str, args: &str) -> Proc {
        Proc { ppid, exe: exe.into(), args: args.into(), ..Proc::default() }
    }

    #[test]
    fn finds_the_terminal_tui_once() {
        let id = "01a116fc-a56b-7421-99e6-449f5c242876";
        let mut t = HashMap::new();
        t.insert(500, proc(1, "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal", "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"));
        t.insert(32808, proc(500, "login", "login -pfl luca /bin/bash -c exec -la zsh /bin/zsh"));
        t.insert(32811, proc(32808, "-zsh", "-zsh"));
        t.insert(41923, proc(32811, "node", &format!("node /opt/homebrew/bin/codex resume {id}")));
        t.insert(41927, proc(41923, "/opt/homebrew/lib/node_modules/@openai/codex/vendor/bin/codex", &format!("/opt/homebrew/lib/node_modules/@openai/codex/vendor/bin/codex resume {id}")));
        // EMP's own driver for the same session, and an unrelated tail of the rollout.
        t.insert(900, proc(1, "/Applications/EMP.app/Contents/MacOS/empd", "empd serve"));
        t.insert(901, proc(900, "codex", "codex app-server"));
        t.insert(902, proc(900, "codex", &format!("codex resume {id}")));
        t.insert(950, proc(32811, "tail", &format!("tail -f /x/rollout-{id}.jsonl")));
        let session = SessionRef { harness: emp_protocol::Harness::Codex, session_id: id.into(), path: None, home: None, profile: None };
        let found = find(&t, &session, 900);
        assert_eq!(found, [SessionHolder { pid: 41923, app: Some("Terminal".into()), command: format!("codex resume {id}") }]);
    }

    #[test]
    fn short_or_missing_ids_match_nothing() {
        let mut t = HashMap::new();
        t.insert(10, proc(1, "codex", "codex resume abc"));
        let session = SessionRef { harness: emp_protocol::Harness::Codex, session_id: "abc".into(), path: None, home: None, profile: None };
        assert!(find(&t, &session, 1).is_empty());
    }
}
