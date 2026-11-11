//! Scheduled tasks: EMP sends a saved prompt to an agent on a timetable, in a new thread each
//! run or in one thread that keeps its history.
//!
//! A run that was due while the daemon was down or the Mac asleep happens once when it's back,
//! not once per missed time. A run due while the previous one is still going is skipped. A run
//! fails when it can't start, its agent process dies, or its turn ends in an error (an API
//! error, not the owner's Stop); three failures in a row pause the schedule, so a broken task
//! doesn't fail forever while one rate limit doesn't stop a daily job.
//!
//! A schedule can end: after a date, after a number of runs, or once a condition holds. The
//! agent checks the condition each run and ends its reply with [`DONE_MARK`] when it does.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{Local, TimeZone};
use emp_protocol::{
    ExternalSchedule, RpcError, RpcNotification, Schedule, ScheduleRunStatus, ScheduleSaveParams, ScheduleWhen, SchedulePreview, SendMode,
    ThreadSendParams, ThreadStartParams, ThreadStatus, codes, notifications,
};
use serde_json::Value;
use tokio::sync::Notify;

use crate::cron::{self, Cron};
use crate::live::{self, TurnOutcome};
use crate::state::State;

/// The longest the loop sleeps between looks (it wakes for the next run, and on any change).
const TICK: Duration = Duration::from_secs(30);
/// How often a run's thread is checked while it works.
const FOLLOW_EVERY: Duration = Duration::from_secs(3);
/// A run still going after this long stops being followed; it's recorded as done.
const FOLLOW_MAX: Duration = Duration::from_secs(12 * 3600);
/// A one-off that comes due while its thread is busy waits this long and tries again.
const ONCE_RETRY_MS: i64 = 60_000;
/// A run whose turn hasn't started after this long is given up on.
const START_MAX: Duration = Duration::from_secs(5 * 60);
/// Failed runs in a row that pause a schedule.
const PAUSE_AFTER_FAILS: u32 = 3;
/// The line an agent ends its reply with when the schedule's stop condition holds.
pub const DONE_MARK: &str = "[[schedule complete]]";

#[derive(Default)]
pub struct Scheduler {
    wake: Notify,
    /// Schedules with a run in flight.
    running: Mutex<HashSet<String>>,
}

impl Scheduler {
    fn busy(&self, id: &str) -> bool {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).contains(id)
    }
    fn claim(&self, id: &str) -> bool {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string())
    }
    fn release(&self, id: &str) {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    }
}

fn err(code: i32, message: impl Into<String>) -> RpcError {
    RpcError { code, message: message.into(), data: None }
}

fn internal(e: anyhow::Error) -> RpcError {
    err(codes::INTERNAL, format!("{e:#}"))
}

// ---- timetables ------------------------------------------------------------------------------

/// Checks a timetable; for cron, the parsed expression.
fn check(when: &ScheduleWhen) -> Result<Option<Cron>, String> {
    match when {
        ScheduleWhen::Once { .. } => Ok(None),
        ScheduleWhen::Every { minutes } if *minutes == 0 => Err("pick at least 1 minute".into()),
        ScheduleWhen::Every { .. } => Ok(None),
        ScheduleWhen::Cron { expr } => Cron::parse(expr).map(Some),
    }
}

/// The first run strictly after `after`. `anchor` is a time `Every` runs at (when it was
/// saved, or the run that just came due), so its rhythm holds.
pub fn next_run(when: &ScheduleWhen, anchor: i64, after: i64) -> Result<Option<i64>, String> {
    Ok(match when {
        ScheduleWhen::Once { at } => (*at > after).then_some(*at),
        ScheduleWhen::Every { minutes } => {
            let period = i64::from(*minutes) * 60_000;
            if period == 0 {
                return Err("pick at least 1 minute".into());
            }
            let k = if after < anchor { 0 } else { (after - anchor) / period + 1 };
            Some(anchor + k * period)
        }
        ScheduleWhen::Cron { expr } => {
            let cron = Cron::parse(expr)?;
            let from = Local.timestamp_millis_opt(after).single().ok_or("that time doesn't exist here")?;
            cron.next_after(from).map(|t| t.timestamp_millis())
        }
    })
}

/// The timetable in words.
pub fn summary(when: &ScheduleWhen) -> String {
    match when {
        ScheduleWhen::Once { at } => match Local.timestamp_millis_opt(*at).single() {
            Some(t) => format!("Once, {}", cron::moment(t)),
            None => "Once".into(),
        },
        ScheduleWhen::Every { minutes } => match *minutes {
            1 => "Every minute".into(),
            m if m % 1440 == 0 => if m == 1440 { "Every day".into() } else { format!("Every {} days", m / 1440) },
            m if m % 60 == 0 => if m == 60 { "Every hour".into() } else { format!("Every {} hours", m / 60) },
            m => format!("Every {m} minutes"),
        },
        ScheduleWhen::Cron { expr } => Cron::parse(expr).ok().and_then(|c| c.describe()).unwrap_or_else(|| format!("Cron {}", expr.trim())),
    }
}

/// `schedules.preview`
pub fn preview(when: &ScheduleWhen) -> SchedulePreview {
    if let Err(e) = check(when) {
        return SchedulePreview { error: Some(e), summary: String::new(), next: Vec::new() };
    }
    let now = emp_core::now_ms();
    let mut next = Vec::new();
    let mut after = now;
    while next.len() < 3 {
        match next_run(when, now, after) {
            Ok(Some(t)) => {
                next.push(t);
                after = t;
            }
            _ => break,
        }
    }
    let error = match when {
        ScheduleWhen::Once { .. } if next.is_empty() => Some("That time has already passed.".into()),
        _ if next.is_empty() => Some("That never comes round.".into()),
        _ => None,
    };
    SchedulePreview { error, summary: summary(when), next }
}

// ---- methods ---------------------------------------------------------------------------------

/// Fills in what the store doesn't keep: the timetable in words, the thread's title.
fn described(store: &emp_core::Store, mut s: Schedule) -> Schedule {
    s.summary = summary(&s.when);
    if let Some(t) = &s.thread_id {
        s.thread_title = store.get_thread(t).ok().flatten().map(|t| t.title);
    }
    s
}

/// `schedules.list`
pub async fn list(state: &State) -> Result<Vec<Schedule>, RpcError> {
    state
        .with_store(|s| Ok(s.list_schedules()?.into_iter().map(|x| described(s, x)).collect()))
        .await
        .map_err(internal)
}

async fn get(state: &State, id: &str) -> Result<Schedule, RpcError> {
    let key = id.to_string();
    state
        .with_store(move |s| Ok(s.get_schedule(&key)?.map(|x| described(s, x))))
        .await
        .map_err(internal)?
        .ok_or_else(|| err(codes::NOT_FOUND, format!("no schedule {id}")))
}

/// Tells every window, and wakes the loop to look at the new timetable.
async fn changed(state: &State) {
    state.schedules.wake.notify_one();
    if let Ok(all) = list(state).await {
        state.notify(RpcNotification::new(notifications::SCHEDULES_CHANGED, &all));
    }
}

/// `schedules.save`
pub async fn save(state: &State, p: ScheduleSaveParams) -> Result<Schedule, RpcError> {
    let prompt = p.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(err(codes::INVALID_PARAMS, "say what the agent should do each run"));
    }
    check(&p.when).map_err(|e| err(codes::INVALID_PARAMS, e))?;
    let thread_id = p.thread_id.filter(|t| !t.is_empty());
    let agent_id = p.agent_id.filter(|a| !a.is_empty());
    if let Some(t) = &thread_id {
        let t = t.clone();
        let thread = state.with_store(move |s| s.get_thread(&t)).await.map_err(internal)?;
        if thread.is_none() {
            return Err(err(codes::NOT_FOUND, "that thread is gone"));
        }
    } else {
        let Some(a) = agent_id.clone() else {
            return Err(err(codes::INVALID_PARAMS, "pick the agent that runs it"));
        };
        if state.with_store(move |s| s.get_agent(&a)).await.map_err(internal)?.is_none() {
            return Err(err(codes::NOT_FOUND, "that agent is gone"));
        }
    }
    let existing = match &p.id {
        Some(id) => Some(get(state, id).await?),
        None => None,
    };
    let now = emp_core::now_ms();
    let enabled = p.enabled.unwrap_or(true);
    let created_at = existing.as_ref().map_or(now, |e| e.created_at);
    // `Every` counts from the save: "every 2 hours" saved at 10:05 runs at 12:05.
    let next_run_at = if enabled { next_run(&p.when, now, now).map_err(|e| err(codes::INVALID_PARAMS, e))? } else { None };
    if enabled && next_run_at.is_none() {
        let why = if matches!(p.when, ScheduleWhen::Once { .. }) { "that time has already passed" } else { "that timetable never comes round" };
        return Err(err(codes::INVALID_PARAMS, why));
    }
    // Ends only apply to repeating schedules.
    let once = matches!(p.when, ScheduleWhen::Once { .. });
    let (ends_at, max_runs) = if once { (None, None) } else { (p.ends_at, p.max_runs) };
    if max_runs == Some(0) {
        return Err(err(codes::INVALID_PARAMS, "let it run at least once"));
    }
    if enabled {
        if let (Some(end), Some(next)) = (ends_at, next_run_at)
            && next > end
        {
            return Err(err(codes::INVALID_PARAMS, "it would end before its first run"));
        }
        let ran = existing.as_ref().map_or(0, |e| e.run_count);
        if let Some(max) = max_runs
            && ran >= max
        {
            return Err(err(codes::INVALID_PARAMS, format!("it has run {ran} times already; allow more runs than that")));
        }
    }
    let title = match p.title.trim() {
        "" => default_title(&prompt),
        t => t.to_string(),
    };
    let schedule = Schedule {
        id: p.id.unwrap_or_else(|| ulid::Ulid::new().to_string()),
        title,
        prompt,
        summary: String::new(),
        when: p.when,
        agent_id: if thread_id.is_some() { None } else { agent_id },
        cwd: p.cwd.filter(|c| !c.trim().is_empty()),
        project_id: p.project_id.filter(|c| !c.is_empty()),
        thread_id,
        thread_title: None,
        enabled,
        next_run_at,
        last_run_at: None,
        last_status: None,
        last_error: None,
        last_thread_id: None,
        run_count: 0,
        ends_at,
        max_runs,
        stop_when: p.stop_when.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
        // Turning it back on starts it over; saving it paused keeps why it finished.
        ended: if enabled { None } else { existing.as_ref().and_then(|e| e.ended.clone()) },
        fail_streak: 0,
        created_at,
    };
    let row = schedule.clone();
    state.with_store(move |s| s.save_schedule(&row)).await.map_err(internal)?;
    changed(state).await;
    get(state, &schedule.id).await
}

/// The prompt's first line, cut to fit a list row.
fn default_title(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        out.push('…');
    }
    out
}

/// `schedules.delete`
pub async fn delete(state: &State, id: &str) -> Result<(), RpcError> {
    let key = id.to_string();
    if !state.with_store(move |s| s.delete_schedule(&key)).await.map_err(internal)? {
        return Err(err(codes::NOT_FOUND, format!("no schedule {id}")));
    }
    changed(state).await;
    Ok(())
}

/// `schedules.enable`: resuming picks the timetable up from now (missed runs aren't made up).
pub async fn enable(state: &State, id: &str, on: bool) -> Result<Schedule, RpcError> {
    let s = get(state, id).await?;
    let next = if on {
        let now = emp_core::now_ms();
        let next = next_run(&s.when, now, now).map_err(|e| err(codes::INVALID_PARAMS, e))?;
        if next.is_none() {
            return Err(err(codes::INVALID_PARAMS, "that time has passed; edit it to pick a new one"));
        }
        if let (Some(end), Some(n)) = (s.ends_at, next)
            && n > end
        {
            return Err(err(codes::INVALID_PARAMS, "its end date has passed; edit it to pick a later one"));
        }
        if let Some(max) = s.max_runs
            && s.run_count >= max
        {
            return Err(err(codes::INVALID_PARAMS, format!("it has used all {max} runs; edit it to allow more")));
        }
        next
    } else {
        None
    };
    let key = id.to_string();
    if on {
        state.with_store(move |st| st.resume_schedule(&key, next)).await.map_err(internal)?;
    } else {
        state.with_store(move |st| st.set_schedule_next(&key, None, false)).await.map_err(internal)?;
    }
    changed(state).await;
    get(state, id).await
}

/// `schedules.run`: one run now; the timetable carries on as it was.
pub async fn run_now(state: &State, id: &str) -> Result<Schedule, RpcError> {
    let s = get(state, id).await?;
    if state.schedules.busy(&s.id) {
        return Err(err(codes::BUSY, "it's running already"));
    }
    if let Some(why) = thread_busy(state, &s) {
        return Err(err(codes::BUSY, why));
    }
    fire(state.clone(), s.clone());
    Ok(s)
}

// ---- the loop --------------------------------------------------------------------------------

pub fn spawn(state: &State) {
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            let nap = match tick(&state).await {
                Ok(Some(next)) => Duration::from_millis((next - emp_core::now_ms()).clamp(0, TICK.as_millis() as i64) as u64),
                Ok(None) => TICK,
                Err(e) => {
                    tracing::warn!("schedules: {e:#}");
                    TICK
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(nap) => {}
                _ = state.schedules.wake.notified() => {}
            }
        }
    });
}

/// Fires what's due; returns when the next run is.
async fn tick(state: &State) -> anyhow::Result<Option<i64>> {
    let now = emp_core::now_ms();
    let due = state.with_store(move |s| s.due_schedules(now)).await?;
    for s in due {
        // "Run now" can use up the last runs between ticks.
        if let Some(max) = s.max_runs
            && s.run_count >= max
            && !matches!(s.when, ScheduleWhen::Once { .. })
        {
            let (key, why) = (s.id.clone(), format!("Ran {max} times"));
            state.with_store(move |st| st.end_schedule(&key, &why)).await?;
            continue;
        }
        let busy = state.schedules.busy(&s.id) || thread_busy(state, &s).is_some();
        // Move the timetable on first, so a slow start can't fire it twice. A one-off whose
        // thread is busy waits a minute; a repeating one skips this time.
        let (next, go) = match &s.when {
            ScheduleWhen::Once { .. } if busy => (Some(now + ONCE_RETRY_MS), false),
            ScheduleWhen::Once { .. } => (None, true),
            when => (next_run(when, s.next_run_at.unwrap_or(now), now).ok().flatten(), !busy),
        };
        let key = s.id.clone();
        match ending(&s, next, go) {
            Some(why) => state.with_store(move |st| st.end_schedule(&key, &why)).await?,
            None => state.with_store(move |st| st.set_schedule_next(&key, next, next.is_some())).await?,
        }
        if go {
            fire(state.clone(), s);
        } else if !matches!(s.when, ScheduleWhen::Once { .. }) {
            tracing::info!(schedule = %s.id, "skipped a run: the last one is still going");
        }
    }
    let all = state.with_store(|s| s.list_schedules()).await?;
    Ok(all.iter().filter(|s| s.enabled).filter_map(|s| s.next_run_at).min())
}

/// Why a repeating schedule finishes at this tick: this run (`go`) is its last allowed one,
/// or its next time is past its end date.
fn ending(s: &Schedule, next: Option<i64>, go: bool) -> Option<String> {
    if matches!(s.when, ScheduleWhen::Once { .. }) {
        return None;
    }
    if let Some(max) = s.max_runs
        && go
        && s.run_count + 1 >= max
    {
        return Some(if max == 1 { "Ran once".into() } else { format!("Ran {max} times") });
    }
    match (s.ends_at, next) {
        (Some(end), Some(n)) if n > end => Some("Reached its end date".into()),
        _ => None,
    }
}

/// Why the schedule's thread can't take a run right now.
fn thread_busy(state: &State, s: &Schedule) -> Option<&'static str> {
    let id = s.thread_id.as_deref()?;
    let live = state.live.list().into_iter().find(|l| l.thread_id == id)?;
    match live.status {
        ThreadStatus::Running => Some("its thread is in the middle of a turn"),
        ThreadStatus::NeedsInput => Some("its thread is waiting for you"),
        _ => None,
    }
}

/// What the agent gets: the prompt, plus a note (hidden in EMP's transcript) on where it came from.
fn message(s: &Schedule) -> String {
    let stop = match &s.stop_when {
        Some(c) => format!(
            "\nThis schedule stops once this is true: {c}\nCheck it this run. Only if it's true, put this line on its own at the very end of your reply: {DONE_MARK}"
        ),
        None => String::new(),
    };
    format!(
        "{}\n\n<emp-context>\nScheduled run of \"{}\" ({}). The owner set this up in EMP and isn't watching; they'll read your reply later, so end with a short report of what you found or did.{stop}\n</emp-context>",
        s.prompt,
        s.title,
        summary(&s.when)
    )
}

/// Starts one run in the background and follows it to the end.
fn fire(state: State, s: Schedule) {
    if !state.schedules.claim(&s.id) {
        return;
    }
    tokio::spawn(async move {
        let id = s.id.clone();
        let ran = run(&state, &s).await.unwrap_or_else(|e| Ran { status: ScheduleRunStatus::Failed, error: Some(e), met: false });
        let (key, status, error) = (id.clone(), ran.status, ran.error.clone());
        let _ = state.with_store(move |st| st.set_schedule_run(&key, false, status, error.as_deref(), None)).await;
        if ran.met {
            tracing::info!(schedule = %id, "its stop condition holds; finished");
            let key = id.clone();
            let _ = state.with_store(move |st| st.end_schedule(&key, "Condition met")).await;
        } else if ran.status == ScheduleRunStatus::Failed {
            let key = id.clone();
            let streak = state.with_store(move |st| st.get_schedule(&key)).await.ok().flatten().map_or(0, |s| s.fail_streak);
            tracing::warn!(schedule = %id, streak, "run failed: {}", ran.error.as_deref().unwrap_or("?"));
            if streak >= PAUSE_AFTER_FAILS {
                let key = id.clone();
                let _ = state.with_store(move |st| st.set_schedule_next(&key, None, false)).await;
            }
        }
        state.schedules.release(&id);
        changed(&state).await;
    });
}

/// How a run went.
#[derive(Debug, PartialEq)]
struct Ran {
    status: ScheduleRunStatus,
    error: Option<String>,
    /// The agent said the stop condition holds.
    met: bool,
}

fn judge(s: &Schedule, turn: &TurnOutcome) -> Ran {
    if turn.stopped {
        return Ran { status: ScheduleRunStatus::Stopped, error: None, met: false };
    }
    if let Some(e) = &turn.error {
        return Ran { status: ScheduleRunStatus::Failed, error: Some(e.clone()), met: false };
    }
    let met = s.stop_when.is_some() && turn.reply.as_deref().is_some_and(|r| r.contains(DONE_MARK));
    Ran { status: ScheduleRunStatus::Done, error: None, met }
}

async fn run(state: &State, s: &Schedule) -> Result<Ran, String> {
    let text = message(s);
    let sent_at = live::turn_clock();
    let thread_id = match &s.thread_id {
        Some(t) => {
            let p = ThreadSendParams { thread_id: t.clone(), text, attachments: None, mode: Some(SendMode::Prompt), force: None, model: None, context: None };
            live::send(state, p).await.map_err(|e| e.message)?;
            t.clone()
        }
        None => {
            let agent_id = s.agent_id.clone().ok_or("it has no agent")?;
            let p = ThreadStartParams {
                agent_id,
                text,
                cwd: s.cwd.clone(),
                project_id: s.project_id.clone(),
                model: None,
                effort: None,
                attachments: None,
                context: None,
            };
            live::start(state, p).await.map_err(|e| e.message)?.id
        }
    };
    let (key, tid) = (s.id.clone(), thread_id.clone());
    let _ = state.with_store(move |st| st.set_schedule_run(&key, true, ScheduleRunStatus::Running, None, Some(&tid))).await;
    changed(state).await;

    // Follow it until a turn that ended after the send.
    let started = tokio::time::Instant::now();
    let mut busy_at = started;
    loop {
        tokio::time::sleep(FOLLOW_EVERY).await;
        let Some((status, error, turn)) = state.live.turn_state(&thread_id) else {
            return Ok(Ran { status: ScheduleRunStatus::Done, error: None, met: false });
        };
        match status {
            ThreadStatus::Failed => return Err(error.unwrap_or_else(|| "the agent stopped".into())),
            _ if started.elapsed() >= FOLLOW_MAX => return Ok(Ran { status: ScheduleRunStatus::Done, error: None, met: false }),
            ThreadStatus::Running | ThreadStatus::NeedsInput => busy_at = tokio::time::Instant::now(),
            _ if turn.ended > sent_at => return Ok(judge(s, &turn)),
            _ if busy_at.elapsed() >= START_MAX => return Err("the agent never picked the run up".into()),
            _ => {}
        }
    }
}

// ---- other tools' schedules ------------------------------------------------------------------

/// `schedules.external`: Hermes's own cron jobs, read from its `cron/jobs.json` files. EMP only
/// reads them; `hermes cron` changes them.
pub fn external() -> Vec<ExternalSchedule> {
    let root = std::env::var_os("HERMES_HOME").map(PathBuf::from).unwrap_or_else(|| emp_core::paths::home_dir().join(".hermes"));
    hermes_jobs(&root)
}

/// `root/cron` is the profile "default"; each `root/profiles/<name>/cron` is another.
fn hermes_jobs(root: &Path) -> Vec<ExternalSchedule> {
    let mut files = vec![("default".to_string(), root.join("cron/jobs.json"))];
    if let Ok(dir) = std::fs::read_dir(root.join("profiles")) {
        let mut names: Vec<String> =
            dir.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        files.extend(names.into_iter().map(|p| {
            let file = root.join("profiles").join(&p).join("cron/jobs.json");
            (p, file)
        }));
    }
    let mut out = Vec::new();
    for (profile, file) in files {
        let Ok(raw) = std::fs::read_to_string(&file) else { continue };
        match serde_json::from_str::<Value>(&raw) {
            Ok(v) => out.extend(v["jobs"].as_array().into_iter().flatten().map(|j| hermes_job(&profile, j))),
            Err(e) => tracing::debug!("hermes cron {}: {e}", file.display()),
        }
    }
    out
}

fn hermes_job(profile: &str, j: &Value) -> ExternalSchedule {
    let text = |k: &str| j[k].as_str().map(str::trim).filter(|x| !x.is_empty()).map(String::from);
    let time = |k: &str| text(k).and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok()).map(|t| t.timestamp_millis());
    let sched = &j["schedule"];
    let summary = match (sched["kind"].as_str(), sched["expr"].as_str()) {
        (Some("cron"), Some(expr)) => summary(&ScheduleWhen::Cron { expr: expr.into() }),
        _ => sched["display"].as_str().or(j["schedule_display"].as_str()).unwrap_or("Custom timetable").to_string(),
    };
    let state = text("state").unwrap_or_else(|| "scheduled".into());
    let script = text("script").map(|s| format!("Runs {s}"));
    let task = if j["no_agent"] == true { script.clone() } else { None }
        .or_else(|| text("prompt"))
        .or(script)
        .or_else(|| text("skill").map(|k| format!("Skill: {k}")))
        .unwrap_or_default();
    ExternalSchedule {
        source: "hermes".into(),
        profile: profile.into(),
        id: text("id").unwrap_or_default(),
        name: text("name").unwrap_or_else(|| "Hermes job".into()),
        summary,
        enabled: j["enabled"] != false && state != "paused" && state != "completed",
        state,
        next_run_at: time("next_run_at"),
        last_run_at: time("last_run_at"),
        last_status: text("last_status"),
        last_error: text("last_error"),
        task,
        deliver: text("deliver"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_keeps_its_rhythm() {
        let every = ScheduleWhen::Every { minutes: 30 };
        let anchor = 1_000_000;
        assert_eq!(next_run(&every, anchor, anchor).unwrap(), Some(anchor + 1_800_000));
        // Long after (the Mac slept): the next slot after now, not a backlog.
        assert_eq!(next_run(&every, anchor, anchor + 10 * 1_800_000 + 5).unwrap(), Some(anchor + 11 * 1_800_000));
        let once = ScheduleWhen::Once { at: 5_000 };
        assert_eq!(next_run(&once, 0, 4_999).unwrap(), Some(5_000));
        assert_eq!(next_run(&once, 0, 5_000).unwrap(), None);
    }

    #[test]
    fn previews_and_words() {
        assert_eq!(summary(&ScheduleWhen::Every { minutes: 120 }), "Every 2 hours");
        assert_eq!(summary(&ScheduleWhen::Every { minutes: 90 }), "Every 90 minutes");
        assert_eq!(summary(&ScheduleWhen::Cron { expr: "0 9 * * 1-5".into() }), "Weekdays at 9:00 AM");
        let p = preview(&ScheduleWhen::Cron { expr: "*/5 * * * *".into() });
        assert_eq!((p.error, p.next.len()), (None, 3));
        assert!(p.next.windows(2).all(|w| w[1] - w[0] == 300_000));
        assert_eq!(preview(&ScheduleWhen::Once { at: 1 }).error.as_deref(), Some("That time has already passed."));
        assert!(preview(&ScheduleWhen::Cron { expr: "bogus".into() }).error.unwrap().contains("5 fields"));
        assert_eq!(default_title("Check the build\nthen more"), "Check the build");
    }

    fn schedule(when: ScheduleWhen) -> Schedule {
        Schedule {
            id: "s".into(),
            title: "t".into(),
            prompt: "p".into(),
            when,
            summary: String::new(),
            agent_id: Some("a".into()),
            cwd: None,
            project_id: None,
            thread_id: None,
            thread_title: None,
            enabled: true,
            next_run_at: Some(1_000),
            last_run_at: None,
            last_status: None,
            last_error: None,
            last_thread_id: None,
            run_count: 0,
            ends_at: None,
            max_runs: None,
            stop_when: None,
            ended: None,
            fail_streak: 0,
            created_at: 0,
        }
    }

    #[test]
    fn schedules_end() {
        let mut s = schedule(ScheduleWhen::Every { minutes: 60 });
        assert_eq!(ending(&s, Some(5_000), true), None);
        s.max_runs = Some(3);
        s.run_count = 1;
        assert_eq!(ending(&s, Some(5_000), true), None);
        s.run_count = 2;
        assert_eq!(ending(&s, Some(5_000), true).as_deref(), Some("Ran 3 times"));
        // A skipped run doesn't use one up.
        assert_eq!(ending(&s, Some(5_000), false), None);
        s.max_runs = None;
        s.ends_at = Some(4_000);
        assert_eq!(ending(&s, Some(5_000), true).as_deref(), Some("Reached its end date"));
        assert_eq!(ending(&s, Some(3_000), true), None);
        assert_eq!(ending(&schedule(ScheduleWhen::Once { at: 1 }), None, true), None);
    }

    #[test]
    fn runs_are_judged_by_their_turn() {
        let mut s = schedule(ScheduleWhen::Every { minutes: 60 });
        let turn = |reply: &str| TurnOutcome { reply: Some(reply.into()), ended: 2, ..Default::default() };
        let ran = |s: &Schedule, t: &TurnOutcome| {
            let r = judge(s, t);
            (r.status, r.met)
        };
        assert_eq!(ran(&s, &turn("all green")), (ScheduleRunStatus::Done, false));
        // The mark only counts when there's a condition to meet.
        assert_eq!(ran(&s, &turn(&format!("live\n{DONE_MARK}"))), (ScheduleRunStatus::Done, false));
        s.stop_when = Some("the deploy is live".into());
        assert_eq!(ran(&s, &turn("not yet")), (ScheduleRunStatus::Done, false));
        assert_eq!(ran(&s, &turn(&format!("It's live.\n{DONE_MARK}"))), (ScheduleRunStatus::Done, true));
        let failed = TurnOutcome { error: Some("rate limited".into()), ended: 2, ..Default::default() };
        assert_eq!(judge(&s, &failed).error.as_deref(), Some("rate limited"));
        assert_eq!(ran(&s, &failed).0, ScheduleRunStatus::Failed);
        let stopped = TurnOutcome { stopped: true, error: Some("aborted".into()), ..Default::default() };
        assert_eq!(ran(&s, &stopped).0, ScheduleRunStatus::Stopped);
        assert!(message(&s).contains(DONE_MARK) && message(&s).contains("the deploy is live"));
    }

    #[test]
    fn reads_hermes_cron_jobs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("cron")).unwrap();
        std::fs::create_dir_all(root.join("profiles/studio/cron")).unwrap();
        std::fs::create_dir_all(root.join("profiles/empty")).unwrap();
        std::fs::write(
            root.join("cron/jobs.json"),
            r#"{"jobs":[{"id":"a1","name":"standup","prompt":"Summarise yesterday","schedule":{"kind":"interval","display":"every 30m"},"enabled":false,"state":"paused"}]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("profiles/studio/cron/jobs.json"),
            r#"{"jobs":[{"id":"b2","name":"monthly-skill-review","prompt":"","script":"review.sh","no_agent":true,
               "schedule":{"kind":"cron","expr":"0 10 1 * *"},"enabled":true,"state":"scheduled",
               "next_run_at":"2026-11-01T10:00:00-04:00","last_run_at":"2026-10-09T13:37:29.733258-04:00","last_status":"ok","deliver":"telegram"}]}"#,
        )
        .unwrap();
        let jobs = hermes_jobs(root);
        assert_eq!(jobs.len(), 2);
        assert_eq!((jobs[0].profile.as_str(), jobs[0].summary.as_str(), jobs[0].enabled), ("default", "every 30m", false));
        assert_eq!(jobs[0].task, "Summarise yesterday");
        let j = &jobs[1];
        assert_eq!((j.profile.as_str(), j.name.as_str(), j.task.as_str()), ("studio", "monthly-skill-review", "Runs review.sh"));
        assert!(j.enabled && j.summary.starts_with("Monthly"), "{}", j.summary);
        assert_eq!(j.next_run_at, Some(1_793_541_600_000));
        assert_eq!((j.last_status.as_deref(), j.deliver.as_deref()), (Some("ok"), Some("telegram")));
        assert!(hermes_jobs(&root.join("nowhere")).is_empty());
    }
}
