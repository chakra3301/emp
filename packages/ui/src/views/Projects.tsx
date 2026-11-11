import { Fragment, useEffect, useState } from "react";
import type { Project } from "@emp/protocol";
import { EmptyState, ErrorState, HarnessBadge, LoadingState } from "../components/common";
import { IconChevron, IconFolder, IconPlus, IconTerminal } from "../components/icons";
import { cleanTitle, formatBytes, shortPath } from "../lib/format";
import { formatRelative, formatShort } from "../lib/time";
import { useWorkspace } from "../store/context";
import { CHAT_HARNESSES } from "../store/workspace";

/** Every project (a folder that groups threads), plus "New project". */
export function Projects() {
  const projects = useWorkspace((s) => s.projects);
  const loadProjects = useWorkspace((s) => s.loadProjects);
  const now = useWorkspace((s) => s.now);
  const selectProject = useWorkspace((s) => s.selectProject);
  const setSidebarTab = useWorkspace((s) => s.setSidebarTab);
  const setProject = useWorkspace((s) => s.setNewThreadProject);
  const openNew = useWorkspace((s) => s.setNewThreadOpen);
  const [creating, setCreating] = useState(false);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const list = projects.data ?? [];
  const toggle = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  return (
    <section className="emp-page" data-orion-ui="panel" aria-label="Projects">
      <div className="emp-toolbar emp-toolbar--page" data-orion-ui="toolbar">
        <div className="emp-toolbar__row">
          <h2 className="emp-heading emp-heading--lg" data-orion-ui="title">
            Projects
          </h2>
          <span className="emp-count">{list.length}</span>
          <span className="emp-row__spacer" />
          <button type="button" className="emp-button emp-button--primary" data-orion-ui="button" onClick={() => setCreating(true)}>
            <IconPlus />
            New project
          </button>
        </div>
        <div className="emp-note">
          A project is a folder. Threads started in it, or imported from it, collect here. New threads can pick a project from the composer.
        </div>
      </div>
      <div className="emp-page__body">
        {creating && <NewProjectForm onDone={() => setCreating(false)} />}
        {projects.status === "loading" && !projects.data && <LoadingState label="Loading projects…" />}
        {projects.status === "error" && <ErrorState message={projects.error ?? "Could not load projects"} onRetry={() => void loadProjects()} />}
        {projects.status === "ready" && list.length === 0 && <EmptyState title="No projects yet" hint="Create one, or import your agents' history from Sources." />}
        {list.length > 0 && (
          <table className="emp-table">
            <thead>
              <tr>
                <th>Project</th>
                <th>Folder</th>
                <th className="emp-cell-num">Threads</th>
                <th>Last activity</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {list.map((p) => {
                const open = expanded.has(p.id);
                return (
                  <Fragment key={p.id}>
                    <tr
                      className={`emp-project-row${open ? " is-open" : ""}`}
                      aria-expanded={open}
                      tabIndex={0}
                      onClick={() => toggle(p.id)}
                      onKeyDown={(e) => {
                        if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) {
                          e.preventDefault();
                          toggle(p.id);
                        }
                      }}
                    >
                      <td className="emp-cell-strong">
                        <span className="emp-project-row__name">
                          <IconChevron open={open} />
                          {p.name}
                        </span>
                      </td>
                      <td className="emp-mono" title={p.rootPath}>
                        {shortPath(p.rootPath, 56)}
                      </td>
                      <td className="emp-cell-num">
                        {p.threadCount}
                        {p.automatedCount > 0 && <span className="emp-count"> +{p.automatedCount} auto</span>}
                      </td>
                      <td>{p.lastActivityAt ? formatRelative(p.lastActivityAt, now) : "—"}</td>
                      <td className="emp-cell-actions" onClick={(e) => e.stopPropagation()}>
                        <button
                          type="button"
                          className="emp-button emp-button--small emp-button--ghost"
                          data-orion-ui="button"
                          onClick={() => {
                            setSidebarTab("threads");
                            selectProject(p.id);
                          }}
                        >
                          Threads
                        </button>
                        <button
                          type="button"
                          className="emp-button emp-button--small"
                          data-orion-ui="button"
                          onClick={() => {
                            setProject(p.id);
                            openNew(true);
                          }}
                        >
                          New thread
                        </button>
                      </td>
                    </tr>
                    {open && (
                      <tr className="emp-project-detail-row">
                        <td colSpan={5}>
                          <ProjectDetail project={p} />
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </section>
  );
}

function NewProjectForm({ onDone }: { onDone(): void }) {
  const state = useWorkspace((s) => s.projectCreate);
  const create = useWorkspace((s) => s.createProject);
  const [path, setPath] = useState("~/Desktop/");
  const [name, setName] = useState("");
  const [createFolder, setCreateFolder] = useState(true);

  const submit = async () => {
    if (path.trim() && (await create(path.trim(), name.trim() || null, createFolder))) onDone();
  };

  return (
    <div className="emp-card emp-card--editing emp-project-form">
      <div className="emp-form">
        <div className="emp-form__grid">
          <label className="emp-form__label">
            Folder
            <input
              className="emp-input emp-mono"
              data-orion-ui="input"
              value={path}
              autoFocus
              onChange={(e) => setPath(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void submit()}
            />
          </label>
          <label className="emp-form__label">
            Name (optional)
            <input
              className="emp-input"
              data-orion-ui="input"
              placeholder="Defaults to the folder name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void submit()}
            />
          </label>
        </div>
        <label className="emp-check">
          <input type="checkbox" checked={createFolder} onChange={(e) => setCreateFolder(e.target.checked)} />
          Create the folder if it doesn't exist
        </label>
        {state.error && <ErrorState message={state.error} />}
        <div className="emp-toolbar__row">
          <span className="emp-row__spacer" />
          <button type="button" className="emp-button emp-button--ghost" data-orion-ui="button" onClick={onDone}>
            Cancel
          </button>
          <button type="button" className="emp-button emp-button--primary" data-orion-ui="button" disabled={!path.trim() || state.saving} onClick={() => void submit()}>
            {state.saving ? "Creating…" : "Create project"}
          </button>
        </div>
      </div>
    </div>
  );
}

/** What's inside a project: its threads, the agents that work there, and the folder's top level. */
function ProjectDetail({ project: p }: { project: Project }) {
  const threads = useWorkspace((s) => s.projectThreads[p.id]);
  const files = useWorkspace((s) => s.projectFiles[p.id]);
  const load = useWorkspace((s) => s.loadProjectDetail);
  const agents = useWorkspace((s) => s.agents.data);
  const live = useWorkspace((s) => s.live);
  const now = useWorkspace((s) => s.now);
  const openThread = useWorkspace((s) => s.openThread);
  const selectProject = useWorkspace((s) => s.selectProject);
  const setSidebarTab = useWorkspace((s) => s.setSidebarTab);
  const setAgent = useWorkspace((s) => s.setNewThreadAgent);
  const setProject = useWorkspace((s) => s.setNewThreadProject);
  const openNew = useWorkspace((s) => s.setNewThreadOpen);
  const runInTerminal = useWorkspace((s) => s.runInTerminal);

  useEffect(() => {
    void load(p.id);
  }, [p.id, load]);

  // Agents whose folder is inside the project, plus any agent that has threads here.
  const threadAgents = new Set((threads?.data?.items ?? []).map((t) => t.agentId).filter(Boolean));
  const projectAgents = (agents ?? []).filter((a) => (a.cwd && isInside(a.cwd, p.rootPath)) || threadAgents.has(a.id));

  return (
    <div className="emp-project-detail">
      <div className="emp-project-detail__col emp-project-detail__col--wide">
        <div className="emp-project-detail__head">
          Threads
          {threads?.data && <span className="emp-count">{threads.data.total}</span>}
        </div>
        {threads?.status === "error" && <ErrorState message={threads.error ?? "Could not load threads"} onRetry={() => void load(p.id)} />}
        {!threads?.data && threads?.status !== "error" && <LoadingState label="Loading threads…" />}
        {threads?.data?.items.length === 0 && <div className="emp-note">No threads yet.</div>}
        <ul className="emp-project-list">
          {threads?.data?.items.map((t) => {
            const status = live[t.id]?.status;
            return (
              <li key={t.id}>
                <button type="button" className="emp-project-item" onClick={() => void openThread(t.id)} title={cleanTitle(t.title)}>
                  <HarnessBadge harness={t.harness} />
                  <span className="emp-project-item__label emp-truncate">{cleanTitle(t.title)}</span>
                  {status && status !== "idle" && <span className="emp-project-item__live">{status}</span>}
                  <span className="emp-project-item__meta">{formatShort(t.updatedAt, now)}</span>
                </button>
              </li>
            );
          })}
        </ul>
        {threads?.data && threads.data.total > threads.data.items.length && (
          <button
            type="button"
            className="emp-link"
            onClick={() => {
              setSidebarTab("threads");
              selectProject(p.id);
            }}
          >
            All {threads.data.total} threads in the sidebar
          </button>
        )}
      </div>

      <div className="emp-project-detail__col">
        <div className="emp-project-detail__head">
          Agents
          <span className="emp-count">{projectAgents.length}</span>
        </div>
        {projectAgents.length === 0 && <div className="emp-note">No agent is set to work here. Any agent can start a thread in this project.</div>}
        <ul className="emp-project-list">
          {projectAgents.map((a) => (
            <li key={a.id}>
              <button
                type="button"
                className="emp-project-item"
                disabled={!CHAT_HARNESSES.includes(a.harness)}
                title={`New thread with ${a.name} in ${p.name}`}
                onClick={() => {
                  setAgent(a.id);
                  setProject(p.id);
                  openNew(true);
                }}
              >
                <span className="emp-project-item__label emp-truncate">{a.name}</span>
                <HarnessBadge harness={a.harness} />
              </button>
            </li>
          ))}
        </ul>
      </div>

      <div className="emp-project-detail__col">
        <div className="emp-project-detail__head">
          Files
          {files?.data?.exists && <span className="emp-count">{files.data.entries.length}{files.data.truncated ? "+" : ""}</span>}
          <span className="emp-row__spacer" />
          <button
            type="button"
            className="emp-link"
            title={`Open a shell in ${p.rootPath}`}
            onClick={() => void runInTerminal("zsh", ["-l"], p.rootPath, p.name)}
          >
            <IconTerminal /> Shell here
          </button>
        </div>
        {files?.status === "error" && <ErrorState message={files.error ?? "Could not list files"} onRetry={() => void load(p.id)} />}
        {!files?.data && files?.status !== "error" && <LoadingState label="Listing files…" />}
        {files?.data && !files.data.exists && <div className="emp-note">This folder no longer exists.</div>}
        {files?.data?.exists && files.data.entries.length === 0 && <div className="emp-note">Empty folder.</div>}
        <ul className="emp-project-list emp-project-list--files">
          {files?.data?.entries.map((f) => (
            <li key={f.name} className="emp-project-file">
              {f.isDir ? <IconFolder /> : <span className="emp-project-file__pad" />}
              <span className="emp-truncate">{f.isDir ? `${f.name}/` : f.name}</span>
              <span className="emp-project-item__meta">{formatBytes(f.size)}</span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

function isInside(path: string, root: string): boolean {
  const r = root.replace(/\/$/, "");
  return path === r || path.startsWith(`${r}/`);
}
