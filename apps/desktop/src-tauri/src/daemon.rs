//! empd discovery and the login item.
//!
//! The daemon is a launchd agent (`~/Library/LaunchAgents/com.lucaorion.emp.empd.plist`) that runs
//! the `empd` bundled next to this binary. It starts at login and is restarted after a crash
//! (`KeepAlive { SuccessfulExit = false }`); a clean exit, including "another empd is already
//! running", stays down. The window never owns the daemon: closing EMP leaves agents running.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const LABEL: &str = "com.lucaorion.emp.empd";
/// Fixed so `tailscale serve` (phone access) keeps pointing at the daemon across restarts.
pub const PORT: u16 = 47357;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectInfo {
    pub ws_url: String,
    pub token: String,
    pub daemon_version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Discovery {
    ws_url: String,
    token_path: String,
    daemon_version: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("EMP_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Library/Application Support/EMP"))
}

fn alive(dir: &Path) -> bool {
    UnixStream::connect(dir.join("empd.sock")).is_ok()
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

fn plist_path() -> PathBuf {
    home().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl").args(args).output().is_ok_and(|o| o.status.success())
}

fn job_loaded() -> bool {
    launchctl(&["print", &format!("gui/{}/{LABEL}", uid())])
}

/// The bundled daemon, only when running from an installed .app (never point launchd at a dev build).
fn bundled_empd() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let empd = dir.join("empd");
    (dir.ends_with("Contents/MacOS") && empd.is_file()).then_some(empd)
}

/// The login shell's PATH, so empd (and the agent CLIs it runs, e.g. pi's `env node`) find the
/// same tools as a terminal. launchd would otherwise give `/usr/bin:/bin:/usr/sbin:/sbin`.
fn login_path() -> String {
    let fallback = format!(
        "{h}/.local/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        h = home().display()
    );
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let Ok(out) = Command::new(shell).args(["-ilc", "printf '__EMP_PATH__%s__EMP_END__' \"$PATH\""]).output() else {
        return fallback;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(path) = text.split("__EMP_PATH__").nth(1).and_then(|s| s.split("__EMP_END__").next()) else {
        return fallback;
    };
    let mut seen = std::collections::HashSet::new();
    let dirs: Vec<&str> = path.split(':').filter(|d| !d.is_empty() && seen.insert(*d)).collect();
    if dirs.is_empty() { fallback } else { dirs.join(":") }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plist(empd: &Path, path: &str) -> String {
    let log = home().join("Library/Logs/EMP/empd.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{empd}</string>
    <string>serve</string>
    <string>--ws-port</string>
    <string>{PORT}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        empd = xml(&empd.to_string_lossy()),
        path = xml(path),
        log = xml(&log.to_string_lossy()),
    )
}

/// Writes the login item when it's missing or stale, and loads it unless that would restart a
/// running daemon (a changed plist then applies at the next login or daemon start).
pub fn install_login_item() -> Result<(), String> {
    let Some(empd) = bundled_empd() else { return Ok(()) };
    let path = plist_path();
    let want = plist(&empd, &login_path());
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    if have != want {
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        let _ = std::fs::create_dir_all(home().join("Library/Logs/EMP"));
        std::fs::write(&path, &want).map_err(|e| format!("write {}: {e}", path.display()))?;
        if job_loaded() && !alive(&data_dir()) {
            launchctl(&["bootout", &format!("gui/{}/{LABEL}", uid())]);
        }
    }
    if !job_loaded() {
        launchctl(&["bootstrap", &format!("gui/{}", uid()), &path.to_string_lossy()]);
    }
    Ok(())
}

/// Starts the daemon through launchd when it isn't running, then waits for its socket.
fn ensure_running(dir: &Path) -> Result<(), String> {
    if alive(dir) {
        return Ok(());
    }
    if bundled_empd().is_none() {
        return Err("empd is not running. Start it with `cargo run -p empd -- serve`.".into());
    }
    install_login_item()?;
    // Nothing is running, so reloading is safe: launchd keeps the plist it loaded, and the file
    // may have changed since (a new app version).
    let target = format!("gui/{}/{LABEL}", uid());
    launchctl(&["bootout", &target]);
    launchctl(&["bootstrap", &format!("gui/{}", uid()), &plist_path().to_string_lossy()]);
    launchctl(&["kickstart", &target]);
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if alive(dir) && dir.join("daemon.json").is_file() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err("empd didn't start. See ~/Library/Logs/EMP/empd.log".into())
}

pub fn connect() -> Result<ConnectInfo, String> {
    let dir = data_dir();
    ensure_running(&dir)?;
    let raw = std::fs::read(dir.join("daemon.json")).map_err(|e| format!("read daemon.json: {e}"))?;
    let d: Discovery = serde_json::from_slice(&raw).map_err(|e| format!("daemon.json: {e}"))?;
    let token_path = if Path::new(&d.token_path).is_absolute() { PathBuf::from(&d.token_path) } else { dir.join(&d.token_path) };
    let token = std::fs::read_to_string(&token_path).map_err(|e| format!("read the daemon token: {}", e.kind()))?;
    Ok(ConnectInfo { ws_url: d.ws_url, token: token.trim().to_string(), daemon_version: d.daemon_version })
}

#[cfg(test)]
mod tests {
    #[test]
    fn plist_is_escaped_and_keeps_crash_restarts_only() {
        let p = super::plist(std::path::Path::new("/Applications/EMP.app/Contents/MacOS/empd"), "/a&b:/usr/bin");
        assert!(p.contains("<string>/a&amp;b:/usr/bin</string>"));
        assert!(p.contains("<key>SuccessfulExit</key>\n    <false/>"));
        assert!(p.contains("<string>serve</string>\n    <string>--ws-port</string>\n    <string>47357</string>"));
    }
}
