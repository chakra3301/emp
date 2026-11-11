//! What's installed and what can be imported.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use emp_protocol::{DoctorReport, Harness, HarnessStatus, ImportSourceKind, SourceStatus};

use crate::state::State;

/// CLI binary for each harness.
const HARNESS_BINARIES: &[(Harness, &str)] = &[
    (Harness::Pi, "pi"),
    (Harness::Hermes, "hermes"),
    (Harness::Claude, "claude"),
    (Harness::Codex, "codex"),
    (Harness::Gemini, "gemini"),
    (Harness::Cursor, "cursor-agent"),
    (Harness::Antigravity, "agy"),
];

pub async fn report(state: &State) -> Result<DoctorReport> {
    let mut checks = Vec::new();
    for &(harness, bin) in HARNESS_BINARIES {
        checks.push(tokio::spawn(probe(harness, bin)));
    }
    let mut harnesses = Vec::new();
    for c in checks {
        harnesses.push(c.await?);
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    harnesses.push(HarnessStatus {
        harness: Harness::Shell,
        installed: Path::new(&shell).exists(),
        path: Some(shell),
        version: None,
    });

    let sources = state
        .with_store(|s| {
            ImportSourceKind::ALL
                .into_iter()
                .map(|kind| {
                    let importer = emp_import::by_kind(kind);
                    let (thread_count, agent_count) = s.source_counts(kind)?;
                    Ok(SourceStatus {
                        source: kind,
                        found: importer.found(),
                        paths: importer.paths().iter().map(|p| p.to_string_lossy().into_owned()).collect(),
                        thread_count,
                        agent_count,
                        last_import_at: s.last_import_at(kind)?,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .await?;

    Ok(DoctorReport {
        daemon_version: env!("CARGO_PKG_VERSION").into(),
        data_dir: state.data_dir.to_string_lossy().into_owned(),
        harnesses,
        sources,
    })
}

async fn probe(harness: Harness, bin: &'static str) -> HarnessStatus {
    let Some(path) = which(bin) else {
        return HarnessStatus { harness, installed: false, path: None, version: None };
    };
    let version = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(&path).arg("--version").kill_on_drop(true).output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .and_then(|out| {
        let text = String::from_utf8_lossy(if out.stdout.is_empty() { &out.stderr } else { &out.stdout }).into_owned();
        text.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| emp_import::util::truncate(l, 120))
    });
    HarnessStatus { harness, installed: true, path: Some(path.to_string_lossy().into_owned()), version }
}

/// Finds `bin` on `$PATH` plus the usual user install dirs, which a launchd-started daemon
/// doesn't have on its minimal `PATH`.
pub fn which(bin: &str) -> Option<PathBuf> {
    search_dirs().into_iter().map(|d| d.join(bin)).find(|p| is_executable(p))
}

pub fn search_dirs() -> Vec<PathBuf> {
    let home = emp_core::paths::home_dir();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for extra in [
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    dirs
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
