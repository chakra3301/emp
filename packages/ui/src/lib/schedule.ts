// The schedule editor's timetable form, and the daemon's `ScheduleWhen` it stands for.
// Daily / weekdays / weekly / monthly are cron underneath; an expression that fits none of
// them is edited as raw cron.

import type { Schedule, ScheduleSaveParams, ScheduleWhen } from "@emp/protocol";

export type WhenKind = "once" | "every" | "daily" | "weekdays" | "weekly" | "monthly" | "cron";
export type EveryUnit = "minutes" | "hours" | "days";

export interface WhenForm {
  kind: WhenKind;
  /** `datetime-local` value: "2026-10-21T09:00". */
  at: string;
  every: number;
  unit: EveryUnit;
  /** `time` value: "09:00". */
  time: string;
  /** 0 = Sunday. */
  days: number[];
  dayOfMonth: number;
  cron: string;
}

export const WHEN_KINDS: readonly (readonly [WhenKind, string])[] = [
  ["once", "Once"],
  ["every", "Every…"],
  ["daily", "Daily"],
  ["weekdays", "Weekdays"],
  ["weekly", "Weekly"],
  ["monthly", "Monthly"],
  ["cron", "Cron"],
];

export const DAY_LETTERS = ["S", "M", "T", "W", "T", "F", "S"] as const;
export const DAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"] as const;

const pad = (n: number) => String(n).padStart(2, "0");

/** A `datetime-local` value for `ms` in local time. */
export function localInput(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** The form a new schedule starts with: weekdays at 9, a one-off defaulting to the next hour. */
export function defaultForm(now = Date.now()): WhenForm {
  const nextHour = Math.ceil((now + 60_000) / 3_600_000) * 3_600_000;
  return { kind: "weekdays", at: localInput(nextHour), every: 1, unit: "hours", time: "09:00", days: [1], dayOfMonth: 1, cron: "0 9 * * 1-5" };
}

/** The form for a saved timetable. */
export function whenToForm(when: ScheduleWhen, now = Date.now()): WhenForm {
  const base = defaultForm(now);
  switch (when.kind) {
    case "once":
      return { ...base, kind: "once", at: localInput(when.at) };
    case "every": {
      const m = when.minutes;
      const [every, unit]: [number, EveryUnit] = m % 1440 === 0 ? [m / 1440, "days"] : m % 60 === 0 ? [m / 60, "hours"] : [m, "minutes"];
      return { ...base, kind: "every", every, unit };
    }
    case "cron":
      return { ...base, ...cronToForm(when.expr), cron: when.expr };
  }
}

function cronToForm(expr: string): Partial<WhenForm> {
  const f = expr.trim().split(/\s+/);
  const num = (s: string | undefined, max: number) => (s !== undefined && /^\d+$/.test(s) && Number(s) <= max ? Number(s) : null);
  const [min, hour, dom, mon, dow] = [num(f[0], 59), num(f[1], 23), f[2], f[3], f[4]];
  if (f.length !== 5 || min === null || hour === null || mon !== "*") return { kind: "cron" };
  const time = `${pad(hour)}:${pad(min)}`;
  if (dom === "*" && dow === "*") return { kind: "daily", time };
  if (dom === "*" && dow === "1-5") return { kind: "weekdays", time };
  if (dom === "*" && dow && /^[0-7](,[0-7])*$/.test(dow)) {
    const days = [...new Set(dow.split(",").map((d) => Number(d) % 7))].sort();
    return { kind: "weekly", time, days };
  }
  const day = num(dom, 31);
  if (day && dow === "*") return { kind: "monthly", time, dayOfMonth: day };
  return { kind: "cron" };
}

/** The timetable the form describes; `null` while it's incomplete. */
export function formToWhen(f: WhenForm): ScheduleWhen | null {
  const [h, m] = f.time.split(":").map(Number);
  const hm = Number.isFinite(h) && Number.isFinite(m) ? `${m} ${h}` : null;
  switch (f.kind) {
    case "once": {
      const at = new Date(f.at).getTime();
      return Number.isFinite(at) ? { kind: "once", at } : null;
    }
    case "every": {
      const per = f.unit === "days" ? 1440 : f.unit === "hours" ? 60 : 1;
      const minutes = Math.round(f.every * per);
      return minutes >= 1 ? { kind: "every", minutes } : null;
    }
    case "daily":
      return hm ? { kind: "cron", expr: `${hm} * * *` } : null;
    case "weekdays":
      return hm ? { kind: "cron", expr: `${hm} * * 1-5` } : null;
    case "weekly":
      return hm && f.days.length ? { kind: "cron", expr: `${hm} * * ${[...f.days].sort().join(",")}` } : null;
    case "monthly":
      return hm && f.dayOfMonth >= 1 && f.dayOfMonth <= 31 ? { kind: "cron", expr: `${hm} ${f.dayOfMonth} * *` } : null;
    case "cron":
      return f.cron.trim() ? { kind: "cron", expr: f.cron.trim() } : null;
  }
}

/** "Thu Oct 23, 9:00 AM" for a run time. */
export function runTime(ms: number): string {
  return new Date(ms).toLocaleString(undefined, { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

export type EndsKind = "never" | "date" | "runs";

export interface EndsForm {
  kind: EndsKind;
  /** `datetime-local` value. */
  date: string;
  runs: number;
}

export const ENDS_KINDS: readonly (readonly [EndsKind, string])[] = [
  ["never", "Never"],
  ["date", "On a date"],
  ["runs", "After some runs"],
];

/** The ends form for a draft; a new end date defaults to a week out. */
export function endsToForm(d: Pick<ScheduleSaveParams, "endsAt" | "maxRuns">, now = Date.now()): EndsForm {
  const weekOut = localInput(Math.ceil((now + 7 * 86_400_000) / 3_600_000) * 3_600_000);
  if (d.endsAt != null) return { kind: "date", date: localInput(d.endsAt), runs: d.maxRuns ?? 5 };
  if (d.maxRuns != null) return { kind: "runs", date: weekOut, runs: d.maxRuns };
  return { kind: "never", date: weekOut, runs: 5 };
}

/** What the ends form saves as; `null` while it's incomplete. */
export function formToEnds(f: EndsForm): { endsAt?: number; maxRuns?: number } | null {
  switch (f.kind) {
    case "never":
      return {};
    case "date": {
      const at = new Date(f.date).getTime();
      return Number.isFinite(at) ? { endsAt: at } : null;
    }
    case "runs":
      return Number.isInteger(f.runs) && f.runs >= 1 ? { maxRuns: f.runs } : null;
  }
}

/** How a schedule ends, for its row: "until Oct 30", "3 of 5 runs", "until the deploy is live". */
export function endsText(s: Schedule): string | null {
  if (s.when.kind === "once") return null;
  const parts: string[] = [];
  if (s.endsAt != null) parts.push(`until ${new Date(s.endsAt).toLocaleDateString(undefined, { month: "short", day: "numeric" })}`);
  if (s.maxRuns != null) parts.push(`${s.runCount} of ${s.maxRuns} runs`);
  if (s.stopWhen) parts.push(`stops when ${clause(s.stopWhen)}`);
  return parts.length ? parts.join(", ") : null;
}

/** A condition read mid-sentence: "The file exists." → "the file exists" (acronyms and paths stay). */
function clause(text: string): string {
  const t = text.trim().replace(/[.!]+$/, "");
  return /^[A-Z][a-z]/.test(t) ? t.charAt(0).toLowerCase() + t.slice(1) : t;
}

/** The row's state line: next run, or why it isn't running. */
export function scheduleState(s: Schedule, now: number): string {
  if (s.lastStatus === "running") return "Running now";
  if (s.enabled && s.nextRunAt) return `Next ${relative(s.nextRunAt, now)}`;
  if (s.ended) return `Finished: ${s.ended.charAt(0).toLowerCase()}${s.ended.slice(1)}`;
  if (s.failStreak >= 3) return `Paused after ${s.failStreak} failed runs`;
  if (s.when.kind === "once" && s.runCount > 0) return s.lastStatus === "failed" ? "Failed" : s.lastStatus === "stopped" ? "Stopped" : "Done";
  return "Paused";
}

/** "in 5 min", "in 3 h", "in 2 days" / "5 min ago" for a schedule's times. */
export function relative(ms: number, now: number): string {
  const diff = ms - now;
  const abs = Math.abs(diff);
  const text =
    abs < 60_000
      ? "under a minute"
      : abs < 3_600_000
        ? `${Math.round(abs / 60_000)} min`
        : abs < 86_400_000
          ? `${Math.round(abs / 3_600_000)} h`
          : `${Math.round(abs / 86_400_000)} day${Math.round(abs / 86_400_000) === 1 ? "" : "s"}`;
  return diff >= 0 ? `in ${text}` : `${text} ago`;
}

/** A blank schedule: the first chat agent, weekdays at 9. */
export function newScheduleDraft(agentId: string | null, extra: Partial<ScheduleSaveParams> = {}): ScheduleSaveParams {
  return { title: "", prompt: "", when: formToWhen(defaultForm())!, ...(agentId ? { agentId } : {}), ...extra };
}

export function scheduleDraft(s: Schedule): ScheduleSaveParams {
  return {
    id: s.id,
    title: s.title,
    prompt: s.prompt,
    when: s.when,
    enabled: s.enabled,
    ...(s.agentId ? { agentId: s.agentId } : {}),
    ...(s.cwd ? { cwd: s.cwd } : {}),
    ...(s.projectId ? { projectId: s.projectId } : {}),
    ...(s.threadId ? { threadId: s.threadId } : {}),
    ...(s.endsAt != null ? { endsAt: s.endsAt } : {}),
    ...(s.maxRuns != null ? { maxRuns: s.maxRuns } : {}),
    ...(s.stopWhen ? { stopWhen: s.stopWhen } : {}),
  };
}
