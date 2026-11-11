//! Method dispatch. Transport-independent: `server` feeds requests in and writes results out.

use std::sync::Arc;

use emp_protocol::{
    AgentIdParams, AgentSaveParams, AttachmentUploadParams, CommandsListParams, CommandsRunParams, DaemonSettings, DeviceIdParams, GroupAddParams, GroupCreateParams, GroupIdParams, GroupUpdateParams, HelloParams, ModelsListParams, ProjectCreateParams, ProjectFilesParams, HelloResult, ImportRunParams, PROTOCOL_VERSION, Page, RpcError, TerminalIdParams,
    TerminalOpenParams, TerminalResizeParams, TerminalWriteParams, ThreadIdParams, ThreadSendParams,
    ThreadStartParams, ThreadTakeoverParams, ThreadsListParams, TranscriptParams, codes, methods,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::state::State;
use crate::{doctor, importing, live, models, phone, schedules, terminals};

pub type RpcResult = Result<Value, RpcError>;

fn error(code: i32, message: impl Into<String>) -> RpcError {
    RpcError { code, message: message.into(), data: None }
}

fn internal(e: anyhow::Error) -> RpcError {
    error(codes::INTERNAL, format!("{e:#}"))
}

fn params<T: DeserializeOwned + Default>(v: Value) -> Result<T, RpcError> {
    if v.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(v).map_err(|e| error(codes::INVALID_PARAMS, e.to_string()))
}

fn required<T: DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    serde_json::from_value(v).map_err(|e| error(codes::INVALID_PARAMS, e.to_string()))
}

fn ok(v: impl Serialize) -> RpcResult {
    serde_json::to_value(v).map_err(|e| error(codes::INTERNAL, e.to_string()))
}

/// Validates a `hello`: the daemon token (constant-time) or a paired device's token.
/// Returns the device id for device logins, so a revoke can close the connection.
pub fn hello(state: &State, raw: Value) -> Result<(Value, Option<String>), RpcError> {
    let p: HelloParams = required(raw)?;
    let device = if constant_time_eq(p.token.as_bytes(), state.token.as_bytes()) {
        None
    } else {
        Some(state.phone.device_for(&p.token).ok_or_else(|| error(codes::NOT_AUTHENTICATED, "invalid token"))?)
    };
    if p.protocol_version != PROTOCOL_VERSION {
        return Err(error(
            codes::INCOMPATIBLE_VERSION,
            format!("client speaks protocol v{}, daemon v{PROTOCOL_VERSION}", p.protocol_version),
        ));
    }
    tracing::info!(kind = %p.client.kind, version = %p.client.version, device = device.as_deref().unwrap_or("-"), "client connected");
    if let Some(id) = &device {
        crate::phone::touch(state, id.clone());
    }
    let result = ok(HelloResult { protocol_version: PROTOCOL_VERSION, daemon_version: env!("CARGO_PKG_VERSION").into() })?;
    Ok((result, device))
}

/// Handles one authenticated request.
pub async fn dispatch(state: &State, method: &str, raw: Value) -> RpcResult {
    match method {
        methods::HELLO => Err(error(codes::INVALID_REQUEST, "already authenticated")),
        methods::DOCTOR => ok(doctor::report(state).await.map_err(internal)?),
        methods::IMPORT_RUN => {
            let p: ImportRunParams = params(raw)?;
            ok(importing::run(state, &p.sources).await.map_err(internal)?)
        }
        methods::PROJECTS_LIST => ok(state.with_store(|s| s.list_projects()).await.map_err(internal)?),
        methods::AGENTS_LIST => ok(state.with_store(|s| s.list_agents()).await.map_err(internal)?),
        methods::THREADS_LIST => {
            let p: ThreadsListParams = params(raw)?;
            ok(state.with_store(move |s| s.list_threads(&p)).await.map_err(internal)?)
        }
        methods::THREADS_GET => {
            let p: ThreadIdParams = required(raw)?;
            ok(get_thread(state, p.thread_id).await?)
        }
        methods::THREADS_TRANSCRIPT => {
            let p: TranscriptParams = required(raw)?;
            ok(transcript(state, p).await?)
        }
        methods::THREADS_CONTEXT => {
            let p: ThreadIdParams = required(raw)?;
            ok(thread_context(state, p.thread_id).await?)
        }
        methods::ATTACHMENTS_LOCATE_FOLDER => {
            let p: emp_protocol::LocateFolderParams = required(raw)?;
            let mut roots: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(projects) = state.with_store(|s| s.list_projects()).await {
                roots.extend(projects.into_iter().map(|p| std::path::PathBuf::from(p.root_path)));
            }
            ok(crate::locate::folder(&p.name, &p.entries, &roots).await)
        }
        methods::ATTACHMENTS_LOCATE_FILE => {
            let p: emp_protocol::LocateFileParams = required(raw)?;
            let Some(path) = crate::locate::file(&p.name, p.size, p.modified).await else { return ok(None::<emp_protocol::Attachment>) };
            let root = crate::attachments::root(&state.data_dir);
            let linked = tokio::task::spawn_blocking(move || crate::attachments::link(&root, &path))
                .await
                .map_err(|e| error(codes::INTERNAL, e.to_string()))?
                .map_err(|e| error(codes::INVALID_PARAMS, format!("{e:#}")))?;
            ok(Some(linked))
        }
        methods::ATTACHMENTS_UPLOAD => {
            let p: AttachmentUploadParams = required(raw)?;
            let root = crate::attachments::root(&state.data_dir);
            let saved = tokio::task::spawn_blocking(move || crate::attachments::save(&root, &p))
                .await
                .map_err(|e| error(codes::INTERNAL, e.to_string()))?
                .map_err(|e| error(codes::INVALID_PARAMS, format!("{e:#}")))?;
            ok(saved)
        }
        methods::COMMANDS_LIST => {
            let p: CommandsListParams = required(raw)?;
            ok(crate::commands::list(state, p).await?)
        }
        methods::COMMANDS_RUN => {
            let p: CommandsRunParams = required(raw)?;
            ok(crate::commands::run(state, p).await?)
        }
        methods::THREADS_RESUME_COMMAND => {
            let p: ThreadIdParams = required(raw)?;
            let thread = get_thread(state, p.thread_id).await?;
            if emp_import::orion::is_orion_only(&thread) {
                return ok(Value::Null);
            }
            ok(emp_import::resume::resume_command(&thread))
        }
        methods::TERMINALS_OPEN => {
            let p: TerminalOpenParams = required(raw)?;
            ok(terminals::open(state, p).await?)
        }
        methods::TERMINALS_LIST => ok(state.terminals.list()),
        methods::TERMINALS_ATTACH => {
            let p: TerminalIdParams = required(raw)?;
            ok(state.terminals.attach(&p.terminal_id)?)
        }
        methods::TERMINALS_WRITE => {
            let p: TerminalWriteParams = required(raw)?;
            state.terminals.write(&p.terminal_id, &p.data)?;
            ok(Value::Null)
        }
        methods::TERMINALS_RESIZE => {
            let p: TerminalResizeParams = required(raw)?;
            state.terminals.resize(&p.terminal_id, p.cols, p.rows)?;
            ok(Value::Null)
        }
        methods::SERVERS_LIST => ok(crate::servers::list(state).await?),
        methods::SERVERS_KILL => {
            let p: emp_protocol::ServersKillParams = required(raw)?;
            ok(crate::servers::kill(state, p).await?)
        }
        methods::TERMINALS_KILL => {
            let p: TerminalIdParams = required(raw)?;
            state.terminals.kill(&p.terminal_id)?;
            ok(Value::Null)
        }
        methods::AGENTS_SAVE => {
            let p: AgentSaveParams = required(raw)?;
            if p.name.trim().is_empty() {
                return Err(error(codes::INVALID_PARAMS, "name is required"));
            }
            if let Some(n) = p.compact_at {
                check_compact_at(n)?;
            }
            let saved = state.with_store(move |s| s.save_agent(&p)).await.map_err(internal)?;
            state.reload_compaction().await.map_err(internal)?;
            ok(saved)
        }
        methods::GROUPS_LIST => ok(state.with_store(|s| s.list_groups()).await.map_err(internal)?),
        methods::GROUPS_CREATE => {
            let p: GroupCreateParams = required(raw)?;
            if p.name.trim().is_empty() {
                return Err(error(codes::INVALID_PARAMS, "a group needs a name"));
            }
            state
                .with_store(move |s| {
                    let id = s.create_group(&p.name, p.pinned.unwrap_or(false))?;
                    s.add_to_group(&id, &p.thread_ids.unwrap_or_default())?;
                    Ok(())
                })
                .await
                .map_err(internal)?;
            ok(groups_changed(state).await?)
        }
        methods::GROUPS_UPDATE => {
            let p: GroupUpdateParams = required(raw)?;
            if p.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
                return Err(error(codes::INVALID_PARAMS, "a group needs a name"));
            }
            let id = p.group_id.clone();
            if !state.with_store(move |s| s.update_group(&p.group_id, p.name.as_deref(), p.pinned)).await.map_err(internal)? {
                return Err(error(codes::NOT_FOUND, format!("no group {id}")));
            }
            ok(groups_changed(state).await?)
        }
        methods::GROUPS_DELETE => {
            let p: GroupIdParams = required(raw)?;
            let id = p.group_id.clone();
            if !state.with_store(move |s| s.delete_group(&p.group_id)).await.map_err(internal)? {
                return Err(error(codes::NOT_FOUND, format!("no group {id}")));
            }
            ok(groups_changed(state).await?)
        }
        methods::GROUPS_ADD => {
            let p: GroupAddParams = required(raw)?;
            let id = p.group_id.clone();
            if !state.with_store(move |s| s.add_to_group(&p.group_id, &p.thread_ids)).await.map_err(internal)? {
                return Err(error(codes::NOT_FOUND, format!("no group {id}")));
            }
            ok(groups_changed(state).await?)
        }
        methods::GROUPS_REMOVE => {
            let p: ThreadIdParams = required(raw)?;
            state.with_store(move |s| s.remove_from_group(&p.thread_id)).await.map_err(internal)?;
            ok(groups_changed(state).await?)
        }
        methods::SETTINGS_GET => ok(state.with_store(|s| s.get_settings()).await.map_err(internal)?),
        methods::SETTINGS_SET => {
            let p: DaemonSettings = required(raw)?;
            if let Some(n) = p.compact_at {
                check_compact_at(n)?;
            }
            let saved = p.clone();
            state.with_store(move |s| s.set_settings(&p)).await.map_err(internal)?;
            state.reload_compaction().await.map_err(internal)?;
            let _ = state.notify.send(emp_protocol::RpcNotification::new(emp_protocol::notifications::SETTINGS_CHANGED, &saved));
            ok(saved)
        }
        methods::AGENTS_DELETE => {
            let p: AgentIdParams = required(raw)?;
            if p.agent_id.is_empty() {
                return Err(error(codes::INVALID_PARAMS, "agentId is required"));
            }
            let deleted = state.with_store(move |s| s.delete_agent(&p.agent_id)).await.map_err(internal)?;
            if !deleted {
                return Err(error(codes::NOT_FOUND, "no such agent"));
            }
            ok(Value::Null)
        }
        methods::MODELS_LIST => {
            let p: ModelsListParams = required(raw)?;
            ok(models::list(p.harness, p.profile).await.map_err(internal)?)
        }
        methods::PROJECTS_CREATE => {
            let p: ProjectCreateParams = required(raw)?;
            ok(create_project(state, p).await?)
        }
        methods::PHONE_STATUS => ok(phone::status(state).await.map_err(internal)?),
        methods::PHONE_ENABLE => ok(phone::enable(state).await.map_err(|m| error(codes::INVALID_REQUEST, m))?),
        methods::PHONE_DISABLE => ok(phone::disable(state).await.map_err(|m| error(codes::INVALID_REQUEST, m))?),
        methods::PHONE_PAIR => ok(phone::pair(state).await.map_err(internal)?),
        methods::DEVICES_REVOKE => {
            let p: DeviceIdParams = required(raw)?;
            if !phone::revoke(state, p.device_id.clone()).await.map_err(internal)? {
                return Err(error(codes::NOT_FOUND, format!("no device {}", p.device_id)));
            }
            ok(phone::status(state).await.map_err(internal)?)
        }
        methods::PROJECTS_FILES => {
            let p: ProjectFilesParams = required(raw)?;
            let id = p.project_id.clone();
            let root = state
                .with_store(move |s| s.project_root(&id))
                .await
                .map_err(internal)?
                .ok_or_else(|| error(codes::NOT_FOUND, format!("no project {}", p.project_id)))?;
            ok(tokio::task::spawn_blocking(move || project_files(&root, 200)).await.map_err(|e| internal(e.into()))?)
        }
        methods::THREADS_START => {
            let p: ThreadStartParams = required(raw)?;
            ok(live::start(state, p).await?)
        }
        methods::THREADS_SEND => {
            let p: ThreadSendParams = required(raw)?;
            ok(live::send(state, p).await?)
        }
        methods::THREADS_INTERRUPT => {
            let p: ThreadIdParams = required(raw)?;
            ok(live::interrupt(state, &p.thread_id).await?)
        }
        methods::THREADS_LIVE => ok(state.live.list()),
        methods::THREADS_GOAL => {
            let p: ThreadIdParams = required(raw)?;
            ok(live::goal(state, &p.thread_id).await?)
        }
        methods::THREADS_HOLDERS => {
            let p: ThreadIdParams = required(raw)?;
            ok(live::holders(state, &p.thread_id).await?)
        }
        methods::THREADS_WATCH => {
            let p: ThreadIdParams = required(raw)?;
            ok(live::watch(state, &p.thread_id).await?)
        }
        methods::THREADS_CLOSE_ELSEWHERE => {
            let p: ThreadIdParams = required(raw)?;
            ok(live::close_elsewhere(state, &p.thread_id).await?)
        }
        methods::THREADS_ANSWER => {
            let p: emp_protocol::ThreadAnswerParams = required(raw)?;
            ok(live::answer(state, p).await?)
        }
        methods::THREADS_COMMAND => {
            let p: emp_protocol::ThreadCommandParams = required(raw)?;
            ok(live::command(state, p).await?)
        }
        methods::THREADS_TAKEOVER => {
            let p: ThreadTakeoverParams = required(raw)?;
            ok(live::takeover(state, p).await?)
        }
        methods::THREADS_RELEASE => {
            let p: ThreadIdParams = required(raw)?;
            ok(live::release(state, &p.thread_id).await?)
        }
        methods::SCHEDULES_LIST => ok(schedules::list(state).await?),
        methods::SCHEDULES_SAVE => {
            let p: emp_protocol::ScheduleSaveParams = required(raw)?;
            ok(schedules::save(state, p).await?)
        }
        methods::SCHEDULES_DELETE => {
            let p: emp_protocol::ScheduleIdParams = required(raw)?;
            schedules::delete(state, &p.schedule_id).await?;
            ok(Value::Null)
        }
        methods::SCHEDULES_ENABLE => {
            let p: emp_protocol::ScheduleEnableParams = required(raw)?;
            ok(schedules::enable(state, &p.schedule_id, p.enabled).await?)
        }
        methods::SCHEDULES_RUN => {
            let p: emp_protocol::ScheduleIdParams = required(raw)?;
            ok(schedules::run_now(state, &p.schedule_id).await?)
        }
        methods::SCHEDULES_PREVIEW => {
            let p: emp_protocol::SchedulePreviewParams = required(raw)?;
            ok(schedules::preview(&p.when))
        }
        methods::SCHEDULES_EXTERNAL => ok(tokio::task::spawn_blocking(schedules::external).await.unwrap_or_default()),
        other => Err(error(codes::METHOD_NOT_FOUND, format!("unknown method {other}"))),
    }
}

/// `threads.context`: the session's last recorded usage (or what a running driver reported),
/// with the window filled in from the model when the harness didn't say.
async fn thread_context(state: &State, id: String) -> Result<Option<emp_protocol::ContextUsage>, RpcError> {
    let thread = get_thread(state, id).await?;
    let reported = live::reported_context(state, &thread.id);
    let on_disk = match thread.imported_from.as_ref().map(|o| o.source) {
        Some(source) => {
            let t = thread.clone();
            // Unreadable usage isn't an error worth surfacing; the status bar just shows nothing.
            tokio::task::spawn_blocking(move || emp_import::by_kind(source).context(&t)).await.ok().and_then(Result::ok).flatten()
        }
        None => None,
    };
    let window_hint = reported.as_ref().and_then(|c| c.window);
    let Some(mut ctx) = on_disk.or(reported) else { return Ok(None) };
    ctx.window = ctx.window.or(window_hint);
    // The thread's agent, else the harness's own "(default)" profile (imported history).
    let (aid, harness) = (thread.agent_id.clone(), thread.harness);
    let agent_model = state
        .with_store(move |s| {
            Ok(match aid.map(|id| s.get_agent(&id)).transpose()?.flatten() {
                Some(a) => a.model,
                None => s.list_agents()?.into_iter().find(|a| a.harness == harness && a.name.ends_with("(default)")).and_then(|a| a.model),
            })
        })
        .await
        .map_err(internal)?;
    if ctx.window.is_none()
        && let Some(model) = ctx.model.clone().or(thread.model.clone()).or(agent_model.clone())
    {
        ctx.window = models::window_for(thread.harness, &model).await;
    }
    // Claude Code runs 200k unless the 1M variant was picked; the session file doesn't say which.
    if thread.harness == emp_protocol::Harness::Claude
        && (agent_model.as_deref().is_some_and(|m| m.contains("[1m]")) || ctx.used > 200_000)
    {
        ctx.window = Some(1_000_000);
    }
    Ok(Some(ctx))
}

async fn get_thread(state: &State, id: String) -> Result<emp_protocol::Thread, RpcError> {
    let lookup = id.clone();
    state
        .with_store(move |s| s.get_thread(&lookup))
        .await
        .map_err(internal)?
        .ok_or_else(|| error(codes::NOT_FOUND, format!("no thread {id}")))
}

/// Size and modification time of the files a thread's transcript comes from: the session file
/// (Codex/Claude/pi rollouts) and the import source (a SQLite store, with its `-wal`).
fn source_files(thread: &emp_protocol::Thread) -> Vec<(u64, u128)> {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    paths.extend(thread.session.path.as_deref().map(std::path::PathBuf::from));
    if let Some(p) = thread.imported_from.as_ref().map(|o| std::path::PathBuf::from(&o.path)) {
        paths.push(std::path::PathBuf::from(format!("{}-wal", p.display())));
        paths.push(p);
    }
    paths
        .iter()
        .map(|p| match std::fs::metadata(p) {
            Ok(m) => (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())),
            Err(_) => (0, 0),
        })
        .collect()
}

const DEFAULT_TRANSCRIPT_PAGE: u32 = 200;
const MAX_TRANSCRIPT_PAGE: u32 = 1000;

async fn transcript(state: &State, p: TranscriptParams) -> Result<Page<emp_protocol::TranscriptItem>, RpcError> {
    let thread = get_thread(state, p.thread_id).await?;
    let key = (thread.id.clone(), thread.updated_at, source_files(&thread));
    let items = match state.cached_transcript(&key) {
        Some(items) => items,
        None => {
            let source = thread
                .imported_from
                .as_ref()
                .map(|o| o.source)
                .ok_or_else(|| error(codes::NOT_FOUND, "thread has no readable source yet"))?;
            let items = tokio::task::spawn_blocking(move || emp_import::by_kind(source).transcript(&thread))
                .await
                .map_err(|e| error(codes::INTERNAL, e.to_string()))?
                .map_err(internal)?;
            let items = Arc::new(items);
            state.cache_transcript(key, items.clone());
            items
        }
    };
    let total = items.len() as u32;
    let offset = p.offset.unwrap_or(0).min(total);
    let limit = p.limit.unwrap_or(DEFAULT_TRANSCRIPT_PAGE).clamp(1, MAX_TRANSCRIPT_PAGE);
    let end = (offset + limit).min(total);
    Ok(Page { items: items[offset as usize..end as usize].to_vec(), total, offset })
}

/// The full group list, also pushed to every connection (other windows, phones).
async fn groups_changed(state: &State) -> Result<Vec<emp_protocol::ThreadGroup>, RpcError> {
    let groups = state.with_store(|s| s.list_groups()).await.map_err(internal)?;
    let _ = state.notify.send(emp_protocol::RpcNotification::new(emp_protocol::notifications::GROUPS_CHANGED, &groups));
    Ok(groups)
}

/// Compaction limits EMP accepts: 50k–1M tokens (Claude Code itself takes 100k–1M; lower values
/// are raised to its minimum there).
pub const COMPACT_MIN: u64 = 50_000;
pub const COMPACT_MAX: u64 = 1_000_000;

fn check_compact_at(n: u64) -> Result<(), RpcError> {
    if (COMPACT_MIN..=COMPACT_MAX).contains(&n) {
        Ok(())
    } else {
        Err(error(codes::INVALID_PARAMS, format!("compactAt must be between {COMPACT_MIN} and {COMPACT_MAX} tokens")))
    }
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `projects.create`: validates the folder (creating it when asked) and registers it.
async fn create_project(state: &State, p: ProjectCreateParams) -> Result<emp_protocol::Project, RpcError> {
    let raw = p.path.trim();
    if raw.is_empty() {
        return Err(error(codes::INVALID_PARAMS, "path is required"));
    }
    let path = emp_import::util::expand_home(raw);
    if !path.is_absolute() {
        return Err(error(codes::INVALID_PARAMS, "use an absolute path or ~/…"));
    }
    if !path.exists() {
        if !p.create_folder {
            return Err(error(codes::NOT_FOUND, format!("{} doesn't exist", path.display())));
        }
        std::fs::create_dir_all(&path).map_err(|e| error(codes::INTERNAL, format!("create {}: {e}", path.display())))?;
    } else if !path.is_dir() {
        return Err(error(codes::INVALID_PARAMS, format!("{} is a file, not a folder", path.display())));
    }
    let root = path.to_string_lossy().trim_end_matches('/').to_string();
    let name = p.name.clone();
    state.with_store(move |s| s.create_project(&root, name.as_deref())).await.map_err(internal)
}

/// Lists the top level of a project folder: folders first, hidden entries skipped.
pub fn project_files(root: &str, limit: usize) -> emp_protocol::ProjectFiles {
    let mut entries = Vec::new();
    let read = std::fs::read_dir(root);
    let exists = read.is_ok();
    for e in read.into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let meta = e.metadata().ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        entries.push(emp_protocol::FileEntry {
            name,
            is_dir,
            size: meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
            modified_at: meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64),
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let truncated = entries.len() > limit;
    entries.truncate(limit);
    emp_protocol::ProjectFiles { root_path: root.to_string(), exists, entries, truncated }
}

#[cfg(test)]
mod project_files_tests {
    #[test]
    fn folders_first_hidden_skipped_and_capped() {
        let dir = std::env::temp_dir().join(format!("emp-pf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("b.txt"), "hi").unwrap();
        std::fs::write(dir.join("A.md"), "x").unwrap();
        std::fs::write(dir.join(".env"), "secret").unwrap();
        let f = super::project_files(dir.to_str().unwrap(), 10);
        let names: Vec<_> = f.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["src", "A.md", "b.txt"]);
        assert_eq!(f.entries[2].size, Some(2));
        assert!(f.exists && !f.truncated);
        assert!(super::project_files(dir.to_str().unwrap(), 2).truncated);
        assert!(!super::project_files(dir.join("gone").to_str().unwrap(), 10).exists);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
