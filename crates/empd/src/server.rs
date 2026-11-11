//! Listeners and per-connection JSON-RPC loop.
//!
//! - Unix socket (`empd.sock`, 0600): newline-delimited JSON. Used by the CLI and by host
//!   relays such as Orion's future `localService` broker.
//! - WebSocket on `127.0.0.1` (`/ws`): one JSON message per text frame. Used by EMP.app and the
//!   dev host. Browser origins other than localhost/Tauri are refused.
//!
//! Every connection must authenticate with `hello` first; notifications are only delivered to
//! authenticated connections.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State as AxumState;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use emp_core::paths::{self, Discovery};
use emp_protocol::{PROTOCOL_VERSION, RpcRequest, RpcResponse, codes, methods};
use futures::{SinkExt, Stream, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::state::State;
use crate::{importing, rpc};

/// Largest accepted message, either transport.
/// Attachments arrive base64-encoded in one request (`attachments::MAX_BYTES` raw).
const MAX_MESSAGE: usize = 32 * 1024 * 1024;

pub async fn serve(state: State, ws_port: u16, initial_import: bool, web_dir: Option<std::path::PathBuf>) -> Result<()> {
    let sock_path = paths::socket_path(&state.data_dir);
    claim_socket(&sock_path).await?;
    let unix = UnixListener::bind(&sock_path).with_context(|| format!("bind {}", sock_path.display()))?;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;

    let tcp = TcpListener::bind(("127.0.0.1", ws_port)).await.context("bind WebSocket port")?;
    let ws_url = format!("ws://{}/ws", tcp.local_addr()?);
    state.phone.set_listen(tcp.local_addr()?.port(), ws_port != 0);
    match &web_dir {
        Some(d) => tracing::info!(web = %d.display(), "serving the web app"),
        None => tracing::info!("web app not built; phone access shows a notice"),
    }
    *state.phone.web_dir.lock().unwrap() = web_dir;
    crate::phone::warm(&state);

    let discovery = Discovery {
        daemon_version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        pid: std::process::id(),
        socket: sock_path.to_string_lossy().into_owned(),
        ws_url: ws_url.clone(),
        token_path: paths::token_path(&state.data_dir).to_string_lossy().into_owned(),
    };
    paths::write_private(&paths::discovery_path(&state.data_dir), &serde_json::to_vec_pretty(&discovery)?)?;
    tracing::info!(socket = %sock_path.display(), %ws_url, "empd listening");

    tokio::spawn(accept_unix(state.clone(), unix));
    crate::live::Live::spawn_reaper(&state);
    crate::live::Live::spawn_codex_watcher(&state);
    crate::schedules::spawn(&state);
    if initial_import {
        let s = state.clone();
        tokio::spawn(async move {
            match importing::run(&s, &[]).await {
                Ok(r) => tracing::info!(sources = r.sources.len(), "initial import finished"),
                Err(e) => tracing::warn!("initial import failed: {e:#}"),
            }
        });
    }

    let app = Router::new()
        .route("/ws", get(ws_upgrade))
        .route("/api/pair", axum::routing::post(pair_redeem))
        .route("/mcp", axum::routing::post(crate::mcp::handle).get(crate::mcp::no_stream))
        .fallback(get(web_file))
        .with_state(state.clone());
    let result = axum::serve(tcp, app).with_graceful_shutdown(shutdown_signal()).await;

    state.live.shutdown().await;
    state.terminals.shutdown();
    let _ = std::fs::remove_file(paths::discovery_path(&state.data_dir));
    let _ = std::fs::remove_file(&sock_path);
    tracing::info!("empd stopped");
    result.map_err(Into::into)
}

/// Another daemon already owns the data dir. `main` exits 0 on it, so launchd's
/// `KeepAlive { SuccessfulExit = false }` doesn't keep relaunching a second copy.
#[derive(Debug)]
pub struct AlreadyRunning(pub std::path::PathBuf);

impl std::fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "another empd is already running ({})", self.0.display())
    }
}

impl std::error::Error for AlreadyRunning {}

/// Refuses to start next to a live daemon; removes a stale socket file.
async fn claim_socket(path: &Path) -> Result<()> {
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(AlreadyRunning(path.to_path_buf()).into());
        }
        std::fs::remove_file(path)?;
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
}

// ---- Unix socket -----------------------------------------------------------------------------

async fn accept_unix(state: State, listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let (tx, mut rx) = mpsc::channel::<String>(256);
                    let writer = tokio::spawn(async move {
                        while let Some(msg) = rx.recv().await {
                            if write.write_all(msg.as_bytes()).await.is_err() || write.write_all(b"\n").await.is_err() {
                                break;
                            }
                        }
                    });
                    let lines = BufReader::new(read).lines();
                    let incoming = futures::stream::unfold(lines, |mut lines| async move {
                        match lines.next_line().await {
                            Ok(Some(line)) => Some((line, lines)),
                            _ => None,
                        }
                    });
                    run_connection(state, Box::pin(incoming), tx).await;
                    writer.abort();
                });
            }
            Err(e) => {
                tracing::warn!("unix accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

// ---- WebSocket -------------------------------------------------------------------------------

async fn ws_upgrade(AxumState(state): AxumState<State>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !request_origin_ok(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(MAX_MESSAGE).on_upgrade(move |socket| ws_connection(state, socket))
}

fn request_origin_ok(state: &State, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else { return true };
    let header_str = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let ok = origin_allowed(origin, state.phone.tailnet_name().as_deref(), [header_str("host"), header_str("x-forwarded-host")]);
    if !ok {
        tracing::warn!(%origin, "refused request from foreign origin");
    }
    ok
}

/// Local pages (the dev host, EMP.app's Tauri webview), this Mac's tailnet address, and pages
/// served by this daemon itself (same origin). Non-browser clients send no Origin.
fn origin_allowed(origin: &str, tailnet: Option<&str>, hosts: [Option<&str>; 2]) -> bool {
    if matches!(origin, "tauri://localhost" | "https://tauri.localhost" | "http://tauri.localhost") {
        return true;
    }
    let Some(authority) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
    let host = authority.split(':').next().unwrap_or("");
    if origin.starts_with("http://") && (host == "127.0.0.1" || host == "localhost") {
        return true;
    }
    if origin.starts_with("https://") && tailnet.is_some_and(|t| t.eq_ignore_ascii_case(host)) {
        return true;
    }
    hosts.into_iter().flatten().any(|h| h.eq_ignore_ascii_case(authority))
}

// ---- web app + pairing (phone access) ---------------------------------------------------------

async fn pair_redeem(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    body: axum::Json<crate::phone::RedeemRequest>,
) -> Response {
    if !request_origin_ok(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match crate::phone::redeem(&state, body.0).await {
        Ok(r) => (no_store(), axum::Json(r)).into_response(),
        Err(msg) => (StatusCode::FORBIDDEN, no_store(), axum::Json(serde_json::json!({ "error": msg }))).into_response(),
    }
}

fn no_store() -> [(header::HeaderName, &'static str); 1] {
    [(header::CACHE_CONTROL, "no-store")]
}

/// Static files from the built web app. Paths never leave the web dir.
async fn web_file(AxumState(state): AxumState<State>, uri: axum::http::Uri) -> Response {
    let Some(dir) = state.phone.web_dir.lock().unwrap().clone() else {
        return (StatusCode::NOT_FOUND, "EMP's web app isn't built. Run `npm run app` (or `npm run build -w @emp/desktop`).").into_response();
    };
    let rel = uri.path().trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.split('/').any(|seg| seg.is_empty() || seg.starts_with('.')) || rel.contains('\\') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(bytes) = tokio::fs::read(dir.join(rel)).await else { return StatusCode::NOT_FOUND.into_response() };
    let ext = rel.rsplit('.').next().unwrap_or("");
    let mime = match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    let cache = if rel.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        bytes,
    )
        .into_response()
}

async fn ws_connection(state: State, socket: WebSocket) {
    let (mut sink, stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let incoming = stream.filter_map(|m| async move {
        match m {
            Ok(Message::Text(t)) => Some(t.to_string()),
            Ok(Message::Binary(b)) => String::from_utf8(b.to_vec()).ok(),
            _ => None,
        }
    });
    run_connection(state, Box::pin(incoming), tx).await;
    writer.abort();
}

// ---- protocol loop ---------------------------------------------------------------------------

async fn run_connection(state: State, mut incoming: impl Stream<Item = String> + Unpin, out: mpsc::Sender<String>) {
    let mut authed = false;
    let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;
    let mut device: Option<String> = None;
    let mut kicks = state.phone.kicks.subscribe();

    loop {
        let text = tokio::select! {
            next = incoming.next() => match next {
                Some(t) => t,
                None => break,
            },
            kicked = kicks.recv() => {
                if let Ok(id) = kicked && device.as_deref() == Some(id.as_str()) {
                    tracing::info!(device = %id, "closing a revoked device's connection");
                    break;
                }
                continue;
            }
        };
        if text.len() > MAX_MESSAGE {
            send(&out, RpcResponse::err(Value::Null, codes::INVALID_REQUEST, "message too large")).await;
            continue;
        }
        let req: RpcRequest = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                send(&out, RpcResponse::err(Value::Null, codes::PARSE_ERROR, e.to_string())).await;
                continue;
            }
        };
        if req.id.is_null() {
            continue; // client notifications are not used yet
        }

        if !authed {
            let response = if req.method == methods::HELLO {
                match rpc::hello(&state, req.params) {
                    Ok((result, dev)) => {
                        authed = true;
                        device = dev;
                        forwarder = Some(spawn_forwarder(&state, out.clone()));
                        RpcResponse::ok(req.id, result)
                    }
                    Err(e) => RpcResponse { jsonrpc: "2.0".into(), id: req.id, result: None, error: Some(e) },
                }
            } else {
                RpcResponse::err(req.id, codes::NOT_AUTHENTICATED, "send hello first")
            };
            send(&out, response).await;
            continue;
        }

        let state = state.clone();
        let out = out.clone();
        tokio::spawn(async move {
            let response = match rpc::dispatch(&state, &req.method, req.params).await {
                Ok(result) => RpcResponse::ok(req.id, result),
                Err(e) => RpcResponse { jsonrpc: "2.0".into(), id: req.id, result: None, error: Some(e) },
            };
            send(&out, response).await;
        });
    }
    if let Some(f) = forwarder {
        f.abort();
    }
}

fn spawn_forwarder(state: &State, out: mpsc::Sender<String>) -> tokio::task::JoinHandle<()> {
    let mut rx = state.notify.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(n) => {
                    let Ok(text) = serde_json::to_string(&n) else { continue };
                    if out.send(text).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    })
}

async fn send(out: &mpsc::Sender<String>, response: RpcResponse) {
    if let Ok(text) = serde_json::to_string(&response) {
        let _ = out.send(text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Feeds `requests` through one connection and returns every message sent back.
    async fn exchange(state: &State, requests: Vec<Value>) -> Vec<Value> {
        let (tx, mut rx) = mpsc::channel(64);
        let incoming = futures::stream::iter(requests.into_iter().map(|r| r.to_string()));
        run_connection(state.clone(), Box::pin(incoming), tx).await;
        // Requests after hello run on spawned tasks; give them a moment to answer.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(serde_json::from_str(&msg).unwrap());
        }
        out
    }

    fn req(id: u32, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn by_id(out: &[Value], id: u32) -> &Value {
        out.iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no response {id} in {out:?}"))
    }

    #[tokio::test]
    async fn requires_hello_with_valid_token_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path().to_path_buf()).unwrap();
        let client = json!({"kind": "test", "version": "0"});
        let out = exchange(
            &state,
            vec![
                req(1, "projects.list", Value::Null),
                req(2, "hello", json!({"protocolVersion": 1, "token": "nope", "client": client})),
                req(3, "hello", json!({"protocolVersion": 99, "token": state.token, "client": client})),
                req(4, "hello", json!({"protocolVersion": 1, "token": state.token, "client": client})),
                req(5, "agents.list", Value::Null),
                req(6, "threads.get", json!({"threadId": "missing"})),
                req(7, "no.such.method", Value::Null),
                req(8, "hello", json!({"protocolVersion": 1, "token": state.token, "client": client})),
            ],
        )
        .await;
        assert_eq!(by_id(&out, 1)["error"]["code"], codes::NOT_AUTHENTICATED);
        assert_eq!(by_id(&out, 2)["error"]["code"], codes::NOT_AUTHENTICATED);
        assert_eq!(by_id(&out, 3)["error"]["code"], codes::INCOMPATIBLE_VERSION);
        assert_eq!(by_id(&out, 4)["result"]["protocolVersion"], 1);
        let agents = by_id(&out, 5)["result"].as_array().unwrap();
        assert!(agents.iter().any(|a| a["name"] == "Suki" && a["model"] == "openai/gpt-6.1-sol" && a["coordinator"] == true));
        assert_eq!(by_id(&out, 6)["error"]["code"], codes::NOT_FOUND);
        assert_eq!(by_id(&out, 7)["error"]["code"], codes::METHOD_NOT_FOUND);
        assert_eq!(by_id(&out, 8)["error"]["code"], codes::INVALID_REQUEST);
    }

    #[tokio::test]
    async fn malformed_json_gets_parse_error_and_connection_survives() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path().to_path_buf()).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let incoming = futures::stream::iter(vec!["{not json".to_string(), req(1, "doctor", Value::Null).to_string()]);
        run_connection(state, Box::pin(incoming), tx).await;
        let first: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        let second: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert_eq!(first["error"]["code"], codes::PARSE_ERROR);
        assert_eq!(second["error"]["code"], codes::NOT_AUTHENTICATED);
    }

    #[test]
    fn token_is_stable_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let a = State::open(dir.path().to_path_buf()).unwrap().token.clone();
        let b = State::open(dir.path().to_path_buf()).unwrap().token.clone();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        let mode = std::fs::metadata(paths::token_path(dir.path())).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn origins() {
        let none = [None, None];
        assert!(origin_allowed("http://127.0.0.1:5174", None, none));
        assert!(origin_allowed("http://localhost:5174", None, none));
        assert!(origin_allowed("tauri://localhost", None, none));
        assert!(!origin_allowed("http://localhost.evil.com", None, none));
        assert!(!origin_allowed("https://example.com", None, none));
        assert!(!origin_allowed("http://127.0.0.1.nip.io", None, none));
        // Phone access: this Mac's tailnet name, or a page served by the daemon itself.
        let ts = Some("mac.tail1.ts.net");
        assert!(origin_allowed("https://mac.tail1.ts.net", ts, none));
        assert!(origin_allowed("https://MAC.tail1.ts.net", ts, none));
        assert!(!origin_allowed("https://evil.tail1.ts.net", ts, none));
        assert!(!origin_allowed("http://mac.tail1.ts.net", ts, none), "tailnet pages are https only");
        assert!(origin_allowed("https://mac.tail1.ts.net", None, [Some("mac.tail1.ts.net"), None]));
        assert!(origin_allowed("http://192.168.1.5:47357", None, [Some("192.168.1.5:47357"), None]));
        assert!(!origin_allowed("https://evil.com", None, [Some("mac.tail1.ts.net"), Some("other")]));
    }
}
