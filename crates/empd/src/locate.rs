//! Finding files and folders dropped on a composer.
//!
//! Folders: Web views hand EMP only the folder's name and the
//! names inside it (never its path), so the daemon looks for folders with that name (Spotlight,
//! then the usual places and known projects) and ranks them by how many of those children they
//! hold, preferring live copies over backups and cloud mirrors, then the most recently changed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Below this share of matching children a same-named folder is someone else's.
const MIN_MATCH: f64 = 0.6;
const MAX_RESULTS: usize = 6;

/// Absolute paths, likeliest first; empty when nothing fits.
pub async fn folder(name: &str, entries: &[String], projects: &[PathBuf]) -> Vec<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Vec::new();
    }
    let mut found = spotlight(name).await;
    let home = emp_core::paths::home_dir();
    for dir in ["Desktop", "Documents", "Downloads", "Projects", "Developer", "code", "src", ""] {
        found.push(home.join(dir).join(name));
    }
    found.extend(projects.iter().filter(|p| p.file_name().is_some_and(|n| n == name)).cloned());
    found.push(home.join(name));
    let name = name.to_string();
    let entries = entries.to_vec();
    tokio::task::spawn_blocking(move || rank(&name, found, &entries)).await.unwrap_or_default()
}

/// Files: a dropped or picked file arrives as its name, size and modification time. Same name
/// and same size (to the byte) is the file; when several match, the one modified when the
/// browser says wins, then live copies, then the newest. `None` when it isn't on this Mac
/// (it came from another device, or it was pasted).
pub async fn file(name: &str, size: u64, modified: Option<i64>) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    let mut found = spotlight_query(&format!("kMDItemFSName == '{}'", escape(name))).await;
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        // Spotlight can lag behind a file saved a moment ago: look in the usual places too.
        let home = emp_core::paths::home_dir();
        for dir in ["Desktop", "Downloads", "Documents", ""] {
            let dir = home.join(dir);
            found.push(dir.join(&name));
            if let Ok(kids) = std::fs::read_dir(&dir) {
                found.extend(kids.flatten().filter(|k| k.path().is_dir()).map(|k| k.path().join(&name)));
            }
        }
        pick_file(&name, size, modified, found)
    })
    .await
    .ok()
    .flatten()
}

/// Sorts ascending: modified at another time, a backup or mirror, then newest first.
type FileRank = (bool, bool, std::cmp::Reverse<SystemTime>);

fn pick_file(name: &str, size: u64, modified: Option<i64>, candidates: Vec<PathBuf>) -> Option<PathBuf> {
    let mut seen = HashSet::new();
    let mut best: Option<(FileRank, PathBuf)> = None;
    for path in candidates {
        let Ok(path) = path.canonicalize() else { continue };
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if !meta.is_file() || meta.len() != size || path.file_name().is_none_or(|n| n != name) || !seen.insert(path.clone()) || hidden_away(&path) {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let ms = mtime.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        let rank = (modified.is_none_or(|m| (m - ms).abs() >= 2000), copy_like(&path), std::cmp::Reverse(mtime));
        if best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, path));
        }
    }
    best.map(|(_, p)| p)
}

fn escape(name: &str) -> String {
    name.chars().flat_map(|c| if matches!(c, '\\' | '\'' | '"' | '*') { vec!['\\', c] } else { vec![c] }).collect()
}

/// Spotlight's folders named exactly `name`; nothing if Spotlight is off or slow.
async fn spotlight(name: &str) -> Vec<PathBuf> {
    spotlight_query(&format!("kMDItemFSName == '{}' && kMDItemContentType == 'public.folder'", escape(name))).await
}

async fn spotlight_query(query: &str) -> Vec<PathBuf> {
    let run = tokio::process::Command::new("/usr/bin/mdfind").arg(query).kill_on_drop(true).output();
    match tokio::time::timeout(Duration::from_secs(3), run).await {
        Ok(Ok(out)) if out.status.success() => String::from_utf8_lossy(&out.stdout).lines().map(PathBuf::from).collect(),
        _ => Vec::new(),
    }
}

fn rank(name: &str, candidates: Vec<PathBuf>, entries: &[String]) -> Vec<String> {
    let want: HashSet<&str> = entries.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    let mut scored: Vec<(f64, bool, SystemTime, String)> = Vec::new();
    for path in candidates {
        let Ok(path) = path.canonicalize() else { continue };
        if !path.is_dir() || path.file_name().is_none_or(|n| n != name) || !seen.insert(path.clone()) || hidden_away(&path) {
            continue;
        }
        let score = if want.is_empty() {
            1.0
        } else {
            let Ok(dir) = std::fs::read_dir(&path) else { continue };
            let has: HashSet<String> = dir.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            want.iter().filter(|w| has.contains(**w)).count() as f64 / want.len() as f64
        };
        if score < MIN_MATCH {
            continue;
        }
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        scored.push((score, copy_like(&path), modified, path.to_string_lossy().into_owned()));
    }
    // Best match, then live copies before backups, then the most recently changed.
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)));
    scored.into_iter().take(MAX_RESULTS).map(|s| s.3).collect()
}

/// Inside the Trash, a package or dependency folder: never what the owner dropped.
fn hidden_away(path: &Path) -> bool {
    path.components().any(|c| {
        let c = c.as_os_str().to_string_lossy();
        c == ".Trash" || c == "node_modules" || c == ".git" || c.ends_with(".app")
    })
}

/// A backup or cloud mirror of a folder that probably also lives somewhere else.
fn copy_like(path: &Path) -> bool {
    let s = path.to_string_lossy().to_lowercase();
    s.contains("/library/cloudstorage/") || s.contains("/library/mobile documents/") || s.contains("backup") || s.contains("/time machine")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_by_contents_then_live_copies() {
        let tmp = std::env::temp_dir().join(format!("emp-locate-{}", std::process::id()));
        let live = tmp.join("work").join("Site");
        let backup = tmp.join("Backup").join("Site");
        let other = tmp.join("other").join("Site");
        for (dir, kids) in [(&live, &["a.txt", "src", "README.md"][..]), (&backup, &["a.txt", "src", "README.md"]), (&other, &["zzz"])] {
            std::fs::create_dir_all(dir).unwrap();
            for k in kids {
                std::fs::write(dir.join(k), "").unwrap();
            }
        }
        let entries: Vec<String> = ["a.txt", "src", "README.md"].iter().map(|s| s.to_string()).collect();
        let got = rank("Site", vec![backup.clone(), other.clone(), live.clone(), live.join("..").join("Site")], &entries);
        let canon = |p: &PathBuf| p.canonicalize().unwrap().to_string_lossy().into_owned();
        assert_eq!(got, vec![canon(&live), canon(&backup)]);
        // An empty folder matches any same-named folder.
        assert_eq!(rank("Site", vec![other.clone()], &[]), vec![canon(&other)]);
        assert!(rank("Elsewhere", vec![other], &entries).is_empty());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn files_match_by_size_then_modification_time() {
        let tmp = std::env::temp_dir().join(format!("emp-locate-file-{}", std::process::id()));
        let (a, b, c) = (tmp.join("a/clip.mov"), tmp.join("b/clip.mov"), tmp.join("c/clip.mov"));
        for (p, bytes) in [(&a, 5), (&b, 5), (&c, 7)] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, vec![0u8; bytes]).unwrap();
        }
        let ms = |p: &PathBuf| std::fs::metadata(p).unwrap().modified().unwrap().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis() as i64;
        let all = vec![a.clone(), b.clone(), c.clone()];
        let canon = |p: &PathBuf| p.canonicalize().unwrap();
        assert_eq!(pick_file("clip.mov", 7, None, all.clone()), Some(canon(&c)), "size decides");
        // Two of the same size: the one modified when the browser said.
        let old = std::fs::File::options().write(true).open(&a).unwrap();
        old.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)).unwrap();
        assert_eq!(pick_file("clip.mov", 5, Some(1_000_000_000), all.clone()), Some(canon(&a)));
        assert_eq!(pick_file("clip.mov", 5, Some(ms(&b)), all.clone()), Some(canon(&b)));
        assert_eq!(pick_file("clip.mov", 9, None, all), None, "nothing that size");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
