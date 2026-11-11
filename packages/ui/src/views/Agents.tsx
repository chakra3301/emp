import { useEffect, useState } from "react";
import type { AgentProfile, AgentSaveParams, Harness } from "@emp/protocol";
import { ClampedText, EmptyState, ErrorState, HarnessBadge, LoadingState } from "../components/common";
import { IconChevron, IconPlus } from "../components/icons";
import { COMPACT_PRESETS, formatTokens } from "../lib/compaction";
import { initials, shortPath } from "../lib/format";
import { useWorkspace } from "../store/context";
import { CHAT_HARNESSES, TERMINAL_PROGRAMS, agentDraft } from "../store/workspace";

const HARNESSES: Harness[] = ["pi", "claude", "hermes", "codex", "antigravity", "gemini", "cursor", "shell"];
const EFFORTS = ["", "off", "minimal", "low", "medium", "high", "xhigh", "max"];

const INSTRUCTIONS_PREVIEW = 400;

const AUTONOMY_SHORT: Record<number, string> = { 0: "asks for everything", 1: "autonomy 1/3", 2: "autonomy 2/3", 3: "full access" };

const AUTONOMY: Record<number, string> = {
  0: "0 · asks for everything",
  1: "1 of 3",
  2: "2 of 3",
  3: "3 · full access, never asks",
};

export function Agents() {
  const agents = useWorkspace((s) => s.agents);
  const loadAgents = useWorkspace((s) => s.loadAgents);
  const draft = useWorkspace((s) => s.agentEdit.draft);
  const editAgent = useWorkspace((s) => s.editAgent);

  return (
    <section className="emp-page" data-orion-ui="panel" aria-label="Agents">
      <div className="emp-toolbar emp-toolbar--page" data-orion-ui="toolbar">
        <div className="emp-toolbar__row">
          <h2 className="emp-heading emp-heading--lg" data-orion-ui="title">
            Agents
          </h2>
          {agents.data && (
            <span className="emp-count" data-orion-ui="muted">
              {agents.data.length}
            </span>
          )}
          <span className="emp-row__spacer" />
          <button type="button" className="emp-button" data-orion-ui="button" onClick={() => editAgent(agentDraft())}>
            <IconPlus />
            New agent
          </button>
        </div>
      </div>
      <div className="emp-page__body">
        {agents.status === "idle" && <LoadingState label="Waiting for empd…" />}
        {agents.status === "loading" && !agents.data && <LoadingState label="Loading agents…" />}
        {agents.status === "error" && <ErrorState message={agents.error ?? "Could not load agents"} onRetry={() => void loadAgents()} />}
        {agents.status === "ready" && agents.data?.length === 0 && (
          <EmptyState title="No agent profiles yet" hint="Import picks up Hermes profiles and Orion Command Center agents." />
        )}
        {draft && !draft.id && <AgentEditor key="new" initial={draft} />}
        <div className="emp-cards">
          {agents.data?.map((a) => (draft?.id === a.id ? <AgentEditor key={a.id} initial={draft} imported={!!a.importedFrom} /> : <AgentCard key={a.id} agent={a} />))}
        </div>
      </div>
    </section>
  );
}

const SOURCE: Record<string, string> = { pi: "pi", hermes: "Hermes", claude: "Claude Code", codex: "Codex", orion: "Orion" };

function AgentCard({ agent: a }: { agent: AgentProfile }) {
  const editAgent = useWorkspace((s) => s.editAgent);
  const openNew = useWorkspace((s) => s.setNewThreadOpen);
  const setAgent = useWorkspace((s) => s.setNewThreadAgent);
  const runInTerminal = useWorkspace((s) => s.runInTerminal);
  const [showInstructions, setShowInstructions] = useState(false);
  return (
    <article className="emp-card emp-agent">
      <div className="emp-agent__head">
        <span className="emp-monogram" data-harness={a.harness} aria-hidden="true">
          {initials(a.name)}
        </span>
        <div className="emp-agent__titles">
          <h3 className="emp-subheading emp-truncate" data-orion-ui="title">
            {a.name}
          </h3>
          <div className="emp-meta">
            <HarnessBadge harness={a.harness} label={a.harnessProfile && a.harnessProfile !== "default" ? `${a.harness} · ${a.harnessProfile}` : a.harness} />
            <span className="emp-truncate emp-mono">{a.model ?? "harness default"}</span>
          </div>
        </div>
      </div>
      {a.role && <p className="emp-agent__role">{a.role}</p>}
      <div className="emp-tags">
        {a.effort && <span className="emp-tag">effort {a.effort}</span>}
        <span className="emp-tag">{AUTONOMY_SHORT[a.autonomy] ?? `autonomy ${a.autonomy}`}</span>
        {a.compactAt && <span className="emp-tag">compacts at {formatTokens(a.compactAt)}</span>}
        <span className="emp-tag">{a.importedFrom ? `from ${SOURCE[a.importedFrom.source] ?? a.importedFrom.source}` : "made in EMP"}</span>
      </div>
      {a.cwd && (
        <div className="emp-agent__dir emp-mono emp-truncate" title={a.cwd}>
          {shortPath(a.cwd, 52)}
        </div>
      )}
      {a.instructions && (
        <div className="emp-agent__instructions">
          <button type="button" className="emp-disclosure" aria-expanded={showInstructions} onClick={() => setShowInstructions((v) => !v)}>
            <IconChevron open={showInstructions} />
            Instructions
          </button>
          {showInstructions && <ClampedText className="emp-msg__text emp-agent__instructions-text" text={a.instructions} limit={INSTRUCTIONS_PREVIEW * 4} />}
        </div>
      )}
      <div className="emp-agent__actions">
        {CHAT_HARNESSES.includes(a.harness) ? (
          <button
            type="button"
            className="emp-button"
            data-orion-ui="button"
            onClick={() => {
              setAgent(a.id);
              openNew(true);
            }}
          >
            New thread
          </button>
        ) : (
          <button
            type="button"
            className="emp-button"
            data-orion-ui="button"
            onClick={() =>
              void (a.harness === "shell"
                ? runInTerminal("zsh", ["-l"], a.cwd, a.name)
                : runInTerminal(TERMINAL_PROGRAMS[a.harness] ?? a.harness, [], a.cwd, a.name))
            }
          >
            Open in terminal
          </button>
        )}
        <button type="button" className="emp-button emp-button--ghost" data-orion-ui="button" onClick={() => editAgent(agentDraft(a))}>
          Edit
        </button>
      </div>
    </article>
  );
}

const nullable = (v: string): string | null => (v.trim() ? v.trim() : null);

function AgentEditor({ initial, imported = false }: { initial: AgentSaveParams; imported?: boolean }) {
  const state = useWorkspace((s) => s.agentEdit);
  const save = useWorkspace((s) => s.saveAgent);
  const remove = useWorkspace((s) => s.deleteAgent);
  const cancel = useWorkspace((s) => s.editAgent);
  const [d, setD] = useState<AgentSaveParams>(initial);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const set = <K extends keyof AgentSaveParams>(k: K, v: AgentSaveParams[K]) => setD((prev) => ({ ...prev, [k]: v }));
  const globalCompact = useWorkspace((s) => s.daemonSettings.data?.compactAt ?? null);
  const loadDaemonSettings = useWorkspace((s) => s.loadDaemonSettings);
  const settingsStatus = useWorkspace((s) => s.daemonSettings.status);
  useEffect(() => {
    if (settingsStatus === "idle") void loadDaemonSettings();
  }, [settingsStatus, loadDaemonSettings]);
  const text = (k: "model" | "role" | "cwd" | "harnessProfile" | "instructions") => d[k] ?? "";

  return (
    <article className="emp-card emp-card--editing" aria-label={d.id ? `Edit ${initial.name}` : "New agent"}>
      <div className="emp-form">
        <label className="emp-form__label" data-orion-ui="muted">
          Name
          <input className="emp-input" data-orion-ui="input" value={d.name} onChange={(e) => set("name", e.target.value)} />
        </label>
        <div className="emp-form__grid">
          <label className="emp-form__label" data-orion-ui="muted">
            Harness
            <select className="emp-input" data-orion-ui="input" value={d.harness} onChange={(e) => set("harness", e.target.value as Harness)}>
              {HARNESSES.map((h) => (
                <option key={h} value={h}>
                  {h}
                  {CHAT_HARNESSES.includes(h) ? "" : " (terminal only for now)"}
                </option>
              ))}
            </select>
          </label>
          <label className="emp-form__label" data-orion-ui="muted">
            Model
            <input
              className="emp-input emp-mono"
              data-orion-ui="input"
              placeholder="harness default, e.g. openai/gpt-6.1-sol"
              value={text("model")}
              onChange={(e) => set("model", nullable(e.target.value))}
            />
          </label>
          <label className="emp-form__label" data-orion-ui="muted">
            Effort
            <select className="emp-input" data-orion-ui="input" value={d.effort ?? ""} onChange={(e) => set("effort", nullable(e.target.value))}>
              {EFFORTS.map((x) => (
                <option key={x} value={x}>
                  {x || "default"}
                </option>
              ))}
            </select>
          </label>
          <label className="emp-form__label" data-orion-ui="muted">
            Autonomy
            <select className="emp-input" data-orion-ui="input" value={d.autonomy} onChange={(e) => set("autonomy", Number(e.target.value))}>
              {[0, 1, 2, 3].map((n) => (
                <option key={n} value={n}>
                  {AUTONOMY[n]}
                </option>
              ))}
            </select>
          </label>
          <label className="emp-form__label" data-orion-ui="muted">
            Compact at
            <select
              className="emp-input"
              data-orion-ui="input"
              value={d.compactAt ?? ""}
              onChange={(e) => set("compactAt", e.target.value ? Number(e.target.value) : null)}
            >
              <option value="">Global setting{globalCompact ? ` (${formatTokens(globalCompact)})` : " (tool default)"}</option>
              {[...new Set([...COMPACT_PRESETS, 500_000, ...(d.compactAt ? [d.compactAt] : [])])]
                .sort((a, b) => a - b)
                .map((n) => (
                  <option key={n} value={n}>
                    {formatTokens(n)} tokens
                  </option>
                ))}
            </select>
          </label>
          <label className="emp-form__label" data-orion-ui="muted">
            Directory
            <input className="emp-input emp-mono" data-orion-ui="input" placeholder="~" value={text("cwd")} onChange={(e) => set("cwd", nullable(e.target.value))} />
          </label>
          {d.harness === "hermes" && (
            <label className="emp-form__label" data-orion-ui="muted">
              Hermes profile
              <input className="emp-input" data-orion-ui="input" placeholder="default" value={text("harnessProfile")} onChange={(e) => set("harnessProfile", nullable(e.target.value))} />
            </label>
          )}
        </div>
        <label className="emp-form__label" data-orion-ui="muted">
          Role
          <input className="emp-input" data-orion-ui="input" placeholder="What this agent is for" value={text("role")} onChange={(e) => set("role", nullable(e.target.value))} />
        </label>
        <label className="emp-form__label" data-orion-ui="muted">
          Instructions
          <textarea
            className="emp-input emp-composer__input"
            data-orion-ui="input"
            rows={6}
            placeholder="Added to the harness's system prompt for new threads"
            value={text("instructions")}
            onChange={(e) => set("instructions", e.target.value || null)}
          />
        </label>
        {imported && (
          <div className="emp-note" data-orion-ui="muted">
            Saving detaches this agent from re-import: later imports won't overwrite your edits.
          </div>
        )}
        <div className="emp-note" data-orion-ui="muted">
          Model, effort and instructions apply to new threads. A running thread keeps the model it started with.
        </div>
        {state.error && <ErrorState message={state.error} />}
        <div className="emp-toolbar__row">
          {d.id &&
            (confirmDelete ? (
              <button type="button" className="emp-button" data-orion-ui="button" onClick={() => void remove(d.id!)}>
                {imported ? "Hide it" : "Really delete"}
              </button>
            ) : (
              <button type="button" className="emp-button emp-button--ghost" data-orion-ui="button" onClick={() => setConfirmDelete(true)}>
                Remove
              </button>
            ))}
          {confirmDelete && (
            <span className="emp-note">
              {imported ? "It stays hidden, even after re-import. Its threads are kept." : "Its threads are kept."}
            </span>
          )}
          <span className="emp-row__spacer" />
          <button type="button" className="emp-button" data-orion-ui="button" onClick={() => cancel(null)}>
            Cancel
          </button>
          <button
            type="button"
            className="emp-button emp-button--primary"
            data-orion-ui="button"
            disabled={!d.name.trim() || state.saving}
            onClick={() => void save({ ...d, name: d.name.trim() })}
          >
            {state.saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </article>
  );
}
