import { EmpClient } from "@emp/client";
import { MockServer, createDemoFixture } from "@emp/client/mock";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  agentDraft,
  appendImportLine,
  createWorkspaceStore,
  hasMoreThreads,
  initialWorkspaceState,
  newThreadProject,
  pageContext,
  summarizeImport,
  threadListParams,
  type ImportLine,
  type WorkspaceStore,
} from "./workspace";

const cleanups: Array<() => void> = [];
afterEach(() => {
  for (const c of cleanups.splice(0)) c();
});

async function setup(server = new MockServer({ importStepMs: 0 })) {
  const prefs = new Map<string, unknown>();
  const notify = vi.fn();
  const host = { notify, prefs: { get: async (k: string) => prefs.get(k), set: async (k: string, v: unknown) => void prefs.set(k, v) } };
  const client = new EmpClient(server.factory, { token: "t", client: { kind: "browser", version: "test" } });
  const store = createWorkspaceStore(client, host);
  const stop = store.getState().start();
  cleanups.push(stop, () => client.dispose());
  await client.connect();
  await until(store, (s) => s.threads.status === "ready" && s.projects.status === "ready");
  return { server, client, store, prefs, notify, stop };
}

async function until(store: WorkspaceStore, pred: (s: ReturnType<WorkspaceStore["getState"]>) => boolean) {
  for (let i = 0; i < 200; i += 1) {
    if (pred(store.getState())) return;
    await new Promise((r) => setTimeout(r, 1));
  }
  throw new Error("condition not reached");
}

describe("pure helpers", () => {
  it("builds threads.list params from the filter", () => {
    expect(threadListParams({ projectId: null, harness: null, query: "  ", includeAutomated: false }, 0)).toEqual({ limit: 100, offset: 0, coordinator: false, ungrouped: true, topLevel: true });
    expect(threadListParams({ projectId: "p", harness: "pi", query: " pty ", includeAutomated: true }, 100)).toEqual({
      limit: 100,
      offset: 100,
      coordinator: false,
      projectId: "p",
      harness: "pi",
      query: "pty",
      includeAutomated: true,
    });
  });

  it("caps the import log", () => {
    let lines: ImportLine[] = [];
    for (let i = 0; i < 520; i += 1) lines = appendImportLine(lines, { id: i, source: "pi", message: String(i), at: 0 });
    expect(lines).toHaveLength(500);
    expect(lines[0]!.id).toBe(20);
  });

  it("summarizes import reports", () => {
    const stats = { threadsSeen: 5, threadsAdded: 2, threadsUpdated: 1, threadsUnchanged: 2, agentsSeen: 0, skipped: 0, warnings: [] };
    expect(
      summarizeImport({
        sources: [
          { source: "pi", stats, error: null, durationMs: 1 },
          { source: "claude", stats, error: "boom", durationMs: 1 },
        ],
      }),
    ).toBe("4 new, 2 updated, 1 source failed");
  });

  it("starts idle on the threads section", () => {
    const s = initialWorkspaceState({ state: "connecting", attempt: 0 }, 0);
    expect(s.section).toBe("threads");
    expect(hasMoreThreads(s.threads)).toBe(false);
  });
});

describe("workspace store against the mock daemon", () => {
  it("loads projects and the first thread page once connected", async () => {
    const { store, server } = await setup();
    const s = store.getState();
    expect(s.connection.state).toBe("ready");
    expect(s.projects.data).toHaveLength(server.fixture.projects.length);
    expect(s.threads.items.length).toBeGreaterThan(0);
    expect(s.threads.total).toBe(server.fixture.threads.filter((t) => !t.automated && !t.parentId && t.agentId !== "a-suki").length);
  });

  it("filters by project and harness, and persists the harness pref", async () => {
    const { store, prefs } = await setup();
    store.getState().selectProject("p-emp");
    await until(store, (s) => s.threads.status === "ready");
    expect(store.getState().threads.items.every((t) => t.projectId === "p-emp")).toBe(true);

    store.getState().setHarness("codex");
    await until(store, (s) => s.threads.status === "ready");
    expect(store.getState().threads.items.every((t) => t.harness === "codex" && t.projectId === "p-emp")).toBe(true);
    await Promise.resolve();
    expect(prefs.get("threads.harness")).toBe("codex");
  });

  it("drops stale thread-list responses when the query changes quickly", async () => {
    const { store } = await setup(new MockServer({ latencyMs: 2 }));
    store.getState().setQuery("zzz-no-match");
    store.getState().setQuery("importer");
    await until(store, (s) => s.threads.status === "ready");
    const items = store.getState().threads.items;
    expect(items.length).toBeGreaterThan(0);
    expect(items.every((t) => t.title.toLowerCase().includes("importer"))).toBe(true);
  });

  it("pages more threads", async () => {
    const fixture = createDemoFixture();
    const base = fixture.threads[0]!;
    fixture.threads = Array.from({ length: 230 }, (_, i) => ({ ...base, id: `x-${i}`, updatedAt: base.updatedAt - i }));
    const { store } = await setup(new MockServer({ fixture }));
    expect(store.getState().threads.items).toHaveLength(100);
    await store.getState().loadMoreThreads();
    await store.getState().loadMoreThreads();
    expect(store.getState().threads.items).toHaveLength(230);
    expect(hasMoreThreads(store.getState().threads)).toBe(false);
  });

  it("opens a long thread at the tail and loads earlier pages", async () => {
    const { store } = await setup();
    await store.getState().openThread("t-001");
    let tr = store.getState().transcript;
    expect(tr).toMatchObject({ status: "ready", start: 3000, total: 3200 });
    expect(tr.items).toHaveLength(200);
    await store.getState().loadEarlier();
    tr = store.getState().transcript;
    expect(tr.start).toBe(2800);
    expect(tr.items).toHaveLength(400);
    expect(store.getState().thread.data?.id).toBe("t-001");
  });

  it("fetches the resume command", async () => {
    const { store } = await setup();
    const claude = store.getState().threads.items.find((t) => t.harness === "claude")!;
    await store.getState().openThread(claude.id);
    await store.getState().fetchResume();
    expect(store.getState().resume.spec?.display).toContain(`claude --resume ${claude.session.sessionId}`);
  });

  it("reports a missing thread as an error", async () => {
    const { store } = await setup();
    await store.getState().openThread("nope");
    await until(store, (s) => s.thread.status === "error");
    expect(store.getState().transcript.status).toBe("error");
  });

  it("runs an import with live progress, then refreshes and notifies", async () => {
    const { store, server, notify } = await setup();
    const before = server.received.filter((r) => r.method === "projects.list").length;
    await store.getState().runImport();
    const run = store.getState().importRun;
    expect(run.running).toBe(false);
    expect(run.report?.sources.length).toBe(server.fixture.doctor.sources.length);
    expect(run.lines.length).toBeGreaterThan(0);
    await until(store, () => server.received.filter((r) => r.method === "projects.list").length > before);
    expect(notify).toHaveBeenCalledWith("EMP import finished", expect.any(String));
  });

  it("stops reacting to the client after the teardown", async () => {
    const { store, server, stop } = await setup();
    stop();
    const lines = store.getState().importRun.lines.length;
    server.notifyAll("import.progress", { source: "pi", message: "late" });
    await new Promise((r) => setTimeout(r, 5));
    expect(store.getState().importRun.lines).toHaveLength(lines);
  });
});

describe("live sessions, terminals and agents (mock daemon)", () => {
  it("streams a reply into the open thread, then reloads the transcript", async () => {
    const { store } = await setup();
    const id = store.getState().threads.items.find((t) => t.harness === "pi")!.id;
    await store.getState().openThread(id);
    await until(store, (s) => s.transcript.status === "ready");
    const before = store.getState().transcript.total;

    expect(await store.getState().send("hello there")).toBe(true);
    await until(store, (s) => s.live[id]?.status === "running");
    await until(store, (s) => s.liveView.tail.some((i) => i.kind === "user"));
    // After the turn the daemon says the transcript changed; the tail folds into it.
    await until(store, (s) => s.transcript.total === before + 2 && s.liveView.tail.length === 0);
    const last = store.getState().transcript.items.at(-1)!;
    expect(last.kind).toBe("assistant");
    expect(store.getState().live[id]?.status).toBe("idle");
  });

  it("refuses chat while a takeover terminal holds the session, and returns it", async () => {
    const { store } = await setup();
    const id = store.getState().threads.items.find((t) => t.harness === "claude")!.id;
    await store.getState().openThread(id);
    await store.getState().takeover();
    const termId = store.getState().paneTerminalId!;
    expect(store.getState().terminals[termId]?.kind).toBe("resume");
    await until(store, (s) => s.live[id]?.owner === "terminal");

    expect(await store.getState().send("hi")).toBe(false);
    expect(store.getState().composer.error).toMatch(/terminal/);

    await store.getState().release();
    await until(store, (s) => !s.live[id]);
    expect(store.getState().paneTerminalId).toBeNull();
    expect(store.getState().terminals[termId]?.state).toBe("exited");
  });

  it("starts a new thread with an agent and opens it", async () => {
    const { store } = await setup();
    store.getState().setNewThreadOpen(true);
    await until(store, (s) => s.agents.status === "ready");
    const desk = store.getState().agents.data!.find((a) => !a.coordinator)!;
    expect(await store.getState().startThread(desk.id, "Plan the week", null)).toBe(true);
    const s = store.getState();
    expect(s.newThread.open).toBe(false);
    expect(s.thread.data?.agentId).toBe(desk.id);
    expect(s.thread.data?.title).toBe("Plan the week");
  });

  it("notifies when a reply finishes elsewhere; the click opens the thread; the switch turns it off", async () => {
    const { store, notify } = await setup();
    await until(store, (s) => s.agents.status === "ready");
    const desk = store.getState().agents.data!.find((a) => !a.coordinator)!;
    expect(await store.getState().startThread(desk.id, "Plan the week", null)).toBe(true);
    const id = store.getState().selectedThreadId!;
    store.getState().activateTab("new");
    await until(store, (s) => s.live[id]?.status === "idle" || !s.live[id]);
    const call = notify.mock.calls.find((c) => c[0] === "Plan the week");
    expect(call?.[1]).toMatch(/Got it/);
    (call?.[2] as () => void)();
    await until(store, (s) => s.activeTab === `t:${id}`);

    notify.mockClear();
    store.getState().updateSettings({ notifications: false });
    store.getState().activateTab("new");
    await store.getState().send("again");
    await until(store, (s) => s.live[id]?.status === "idle" || !s.live[id]);
    await new Promise((r) => setTimeout(r, 50));
    expect(notify).not.toHaveBeenCalled();
  });

  it("creates, edits and deletes an agent", async () => {
    const { store } = await setup();
    await until(store, (s) => s.agents.status === "ready");
    const count = store.getState().agents.data!.length;
    expect(await store.getState().saveAgent({ ...agentDraft(), name: "Reviewer", harness: "claude", autonomy: 0 })).toBe(true);
    const created = store.getState().agents.data!.find((a) => a.name === "Reviewer")!;
    expect(store.getState().agents.data).toHaveLength(count + 1);
    expect(await store.getState().saveAgent({ ...agentDraft(created), model: "opus" })).toBe(true);
    expect(store.getState().agents.data!.find((a) => a.id === created.id)!.model).toBe("opus");
    await store.getState().deleteAgent(created.id);
    expect(store.getState().agents.data).toHaveLength(count);
  });

  it("opens a side shell for the thread and kills it", async () => {
    const { store } = await setup();
    await store.getState().openThread(store.getState().threads.items[0]!.id);
    await store.getState().openShell();
    const termId = store.getState().paneTerminalId!;
    expect(store.getState().terminals[termId]?.kind).toBe("shell");
    await store.getState().killTerminal(termId);
    await until(store, (s) => s.terminals[termId]?.state === "exited");
    await store.getState().killTerminal(termId);
    expect(store.getState().terminals[termId]).toBeUndefined();
  });
});

describe("models, projects and terminal agents (mock daemon)", () => {
  it("removes every Copilot provider from an older daemon's model catalogue", async () => {
    const { store, client } = await setup();
    vi.spyOn(client, "request").mockResolvedValueOnce([
      { id: "copilot:gpt-6-astra", label: "GPT Astra", group: "copilot" },
      { id: "copilot-acp:gpt-6-astra", label: "GPT Astra", group: "copilot-acp" },
      { id: "github-copilot/gpt-6-astra", label: "GPT Astra", group: "github-copilot" },
      { id: "gpt-6-astra", label: "GPT Astra", group: "GitHub Copilot" },
      { id: "openai-codex:gpt-6-astra", label: "GPT Astra · ChatGPT", group: "ChatGPT" },
    ]);
    await store.getState().loadModels("hermes", "studio");
    expect(store.getState().models["hermes:studio"]?.data?.map((m) => m.id)).toEqual(["openai-codex:gpt-6-astra"]);
  });

  it("loads a harness model catalogue once and resets the pick when the agent changes", async () => {
    const { store } = await setup();
    await store.getState().loadModels("pi");
    expect(store.getState().models["pi:"]?.data?.map((m) => m.id)).toContain("openai/gpt-6.1-sol");
    store.getState().setNewThreadAgent("a");
    store.getState().setNewThreadModel("openai/gpt-6.1-sol");
    store.getState().setNewThreadAgent("b");
    expect(store.getState().newThreadModel).toBeNull();
  });

  it("keeps model catalogues separate for each harness and Hermes profile", async () => {
    const { store, server } = await setup();
    const s = () => store.getState();
    await s().loadModels("hermes", "studio");
    await s().loadModels("hermes", "default");
    await s().loadModels("codex");
    await s().loadModels("hermes", "studio");
    expect(s().models["hermes:studio"]?.status).toBe("ready");
    expect(s().models["hermes:default"]?.status).toBe("ready");
    expect(s().models["codex:"]?.status).toBe("ready");
    expect(server.received.filter((r) => r.method === "models.list").map((r) => r.params)).toEqual([
      { harness: "hermes", profile: "studio" },
      { harness: "hermes", profile: "default" },
      { harness: "codex" },
    ]);
    // Opening a picker refreshes provider compatibility without discarding other profiles.
    await s().loadModels("hermes", "studio", true);
    expect(server.received.filter((r) => r.method === "models.list")).toHaveLength(4);
    expect(s().models["hermes:default"]?.status).toBe("ready");
  });

  it("follows the sidebar until a project is picked, including an explicit No project", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    const [first, second] = s().projects.data!;
    s().selectProject(first!.id);
    expect(newThreadProject(s())?.id).toBe(first!.id);
    s().setNewThreadProject(second!.id);
    expect(newThreadProject(s())?.id).toBe(second!.id);
    s().setNewThreadAgent("a-studio");
    expect(newThreadProject(s())?.id).toBe(second!.id);
    s().setNewThreadProject(null);
    expect(newThreadProject(s())).toBeNull();
  });

  it("starts with the chosen project, agent and model together", async () => {
    const { store, server } = await setup();
    await until(store, (s) => s.agents.status === "ready");
    const s = () => store.getState();
    const studio = s().agents.data!.find((a) => a.name === "studio")!;
    const project = s().projects.data![0]!;
    s().selectProject(s().projects.data![1]!.id);
    s().setNewThreadProject(project.id);
    s().setNewThreadAgent(studio.id);
    s().setNewThreadModel("openai-codex:gpt-6-astra");
    expect(await s().startThread(studio.id, "Reply OK", newThreadProject(s())!.rootPath, {
      model: s().newThreadModel,
      projectId: newThreadProject(s())!.id,
    })).toBe(true);
    expect(server.received.find((r) => r.method === "threads.start")?.params).toEqual({
      agentId: studio.id,
      text: "Reply OK",
      cwd: project.rootPath,
      projectId: project.id,
      model: "openai-codex:gpt-6-astra",
    });
    expect(s().thread.data).toMatchObject({ agentId: studio.id, harness: "hermes", cwd: project.rootPath, projectId: project.id });
    expect(s().newThreadAgentId).toBe(studio.id);
  });

  it("doesn't attach a sidebar project when the new thread explicitly clears it or uses a custom folder", async () => {
    const { store, server } = await setup();
    await until(store, (s) => s.agents.status === "ready");
    const s = () => store.getState();
    s().selectProject(s().projects.data![0]!.id);
    const agent = s().agents.data!.find((a) => !a.coordinator)!;
    expect(await s().startThread(agent.id, "Reply OK", "/tmp/emp-picker-test", { projectId: null })).toBe(true);
    const request = server.received.find((r) => r.method === "threads.start")!;
    expect(request.params).toMatchObject({ cwd: "/tmp/emp-picker-test" });
    expect(request.params).not.toHaveProperty("projectId");
  });

  it("creates a project, selects it for the next thread and lists it", async () => {
    const { store } = await setup();
    expect(await store.getState().createProject("~/Desktop/new-game", "New game", true)).toBe(true);
    const created = store.getState().projects.data!.find((p) => p.name === "New game")!;
    expect(created.rootPath).toBe("~/Desktop/new-game");
    expect(store.getState().newThreadProjectId).toBe(created.id);
    expect(await store.getState().createProject("relative/path", null, true)).toBe(false);
    expect(store.getState().projectCreate.error).toMatch(/absolute/);
  });

  it("loads one project's threads and files for its expanded row", async () => {
    const { store } = await setup();
    const project = store.getState().projects.data![0]!;
    await store.getState().loadProjectDetail(project.id);
    const s = store.getState();
    expect(s.projectThreads[project.id]?.status).toBe("ready");
    expect(s.projectThreads[project.id]?.data?.items.every((t) => t.projectId === project.id)).toBe(true);
    expect(s.projectFiles[project.id]?.data?.entries[0]).toMatchObject({ name: "src", isDir: true });
    await store.getState().loadProjectDetail("nope");
    expect(store.getState().projectFiles.nope?.status).toBe("error");
  });

  it("persists settings through host.prefs; live drags save only on release", async () => {
    const { store, prefs } = await setup();
    store.getState().updateSettings({ sidebarScale: 1.4 }, { persist: false });
    expect(store.getState().settings.sidebarScale).toBe(1.4);
    expect(prefs.get("ui.settings")).toBeUndefined();
    store.getState().updateSettings({ accent: "#1a7f37", statusBar: false });
    expect(prefs.get("ui.settings")).toMatchObject({ accent: "#1a7f37", statusBar: false, sidebarScale: 1.4 });
    store.getState().resetLayout();
    expect(store.getState().settings).toMatchObject({ accent: "#1a7f37", statusBar: true, sidebarScale: 1 });
  });

  it("Settings overlay reopens on its last page; the layout editor closes it; layouts save and apply", async () => {
    const { store, prefs } = await setup();
    const s = () => store.getState();
    s().showSection("settings");
    expect(s().settingsPage).toBe("appearance/theme");
    s().openSettings("about");
    s().closeSettings();
    expect(s().settingsPage).toBeNull();
    s().openSettings();
    expect(s().settingsPage).toBe("about");
    s().setLayoutEditing(true);
    expect(s().settingsPage).toBeNull();
    expect(s().layoutEditing).toBe(true);
    s().applyArrangement({ sidebarSide: "right", sidebarScale: 1.2, threadWidth: "readable" });
    s().saveLayout("Mine");
    s().applyArrangement({ sidebarSide: "left", sidebarScale: 1, threadWidth: "full" });
    const saved = s().settings.savedLayouts[0]!;
    expect(saved).toMatchObject({ name: "Mine", arrangement: { sidebarSide: "right", sidebarScale: 1.2, threadWidth: "readable" } });
    expect((prefs.get("ui.settings") as { savedLayouts: unknown[] }).savedLayouts).toHaveLength(1);
    s().deleteLayout(saved.id);
    expect(s().settings.savedLayouts).toEqual([]);
  });

  it("loads and saves the daemon-wide compaction limit, and follows changes from other windows", async () => {
    const { store, client } = await setup();
    await store.getState().loadDaemonSettings();
    expect(store.getState().daemonSettings.data).toEqual({ compactAt: null });
    expect(await store.getState().saveDaemonSettings({ compactAt: 150_000 })).toBeNull();
    expect(store.getState().daemonSettings.data?.compactAt).toBe(150_000);
    expect(await store.getState().saveDaemonSettings({ compactAt: 5 })).toMatch(/between/);
    expect(store.getState().daemonSettings.data?.compactAt).toBe(150_000);
    // Another window (or a phone) changes it: this store hears settings.changed.
    await client.request("settings.set", { compactAt: 200_000 });
    await until(store, (s) => s.daemonSettings.data?.compactAt === 200_000);
    // Per-agent overrides ride on agents.save.
    expect(agentDraft().compactAt).toBeNull();
  });

  it("attaches files to a draft and sends them with the message", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    await until(store, (st) => st.threads.items.length > 0);
    const thread = s().threads.items[0]!;
    await s().openThread(thread.id);
    await s().attachFiles(thread.id, [
      { name: "shot.png", mime: "image/png", data: btoa("png"), size: 3, preview: "data:image/png;base64,cG5n" },
      { name: "huge.mov", mime: "video/quicktime", data: "", size: 9e9, preview: null, error: "huge.mov is too big" },
    ]);
    const files = s().drafts[thread.id]!;
    expect(files.map((f) => f.status)).toEqual(["ready", "error"]);
    expect(await s().send("look", "prompt", false, [files[0]!.id!])).toBe(true);
    await until(store, (st) => st.liveView.tail.some((i) => i.kind === "user" && i.text.includes("[Attached: /mock/attachments/")));
    s().clearDraft(thread.id);
    expect(s().drafts[thread.id]).toBeUndefined();
  });

  it("attaches files on the Mac where they are, any size, and uploads the rest", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    let uploads = 0;
    const upload = async () => (uploads++, { name: "elsewhere.txt", mime: "text/plain", data: btoa("hi"), size: 2, preview: null });
    await s().attachFiles("new", [
      { name: "launch.mov", mime: "video/quicktime", data: "", size: 9e9, preview: null, find: { size: 9e9, modified: 1 } },
      { name: "elsewhere.txt", mime: "text/plain", data: "", size: 2, preview: null, find: { size: 2, modified: 1 }, upload },
      { name: "elsewhere.mov", mime: "video/quicktime", data: "", size: 9e9, preview: null, find: { size: 9e9, modified: 1 } },
    ]);
    const files = s().drafts.new!;
    expect(files.map((f) => f.status)).toEqual(["ready", "ready", "error"]);
    expect(uploads).toBe(1);
    expect(files[2]!.error).toContain("Couldn't find elsewhere.mov on this Mac");
  });

  it("finds dropped folders, as the working folder or context", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    await s().attachFolders("new", [{ name: "Site", entries: ["src"] }, { name: "Notes", entries: [] }], "workdir");
    let chips = s().drafts.new!;
    expect(chips.map((c) => [c.status, c.folder?.use, c.folder?.path])).toEqual([
      ["ready", "workdir", "/Users/demo/Desktop/Site"],
      ["ready", "context", "/Users/demo/Desktop/Notes"],
    ]);
    expect(chips[0]!.folder!.options).toHaveLength(2);
    // One working folder at a time.
    s().setDraftFolder("new", chips[1]!.key, { use: "workdir" });
    chips = s().drafts.new!;
    expect(chips.map((c) => c.folder?.use)).toEqual(["context", "workdir"]);
    await s().attachFolders("new", [{ name: "Nowhere", entries: ["x"] }], "workdir");
    expect(s().drafts.new![2]!.error).toMatch(/Couldn't find Nowhere/);

    // In a thread a folder is context; "New thread in this folder" moves it over as the working folder.
    await s().attachFolders("t1", [{ name: "App", entries: [] }], "context");
    const chip = s().drafts.t1![0]!;
    expect(chip.folder?.use).toBe("context");
    s().newThreadInFolder("t1", chip.key, null);
    expect(s().drafts.t1).toEqual([]);
    expect(s().drafts.new!.filter((c) => c.folder?.use === "workdir").map((c) => c.name)).toEqual(["App"]);
    expect(s().activeTab).toBe("new");
  });

  it("sends Suki the files and folders dropped on its panel", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    await until(store, (st) => (st.agents.data?.length ?? 0) > 0);
    await s().attachFiles("suki", [{ name: "plan.md", mime: "text/markdown", data: btoa("# plan"), size: 6, preview: null }]);
    const id = s().drafts.suki![0]!.id!;
    expect(await s().sendToSuki("[Attached: /Users/demo/Desktop/Site/]", [id])).toBe(true);
    await until(store, (st) => !!st.suki.threadId);
    // Nothing typed but something attached still sends.
    expect(await s().sendToSuki("", [id])).toBe(true);
    expect(await s().sendToSuki("", [])).toBe(false);
  });

  it("makes, pauses, runs and deletes scheduled tasks", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    await until(store, (st) => st.schedules.status === "ready");
    expect(s().schedules.data!.map((x) => x.id)).toEqual(["s-goal", "s-build", "s-deps", "s-deploy"]);
    // The Codex thread's schedule names its thread.
    expect(s().schedules.data![0]!.threadTitle).toBeTruthy();

    s().editSchedule({ title: "", prompt: "", when: { kind: "cron", expr: "0 9 * * *" }, agentId: "a-worker" });
    expect(s().activeTab).toBe("page:schedules");
    expect(await s().saveSchedule({ title: "", prompt: "", when: { kind: "cron", expr: "0 9 * * *" }, agentId: "a-worker" })).toBe(false);
    expect(s().scheduleEditor!.error).toMatch(/what the agent should do/);
    expect(await s().saveSchedule({ title: "", prompt: "Tidy the inbox\nthen report", when: { kind: "cron", expr: "0 9 * * *" }, agentId: "a-worker" })).toBe(true);
    expect(s().scheduleEditor).toBeNull();
    await until(store, (st) => st.schedules.data!.some((x) => x.title === "Tidy the inbox"));
    const made = s().schedules.data!.find((x) => x.title === "Tidy the inbox")!;
    expect(made.summary).toBe("Daily at 9:00 AM");
    // Ends are kept on save.
    expect(await s().saveSchedule({ id: made.id, title: made.title, prompt: made.prompt, when: made.when, agentId: "a-worker", maxRuns: 3, stopWhen: "inbox zero" })).toBe(true);
    await until(store, (st) => st.schedules.data!.find((x) => x.id === made.id)!.maxRuns === 3);
    expect(s().schedules.data!.find((x) => x.id === made.id)!.stopWhen).toBe("inbox zero");
    // Resuming a finished one starts it over.
    await s().enableSchedule("s-deploy", true);
    await until(store, (st) => st.schedules.data!.find((x) => x.id === "s-deploy")!.enabled);
    expect(s().schedules.data!.find((x) => x.id === "s-deploy")!.ended).toBeNull();
    // Hermes's own jobs load with the page.
    s().showSection("schedules");
    await until(store, (st) => st.externalSchedules.status === "ready");
    expect(s().externalSchedules.data![0]!.name).toBe("monthly-skill-review");

    await s().enableSchedule(made.id, false);
    await until(store, (st) => st.schedules.data!.find((x) => x.id === made.id)!.enabled === false);
    await s().runSchedule(made.id);
    await until(store, (st) => st.schedules.data!.find((x) => x.id === made.id)!.lastStatus === "running");
    await s().deleteSchedule(made.id);
    await until(store, (st) => !st.schedules.data!.some((x) => x.id === made.id));

    const preview = await s().previewSchedule({ kind: "cron", expr: "0 9 * *" });
    expect(preview!.error).toMatch(/5 fields/);
  });

  it("switches an open chat's model from its next message", async () => {
    const { store, server } = await setup();
    const s = () => store.getState();
    await until(store, (st) => st.threads.status === "ready");
    const t = server.fixture.threads.find((x) => x.harness === "codex" && !x.parentId)!;
    await s().openThread(t.id);
    s().setThreadModel(t.id, "gpt-6-luna");
    expect(await s().send("hi")).toBe(true);
    expect(server.fixture.threads.find((x) => x.id === t.id)!.model).toBe("gpt-6-luna");
    expect(s().threadModels[t.id]).toBe("gpt-6-luna");
  });

  it("runs terminal-only slash commands in a card in the chat", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    await s().loadCommands("pi", null);
    expect(s().commands["pi:"]!.data!.find((c) => c.name === "login")!.run).toBe("terminal");
    expect(s().commands["pi:"]!.data!.find((c) => c.name === "compact")!.run).toBe("native");
    await until(store, (st) => (st.agents.data?.length ?? 0) > 0);
    const pi = s().agents.data!.find((a) => a.harness === "pi")!;
    s().setNewThreadOpen(true);
    expect(await s().runCommand("/login", { agentId: pi.id })).toBe(true);
    // It stays on the page it was typed on.
    expect(s().activeTab).toBe("new");
    const id = s().commandCards.new!;
    expect(s().terminals[id]?.state).toBe("running");
    await s().closeCommandCard("new");
    expect(s().commandCards.new).toBeUndefined();
  });

  it("shows a Codex goal and the Terminal window that has the session open", async () => {
    const { store, server } = await setup();
    const s = () => store.getState();
    const t = server.fixture.threads.find((x) => x.harness === "codex" && !x.parentId)!;
    await s().loadGoal(t.id);
    expect(s().goals[t.id]?.status).toBe("paused");
    await s().loadHolders(t.id);
    expect(s().holders[t.id]?.[0]?.app).toBe("Terminal");
    expect(await s().runNative(t.id, "/goal resume")).toBe(true);
    await until(store, (st) => st.live[t.id]?.goal?.status === "active");
    expect(s().live[t.id]!.status).toBe("running");
    expect(await s().closeElsewhere(t.id)).toBe(true);
    expect(s().holders[t.id]).toEqual([]);
  });

  it("runs native commands in the chat and answers the question card", async () => {
    const { store, server } = await setup();
    const s = () => store.getState();
    const t = server.fixture.threads.find((x) => x.harness === "pi" && !x.parentId && x.agentId !== "a-suki")!;
    await s().openThread(t.id);
    expect(await s().runNative(t.id, "/model")).toBe(true);
    await until(store, (st) => (st.live[t.id]?.questions.length ?? 0) > 0);
    const q = s().live[t.id]!.questions[0]!;
    expect(s().live[t.id]!.status).toBe("needs-input");
    expect(q.options).toContain("openai/gpt-6.1-sol");
    await s().answerQuestion(t.id, q.id, { value: "openai/gpt-6.1-sol" });
    expect(s().live[t.id]!.questions).toEqual([]);
    expect(server.fixture.threads.find((x) => x.id === t.id)!.model).toBe("openai/gpt-6.1-sol");
    // An answer that's no longer wanted shows an error instead of throwing.
    await s().answerQuestion(t.id, q.id, { cancelled: true });
    expect(s().composer.error).toMatch(/already answered/);
  });

  it("lists dev servers on connect and stops one", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    await until(store, (st) => st.servers.status === "ready");
    const list = s().servers.data!;
    expect(list.filter((d) => d.kind === "dev").map((d) => d.ports[0])).toEqual([5173, 3000, 4173, 8000]);
    s().showSection("servers");
    expect(s().activeTab).toBe("page:servers");
    await s().killServer(4103);
    expect(s().servers.data!.some((d) => d.pid === 4103)).toBe(false);
    expect(s().serverOps[4103]).toBeUndefined();
    // EMP's own daemon is refused, and the refusal is kept for the row to show.
    await s().killServer(4302);
    expect(s().serverOps[4302]).toEqual({ state: "error", error: "EMP's own daemon can't be stopped from here" });
    await until(store, (st) => st.servers.status === "ready");
    expect(s().servers.data!.some((d) => d.pid === 4302)).toBe(true);
  });

  it("Suki: opens on the latest conversation, starts a new one with the page as context, stays out of the thread list", async () => {
    const { store, server } = await setup();
    const s = () => store.getState();
    expect(s().threads.items.some((t) => t.agentId === "a-suki")).toBe(false);
    s().setSukiOpen(true);
    await until(store, (st) => st.suki.threadId === "t-suki-1" && st.suki.status === "ready");
    expect(s().suki.items.some((i) => i.kind === "toolCall" && i.name === "mcp__emp__start_thread")).toBe(true);
    // The thread it started knows who started it.
    expect(server.fixture.threads.find((t) => t.assignedBy === "t-suki-1")).toBeTruthy();

    s().selectSukiChat(null);
    s().showSection("servers");
    expect(pageContext(s())).toBe("the Dev servers page");
    expect(await s().sendToSuki("what can I stop?")).toBe(true);
    const id = s().suki.threadId!;
    expect(id).not.toBe("t-suki-1");
    await until(store, (st) => st.live[id]?.status === "idle" || (!st.live[id] && st.suki.status === "ready"));
    const sent = server.fixture.transcripts[id]![0]!;
    expect(sent.kind === "user" && sent.text).toBe("what can I stop?\n\n<emp-context>\nOwner is viewing: the Dev servers page\n</emp-context>");
    await until(store, (st) => st.sukiChats.data?.[0]?.id === id);
    expect(s().threads.items.some((t) => t.id === id)).toBe(false);

    const thread = s().threads.items[0]!;
    await s().openThread(thread.id);
    expect(pageContext(s())).toMatch(new RegExp(`^thread ".+" \\[${thread.id}\\]`));
    s().setSukiOpen(false);
    expect(s().suki.open).toBe(false);
  });

  it("lists subagents under their parent", async () => {
    const { store } = await setup();
    const s = () => store.getState();
    const parent = s().threads.items.find((t) => t.childCount > 0)!;
    expect(parent.childCount).toBe(3);
    expect(s().threads.items.some((t) => t.parentId)).toBe(false);
    s().toggleChildren(parent.id);
    await until(store, (st) => st.children[parent.id]?.status === "ready");
    expect(s().children[parent.id]!.data!.map((t) => t.title)).toEqual(["Subagent: Pasteur", "Subagent: Descartes", "Subagent: Laplace"]);
    s().toggleChildren(parent.id);
    expect(s().expandedThreads).toEqual([]);
  });

  it("loads the open thread's context usage and clears it", async () => {
    const { store, client } = await setup();
    const s = () => store.getState();
    await until(store, (st) => st.threads.status === "ready");
    let withItems: string | null = null;
    for (const t of s().threads.items) {
      if ((await client.request("threads.transcript", { threadId: t.id })).total > 0) {
        withItems = t.id;
        break;
      }
    }
    expect(withItems).not.toBeNull();
    await s().loadContext(withItems);
    const c = s().context;
    expect(c.threadId).toBe(withItems);
    expect(c.data!.used).toBeGreaterThan(0);
    expect(c.data!.window).toBeGreaterThanOrEqual(c.data!.used);
    await s().loadContext(null);
    expect(s().context).toEqual({ threadId: null, data: null });
  });

  it("groups threads: create, move, pin, hide from the main list, search, delete, collapse", async () => {
    const { store, client } = await setup();
    const s = () => store.getState();
    await s().loadGroups();
    await until(store, (st) => st.threads.status === "ready");
    const [t1, t2] = s().threads.items;
    const before = s().threads.total;
    expect(await s().createGroup("Survivor games", { threadIds: [t1!.id] })).toBeNull();
    expect(await s().createGroup("   ")).toMatch(/name/);
    const g = s().groups.data![0]!;
    expect(g.threads.map((t) => t.id)).toEqual([t1!.id]);
    await until(store, (st) => st.threads.total === before - 1 && !st.threads.items.some((t) => t.id === t1!.id));

    // Moving a thread opens its group; pinning sorts it first.
    expect(await s().createGroup("Later")).toBeNull();
    const later = s().groups.data!.find((x) => x.name === "Later")!;
    expect(await s().addToGroup(later.id, [t1!.id, t2!.id])).toBeNull();
    expect(s().settings.expandedGroups).toContain(later.id);
    expect(s().groups.data!.find((x) => x.id === g.id)!.threads).toHaveLength(0);
    expect(await s().setGroupPinned(later.id, true)).toBeNull();
    expect(s().groups.data![0]!.id).toBe(later.id);

    // Searching shows grouped threads in the main list again.
    s().setQuery(t1!.title.slice(0, 6));
    await until(store, (st) => st.threads.status === "ready" && st.threads.items.some((t) => t.id === t1!.id));
    s().setQuery("");

    // Another window's change arrives as groups.changed.
    await client.request("groups.update", { groupId: later.id, name: "Pinned work" });
    await until(store, (st) => st.groups.data?.[0]?.name === "Pinned work");

    expect(await s().removeFromGroup(t2!.id)).toBeNull();
    expect(await s().deleteGroup(later.id)).toBeNull();
    await until(store, (st) => st.threads.status === "ready" && st.threads.total === before);
    s().toggleGroup(g.id);
    expect(s().settings.expandedGroups).toContain(g.id);
    s().toggleGroup(g.id);
    expect(s().settings.expandedGroups).not.toContain(g.id);
  });

  it("runs a non-chat agent in a terminal and shows the Terminals page", async () => {
    const { store } = await setup();
    await store.getState().runInTerminal("gemini", [], "/work", "Gemini");
    const s = store.getState();
    expect(s.activeTab).toBe("page:terminals");
    expect(s.terminals[s.focusedTerminalId!]?.title).toBe("Gemini");
  });
});
