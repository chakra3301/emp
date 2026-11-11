//! Files dropped or pasted into a chat. A file that's on this Mac is attached where it is, any
//! size (`link`: `<data>/attachments/<id>/<name>` is a symlink to it). Anything else (pasted
//! screenshots, shrunk pictures, files from the phone) is uploaded and kept as a copy there, up
//! to [`MAX_BYTES`]. Every agent gets the real path in the message text (they all have file
//! tools), and harnesses that take images inline get the pictures too.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use emp_protocol::{Attachment, AttachmentUploadParams};

use crate::drivers::{Msg, image_mime};

/// Largest upload (the UI shrinks big screenshots first). Files found on disk have no limit.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;
/// Attachments older than this are removed at daemon start.
const KEEP_FOR: Duration = Duration::from_secs(30 * 24 * 3600);

pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("attachments")
}

/// Stores one upload.
pub fn save(root: &Path, p: &AttachmentUploadParams) -> Result<Attachment> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(p.data.trim()).context("data isn't base64")?;
    if bytes.is_empty() {
        bail!("{} is empty", p.name);
    }
    if bytes.len() > MAX_BYTES {
        bail!("{} is {} MB; attachments can be at most {} MB", p.name, bytes.len() >> 20, MAX_BYTES >> 20);
    }
    let id = ulid::Ulid::new().to_string();
    let name = clean_name(&p.name);
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir)?;
    restrict(root, 0o700);
    let path = dir.join(&name);
    std::fs::write(&path, &bytes)?;
    restrict(&path, 0o600);
    Ok(Attachment {
        mime: p.mime.clone().filter(|m| !m.is_empty()).or_else(|| image_mime(&path).map(String::from)),
        id,
        name,
        size: bytes.len() as u64,
        path: path.to_string_lossy().into_owned(),
    })
}

/// Attaches a file that's already on this Mac, without copying it.
pub fn link(root: &Path, file: &Path) -> Result<Attachment> {
    let meta = std::fs::metadata(file).with_context(|| format!("{} is gone", file.display()))?;
    if !meta.is_file() {
        bail!("{} isn't a file", file.display());
    }
    let id = ulid::Ulid::new().to_string();
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir)?;
    restrict(root, 0o700);
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    std::os::unix::fs::symlink(file, dir.join(clean_name(&name)))?;
    Ok(Attachment {
        mime: image_mime(file).map(String::from),
        id,
        name,
        size: meta.len(),
        path: file.to_string_lossy().into_owned(),
    })
}

fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// Looks up uploads by id. Only ids this daemon issued resolve, so a client can't point an
/// agent at an arbitrary file.
pub fn resolve(root: &Path, ids: &[String]) -> Result<Vec<Attachment>> {
    ids.iter()
        .map(|id| {
            if id.len() != 26 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
                bail!("unknown attachment {id}");
            }
            let dir = root.join(id);
            let entry = std::fs::read_dir(&dir)
                .ok()
                .and_then(|mut d| d.find_map(|e| e.ok().filter(|e| e.path().is_file())))
                .with_context(|| format!("attachment {id} is gone; attach the file again"))?;
            // A linked file: the agent gets where it really is.
            let path = match std::fs::read_link(entry.path()) {
                Ok(target) => target,
                Err(_) => entry.path(),
            };
            Ok(Attachment {
                id: id.clone(),
                name: path.file_name().map_or_else(|| entry.file_name().to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned()),
                mime: image_mime(&path).map(String::from),
                size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                path: path.to_string_lossy().into_owned(),
            })
        })
        .collect()
}

/// The message an agent gets: the owner's text, then one `[Attached: <path>]` line per file.
pub fn message(text: &str, attachments: &[Attachment]) -> Msg {
    let mut out = text.trim().to_string();
    for (i, a) in attachments.iter().enumerate() {
        out.push_str(if i == 0 && !out.is_empty() { "\n\n" } else if i == 0 { "" } else { "\n" });
        out.push_str(&format!("[Attached: {}]", a.path));
    }
    // Pictures too big to send inline (a huge PNG found on disk) go by path only.
    let images = attachments
        .iter()
        .filter(|a| a.size <= crate::drivers::INLINE_IMAGE_MAX)
        .map(|a| PathBuf::from(&a.path))
        .filter(|p| image_mime(p).is_some())
        .collect();
    Msg { text: out, images }
}

/// A safe file name: the last path component, odd characters (and commas, which Codex's
/// `-i` list would split on) replaced.
pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || " ._-()+@".contains(c) { c } else { '_' })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let cleaned: String = cleaned.chars().rev().take(100).collect::<Vec<_>>().into_iter().rev().collect();
    if cleaned.is_empty() { "file".into() } else { cleaned }
}

/// Deletes attachments older than [`KEEP_FOR`].
pub fn sweep(root: &Path) {
    let Ok(dirs) = std::fs::read_dir(root) else { return };
    for d in dirs.flatten() {
        let old = d.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > KEEP_FOR);
        if old && d.path().is_dir() {
            let _ = std::fs::remove_dir_all(d.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(name: &str, bytes: &[u8]) -> AttachmentUploadParams {
        AttachmentUploadParams { name: name.into(), mime: None, data: base64::engine::general_purpose::STANDARD.encode(bytes) }
    }

    #[test]
    fn saves_resolves_and_builds_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let shot = save(dir.path(), &upload("../../Screen Shot, 1.png", b"png")).unwrap();
        assert_eq!(shot.name, "Screen Shot_ 1.png");
        assert_eq!(shot.mime.as_deref(), Some("image/png"));
        assert!(shot.path.starts_with(dir.path().to_str().unwrap()));
        let notes = save(dir.path(), &upload("notes.txt", b"hi")).unwrap();

        let got = resolve(dir.path(), &[shot.id.clone(), notes.id.clone()]).unwrap();
        assert_eq!(got[0].path, shot.path);
        assert!(resolve(dir.path(), &["../../etc".into()]).is_err());
        assert!(resolve(dir.path(), &["01ARZ3NDEKTSV4RRFFQ69G5FAV".into()]).is_err());

        let m = message("look", &got);
        assert_eq!(m.text, format!("look\n\n[Attached: {}]\n[Attached: {}]", shot.path, notes.path));
        assert_eq!(m.images, vec![PathBuf::from(&shot.path)]);
        assert_eq!(message("", &got[1..]).text, format!("[Attached: {}]", notes.path));
    }

    #[test]
    fn files_on_disk_are_linked_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("attachments");
        let movie = dir.path().join("My clip, final.mov");
        std::fs::write(&movie, vec![0u8; 64]).unwrap();
        let a = link(&root, &movie).unwrap();
        assert_eq!((a.path.as_str(), a.name.as_str(), a.size), (movie.to_str().unwrap(), "My clip, final.mov", 64));
        let got = resolve(&root, std::slice::from_ref(&a.id)).unwrap();
        assert_eq!((got[0].path.as_str(), got[0].size), (movie.to_str().unwrap(), 64));
        assert!(message("", &got).text.ends_with("final.mov]"));
        // Sweeping the record leaves the file alone.
        std::fs::remove_dir_all(root.join(&a.id)).unwrap();
        assert!(movie.exists());
        // A big picture on disk goes by path only.
        let big = dir.path().join("huge.png");
        std::fs::write(&big, vec![0u8; crate::drivers::INLINE_IMAGE_MAX as usize + 1]).unwrap();
        let big = link(&root, &big).unwrap();
        assert!(message("", &[big]).images.is_empty());
    }

    #[test]
    fn refuses_empty_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(save(dir.path(), &upload("a.png", b"")).is_err());
        assert!(save(dir.path(), &upload("a.bin", &vec![0u8; MAX_BYTES + 1])).is_err());
        assert_eq!(clean_name(".."), "file");
        assert_eq!(clean_name(".env"), "env");
    }
}
