import { describe, expect, it } from "vitest";
import type { Schedule } from "@emp/protocol";
import { defaultForm, endsText, endsToForm, formToEnds, formToWhen, localInput, relative, scheduleState, whenToForm } from "./schedule";

describe("schedule timetables", () => {
  const now = new Date(2026, 9, 20, 15, 13).getTime();

  it("turns the form into a timetable", () => {
    const f = defaultForm(now);
    expect(f.at).toBe("2026-10-20T16:00");
    expect(formToWhen(f)).toEqual({ kind: "cron", expr: "0 9 * * 1-5" });
    expect(formToWhen({ ...f, kind: "daily", time: "17:30" })).toEqual({ kind: "cron", expr: "30 17 * * *" });
    expect(formToWhen({ ...f, kind: "weekly", days: [4, 1] })).toEqual({ kind: "cron", expr: "0 9 * * 1,4" });
    expect(formToWhen({ ...f, kind: "weekly", days: [] })).toBeNull();
    expect(formToWhen({ ...f, kind: "monthly", dayOfMonth: 15 })).toEqual({ kind: "cron", expr: "0 9 15 * *" });
    expect(formToWhen({ ...f, kind: "every", every: 2, unit: "hours" })).toEqual({ kind: "every", minutes: 120 });
    expect(formToWhen({ ...f, kind: "every", every: 0, unit: "hours" })).toBeNull();
    expect(formToWhen({ ...f, kind: "once" })).toEqual({ kind: "once", at: new Date(2026, 9, 20, 16, 0).getTime() });
    expect(formToWhen({ ...f, kind: "cron", cron: "  */5 * * * * " })).toEqual({ kind: "cron", expr: "*/5 * * * *" });
  });

  it("reads a saved timetable back into the form", () => {
    expect(whenToForm({ kind: "cron", expr: "0 9 * * 1-5" }, now)).toMatchObject({ kind: "weekdays", time: "09:00" });
    expect(whenToForm({ kind: "cron", expr: "30 17 * * *" }, now)).toMatchObject({ kind: "daily", time: "17:30" });
    expect(whenToForm({ kind: "cron", expr: "0 8 * * 7,1" }, now)).toMatchObject({ kind: "weekly", days: [0, 1] });
    expect(whenToForm({ kind: "cron", expr: "0 8 1 * *" }, now)).toMatchObject({ kind: "monthly", dayOfMonth: 1 });
    expect(whenToForm({ kind: "cron", expr: "*/15 9-17 * * 1-5" }, now)).toMatchObject({ kind: "cron", cron: "*/15 9-17 * * 1-5" });
    expect(whenToForm({ kind: "every", minutes: 2880 }, now)).toMatchObject({ kind: "every", every: 2, unit: "days" });
    expect(whenToForm({ kind: "every", minutes: 45 }, now)).toMatchObject({ kind: "every", every: 45, unit: "minutes" });
    const at = new Date(2026, 9, 21, 9, 5).getTime();
    expect(whenToForm({ kind: "once", at }, now).at).toBe(localInput(at));
  });

  it("says when", () => {
    expect(relative(now + 3 * 3_600_000, now)).toBe("in 3 h");
    expect(relative(now - 5 * 60_000, now)).toBe("5 min ago");
    expect(relative(now + 86_400_000, now)).toBe("in 1 day");
  });

  it("ends a schedule by date, runs, or never", () => {
    const blank = endsToForm({}, now);
    expect(blank).toMatchObject({ kind: "never", runs: 5 });
    expect(formToEnds(blank)).toEqual({});
    expect(formToEnds({ ...blank, kind: "runs", runs: 3 })).toEqual({ maxRuns: 3 });
    expect(formToEnds({ ...blank, kind: "runs", runs: 0 })).toBeNull();
    const end = new Date(2026, 9, 30, 18, 0).getTime();
    expect(formToEnds({ ...blank, kind: "date", date: localInput(end) })).toEqual({ endsAt: end });
    expect(endsToForm({ endsAt: end }, now)).toMatchObject({ kind: "date", date: localInput(end) });
    expect(endsToForm({ maxRuns: 4 }, now)).toMatchObject({ kind: "runs", runs: 4 });
  });

  it("says how a schedule stands", () => {
    const base: Schedule = {
      id: "s",
      title: "t",
      prompt: "p",
      when: { kind: "every", minutes: 60 },
      summary: "Every hour",
      agentId: "a",
      cwd: null,
      projectId: null,
      threadId: null,
      threadTitle: null,
      enabled: true,
      nextRunAt: now + 3_600_000,
      lastRunAt: null,
      lastStatus: null,
      lastError: null,
      lastThreadId: null,
      runCount: 2,
      endsAt: null,
      maxRuns: 5,
      stopWhen: "the deploy is live",
      ended: null,
      failStreak: 0,
      createdAt: now,
    };
    expect(scheduleState(base, now)).toBe("Next in 1 h");
    expect(endsText(base)).toBe("2 of 5 runs, stops when the deploy is live");
    const off = { ...base, enabled: false, nextRunAt: null };
    expect(scheduleState({ ...off, ended: "Condition met" }, now)).toBe("Finished: condition met");
    expect(scheduleState({ ...off, failStreak: 3, lastStatus: "failed" }, now)).toBe("Paused after 3 failed runs");
    expect(scheduleState(off, now)).toBe("Paused");
    const once = { ...off, when: { kind: "once", at: now } as const, runCount: 1 };
    expect(scheduleState({ ...once, lastStatus: "done" }, now)).toBe("Done");
    expect(scheduleState({ ...once, lastStatus: "stopped" }, now)).toBe("Stopped");
    expect(endsText(once)).toBeNull();
    expect(endsText({ ...base, maxRuns: null, stopWhen: "The file /tmp/x exists." })).toBe("stops when the file /tmp/x exists");
    expect(endsText({ ...base, maxRuns: null, stopWhen: "CI is green" })).toBe("stops when CI is green");
  });
});
