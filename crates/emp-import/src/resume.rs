//! Builds the command that reopens a thread's session in its harness's own TUI.
//!
//! Flags verified against each CLI's `--help` on 2026-10-19 (see FOUNDATION §4):
//! - pi:      `pi --session <path|id>`
//! - Hermes:  `hermes -p <profile> --resume <id>` (NOT `hermes resume`, which lifts the
//!   emergency stop)
//! - Claude:  `claude --resume <id>`
//! - Codex:   `codex resume <id>`, with `CODEX_HOME` for non-default homes (Orion's engine home)
//! - Antigravity: `agy --conversation <id>`
//! - Gemini:  `--resume` only takes `latest` or an index, so it is not resumable by id here.

use emp_protocol::{Harness, SpawnSpec, Thread};

pub fn resume_command(thread: &Thread) -> Option<SpawnSpec> {
    let s = &thread.session;
    let mut env = Vec::new();
    let (program, args): (&str, Vec<String>) = match thread.harness {
        Harness::Pi => {
            let target = s.path.clone().unwrap_or_else(|| s.session_id.clone());
            ("pi", vec!["--session".into(), target])
        }
        Harness::Hermes => {
            let mut args = Vec::new();
            if let Some(profile) = s.profile.as_deref().filter(|p| *p != "default") {
                args.push("-p".into());
                args.push(profile.into());
            }
            args.push("--resume".into());
            args.push(s.session_id.clone());
            ("hermes", args)
        }
        Harness::Claude => ("claude", vec!["--resume".into(), s.session_id.clone()]),
        Harness::Codex => {
            if let Some(home) = &s.home {
                env.push(("CODEX_HOME".to_string(), home.clone()));
            }
            ("codex", vec!["resume".into(), s.session_id.clone()])
        }
        Harness::Cursor => ("cursor-agent", vec!["--resume".into(), s.session_id.clone()]),
        Harness::Antigravity => ("agy", vec!["--conversation".into(), s.session_id.clone()]),
        Harness::Gemini | Harness::Shell => return None,
    };
    let display = {
        let mut parts: Vec<String> = Vec::new();
        if let Some(cwd) = &thread.cwd {
            parts.push(format!("cd {} &&", shell_quote(cwd)));
        }
        for (k, v) in &env {
            parts.push(format!("{k}={}", shell_quote(v)));
        }
        parts.push(program.to_string());
        parts.extend(args.iter().map(|a| shell_quote(a)));
        parts.join(" ")
    };
    Some(SpawnSpec { program: program.into(), args, cwd: thread.cwd.clone(), env, display })
}

/// POSIX single-quote quoting, leaving plain words bare.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use emp_protocol::{SessionRef, ThreadStatus};

    fn thread(harness: Harness, profile: Option<&str>, home: Option<&str>) -> Thread {
        Thread {
            id: "t".into(),
            harness,
            title: "x".into(),
            project_id: None,
            cwd: Some("/Users/me/my repo".into()),
            model: None,
            agent_id: None,
            parent_id: None,
            child_count: 0,
            assigned_by: None,
            status: ThreadStatus::Idle,
            automated: false,
            session: SessionRef {
                harness,
                session_id: "abc-123".into(),
                path: None,
                home: home.map(Into::into),
                profile: profile.map(Into::into),
            },
            message_count: None,
            created_at: 0,
            updated_at: 0,
            imported_from: None,
        }
    }

    #[test]
    fn hermes_uses_resume_flag_not_subcommand() {
        let spec = resume_command(&thread(Harness::Hermes, Some("studio"), None)).unwrap();
        assert_eq!(spec.args, vec!["-p", "studio", "--resume", "abc-123"]);
        let spec = resume_command(&thread(Harness::Hermes, Some("default"), None)).unwrap();
        assert_eq!(spec.args, vec!["--resume", "abc-123"]);
    }

    #[test]
    fn codex_carries_home_and_display_is_quoted() {
        let spec = resume_command(&thread(Harness::Codex, None, Some("/x/codex home"))).unwrap();
        assert_eq!(spec.env, vec![("CODEX_HOME".to_string(), "/x/codex home".to_string())]);
        assert_eq!(spec.display, "cd '/Users/me/my repo' && CODEX_HOME='/x/codex home' codex resume abc-123");
    }

    #[test]
    fn gemini_is_not_resumable_by_id() {
        assert!(resume_command(&thread(Harness::Gemini, None, None)).is_none());
    }
}
