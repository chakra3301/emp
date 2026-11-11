import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { CommandCard } from "../components/ChatCards";
import { ErrorState, HarnessBadge } from "../components/common";
import {
  AttachButton,
  DraftFiles,
  DropHint,
  commandRun,
  slashName,
  useDraft,
  useFileDrop,
  usePasteFiles,
  useSlashMenu,
} from "../components/ComposerParts";
import { IconClock, IconPlus, IconSend } from "../components/icons";
import { Picker } from "../components/Picker";
import { folderLines } from "../lib/attach";
import { newScheduleDraft } from "../lib/schedule";
import { shortPath } from "../lib/format";
import { isCopilotModel } from "../lib/models";
import { useWorkspace } from "../store/context";
import { CHAT_HARNESSES, NEW_DRAFT, newThreadProject } from "../store/workspace";

/** The New thread tab, Hermes-style: a wordmark and one composer that picks the agent and folder. */
export function NewThread() {
  const agents = useWorkspace((s) => s.agents);
  const pickedProject = useWorkspace(newThreadProject);
  const state = useWorkspace((s) => s.newThread);
  const start = useWorkspace((s) => s.startThread);
  const agentId = useWorkspace((s) => s.newThreadAgentId);
  const setAgentId = useWorkspace((s) => s.setNewThreadAgent);
  const loadAgents = useWorkspace((s) => s.loadAgents);
  const model = useWorkspace((s) => s.newThreadModel);
  const setModel = useWorkspace((s) => s.setNewThreadModel);
  const loadModels = useWorkspace((s) => s.loadModels);
  const models = useWorkspace((s) => s.models);
  const projects = useWorkspace((s) => s.projects.data);
  const setProjectId = useWorkspace((s) => s.setNewThreadProject);
  const showSection = useWorkspace((s) => s.showSection);
  // Suki has its own panel (⌘/).
  const usable = useMemo(() => (agents.data ?? []).filter((a) => CHAT_HARNESSES.includes(a.harness) && !a.coordinator), [agents.data]);
  const [cwd, setCwd] = useState<string | null>(null);
  const [editingDir, setEditingDir] = useState(false);
  const [text, setText] = useState("");
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const runCommand = useWorkspace((s) => s.runCommand);
  const setNewThreadError = useWorkspace((s) => s.setNewThreadError);
  const signingIn = useWorkspace((s) => !!s.commandCards.new);
  const clearDraft = useWorkspace((s) => s.clearDraft);
  const draft = useDraft(DRAFT);
  const drop = useFileDrop(DRAFT, "workdir");
  const onPaste = usePasteFiles(DRAFT);

  useEffect(() => {
    if (agents.status === "idle") void loadAgents();
  }, [agents.status, loadAgents]);

  // Default to the agent picked in Settings, else the first chat-capable agent.
  const defaultAgentId = useWorkspace((s) => s.settings.defaultAgentId);
  useEffect(() => {
    if (usable.length === 0 || usable.some((a) => a.id === agentId)) return;
    setAgentId((usable.find((a) => a.id === defaultAgentId) ?? usable[0])!.id);
  }, [usable, agentId, setAgentId, defaultAgentId]);

  useEffect(() => inputRef.current?.focus(), [agentId]);

  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [text]);

  const agent = usable.find((a) => a.id === agentId) ?? null;
  const modelsKey = agent ? `${agent.harness}:${agent.harness === "hermes" ? (agent.harnessProfile ?? "") : ""}` : "";
  const modelState = models[modelsKey];
  const catalogue = modelState?.data ?? [];

  useEffect(() => {
    if (agent) void loadModels(agent.harness, agent.harness === "hermes" ? agent.harnessProfile : null);
  }, [agent, loadModels]);

  // pi lists only providers it's signed in to, so a default model from a missing provider
  // means the agent can't start until the owner logs in.
  const provider = agent?.harness === "pi" ? agent.model?.split("/")[0] : undefined;
  const signInNeeded =
    !!provider && models[modelsKey]?.status === "ready" && catalogue.length > 0 && !catalogue.some((m) => m.group === provider);

  // Picked project (composer) > sidebar project filter > the agent's own folder.
  // A dropped working folder beats them all (it's the project if one lives there).
  const dir = draft.workdir ?? cwd ?? pickedProject?.rootPath ?? agent?.cwd ?? null;
  const droppedProject = draft.workdir ? (projects?.find((p) => p.rootPath === draft.workdir) ?? null) : null;
  const projectFor = draft.workdir ? (droppedProject?.id ?? null) : cwd ? null : (pickedProject?.id ?? null);
  const editSchedule = useWorkspace((s) => s.editSchedule);
  const setDraftFolder = useWorkspace((s) => s.setDraftFolder);
  /** Picking a folder another way makes a dropped working folder context. */
  const dropWorkdir = () => {
    const chip = draft.files.find((f) => f.folder?.use === "workdir");
    if (chip) setDraftFolder(DRAFT, chip.key, { use: "context" });
  };
  const defaultModel = agent?.model ?? (agent?.harness === "hermes" ? "profile model" : "harness default");
  const defaultAllowed = !isCopilotModel(defaultModel);

  const submitText = async (raw: string) => {
    const msg = raw.trim();
    const attached = draft.ids.length + draft.folders.length;
    if (!agent || (!msg && attached === 0) || draft.uploading || state.sending) return;
    const run = slashName(msg) && attached === 0 ? commandRun(msg, await slash.ensure()) : "chat";
    // Commands that need the harness's own UI (`/login`, `/settings`, …) open in a card below.
    if (run === "terminal") {
      if (await runCommand(msg, { agentId: agent.id })) setText("");
      return;
    }
    // Before a thread exists only `/model <id>` means something (it picks the model), and
    // Codex's `/goal <objective>`, which starts the thread on that goal.
    const goalStart = run === "native" && agent.harness === "codex" && /^\/goal\s+\S/.test(msg) && !/^\/goal\s+(pause|resume|clear)\s*$/.test(msg);
    if (run === "native" && !goalStart) {
      const [name, ...rest] = msg.slice(1).split(/\s+/);
      if (name === "model" && rest.length > 0) {
        setModel(rest.join(" "));
        setText("");
      } else if (name === "new") setText("");
      else setNewThreadError(`/${name} works inside a thread; start one first.`);
      return;
    }
    const full = draft.folders.length ? [msg, folderLines(draft.folders)].filter(Boolean).join("\n\n") : msg;
    const ok = await start(agent.id, full, dir, {
      model,
      projectId: projectFor,
      attachments: draft.ids,
    });
    if (ok) clearDraft(DRAFT);
  };
  const slash = useSlashMenu(agent?.harness ?? null, agent?.id ?? null, text, setText, (t) => void submitText(t));
  const submit = () => void submitText(text);
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (slash.onKeyDown(e)) return;
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    }
  };

  return (
    <section className="emp-detail emp-new" aria-label="New thread" {...drop.props}>
      <DropHint dropping={drop.dropping} />
      <div className="emp-new__hero">
        <h1 className="emp-wordmark" aria-label="EMP">
          EMP
        </h1>
        <p className="emp-new__tagline">
          {agent?.role ?? "Start a thread with any of your agents. Pick one below, say what you need, and it starts working."}
        </p>
      </div>

      <div className="emp-composer">
        {state.error && <ErrorState message={state.error} />}
        <div className="emp-composer__card">
          {slash.menu}
          <DraftFiles draft={DRAFT} />
          <div className="emp-composer__row">
            <button
              type="button"
              className="emp-icon-button emp-composer__plus"
              aria-label="Choose folder"
              title={dir ? `Folder: ${dir}` : "Choose folder"}
              onClick={() => setEditingDir((v) => !v)}
            >
              <IconPlus />
            </button>
            <textarea
              ref={inputRef}
              className="emp-composer__input"
              data-orion-ui="input"
              rows={1}
              aria-label="First message"
              placeholder={state.sending ? "Starting…" : "What are we building? / for commands"}
              value={text}
              disabled={state.sending}
              onPaste={onPaste}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={onKeyDown}
            />
            <div className="emp-composer__controls">
              <AttachButton draft={DRAFT} disabled={state.sending} />
              <Picker
                label={agent ? agent.name : agents.status === "loading" ? "Loading…" : "Pick an agent"}
                title="Agent"
                value={agentId}
                onChange={setAgentId}
                options={usable.map((a) => ({
                  id: a.id,
                  label: a.name,
                  detail: (
                    <>
                      <HarnessBadge harness={a.harness} /> <span className="emp-mono">{a.model ?? "default"}</span>
                    </>
                  ),
                }))}
              />
              {agent && (
                <Picker
                  key={modelsKey}
                  label={catalogue.find((m) => m.id === (model ?? defaultModel))?.label ?? (model || (defaultAllowed ? shortModel(defaultModel) : "Pick a model"))}
                  title="Model"
                  value={model ?? ""}
                  onChange={(id) => setModel(id || null)}
                  onOpen={() => void loadModels(agent.harness, agent.harness === "hermes" ? agent.harnessProfile : null, true)}
                  options={[
                    ...(defaultAllowed ? [{ id: "", label: "Agent default", detail: <span className="emp-mono">{defaultModel}</span> }] : []),
                    ...catalogue.map((m) => ({ id: m.id, label: m.label, group: m.group ?? undefined })),
                    ...(!modelState || modelState.status === "loading"
                      ? [{ id: "__loading", label: "Loading models…", disabled: true }]
                      : modelState.status === "error"
                        ? [{ id: "__error", label: "Couldn't load models · reopen to retry", detail: modelState.error, disabled: true }]
                        : catalogue.length === 0
                          ? [{ id: "__empty", label: "No additional models available", disabled: true }]
                          : []),
                  ]}
                />
              )}
              <button
                type="button"
                className="emp-icon-button"
                aria-label="Schedule it instead"
                title="Schedule it instead: run later, or on a timetable"
                disabled={!agent}
                onClick={() =>
                  agent &&
                  editSchedule(
                    newScheduleDraft(agent.id, {
                      prompt: text.trim(),
                      ...(draft.workdir || cwd ? { cwd: (draft.workdir ?? cwd)! } : {}),
                      ...(projectFor ? { projectId: projectFor } : {}),
                    }),
                  )
                }
              >
                <IconClock />
              </button>
              <button
                type="button"
                className="emp-send"
                aria-label="Start thread"
                title="Start thread"
                disabled={!agent || (!text.trim() && draft.ids.length + draft.folders.length === 0) || draft.uploading || state.sending}
                onClick={submit}
              >
                <IconSend />
              </button>
            </div>
          </div>
          {editingDir && (
            <div className="emp-composer__extra">
              <span className="emp-composer__extra-label">Folder</span>
              <input
                className="emp-composer__dir emp-mono"
                aria-label="Folder"
                placeholder={shortPath(pickedProject?.rootPath ?? agent?.cwd ?? "~", 64)}
                value={cwd ?? ""}
                onChange={(e) => {
                  dropWorkdir();
                  setCwd(e.target.value || null);
                }}
                onKeyDown={(e) => e.key === "Enter" && inputRef.current?.focus()}
              />
            </div>
          )}
        </div>
        <CommandCard scope="new" />
        {signInNeeded && !model && !signingIn && (
          <div className="emp-composer__note" role="note">
            pi isn't signed in to <span className="emp-mono">{provider}</span>, so {agent?.name} can't use{" "}
            <span className="emp-mono">{agent?.model}</span> yet.{" "}
            <button type="button" className="emp-link" onClick={() => agent && void runCommand("/login", { agentId: agent.id })}>
              Sign in
            </button>{" "}
            (pi's <span className="emp-mono">/login</span>, right here), or pick another model.
          </div>
        )}
        <div className="emp-composer__meta">
          <Picker
            label={
              draft.workdir
                ? (droppedProject?.name ?? (draft.workdir.split("/").pop() || draft.workdir))
                : cwd
                  ? "Custom folder"
                  : (pickedProject?.name ?? "No project")
            }
            title="Project"
            value={draft.workdir ? (droppedProject?.id ?? null) : cwd ? null : (pickedProject?.id ?? "")}
            onChange={(id) => {
              if (id === "__new") {
                showSection("projects");
                return;
              }
              dropWorkdir();
              setCwd(null);
              setProjectId(id || null);
            }}
            options={[
              { id: "", label: "No project", detail: <span className="emp-mono">{shortPath(agent?.cwd ?? "~", 40)}</span> },
              ...(projects ?? []).map((p) => ({ id: p.id, label: p.name, detail: <span className="emp-mono">{shortPath(p.rootPath, 40)}</span> })),
              { id: "__new", label: "New project…" },
            ]}
          />
          <span className="emp-mono" title={dir ?? undefined}>
            {dir ? shortPath(dir, 60) : "~"}
          </span>
          {agent?.effort && <span>{agent.effort}</span>}
        </div>
      </div>
    </section>
  );
}

/** Draft key for the New thread composer's attached files. */
const DRAFT = NEW_DRAFT;

/** `openai/gpt-6.1-sol` → `gpt-6.1-sol`; keeps the picker label short. */
function shortModel(m: string): string {
  return m.replace(/^[\w-]+[/:]/, "");
}
