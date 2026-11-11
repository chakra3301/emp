// Bars above a thread's composer: the Codex goal the agent keeps working on, and the other
// window (a Terminal Codex, a claude TUI) that has the same session open.

import { useEffect, useState } from "react";
import type { GoalStatus, Schedule, Thread, ThreadGoal } from "@emp/protocol";
import { relative, scheduleDraft } from "../lib/schedule";
import { useWorkspace } from "../store/context";
import { liveFor } from "../store/workspace";
import { IconClock, IconGoal, IconTerminal } from "./icons";

const STATUS: Record<GoalStatus, string> = {
  active: "Working on it",
  paused: "Paused",
  blocked: "Blocked: needs you",
  "usage-limited": "Paused at the usage limit",
  "budget-limited": "Out of budget",
  complete: "Done",
};

function tokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}k`;
  return String(n);
}

function elapsed(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86_400) return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
  return `${Math.floor(secs / 86_400)}d ${Math.floor((secs % 86_400) / 3600)}h`;
}

/** The thread's goal: live while EMP runs it, else what Codex stored. */
const NO_SCHEDULES: Schedule[] = [];

/** The schedules that send this thread a message on a timetable. */
export function ScheduleBar({ thread }: { thread: Thread }) {
  const all = useWorkspace((s) => s.schedules.data) ?? NO_SCHEDULES;
  const now = useWorkspace((s) => s.now);
  const enable = useWorkspace((s) => s.enableSchedule);
  const run = useWorkspace((s) => s.runSchedule);
  const edit = useWorkspace((s) => s.editSchedule);
  const mine = all.filter((s) => s.threadId === thread.id);
  return (
    <>
      {mine.map((s) => {
        const running = s.lastStatus === "running";
        const state = running ? "Running now" : s.enabled && s.nextRunAt ? `next ${relative(s.nextRunAt, now)}` : s.lastStatus === "failed" ? "Paused after a failed run" : "Paused";
        return (
          <section key={s.id} className="emp-goal" data-status={running ? "active" : "paused"} aria-label="Schedule">
            <span className="emp-goal__icon" aria-hidden="true">
              <IconClock />
            </span>
            <button type="button" className="emp-goal__body" title={s.prompt} onClick={() => edit(scheduleDraft(s))}>
              <span className="emp-goal__label">
                Scheduled · {s.summary} <span className="emp-goal__usage">{state}</span>
              </span>
              <span className="emp-goal__objective emp-truncate">{s.title}</span>
            </button>
            <span className="emp-goal__actions">
              <button type="button" className="emp-button emp-button--small" data-orion-ui="button" disabled={running} onClick={() => void run(s.id)}>
                Run now
              </button>
              <button type="button" className="emp-button emp-button--small" data-orion-ui="button" onClick={() => void enable(s.id, !s.enabled)}>
                {s.enabled ? "Pause" : "Resume"}
              </button>
            </span>
          </section>
        );
      })}
    </>
  );
}

export function GoalBar({ thread }: { thread: Thread }) {
  const liveGoal = useWorkspace((s) => liveFor(s, thread.id)?.goal);
  const hasDriver = useWorkspace((s) => liveFor(s, thread.id)?.owner === "driver");
  const stored = useWorkspace((s) => s.goals[thread.id]);
  const loadGoal = useWorkspace((s) => s.loadGoal);
  const watch = useWorkspace((s) => s.watch);
  // Codex isn't working on it right now (an "active" goal whose window closed, say).
  const [stalled, setStalled] = useState(false);
  const runNative = useWorkspace((s) => s.runNative);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const codex = thread.harness === "codex";

  useEffect(() => {
    if (codex) void loadGoal(thread.id);
  }, [codex, thread.id, loadGoal]);
  // The live goal ends with the driver; re-read the stored one then. If Codex is working on
  // the thread without EMP (a goal, a turn from its Terminal), attach and stream it.
  useEffect(() => {
    if (!codex || hasDriver) return;
    let gone = false;
    void loadGoal(thread.id);
    void watch(thread.id).then((attached) => {
      if (!gone) setStalled(!attached);
    });
    return () => {
      gone = true;
    };
  }, [codex, hasDriver, thread.id, loadGoal, watch]);

  const goal: ThreadGoal | null | undefined = hasDriver ? liveGoal : (stored ?? liveGoal);
  if (!codex || !goal) return null;
  const run = async (line: string) => {
    setBusy(true);
    await runNative(thread.id, line);
    setBusy(false);
  };
  const waiting = goal.status === "active" && !hasDriver && stalled;
  const usage = `${tokens(goal.tokensUsed)}${goal.tokenBudget ? ` of ${tokens(goal.tokenBudget)}` : ""} tokens · ${elapsed(goal.timeUsedSeconds)}`;

  return (
    <section className="emp-goal" data-status={waiting ? "paused" : goal.status} aria-label="Goal">
      <span className="emp-goal__icon" aria-hidden="true">
        <IconGoal />
      </span>
      <button type="button" className="emp-goal__body" aria-expanded={open} title={open ? "Show less" : goal.objective} onClick={() => setOpen(!open)}>
        <span className="emp-goal__label">
          Goal · {waiting ? "Active, but Codex isn't working on it" : STATUS[goal.status]} <span className="emp-goal__usage">{usage}</span>
        </span>
        <span className={open ? "emp-goal__objective" : "emp-goal__objective emp-truncate"}>{goal.objective}</span>
      </button>
      <span className="emp-goal__actions">
        {goal.status === "active" && !waiting ? (
          <button type="button" className="emp-button emp-button--small" data-orion-ui="button" disabled={busy} onClick={() => void run("/goal pause")}>
            Pause
          </button>
        ) : goal.status !== "complete" && goal.status !== "budget-limited" ? (
          <button type="button" className="emp-button emp-button--small emp-button--primary" data-orion-ui="button" disabled={busy} onClick={() => void run("/goal resume")}>
            Resume
          </button>
        ) : null}
        <button type="button" className="emp-button emp-button--small emp-button--ghost" data-orion-ui="button" disabled={busy} onClick={() => void run("/goal clear")}>
          Clear
        </button>
      </span>
    </section>
  );
}

/** Another window with this session open. Codex threads share it safely; others must close it. */
export function ElsewhereBar({ thread }: { thread: Thread }) {
  const holders = useWorkspace((s) => s.holders[thread.id]);
  const hasDriver = useWorkspace((s) => liveFor(s, thread.id)?.owner === "driver");
  const loadHolders = useWorkspace((s) => s.loadHolders);
  const closeElsewhere = useWorkspace((s) => s.closeElsewhere);
  const [closing, setClosing] = useState(false);
  const [sure, setSure] = useState(false);

  useEffect(() => {
    if (!hasDriver) void loadHolders(thread.id);
  }, [hasDriver, thread.id, loadHolders]);

  const h = holders?.[0];
  if (!h || hasDriver) return null;
  const where = h.app ?? "another terminal";
  const shared = thread.harness === "codex";
  const close = async () => {
    if (!sure) {
      setSure(true);
      return;
    }
    setClosing(true);
    await closeElsewhere(thread.id);
    setClosing(false);
    setSure(false);
  };

  return (
    <section className="emp-elsewhere" data-shared={shared || undefined} aria-label="Open in another window">
      <span className="emp-elsewhere__icon" aria-hidden="true">
        <IconTerminal />
      </span>
      <span className="emp-elsewhere__text">
        <span>
          {shared ? `Also open in ${where}. You can keep chatting here: EMP and that window share this session.` : `Open in ${where}. Close it there before you chat here.`}
        </span>
        <span className="emp-elsewhere__command emp-mono emp-truncate" title={h.command}>
          {h.command}
          {holders.length > 1 ? ` (+${holders.length - 1} more)` : ""}
        </span>
      </span>
      <button
        type="button"
        className={sure ? "emp-button emp-button--small emp-button--danger" : "emp-button emp-button--small"}
        data-orion-ui="button"
        disabled={closing}
        onClick={() => void close()}
        onBlur={() => setSure(false)}
      >
        {closing ? "Closing…" : sure ? `Close it in ${where}?` : shared ? "Close that window" : "Close it and continue here"}
      </button>
    </section>
  );
}
