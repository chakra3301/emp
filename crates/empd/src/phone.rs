//! Phone access: the web app over `tailscale serve`, and paired devices.
//!
//! - The daemon serves the EMP web app and `/ws` on 127.0.0.1. `tailscale serve` (HTTPS, tailnet
//!   only — never `funnel`) proxies `https://<mac>.<tailnet>.ts.net` to it.
//! - A phone pairs with a short code from Settings → Phone (also carried in a QR link) and gets
//!   its own random token. Only a SHA-256 of it is stored; revoking a device drops its live
//!   connections at once.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use emp_core::now_ms;
use emp_protocol::{Device, PairingCode, PhoneStatus};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::sync::broadcast;

use crate::doctor;
use crate::state::State;

const PAIR_TTL_MS: i64 = 10 * 60 * 1000;
/// A code can pair a few devices (Safari and its home-screen copy keep separate storage on iOS).
const MAX_REDEEMS: u32 = 3;
/// Wrong guesses before the code is thrown away.
const MAX_FAILURES: u32 = 10;
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

struct Pairing {
    code: String,
    expires_at: i64,
    redeemed: u32,
    failures: u32,
}

/// Daemon-side phone state.
pub struct Phone {
    /// token hash → device id
    devices: RwLock<HashMap<String, String>>,
    pairing: Mutex<Option<Pairing>>,
    /// Revoked device ids; connections authenticated as one of them close.
    pub kicks: broadcast::Sender<String>,
    /// Local port and whether it was asked for (a fixed port) rather than picked at random.
    listen: Mutex<Option<(u16, bool)>>,
    /// This Mac's tailnet name, cached for the WebSocket origin check.
    tailnet_name: Mutex<Option<String>>,
    pub web_dir: Mutex<Option<PathBuf>>,
}

impl Phone {
    pub fn new(devices: Vec<(String, String)>) -> Self {
        Phone {
            devices: RwLock::new(devices.into_iter().collect()),
            pairing: Mutex::new(None),
            kicks: broadcast::channel(16).0,
            listen: Mutex::new(None),
            tailnet_name: Mutex::new(None),
            web_dir: Mutex::new(None),
        }
    }

    pub fn set_listen(&self, port: u16, fixed: bool) {
        *self.listen.lock().unwrap() = Some((port, fixed));
    }

    /// The local HTTP/WebSocket port (0 before `serve` binds it).
    pub fn port(&self) -> u16 {
        self.listen().0
    }

    fn listen(&self) -> (u16, bool) {
        self.listen.lock().unwrap().unwrap_or((0, false))
    }

    /// The device a token belongs to, if any.
    pub fn device_for(&self, token: &str) -> Option<String> {
        self.devices.read().unwrap().get(&hash_token(token)).cloned()
    }

    pub fn tailnet_name(&self) -> Option<String> {
        self.tailnet_name.lock().unwrap().clone()
    }
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn normalize_code(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

fn new_code() -> String {
    let mut rng = rand::rng();
    (0..8).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
}

fn pretty(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}

/// Finds the web app: `--web-dir`/`$EMP_WEB_DIR`, the app bundle's `Resources/web`, or the
/// repo's `apps/desktop/dist` (dev builds).
pub fn resolve_web_dir(explicit: Option<PathBuf>) -> Option<PathBuf> {
    let has_index = |d: &PathBuf| d.join("index.html").is_file();
    if let Some(d) = explicit.or_else(|| std::env::var_os("EMP_WEB_DIR").map(PathBuf::from)) {
        return has_index(&d).then_some(d);
    }
    let bundled = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("../Resources/web")));
    let dev = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/desktop/dist"));
    bundled.into_iter().chain([dev]).find(has_index)
}

// ---- tailscale --------------------------------------------------------------------------------

fn tailscale_bin() -> Option<PathBuf> {
    doctor::which("tailscale").or_else(|| {
        let app = PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
        app.is_file().then_some(app)
    })
}

struct Run {
    ok: bool,
    output: String,
}

/// Runs the tailscale CLI with a timeout; output is kept even when it had to be killed.
async fn tailscale(args: &[&str], timeout: Duration) -> Option<Run> {
    let bin = tailscale_bin()?;
    let mut child = tokio::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let read = tokio::spawn(async move {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let _ = tokio::join!(out.read_to_end(&mut a), err.read_to_end(&mut b));
        a.extend_from_slice(&b);
        a
    });
    let ok = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.kill().await;
            false
        }
    };
    let output = String::from_utf8_lossy(&read.await.unwrap_or_default()).into_owned();
    Some(Run { ok, output })
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TsStatus {
    backend_state: Option<String>,
    #[serde(rename = "Self")]
    me: Option<TsSelf>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TsSelf {
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
}

/// `(state, dns name)`.
async fn tailscale_state() -> (String, Option<String>) {
    let Some(run) = tailscale(&["status", "--json"], Duration::from_secs(8)).await else {
        return ("missing".into(), None);
    };
    let Ok(st) = serde_json::from_str::<TsStatus>(&run.output) else { return ("unknown".into(), None) };
    let state = match st.backend_state.as_deref() {
        Some("Running") => "running",
        Some("Stopped") => "stopped",
        Some("NeedsLogin" | "NoState" | "NeedsMachineAuth") => "needs-login",
        _ => "unknown",
    };
    let dns = st.me.and_then(|m| m.dns_name).map(|d| d.trim_end_matches('.').to_string()).filter(|d| !d.is_empty());
    (state.into(), dns)
}

/// The proxy target `tailscale serve` uses for this Mac's HTTPS root, if any.
async fn serve_target() -> Option<String> {
    let run = tailscale(&["serve", "status", "--json"], Duration::from_secs(8)).await?;
    serve_target_from(&run.output)
}

fn serve_target_from(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v["Web"]
        .as_object()?
        .iter()
        .find(|(host, _)| host.ends_with(":443"))
        .and_then(|(_, cfg)| cfg["Handlers"]["/"]["Proxy"].as_str().map(String::from))
}

fn is_ours(target: &str, port: u16) -> bool {
    let t = target.trim_end_matches('/');
    [format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}"), format!("127.0.0.1:{port}"), format!("localhost:{port}")]
        .iter()
        .any(|o| t == o)
}

pub async fn status(state: &State) -> anyhow::Result<PhoneStatus> {
    let (ts, dns) = tailscale_state().await;
    *state.phone.tailnet_name.lock().unwrap() = dns.clone();
    let (port, port_fixed) = state.phone.listen();
    let target = if ts == "missing" { None } else { serve_target().await };
    let ours = target.as_deref().is_some_and(|t| is_ours(t, port));
    let devices = state.with_store(|s| s.list_devices()).await?;
    Ok(PhoneStatus {
        url: (ours && ts == "running").then(|| dns.as_ref().map(|d| format!("https://{d}"))).flatten(),
        serving_other: target.filter(|_| !ours),
        tailscale: ts,
        dns_name: dns,
        port,
        port_fixed,
        devices,
    })
}

/// Turns on `tailscale serve` for this daemon. Errors are written for the person reading them.
pub async fn enable(state: &State) -> Result<PhoneStatus, String> {
    let st = status(state).await.map_err(|e| e.to_string())?;
    match st.tailscale.as_str() {
        "missing" => return Err("Install Tailscale first (tailscale.com/download), on this Mac and on your phone.".into()),
        "running" => {}
        "needs-login" => return Err("Sign in to Tailscale on this Mac first (open the Tailscale app).".into()),
        _ => return Err("Turn Tailscale on first: open the Tailscale app on this Mac and connect.".into()),
    }
    if !st.port_fixed {
        return Err("This daemon was started by hand on a random port. Restart EMP's own daemon first (Settings → Phone explains how).".into());
    }
    if let Some(other) = &st.serving_other {
        return Err(format!("Tailscale already serves something else on this Mac's HTTPS address ({other}). Turn that off first, or ask to move EMP to another port."));
    }
    if st.url.is_some() {
        return Ok(st);
    }
    let target = format!("http://127.0.0.1:{}", st.port);
    let run = tailscale(&["serve", "--bg", "--yes", "--https=443", &target], Duration::from_secs(20))
        .await
        .ok_or("Couldn't run the tailscale command.")?;
    if !run.ok {
        // HTTPS isn't enabled for the tailnet: Tailscale prints a link to switch it on.
        if let Some(link) = run.output.split_whitespace().find(|w| w.starts_with("https://login.tailscale.com/")) {
            let _ = std::process::Command::new("/usr/bin/open").arg(link).status();
            return Err("Tailscale needs HTTPS switched on for your network. A browser window just opened to do that; then press Turn on again.".into());
        }
        let msg = run.output.trim();
        return Err(if msg.is_empty() { "tailscale serve failed.".into() } else { format!("tailscale serve failed: {msg}") });
    }
    status(state).await.map_err(|e| e.to_string())
}

/// Turns off `tailscale serve`, only when it points at this daemon.
pub async fn disable(state: &State) -> Result<PhoneStatus, String> {
    let st = status(state).await.map_err(|e| e.to_string())?;
    if st.url.is_some() {
        let run = tailscale(&["serve", "--https=443", "off"], Duration::from_secs(10))
            .await
            .ok_or("Couldn't run the tailscale command.")?;
        if !run.ok {
            return Err(format!("tailscale serve off failed: {}", run.output.trim()));
        }
    }
    status(state).await.map_err(|e| e.to_string())
}

// ---- pairing ----------------------------------------------------------------------------------

/// A fresh pairing code (replacing any earlier one), with the QR link when phone access is on.
pub async fn pair(state: &State) -> anyhow::Result<PairingCode> {
    let code = new_code();
    let expires_at = now_ms() + PAIR_TTL_MS;
    *state.phone.pairing.lock().unwrap() = Some(Pairing { code: code.clone(), expires_at, redeemed: 0, failures: 0 });
    let url = status(state).await.ok().and_then(|s| s.url).map(|u| format!("{u}/#pair={}", pretty(&code)));
    let qr_svg = url.as_deref().and_then(qr_svg);
    Ok(PairingCode { code: pretty(&code), url, qr_svg, expires_at })
}

fn qr_svg(text: &str) -> Option<String> {
    use qrcode::render::svg;
    let qr = qrcode::QrCode::new(text.as_bytes()).ok()?;
    Some(
        qr.render::<svg::Color>()
            .min_dimensions(232, 232)
            .quiet_zone(true)
            .dark_color(svg::Color("#1f2328"))
            .light_color(svg::Color("#ffffff"))
            .build(),
    )
}

#[derive(Deserialize)]
pub struct RedeemRequest {
    pub code: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedeemResponse {
    pub token: String,
    pub device: Device,
}

/// Exchanges a pairing code for a new device token.
pub async fn redeem(state: &State, req: RedeemRequest) -> Result<RedeemResponse, String> {
    let given = normalize_code(&req.code);
    {
        let mut slot = state.phone.pairing.lock().unwrap();
        let Some(p) = slot.as_mut() else {
            return Err("No pairing code is active. On your Mac, open EMP → Settings → Phone and press Pair a phone.".into());
        };
        if now_ms() > p.expires_at {
            *slot = None;
            return Err("That code has expired. Get a new one in Settings → Phone on your Mac.".into());
        }
        if !crate::rpc::constant_time_eq(given.as_bytes(), p.code.as_bytes()) {
            p.failures += 1;
            if p.failures >= MAX_FAILURES {
                *slot = None;
            }
            return Err("That code doesn't match. Check it in Settings → Phone on your Mac.".into());
        }
        p.redeemed += 1;
        if p.redeemed >= MAX_REDEEMS {
            *slot = None;
        }
    }
    let token = hex::encode(rand::rng().random::<[u8; 32]>());
    let hash = hash_token(&token);
    let name = req.name.clone();
    let h = hash.clone();
    let device = state.with_store(move |s| s.create_device(&name, &h)).await.map_err(|e| e.to_string())?;
    state.phone.devices.write().unwrap().insert(hash, device.id.clone());
    tracing::info!(device = %device.name, "device paired");
    Ok(RedeemResponse { token, device })
}

pub async fn revoke(state: &State, id: String) -> anyhow::Result<bool> {
    let i = id.clone();
    let removed = state.with_store(move |s| s.revoke_device(&i)).await?;
    state.phone.devices.write().unwrap().retain(|_, d| *d != id);
    let _ = state.phone.kicks.send(id);
    Ok(removed)
}

/// Records that a device connected (fire and forget).
pub fn touch(state: &State, id: String) {
    let state = state.clone();
    tokio::spawn(async move {
        let _ = state.with_store(move |s| s.touch_device(&id)).await;
    });
}

/// Caches the tailnet name at startup so the origin check knows it before Settings asks.
pub fn warm(state: &State) {
    let state = state.clone();
    tokio::spawn(async move {
        let (_, dns) = tailscale_state().await;
        *state.phone.tailnet_name.lock().unwrap() = dns;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_normalize_and_look_right() {
        let c = new_code();
        assert_eq!(c.len(), 8);
        assert!(c.bytes().all(|b| ALPHABET.contains(&b)));
        assert_eq!(normalize_code(" abcd-efgh "), "ABCDEFGH");
        assert_eq!(pretty("ABCDEFGH"), "ABCD-EFGH");
    }

    #[test]
    fn reads_the_serve_target() {
        let json = r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"mac.tail1.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:47357"}}}}}"#;
        let t = serve_target_from(json).unwrap();
        assert!(is_ours(&t, 47357));
        assert!(!is_ours(&t, 5174));
        assert_eq!(serve_target_from("{}"), None);
    }

    #[tokio::test]
    async fn pairing_codes_redeem_count_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path().to_path_buf()).unwrap();
        let set = |code: &str, expires_at: i64| {
            *state.phone.pairing.lock().unwrap() = Some(Pairing { code: code.into(), expires_at, redeemed: 0, failures: 0 });
        };
        let req = |code: &str| RedeemRequest { code: code.into(), name: "Phone".into() };

        set("ABCDEFGH", now_ms() + 60_000);
        assert!(redeem(&state, req("ABCD-EFGX")).await.err().unwrap().contains("doesn't match"));
        let ok = redeem(&state, req("abcd-efgh")).await.unwrap();
        assert_eq!(state.phone.device_for(&ok.token), Some(ok.device.id.clone()));
        assert_eq!(state.phone.device_for("nope"), None);

        // A code pairs at most MAX_REDEEMS devices.
        for _ in 1..MAX_REDEEMS {
            redeem(&state, req("ABCDEFGH")).await.unwrap();
        }
        assert!(redeem(&state, req("ABCDEFGH")).await.err().unwrap().contains("No pairing code"));

        // Too many wrong guesses throw the code away; expired codes are refused.
        set("ABCDEFGH", now_ms() + 60_000);
        for _ in 0..MAX_FAILURES {
            let _ = redeem(&state, req("ZZZZZZZZ")).await;
        }
        assert!(redeem(&state, req("ABCDEFGH")).await.err().unwrap().contains("No pairing code"));
        set("ABCDEFGH", now_ms() - 1);
        assert!(redeem(&state, req("ABCDEFGH")).await.err().unwrap().contains("expired"));

        // Revoking signs the device out and tells its connections.
        let mut kicks = state.phone.kicks.subscribe();
        assert!(revoke(&state, ok.device.id.clone()).await.unwrap());
        assert_eq!(state.phone.device_for(&ok.token), None);
        assert_eq!(kicks.recv().await.unwrap(), ok.device.id);
    }

    #[test]
    fn qr_is_svg() {
        assert!(qr_svg("https://mac.tail1.ts.net/#pair=ABCD-EFGH").unwrap().contains("<svg"));
    }
}
