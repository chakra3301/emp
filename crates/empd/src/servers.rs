//! Dev servers: the owner's processes that listen on a TCP port, so stale ones can be found
//! and stopped. One `lsof` for sockets, two `ps` for the process table, one `lsof` for cwds.
//! Kill re-scans first and only signals a pid that is still listening and isn't EMP.

use std::collections::{BTreeSet, HashMap};
use std::process::Command;
use std::time::{Duration, Instant};

use emp_protocol::{DevServer, RpcError, ServerKind, ServersKillParams, ServersKillResult, codes};

use crate::state::State;

const LSOF: &str = "/usr/sbin/lsof";
const PS: &str = "/bin/ps";
const KILL_WAIT: Duration = Duration::from_secs(3);

fn error(code: i32, message: impl Into<String>) -> RpcError {
    RpcError { code, message: message.into(), data: None }
}

pub async fn list(state: &State) -> Result<Vec<DevServer>, RpcError> {
    let terminals = terminal_pids(state);
    tokio::task::spawn_blocking(move || scan(&terminals))
        .await
        .map_err(|e| error(codes::INTERNAL, e.to_string()))?
        .map_err(|e| error(codes::INTERNAL, format!("couldn't list listening ports: {e}")))
}

pub async fn kill(state: &State, p: ServersKillParams) -> Result<ServersKillResult, RpcError> {
    let servers = list(state).await?;
    let Some(server) = servers.into_iter().find(|s| s.pid == p.pid) else {
        return Err(error(codes::NOT_FOUND, "that process isn't listening on a port any more"));
    };
    if server.protected {
        return Err(error(codes::INVALID_PARAMS, "EMP's own daemon can't be stopped from here"));
    }
    let force = p.force.unwrap_or(false);
    let pid = server.pid;
    tokio::task::spawn_blocking(move || {
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        // Restarters (nodemon, `tsx watch`, `next dev`, npm) would bring the server straight
        // back, so they go first. Shells and EMP terminals stop the walk.
        let table = process_table().unwrap_or_default();
        for parent in runner_ancestors(pid, &table) {
            signal(parent, sig);
        }
        signal(pid, sig);
        let deadline = Instant::now() + KILL_WAIT;
        while Instant::now() < deadline {
            if !alive(pid) {
                return ServersKillResult { stopped: true };
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        ServersKillResult { stopped: !alive(pid) }
    })
    .await
    .map_err(|e| error(codes::INTERNAL, e.to_string()))
}

fn terminal_pids(state: &State) -> HashMap<u32, String> {
    state.terminals.list().into_iter().filter_map(|t| t.pid.map(|pid| (pid, t.id))).collect()
}

pub(crate) fn signal(pid: u32, sig: libc::c_int) {
    if pid > 1 {
        // SAFETY: plain kill(2) on a pid the owner chose from the listener list.
        unsafe {
            libc::kill(pid as libc::pid_t, sig);
        }
    }
}

/// Still running and not a zombie waiting for its parent.
pub(crate) fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return false;
    }
    let out = Command::new(PS).args(["-o", "stat=", "-p", &pid.to_string()]).output();
    match out {
        Ok(o) => {
            let stat = String::from_utf8_lossy(&o.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        }
        Err(_) => true,
    }
}

// ---- scanning -----------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub(crate) struct Proc {
    pub ppid: u32,
    pub elapsed_secs: i64,
    pub rss_kb: u64,
    pub exe: String,
    pub args: String,
}

#[derive(Debug, Default)]
struct Sockets {
    /// Listening (address, port) per pid.
    listening: HashMap<u32, Vec<(String, u16)>>,
    /// Local port of every established connection per pid.
    established: HashMap<u32, Vec<u16>>,
}

fn scan(terminals: &HashMap<u32, String>) -> anyhow::Result<Vec<DevServer>> {
    // SAFETY: getuid never fails.
    let uid = unsafe { libc::getuid() };
    let out = Command::new(LSOF)
        .args(["-nP", "-a", "-u", &uid.to_string(), "-iTCP", "-sTCP:LISTEN,ESTABLISHED", "-F", "pnT"])
        .output()?;
    // lsof exits 1 when a process vanished mid-scan; whatever it printed is still good.
    let sockets = parse_lsof(&String::from_utf8_lossy(&out.stdout));
    if sockets.listening.is_empty() {
        return Ok(Vec::new());
    }
    let table = process_table()?;
    let pids: Vec<u32> = sockets.listening.keys().copied().collect();
    let cwds = cwds(&pids);
    let home = std::env::var("HOME").unwrap_or_default();
    let me = std::process::id();
    let mine = ancestors(me, &table);
    let now = emp_core::now_ms();

    let mut servers: Vec<DevServer> = sockets
        .listening
        .iter()
        .filter_map(|(&pid, addrs)| {
            let p = table.get(&pid)?;
            let ports: BTreeSet<u16> = addrs.iter().map(|(_, port)| *port).collect();
            let exposed = addrs.iter().any(|(a, _)| a == "*" || a == "0.0.0.0" || a == "[::]");
            let connections = sockets.established.get(&pid).map_or(0, |locals| locals.iter().filter(|l| ports.contains(l)).count()) as u32;
            let cwd = cwds.get(&pid).cloned();
            let terminal_id = ancestors(pid, &table).iter().find_map(|a| terminals.get(a).cloned()).or_else(|| terminals.get(&pid).cloned());
            Some(DevServer {
                pid,
                ppid: p.ppid,
                name: label(&p.exe, &p.args),
                command: p.args.clone(),
                kind: classify(&p.exe, &p.args, cwd.as_deref(), &home),
                cwd,
                ports: ports.into_iter().collect(),
                exposed,
                connections,
                started_at: now - p.elapsed_secs * 1000,
                memory: p.rss_kb * 1024,
                terminal_id,
                // Any EMP daemon: stopping one from its own UI would cut the UI off.
                protected: pid == me || pid <= 1 || mine.contains(&pid) || base(&p.exe) == "empd",
            })
        })
        .collect();
    servers.sort_by(|a, b| a.ports.first().cmp(&b.ports.first()).then(a.pid.cmp(&b.pid)));
    Ok(servers)
}

fn parse_lsof(text: &str) -> Sockets {
    let mut s = Sockets::default();
    let mut pid = 0u32;
    let mut name: Option<String> = None;
    let flush = |s: &mut Sockets, pid: u32, name: Option<String>, st: &str| {
        let Some(name) = name else { return };
        match st {
            "LISTEN" if !name.contains("->") => {
                if let Some((addr, port)) = split_port(&name) {
                    let v = s.listening.entry(pid).or_default();
                    if !v.contains(&(addr.clone(), port)) {
                        v.push((addr, port));
                    }
                }
            }
            "ESTABLISHED" => {
                if let Some((local, _)) = name.split_once("->")
                    && let Some((_, port)) = split_port(local)
                {
                    s.established.entry(pid).or_default().push(port);
                }
            }
            _ => {}
        }
    };
    for line in text.lines() {
        let (tag, rest) = line.split_at(line.len().min(1));
        match tag {
            "p" => {
                pid = rest.parse().unwrap_or(0);
                name = None;
            }
            "f" => name = None,
            "n" => name = Some(rest.to_string()),
            "T" => {
                if let Some(st) = rest.strip_prefix("ST=") {
                    flush(&mut s, pid, name.take(), st);
                }
            }
            _ => {}
        }
    }
    s
}

/// `127.0.0.1:5173`, `*:3000`, `[::1]:5432` → (address, port).
fn split_port(addr: &str) -> Option<(String, u16)> {
    let (host, port) = addr.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

/// pid → ppid, age, memory, executable and arguments for every process. `comm` and `args` can
/// both contain spaces, so each comes from its own `ps` as the last column.
pub(crate) fn process_table() -> anyhow::Result<HashMap<u32, Proc>> {
    let info = Command::new(PS).args(["-axww", "-o", "pid=,ppid=,etime=,rss=,comm="]).output()?;
    let args = Command::new(PS).args(["-axww", "-o", "pid=,args="]).output()?;
    let mut table: HashMap<u32, Proc> = HashMap::new();
    for line in String::from_utf8_lossy(&info.stdout).lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(etime), Some(rss)) = (it.next(), it.next(), it.next(), it.next()) else { continue };
        let Ok(pid) = pid.parse::<u32>() else { continue };
        let exe = rest_after(line, 4);
        table.insert(
            pid,
            Proc {
                ppid: ppid.parse().unwrap_or(0),
                elapsed_secs: parse_etime(etime).unwrap_or(0),
                rss_kb: rss.parse().unwrap_or(0),
                exe: exe.trim().to_string(),
                args: String::new(),
            },
        );
    }
    for line in String::from_utf8_lossy(&args.stdout).lines() {
        let Some(pid) = line.split_whitespace().next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if let Some(p) = table.get_mut(&pid) {
            p.args = rest_after(line, 1).trim().to_string();
        }
    }
    Ok(table)
}

/// The text after the first `n` whitespace-separated columns.
fn rest_after(line: &str, n: usize) -> &str {
    let mut rest = line.trim_start();
    for _ in 0..n {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = &rest[end..];
    }
    rest.trim_start()
}

/// `ps` elapsed time: `[[dd-]hh:]mm:ss`.
fn parse_etime(s: &str) -> Option<i64> {
    let (days, clock) = match s.split_once('-') {
        Some((d, c)) => (d.parse::<i64>().ok()?, c),
        None => (0, s),
    };
    let parts: Vec<i64> = clock.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [m, s] => (0, *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    Some(days * 86_400 + h * 3600 + m * 60 + sec)
}

fn cwds(pids: &[u32]) -> HashMap<u32, String> {
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let Ok(out) = Command::new(LSOF).args(["-a", "-p", &list, "-d", "cwd", "-Fn"]).output() else { return HashMap::new() };
    let mut map = HashMap::new();
    let mut pid = 0;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(n) = line.strip_prefix('n')
            && pid != 0
        {
            map.insert(pid, n.to_string());
        }
    }
    map
}

pub(crate) fn ancestors(pid: u32, table: &HashMap<u32, Proc>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = pid;
    while let Some(p) = table.get(&cur) {
        if p.ppid <= 1 || out.contains(&p.ppid) || out.len() > 64 {
            break;
        }
        out.push(p.ppid);
        cur = p.ppid;
    }
    out
}

/// Parents that would restart or wrap the server: package runners and file watchers, up to the
/// first shell. `sh -c` (how npm runs scripts) is passed through, never killed.
fn runner_ancestors(pid: u32, table: &HashMap<u32, Proc>) -> Vec<u32> {
    let mut out = Vec::new();
    for a in ancestors(pid, table).into_iter().take(4) {
        let Some(p) = table.get(&a) else { break };
        if is_runner(&p.args) {
            out.push(a);
        } else if !is_sh_c(&p.args) {
            break;
        }
    }
    out
}

pub(crate) fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

const INTERPRETERS: &[&str] = &["node", "bun", "deno", "python", "python3", "ruby", "php", "java", "perl", "beam.smp", "dotnet"];
const SERVICES: &[&str] = &[
    "postgres", "postmaster", "redis-server", "mysqld", "mariadbd", "mongod", "memcached", "docker", "com.docker.backend", "limactl", "colima", "adb", "ollama",
    "qemu-system-aarch64", "vpnkit", "nginx", "httpd", "caddy", "rabbitmq-server", "clickhouse", "meilisearch", "typesense-server", "temporal",
];
const AGENTS: &[&str] = &["codex", "agy", "claude", "pi", "hermes", "gemini", "cursor-agent", "empd", "emp-desktop"];
/// Paths that belong to coding agents and their extensions.
const AGENT_MARKERS: &[&str] = &["/.pi/", "/.hermes/", "/.claude/", "/.codex/", "/.gemini/", "/.cursor/", "@openai/codex", "@anthropic-ai/claude-code", "@earendil-works/"];

pub(crate) fn is_interpreter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    INTERPRETERS.contains(&n.as_str()) || (n.starts_with("python") && n[6..].chars().all(|c| c.is_ascii_digit() || c == '.'))
}

fn classify(exe: &str, args: &str, cwd: Option<&str>, home: &str) -> ServerKind {
    let name = base(exe);
    let first = base(args.split_whitespace().next().unwrap_or(""));
    // A harness's bundled node (`~/.hermes/node/bin/node`) may run the owner's own server, so
    // for interpreters only the script decides.
    let script = rest_after(args, 1);
    let marked = |s: &str| AGENT_MARKERS.iter().any(|m| s.contains(m));
    if AGENTS.contains(&name) || AGENTS.contains(&first) || marked(script) || (!is_interpreter(name) && marked(exe)) {
        return ServerKind::Agent;
    }
    if is_interpreter(name) {
        return ServerKind::Dev;
    }
    if exe.starts_with("/System/") || exe.starts_with("/usr/libexec/") || exe.starts_with("/usr/sbin/") || exe.starts_with("/sbin/") || exe.contains(".app/Contents/") {
        return ServerKind::App;
    }
    if SERVICES.contains(&name) || SERVICES.contains(&first) || name.starts_with("com.docker") {
        return ServerKind::Service;
    }
    if exe.contains("/target/debug/") || exe.contains("/target/release/") || exe.contains("node_modules/") {
        return ServerKind::Dev;
    }
    // Renamed node processes (`next-server (v16)`) and project-built binaries run from a project.
    let in_projects = cwd.is_some_and(|c| {
        let in_home = !home.is_empty() && c.starts_with(home) && c != home && !c.starts_with(&format!("{home}/Library"));
        in_home || c.starts_with("/tmp/") || c.starts_with("/private/tmp/") || c.starts_with("/Volumes/")
    });
    if in_projects { ServerKind::Dev } else { ServerKind::Service }
}

fn is_runner(args: &str) -> bool {
    let words: Vec<&str> = args.split_whitespace().collect();
    // Skip the interpreter: `node /opt/homebrew/bin/npm run dev`.
    let skip = usize::from(words.first().is_some_and(|w| is_interpreter(base(w))));
    let Some(cmd) = words.get(skip).map(|w| base(w)) else { return false };
    let sub = words.get(skip + 1).copied().unwrap_or("");
    match cmd {
        "npm" | "npx" | "pnpm" | "yarn" | "nodemon" | "concurrently" | "turbo" | "watchexec" | "cargo-watch" => true,
        "bun" => matches!(sub, "run" | "x" | "--watch" | "--hot"),
        "tsx" => sub == "watch",
        "next" => matches!(sub, "dev" | "start"),
        "cargo" => sub == "watch",
        _ => words.iter().skip(skip).any(|w| *w == "--watch"),
    }
}

fn is_sh_c(args: &str) -> bool {
    let mut it = args.split_whitespace();
    matches!(it.next().map(base), Some("sh" | "bash" | "zsh")) && it.next() == Some("-c")
}

/// `vite preview`, `serve.mjs pokesurvivor`, `python -m http.server`, `next-server (v16.3.4)`.
fn label(exe: &str, args: &str) -> String {
    // App bundles: paths have spaces (`Application Support`), but the binary name is clean.
    if exe.contains(".app/Contents/") && !is_interpreter(base(exe)) {
        return truncate(base(exe), 48);
    }
    let words: Vec<&str> = args.split_whitespace().collect();
    let Some(first) = words.first() else { return base(exe).to_string() };
    if !first.contains('/') && !is_interpreter(first) && !exe.ends_with(first) && !AGENTS.contains(first) && words.len() <= 3 {
        // A process that set its own title.
        return truncate(args.trim(), 48);
    }
    let first_name = base(first);
    let mut rest = words[1..].iter().copied();
    let mut out: Vec<String> = Vec::new();
    if is_interpreter(first_name) {
        let interp = if first_name.to_ascii_lowercase().starts_with("python") { "python".to_string() } else { first_name.to_string() };
        loop {
            let Some(w) = rest.next() else { return interp };
            if w == "-m" {
                return match rest.next() {
                    Some(m) => format!("{interp} -m {m}"),
                    None => interp,
                };
            }
            if w == "--import" || w == "--require" || w == "-r" || w == "--loader" {
                rest.next();
                continue;
            }
            if w.starts_with('-') {
                continue;
            }
            out.push(script_name(w));
            break;
        }
    } else {
        out.push(first_name.to_string());
    }
    if let Some(w) = rest.find(|w| !w.starts_with('-') && !w.contains('/') && !w.contains('=') && w.parse::<u32>().is_err()) {
        out.push(w.to_string());
    }
    truncate(&out.join(" "), 48)
}

/// `node_modules/.bin/vite` → `vite`; `server/index.ts` stays; long paths keep their file name.
fn script_name(path: &str) -> String {
    if let Some((_, bin)) = path.rsplit_once("node_modules/.bin/") {
        return bin.to_string();
    }
    if let Some((_, pkg)) = path.rsplit_once("node_modules/") {
        return pkg.split('/').next().unwrap_or(pkg).to_string();
    }
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [] => path.to_string(),
        [.., dir, file] if file.starts_with("index.") || file.starts_with("main.") || file.starts_with("server.") => format!("{dir}/{file}"),
        [.., file] => file.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listeners_and_connections() {
        let text = "p100\nf10\nn*:5173\nTST=LISTEN\nTQR=0\nf11\nn[::1]:5173\nTST=LISTEN\nf12\nn127.0.0.1:5173->127.0.0.1:60000\nTST=ESTABLISHED\n\
                    f13\nn127.0.0.1:61000->1.2.3.4:443\nTST=ESTABLISHED\np200\nf3\nn127.0.0.1:60000->127.0.0.1:5173\nTST=ESTABLISHED\n";
        let s = parse_lsof(text);
        assert_eq!(s.listening[&100], vec![("*".to_string(), 5173), ("[::1]".to_string(), 5173)]);
        assert_eq!(s.established[&100], vec![5173, 61000]);
        assert!(!s.listening.contains_key(&200));
    }

    #[test]
    fn etime() {
        assert_eq!(parse_etime("05:07"), Some(307));
        assert_eq!(parse_etime("15:52:41"), Some(57_161));
        assert_eq!(parse_etime("01-00:20:41"), Some(87_641));
        assert_eq!(parse_etime("x"), None);
    }

    #[test]
    fn columns_with_spaces() {
        assert_eq!(rest_after(" 39805 95000  15:54 15904 /Applications/ego lite.app/x", 4), "/Applications/ego lite.app/x");
        assert_eq!(rest_after("  7 node a b", 1), "node a b");
    }

    #[test]
    fn labels() {
        assert_eq!(label("/opt/homebrew/bin/node", "node /Users/x/site/node_modules/.bin/vite preview --host 127.0.0.1 --port 4390"), "vite preview");
        assert_eq!(label("/opt/homebrew/bin/node", "node tools/serve.mjs pokesurvivor"), "serve.mjs pokesurvivor");
        assert_eq!(label("/opt/homebrew/opt/node@24/bin/node", "/opt/homebrew/opt/node@24/bin/node --import tsx server/index.ts"), "server/index.ts");
        assert_eq!(label("/x/Python.app/Contents/MacOS/Python", "/x/Python.app/Contents/MacOS/Python -u -m http.server 43991 --directory s"), "python -m http.server");
        assert_eq!(label("next-server (v16.3.4)", "next-server (v16.3.4)"), "next-server (v16.3.4)");
        assert_eq!(label("/opt/homebrew/bin/node", "node artifacts/dungeonview/serve.mjs 4389"), "serve.mjs");
        assert_eq!(label("/Users/x/p/target/debug/api", "/Users/x/p/target/debug/api serve"), "api serve");
        assert_eq!(label("/Users/x/Library/Application Support/Figma/FigmaAgent.app/Contents/MacOS/figma_agent", "/Users/x/Library/Application Support/Figma/FigmaAgent.app/Contents/MacOS/figma_agent"), "figma_agent");
    }

    #[test]
    fn kinds() {
        let home = "/Users/x";
        let k = |exe: &str, args: &str, cwd: Option<&str>| classify(exe, args, cwd, home);
        assert_eq!(k("/opt/homebrew/bin/node", "node tools/serve.mjs", Some("/Users/x/game")), ServerKind::Dev);
        assert_eq!(k("/x/Python.app/Contents/MacOS/Python", "Python -m http.server", Some("/Users/x/s")), ServerKind::Dev);
        assert_eq!(k("next-server (v16.3.4)", "next-server (v16.3.4)", Some("/Users/x/ui")), ServerKind::Dev);
        assert_eq!(k("/opt/homebrew/opt/postgresql@14/bin/postgres", "postgres -D /v", Some("/opt/homebrew/var")), ServerKind::Service);
        assert_eq!(k("/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter", "ControlCenter", Some("/")), ServerKind::App);
        assert_eq!(k("/Applications/ego lite.app/Contents/MacOS/ego Helper (Node)", "ego Helper (Node) --type=utility", Some("/")), ServerKind::App);
        assert_eq!(k("/opt/homebrew/lib/node_modules/@openai/codex/vendor/codex", "codex --yolo", Some("/Users/x/p")), ServerKind::Agent);
        assert_eq!(k("/Users/x/.hermes/node/bin/node", "node /Users/x/.pi/agent/npm/billion-context/dist/index.js start", Some("/Users/x")), ServerKind::Agent);
        assert_eq!(k("/usr/sbin/rapportd", "rapportd", Some("/")), ServerKind::App);
        assert_eq!(k("/opt/homebrew/bin/mystery", "mystery", Some("/")), ServerKind::Service);
        assert_eq!(k("/Users/x/.hermes/node/bin/node", "/Users/x/.hermes/node/bin/node server.js", Some("/Users/x/app")), ServerKind::Dev);
    }

    #[test]
    fn runners() {
        assert!(is_runner("node /opt/homebrew/bin/npm run dev"));
        assert!(is_runner("npm exec vite"));
        assert!(is_runner("node /x/node_modules/.bin/tsx watch src/index.ts"));
        assert!(is_runner("node /x/node_modules/.bin/next dev"));
        assert!(is_runner("node --watch server.js"));
        assert!(!is_runner("node server.js"));
        assert!(!is_runner("-zsh"));
        assert!(is_sh_c("sh -c vite"));
        assert!(!is_sh_c("zsh -l"));
    }

    #[test]
    fn runner_walk_stops_at_the_shell() {
        let p = |ppid: u32, args: &str| Proc { ppid, args: args.into(), ..Proc::default() };
        let table: HashMap<u32, Proc> =
            [(10, p(9, "node /x/.bin/vite")), (9, p(8, "sh -c vite")), (8, p(7, "node /opt/homebrew/bin/npm run dev")), (7, p(6, "-zsh")), (6, p(1, "npm run outer"))].into();
        assert_eq!(runner_ancestors(10, &table), vec![8]);
        assert_eq!(ancestors(10, &table), vec![9, 8, 7, 6]);
    }
}
