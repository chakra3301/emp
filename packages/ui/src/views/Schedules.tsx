// Scheduled tasks (EMP's cron jobs): a saved prompt an agent runs on a timetable, in a new
// thread each run or in one thread that keeps its history. Suki can make them too.

import { useEffect, useMemo, useState } from "react";
import type { ExternalSchedule, Schedule, SchedulePreview, ScheduleSaveParams } from "@emp/protocol";
import { EmptyState, ErrorState, LoadingState } from "../components/common";
import { ConfirmButton, Segmented } from "../components/controls";
import { IconClock, IconPlus } from "../components/icons";
import { cleanTitle, shortPath } from "../lib/format";
import {
  DAY_LETTERS,
  DAY_NAMES,
  ENDS_KINDS,
  WHEN_KINDS,
  endsText,
  endsToForm,
  formToEnds,
  formToWhen,
  localInput,
  newScheduleDraft,
  relative,
  runTime,
  scheduleDraft,
  scheduleState,
  whenToForm,
  type EndsForm,
  type EndsKind,
  type EveryUnit,
  type WhenForm,
  type WhenKind,
} from "../lib/schedule";
import { formatDateTime } from "../lib/time";
import { useWorkspace } from "../store/context";
import { CHAT_HARNESSES } from "../store/workspace";

export function Schedules() {
  const schedules = useWorkspace((s) => s.schedules);
  const load = useWorkspace((s) => s.loadSchedules);
  const editor = useWorkspace((s) => s.scheduleEditor);
  const edit = useWorkspace((s) => s.editSchedule);
  const agents = useWorkspace((s) => s.agents);
  const loadAgents = useWorkspace((s) => s.loadAgents);
  useEffect(() => {
    if (agents.status === "idle") void loadAgents();
  }, [agents.status, loadAgents]);

  const all = schedules.data ?? [];
  const active = all.filter((s) => s.enabled);
  const rest = all.filter((s) => !s.enabled);
  const firstAgent = agents.data?.find((a) => !a.coordinator && CHAT_HARNESSES.includes(a.harness))?.id ?? null;
  const creating = editor && !editor.draft.id;

  return (
    <section className="emp-page" data-orion-ui="panel" aria-label="Scheduled">
      <div className="emp-toolbar emp-toolbar--page" data-orion-ui="toolbar">
        <div className="emp-toolbar__row">
          <h2 className="emp-heading emp-heading--lg" data-orion-ui="title">
            Scheduled
          </h2>
          {schedules.data && (
            <span className="emp-count" data-orion-ui="muted">
              {active.length}
            </span>
          )}
          <span className="emp-row__spacer" />
          <button type="button" className="emp-button emp-button--primary" data-orion-ui="button" disabled={!!creating} onClick={() => edit(newScheduleDraft(firstAgent))}>
            <IconPlus />
            New schedule
          </button>
        </div>
      </div>
      <div className="emp-page__body emp-sched">
        {schedules.error && schedules.data && <ErrorState message={schedules.error} />}
        {creating && <ScheduleEditor key="new" initial={editor.draft} />}
        {schedules.status === "idle" && <LoadingState label="Waiting for empd…" />}
        {schedules.status === "loading" && !schedules.data && <LoadingState />}
        {schedules.status === "error" && !schedules.data && <ErrorState message={schedules.error ?? "Could not load schedules"} onRetry={() => void load()} />}
        {schedules.data && all.length === 0 && !creating && (
          <EmptyState
            title="Nothing scheduled"
            hint={
              <>
                Have an agent run a prompt on a timetable: every weekday at 9, every hour, or once tomorrow. Or ask Suki, e.g. “every morning at 9, have Codex check
                the build and tell me what broke”.
              </>
            }
          />
        )}
        {active.length > 0 && <ScheduleGroup title="Active" items={active} />}
        {rest.length > 0 && <ScheduleGroup title="Paused and finished" items={rest} />}
        <ExternalGroup />
      </div>
    </section>
  );
}

/** Hermes's own cron jobs: shown so everything scheduled is in one place, changed in Hermes. */
function ExternalGroup() {
  const external = useWorkspace((s) => s.externalSchedules.data);
  if (!external?.length) return null;
  return (
    <div className="emp-sched__group" role="group" aria-label="In Hermes">
      <div className="emp-sched__head">
        <span className="emp-sched__title">In Hermes</span>
        <span className="emp-count" data-orion-ui="muted">
          {external.length}
        </span>
      </div>
      <div className="emp-note" data-orion-ui="muted">
        Hermes runs these itself. EMP only shows them; change them with <code className="emp-mono">hermes cron</code> or by asking Hermes.
      </div>
      <div className="emp-sched__list" role="list">
        {external.map((j) => (
          <ExternalRow key={`${j.profile}:${j.id}`} job={j} />
        ))}
      </div>
    </div>
  );
}

function ExternalRow({ job: j }: { job: ExternalSchedule }) {
  const now = useWorkspace((st) => st.now);
  const failed = !!j.lastStatus && !["ok", "success", "completed"].includes(j.lastStatus);
  const state = failed ? "failed" : j.enabled ? "on" : "off";
  const next = j.enabled && j.nextRunAt ? `Next ${relative(j.nextRunAt, now)}` : j.state === "completed" ? "Finished" : "Paused";
  return (
    <div className="emp-sched__row" role="listitem" data-state={state}>
      <span className="emp-sched__state" role="img" aria-label={state === "on" ? "Active" : state === "off" ? "Paused" : "Failed"}>
        <span className="emp-dot" aria-hidden="true" />
      </span>
      <span className="emp-sched__text">
        <span className="emp-sched__name emp-truncate" title={j.task}>
          {j.name}
        </span>
        <span className="emp-sched__detail emp-truncate">
          {[j.summary, j.task, j.profile === "default" ? null : `profile ${j.profile}`, j.deliver ? `to ${j.deliver}` : null].filter(Boolean).join(" · ")}
        </span>
      </span>
      <span className="emp-sched__when" title={j.nextRunAt ? formatDateTime(j.nextRunAt) : undefined}>
        {next}
        {j.lastRunAt && (
          <span className="emp-sched__last">
            Last {relative(j.lastRunAt, now)}
            {j.lastStatus && ` · ${j.lastStatus}`}
          </span>
        )}
      </span>
      {failed && j.lastError && (
        <span className="emp-sched__error" role="alert">
          {j.lastError}
        </span>
      )}
    </div>
  );
}

function ScheduleGroup({ title, items }: { title: string; items: Schedule[] }) {
  const editor = useWorkspace((s) => s.scheduleEditor);
  return (
    <div className="emp-sched__group" role="group" aria-label={title}>
      <div className="emp-sched__head">
        <span className="emp-sched__title">{title}</span>
        <span className="emp-count" data-orion-ui="muted">
          {items.length}
        </span>
      </div>
      <div className="emp-sched__list" role="list">
        {items.map((s) => (editor?.draft.id === s.id ? <ScheduleEditor key={s.id} initial={editor.draft} /> : <ScheduleRow key={s.id} schedule={s} />))}
      </div>
    </div>
  );
}

function ScheduleRow({ schedule: s }: { schedule: Schedule }) {
  const now = useWorkspace((st) => st.now);
  const agents = useWorkspace((st) => st.agents.data);
  const edit = useWorkspace((st) => st.editSchedule);
  const enable = useWorkspace((st) => st.enableSchedule);
  const run = useWorkspace((st) => st.runSchedule);
  const remove = useWorkspace((st) => st.deleteSchedule);
  const openThread = useWorkspace((st) => st.openThread);
  const running = s.lastStatus === "running";
  const agent = agents?.find((a) => a.id === s.agentId);
  const where = s.threadId
    ? `in “${cleanTitle(s.threadTitle ?? "its thread")}”`
    : [agent?.name ?? "a removed agent", s.cwd ? shortPath(s.cwd, 40) : null].filter(Boolean).join(" · ");
  const state = running ? "running" : s.lastStatus === "failed" && !s.enabled ? "failed" : s.enabled ? "on" : "off";
  const ends = endsText(s);

  return (
    <div className="emp-sched__row" role="listitem" data-state={state}>
      <span className="emp-sched__state" role="img" aria-label={state === "on" ? "Active" : state === "off" ? "Paused" : state === "running" ? "Running" : "Failed"}>
        <span className="emp-dot" aria-hidden="true" />
      </span>
      <span className="emp-sched__text">
        <span className="emp-sched__name emp-truncate" title={s.prompt}>
          {s.title}
        </span>
        <span className="emp-sched__detail emp-truncate">
          {[s.summary, ends, where].filter(Boolean).join(" · ")}
        </span>
      </span>
      <span className="emp-sched__when" title={s.nextRunAt ? formatDateTime(s.nextRunAt) : undefined}>
        {scheduleState(s, now)}
        {s.lastRunAt && (
          <span className="emp-sched__last">
            Last {relative(s.lastRunAt, now)}
            {s.lastThreadId && (
              <>
                {" · "}
                <button type="button" className="emp-link" onClick={() => void openThread(s.lastThreadId!)}>
                  Open
                </button>
              </>
            )}
          </span>
        )}
      </span>
      <span className="emp-sched__actions">
        <button type="button" className="emp-button emp-button--small emp-button--ghost" data-orion-ui="button" disabled={running} onClick={() => void run(s.id)}>
          Run now
        </button>
        <button type="button" className="emp-button emp-button--small emp-button--ghost" data-orion-ui="button" onClick={() => void enable(s.id, !s.enabled)}>
          {s.enabled ? "Pause" : s.ended ? "Start again" : "Resume"}
        </button>
        <button type="button" className="emp-button emp-button--small emp-button--ghost" data-orion-ui="button" onClick={() => edit(scheduleDraft(s))}>
          Edit
        </button>
        <ConfirmButton className="emp-button emp-button--small emp-button--ghost" label="Delete" confirm="Delete it?" onConfirm={() => void remove(s.id)} />
      </span>
      {s.lastStatus === "failed" && s.lastError && (
        <span className="emp-sched__error" role="alert">
          {s.enabled ? `Last run failed${s.failStreak > 1 ? ` (${s.failStreak} in a row; 3 pause it)` : ""}: ` : ""}
          {s.lastError}
        </span>
      )}
    </div>
  );
}

type Mode = "new" | "thread";

function ScheduleEditor({ initial }: { initial: ScheduleSaveParams }) {
  const editor = useWorkspace((s) => s.scheduleEditor);
  const save = useWorkspace((s) => s.saveSchedule);
  const cancel = useWorkspace((s) => s.editSchedule);
  const previewSchedule = useWorkspace((s) => s.previewSchedule);
  const agents = useWorkspace((s) => s.agents.data) ?? [];
  const projects = useWorkspace((s) => s.projects.data) ?? [];
  const threads = useWorkspace((s) => s.threads.items);
  const ranSoFar = useWorkspace((s) => s.schedules.data?.find((x) => x.id === initial.id)?.runCount ?? 0);
  const [d, setD] = useState<ScheduleSaveParams>(initial);
  const [form, setForm] = useState<WhenForm>(() => whenToForm(initial.when));
  const [mode, setMode] = useState<Mode>(initial.threadId ? "thread" : "new");
  const [preview, setPreview] = useState<SchedulePreview | null>(null);
  const [endsForm, setEnds] = useState<EndsForm>(() => endsToForm(initial));
  const when = formToWhen(form);
  const once = form.kind === "once";
  const ends = once ? {} : formToEnds(endsForm);
  const whenKey = JSON.stringify(when);

  useEffect(() => {
    if (!when) {
      setPreview(null);
      return;
    }
    let alive = true;
    const t = setTimeout(() => void previewSchedule(when).then((p) => alive && setPreview(p)), 250);
    return () => {
      alive = false;
      clearTimeout(t);
    };
    // `whenKey` stands for `when`, which is a new object every render.
  }, [whenKey, previewSchedule]);

  const chatAgents = agents.filter((a) => CHAT_HARNESSES.includes(a.harness));
  const threadOptions = useMemo(() => {
    const list = threads.filter((t) => CHAT_HARNESSES.includes(t.harness)).slice(0, 60);
    if (initial.threadId && !list.some((t) => t.id === initial.threadId)) list.unshift({ id: initial.threadId, title: "This thread" } as (typeof list)[number]);
    return list;
  }, [threads, initial.threadId]);
  const patch = (p: Partial<ScheduleSaveParams>) => setD((prev) => ({ ...prev, ...p }));
  const setWhen = (p: Partial<WhenForm>) => setForm((prev) => ({ ...prev, ...p }));
  const target = mode === "thread" ? !!d.threadId : !!d.agentId;
  const canSave = !!d.prompt.trim() && !!when && !!ends && target && !preview?.error && !editor?.saving;

  const submit = () => {
    if (!when || !ends) return;
    const out: ScheduleSaveParams = { ...d, prompt: d.prompt.trim(), title: d.title.trim(), when, ...ends };
    if (!("endsAt" in ends)) delete out.endsAt;
    if (!("maxRuns" in ends)) delete out.maxRuns;
    const stop = once ? "" : (d.stopWhen ?? "").trim();
    if (stop) out.stopWhen = stop;
    else delete out.stopWhen;
    if (mode === "thread") {
      delete out.agentId;
      delete out.cwd;
      delete out.projectId;
    } else {
      delete out.threadId;
    }
    void save(out);
  };

  return (
    <article className="emp-card emp-card--editing emp-sched__editor" aria-label={d.id ? `Edit ${initial.title}` : "New schedule"}>
      <div className="emp-form">
        <label className="emp-form__label" data-orion-ui="muted">
          What should the agent do each run?
          <textarea
            className="emp-input emp-composer__input"
            data-orion-ui="input"
            rows={4}
            autoFocus
            placeholder="Check the overnight build, fix anything simple, and tell me what's still broken. If everything passed, just say so."
            value={d.prompt}
            onChange={(e) => patch({ prompt: e.target.value })}
          />
        </label>
        <label className="emp-form__label" data-orion-ui="muted">
          Name
          <input className="emp-input" data-orion-ui="input" placeholder="Defaults to the first line" value={d.title} onChange={(e) => patch({ title: e.target.value })} />
        </label>

        <div className="emp-form__label" data-orion-ui="muted">
          Runs
          <Segmented<Mode>
            label="Runs"
            value={mode}
            options={[
              ["new", "A new thread each time"],
              ["thread", "Keep going in one thread"],
            ]}
            onChange={setMode}
          />
        </div>
        {mode === "new" ? (
          <div className="emp-form__grid">
            <label className="emp-form__label" data-orion-ui="muted">
              Agent
              <select className="emp-input" data-orion-ui="input" value={d.agentId ?? ""} onChange={(e) => patch({ agentId: e.target.value || undefined })}>
                <option value="">Pick an agent</option>
                {chatAgents.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name} ({a.harness})
                  </option>
                ))}
              </select>
            </label>
            <label className="emp-form__label" data-orion-ui="muted">
              Project
              <select className="emp-input" data-orion-ui="input" value={d.projectId ?? ""} onChange={(e) => patch({ projectId: e.target.value || undefined })}>
                <option value="">None (the agent's folder)</option>
                {projects.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="emp-form__label" data-orion-ui="muted">
              Folder
              <input
                className="emp-input emp-mono"
                data-orion-ui="input"
                placeholder={shortPath(projects.find((p) => p.id === d.projectId)?.rootPath ?? agents.find((a) => a.id === d.agentId)?.cwd ?? "~", 40)}
                value={d.cwd ?? ""}
                onChange={(e) => patch({ cwd: e.target.value || undefined })}
              />
            </label>
          </div>
        ) : (
          <label className="emp-form__label" data-orion-ui="muted">
            Thread
            <select className="emp-input" data-orion-ui="input" value={d.threadId ?? ""} onChange={(e) => patch({ threadId: e.target.value || undefined })}>
              <option value="">Pick a thread</option>
              {threadOptions.map((t) => (
                <option key={t.id} value={t.id}>
                  {cleanTitle(t.title)}
                </option>
              ))}
            </select>
          </label>
        )}

        <div className="emp-form__label" data-orion-ui="muted">
          When
          <Segmented<WhenKind> label="When" value={form.kind} options={WHEN_KINDS} onChange={(kind) => setWhen({ kind })} />
        </div>
        <WhenFields form={form} set={setWhen} />
        <div className="emp-sched__preview" data-error={preview?.error ? true : undefined} role="status">
          <IconClock />
          {!when ? (
            <span>Finish the timetable above.</span>
          ) : preview?.error ? (
            <span>{preview.error}</span>
          ) : preview ? (
            <span>
              {preview.summary}
              {preview.next.length > 0 && <> · next {preview.next.map(runTime).join(" · ")}</>}
            </span>
          ) : (
            <span>…</span>
          )}
        </div>
        {!once && (
          <>
            <div className="emp-form__label" data-orion-ui="muted">
              Ends
              <Segmented<EndsKind> label="Ends" value={endsForm.kind} options={ENDS_KINDS} onChange={(kind) => setEnds((e) => ({ ...e, kind }))} />
            </div>
            {endsForm.kind === "date" && (
              <div className="emp-form__grid">
                <label className="emp-form__label" data-orion-ui="muted">
                  No runs after
                  <input
                    className="emp-input"
                    data-orion-ui="input"
                    type="datetime-local"
                    min={localInput(Date.now())}
                    value={endsForm.date}
                    onChange={(e) => setEnds((f) => ({ ...f, date: e.target.value }))}
                  />
                </label>
              </div>
            )}
            {endsForm.kind === "runs" && (
              <div className="emp-form__grid">
                <label className="emp-form__label" data-orion-ui="muted">
                  Number of runs
                  <input
                    className="emp-input"
                    data-orion-ui="input"
                    type="number"
                    min={1}
                    value={endsForm.runs}
                    onChange={(e) => setEnds((f) => ({ ...f, runs: Number(e.target.value) }))}
                  />
                </label>
                {ranSoFar > 0 && (
                  <span className="emp-note emp-sched__hint" data-orion-ui="muted">
                    Counts the {ranSoFar} run{ranSoFar === 1 ? "" : "s"} so far
                  </span>
                )}
              </div>
            )}
            <label className="emp-form__label" data-orion-ui="muted">
              Stop when (optional)
              <input
                className="emp-input"
                data-orion-ui="input"
                placeholder="e.g. the release build passes. The agent checks each run and the schedule finishes when it's true"
                value={d.stopWhen ?? ""}
                onChange={(e) => patch({ stopWhen: e.target.value })}
              />
            </label>
          </>
        )}
        {mode === "thread" && (
          <div className="emp-note" data-orion-ui="muted">
            Each run is a new message in that thread, so the agent remembers earlier runs. A run that comes due while the thread is busy is skipped.
          </div>
        )}
        {editor?.error && <ErrorState message={editor.error} />}
        <div className="emp-toolbar__row">
          <span className="emp-row__spacer" />
          <button type="button" className="emp-button" data-orion-ui="button" onClick={() => cancel(null)}>
            Cancel
          </button>
          <button type="button" className="emp-button emp-button--primary" data-orion-ui="button" disabled={!canSave} onClick={submit}>
            {editor?.saving ? "Saving…" : d.id ? "Save" : "Schedule it"}
          </button>
        </div>
      </div>
    </article>
  );
}

function WhenFields({ form, set }: { form: WhenForm; set(p: Partial<WhenForm>): void }) {
  const time = (
    <label className="emp-form__label" data-orion-ui="muted">
      At
      <input className="emp-input" data-orion-ui="input" type="time" value={form.time} onChange={(e) => set({ time: e.target.value })} />
    </label>
  );
  switch (form.kind) {
    case "once":
      return (
        <label className="emp-form__label" data-orion-ui="muted">
          On
          <input className="emp-input" data-orion-ui="input" type="datetime-local" value={form.at} onChange={(e) => set({ at: e.target.value })} />
        </label>
      );
    case "every":
      return (
        <div className="emp-form__grid">
          <label className="emp-form__label" data-orion-ui="muted">
            Every
            <input className="emp-input" data-orion-ui="input" type="number" min={1} value={form.every} onChange={(e) => set({ every: Number(e.target.value) })} />
          </label>
          <label className="emp-form__label" data-orion-ui="muted">
            Unit
            <select className="emp-input" data-orion-ui="input" value={form.unit} onChange={(e) => set({ unit: e.target.value as EveryUnit })}>
              <option value="minutes">minutes</option>
              <option value="hours">hours</option>
              <option value="days">days</option>
            </select>
          </label>
        </div>
      );
    case "daily":
    case "weekdays":
      return <div className="emp-form__grid">{time}</div>;
    case "weekly":
      return (
        <>
          <div className="emp-form__label" data-orion-ui="muted">
            On
            <div className="emp-sched__days" role="group" aria-label="Days">
              {DAY_LETTERS.map((letter, i) => (
                <button
                  key={DAY_NAMES[i]}
                  type="button"
                  className="emp-sched__day"
                  aria-pressed={form.days.includes(i)}
                  aria-label={DAY_NAMES[i]}
                  title={DAY_NAMES[i]}
                  onClick={() => set({ days: form.days.includes(i) ? form.days.filter((x) => x !== i) : [...form.days, i] })}
                >
                  {letter}
                </button>
              ))}
            </div>
          </div>
          <div className="emp-form__grid">{time}</div>
        </>
      );
    case "monthly":
      return (
        <div className="emp-form__grid">
          <label className="emp-form__label" data-orion-ui="muted">
            Day of the month
            <input className="emp-input" data-orion-ui="input" type="number" min={1} max={31} value={form.dayOfMonth} onChange={(e) => set({ dayOfMonth: Number(e.target.value) })} />
          </label>
          {time}
        </div>
      );
    case "cron":
      return (
        <label className="emp-form__label" data-orion-ui="muted">
          Cron expression (minute hour day month weekday)
          <input className="emp-input emp-mono" data-orion-ui="input" placeholder="0 9 * * 1-5" value={form.cron} onChange={(e) => set({ cron: e.target.value })} />
        </label>
      );
  }
}
