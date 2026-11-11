//! On-disk layout of EMP's data directory.
//!
//! ```text
//! ~/Library/Application Support/EMP/      (0700)
//!   emp.db          SQLite store
//!   daemon.json     discovery file for clients (0600)
//!   token           bearer token for clients (0600)
//!   empd.sock       Unix socket (newline-delimited JSON-RPC)
//!   desk/           working dir of the Desk coordinator agent
//! ```
//!
//! `EMP_DATA_DIR` overrides the location (tests, side-by-side dev builds).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("EMP_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(|| home_dir().join("Library/Application Support"))
        .join("EMP")
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn db_path(data: &Path) -> PathBuf {
    data.join("emp.db")
}

pub fn token_path(data: &Path) -> PathBuf {
    data.join("token")
}

pub fn discovery_path(data: &Path) -> PathBuf {
    data.join("daemon.json")
}

pub fn socket_path(data: &Path) -> PathBuf {
    data.join("empd.sock")
}

/// Creates the data dir with owner-only permissions.
pub fn ensure_data_dir(data: &Path) -> Result<()> {
    fs::create_dir_all(data).with_context(|| format!("create {}", data.display()))?;
    fs::set_permissions(data, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Writes `contents` to `path` atomically with mode 0600.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("open {}", tmp.display()))?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// What a client needs to find and authenticate to a running daemon.
/// The token itself lives in the `token` file next to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub daemon_version: String,
    pub protocol_version: u32,
    pub pid: u32,
    pub socket: String,
    pub ws_url: String,
    pub token_path: String,
}

pub fn read_discovery(data: &Path) -> Result<Discovery> {
    let raw = fs::read(discovery_path(data)).context("daemon.json not found; is empd running?")?;
    Ok(serde_json::from_slice(&raw)?)
}

pub fn read_token(data: &Path) -> Result<String> {
    Ok(fs::read_to_string(token_path(data))?.trim().to_string())
}
