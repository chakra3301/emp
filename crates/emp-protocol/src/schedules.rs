//! Scheduled tasks: a saved prompt that EMP sends an agent on a timetable, either in a new
//! thread each run or in one thread that keeps its history.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// When a schedule runs. Times are the Mac's local time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScheduleWhen {
    /// Once, at this moment (ms since epoch).
    #[serde(rename_all = "camelCase")]
    Once {
        #[ts(type = "number")]
        at: i64,
    },
    /// Every `minutes`, counted from when the schedule was saved.
    #[serde(rename_all = "camelCase")]
    Every { minutes: u32 },
    /// A five-field cron expression: minute hour day-of-month month day-of-week.
    #[serde(rename_all = "camelCase")]
    Cron { expr: String },
}

/// How the last run went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum ScheduleRunStatus {
    Running,
    Done,
    /// The run couldn't start, or its turn ended in an error.
    Failed,
    /// It was due while the previous run was still going.
    Skipped,
    /// The owner stopped the turn.
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub title: String,
    /// What the agent is told each run.
    pub prompt: String,
    pub when: ScheduleWhen,
    /// The timetable in words ("Weekdays at 9:00").
    pub summary: String,
    /// The agent that runs it (new thread each run). Ignored when `thread_id` is set.
    pub agent_id: Option<String>,
    /// Folder for new threads; falls back to the project root, then the agent's folder.
    pub cwd: Option<String>,
    pub project_id: Option<String>,
    /// Runs continue this thread instead of starting new ones.
    pub thread_id: Option<String>,
    /// That thread's title, for the list.
    pub thread_title: Option<String>,
    /// False while paused, and after a one-off has run.
    pub enabled: bool,
    #[ts(type = "number | null")]
    pub next_run_at: Option<i64>,
    #[ts(type = "number | null")]
    pub last_run_at: Option<i64>,
    pub last_status: Option<ScheduleRunStatus>,
    pub last_error: Option<String>,
    /// The thread the last run went to.
    pub last_thread_id: Option<String>,
    pub run_count: u32,
    /// No runs after this moment (ms since epoch).
    #[ts(type = "number | null")]
    pub ends_at: Option<i64>,
    /// It finishes after this many runs.
    pub max_runs: Option<u32>,
    /// It finishes once this is true: the agent checks each run and says when it is.
    pub stop_when: Option<String>,
    /// Why it finished on its own ("Condition met"). None while it's on or only paused.
    pub ended: Option<String>,
    /// Failed runs in a row; three in a row pause it.
    pub fail_streak: u32,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// Creates a schedule (no `id`) or replaces one.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleSaveParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    pub title: String,
    pub prompt: String,
    pub when: ScheduleWhen,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_id: Option<String>,
    /// Default true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub ends_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_runs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_when: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleIdParams {
    pub schedule_id: String,
}

/// Pauses or resumes a schedule.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleEnableParams {
    pub schedule_id: String,
    pub enabled: bool,
}

/// `schedules.preview`: checks a timetable before saving it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SchedulePreviewParams {
    pub when: ScheduleWhen,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SchedulePreview {
    /// Why the timetable can't be used; the rest is empty then.
    pub error: Option<String>,
    pub summary: String,
    /// The next few runs (ms since epoch).
    #[ts(type = "number[]")]
    pub next: Vec<i64>,
}

/// A job another tool schedules itself (Hermes's cron), shown read-only next to EMP's.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSchedule {
    /// The tool that runs it: "hermes".
    pub source: String,
    /// Its profile ("default" for `~/.hermes`).
    pub profile: String,
    pub id: String,
    pub name: String,
    /// The timetable in words.
    pub summary: String,
    pub enabled: bool,
    /// The tool's own state word ("scheduled", "paused", "completed").
    pub state: String,
    #[ts(type = "number | null")]
    pub next_run_at: Option<i64>,
    #[ts(type = "number | null")]
    pub last_run_at: Option<i64>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    /// What it does: the prompt, or the script it runs.
    pub task: String,
    /// Where the result goes ("telegram", "local").
    pub deliver: Option<String>,
}
