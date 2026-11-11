import { errorMessage, type ConnectionStatus, type EmpClient } from "@emp/client";
import type {
  AgentProfile,
  AgentSaveParams,
  Attachment,
  ContextUsage,
  DaemonSettings,
  DevServer,
  DoctorReport,
  LiveThread,
  ModelOption,
  ProjectFiles,
  PartialKind,
  ExternalSchedule,
  Schedule,
  SchedulePreview,
  ScheduleSaveParams,
  ScheduleWhen,
  SlashCommand,
  SendMode,
  SessionHolder,
  TerminalInfo,
  ThreadGoal,
  Harness,
  ImportReport,
  ImportSourceKind,
  Project,
  SpawnSpec,
  Thread,
  ThreadGroup,
  ThreadsListParams,
  TranscriptItem,
} from "@emp/protocol";
import { createStore, type StoreApi } from "zustand/vanilla";
import type { EmpHost } from "../host";
import type { DroppedFolder } from "../lib/attach";
import { cleanTitle } from "../lib/format";
import { isCopilotModel } from "../lib/models";
import { DEFAULT_LAYOUT, DEFAULT_SETTINGS, normalizeSettings, type Arrangement, type UiSettings } from "./settings";
import {
  THREAD_PAGE_SIZE,
  TRANSCRIPT_PAGE_SIZE,
  appendUnique,
  earlierRange,
  laterRange,
  mergePage,
  tailRange,
  type Window,
} from "../lib/paging";

/** The client surface the UI uses. `EmpClient` satisfies it; tests may pass a fake. */
export type WorkspaceClient = Pick<EmpClient, "request" | "on" | "onStateChange" | "status" | "reconnect">;

export type Section = "threads" | "import" | "agents" | "terminals" | "servers" | "schedules" | "projects" | "settings";

/** The Suki panel: one coordinator conversation at a time, over whatever page is open. */
export interface SukiState {
  open: boolean;
  /** The conversation shown; null = a new one, started by the next message. */
  threadId: string | null;
  items: TranscriptItem[];
  /** Items streamed since `items` was fetched, and the block being written. */
  tail: TranscriptItem[];
  partial: { kind: PartialKind; text: string } | null;
  status: "idle" | "loading" | "ready" | "error";
  error: string | null;
  sending: boolean;
  /** The first message of a conversation that's still being created. */
  pending: string | null;
  /** Model for new conversations; null = Suki's own. */
  model: string | null;
}

/** A kill in flight, one that outlived SIGTERM (`stuck`), or its error. */
export type ServerOp = { state: "stopping" | "stuck" } | { state: "error"; error: string };

/** Open tabs, Hermes-style: the new-thread page, threads (`t:<id>`) and pages (`page:<section>`). */
export type TabId = "new" | `t:${string}` | `page:${Exclude<Section, "threads">}`;

export type SidebarTab = "threads" | "agents";
export type LoadStatus = "idle" | "loading" | "ready" | "error";

export const HARNESS_FILTERS = ["pi", "hermes", "claude", "codex", "antigravity"] as const satisfies readonly Harness[];
export type HarnessFilter = (typeof HARNESS_FILTERS)[number];

export interface Resource<T> {
  status: LoadStatus;
  data: T | null;
  error: string | null;
}

export interface ThreadFilter {
  projectId: string | null;
  harness: HarnessFilter | null;
  query: string;
  /** Show machine-started runs (e.g. tools calling `codex exec`); hidden by default. */
  includeAutomated: boolean;
}

export interface ThreadListState {
  items: Thread[];
  total: number;
  status: LoadStatus;
  error: string | null;
  loadingMore: boolean;
}

export interface TranscriptState extends Window<TranscriptItem> {
  threadId: string | null;
  status: LoadStatus;
  error: string | null;
  loadingEarlier: boolean;
  loadingLater: boolean;
}

export interface ResumeState {
  threadId: string | null;
  status: LoadStatus;
  spec: SpawnSpec | null;
  error: string | null;
}

export interface ImportLine {
  id: number;
  source: ImportSourceKind;
  message: string;
  at: number;
}

export interface ImportRunState {
  running: boolean;
  lines: ImportLine[];
  report: ImportReport | null;
  error: string | null;
}

/** Live additions to the open thread since its transcript was last fetched. */
export interface LiveView {
  threadId: string | null;
  tail: TranscriptItem[];
  partial: { kind: PartialKind; text: string } | null;
}

export interface ComposerState {
  sending: boolean;
  error: string | null;
}

export interface NewThreadState {
  open: boolean;
  sending: boolean;
  error: string | null;
}

export interface AgentEditState {
  /** Draft being edited; `id` absent for a new agent. */
  draft: AgentSaveParams | null;
  saving: boolean;
  error: string | null;
}

/** Default size for terminals opened before the view has measured itself. */
export const DEFAULT_TERMINAL_SIZE = { cols: 120, rows: 36 } as const;
/** About what an in-chat card shows, so the first resize after attaching is small. */
export const COMMAND_CARD_SIZE = { cols: 100, rows: 22 } as const;

export interface WorkspaceState {
  connection: ConnectionStatus;
  section: Section;
  filter: ThreadFilter;
  selectedThreadId: string | null;
  projects: Resource<Project[]>;
  agents: Resource<AgentProfile[]>;
  /** Daemon-wide settings (compaction limit), shared with every window and phone. */
  daemonSettings: Resource<DaemonSettings>;
  /** The owner's thread groups (pinned first), shared with every window and phone. */
  groups: Resource<ThreadGroup[]>;
  /** How full the open thread's context window is (status bar). */
  context: { threadId: string | null; data: ContextUsage | null };
  doctor: Resource<DoctorReport>;
  /** Listening processes; refreshed on connect and polled while the Dev servers page is open. */
  servers: Resource<DevServer[]>;
  /** Scheduled tasks, next run first; the daemon pushes every change. */
  schedules: Resource<Schedule[]>;
  /** The schedule being written or edited (Scheduled page), with the last save error. */
  scheduleEditor: { draft: ScheduleSaveParams; error: string | null; saving: boolean } | null;
  /** Jobs other tools schedule themselves (Hermes cron), read when the page opens. */
  externalSchedules: Resource<ExternalSchedule[]>;
  serverOps: Record<number, ServerOp>;
  suki: SukiState;
  /** Suki's conversations, newest first. */
  sukiChats: Resource<Thread[]>;
  threads: ThreadListState;
  thread: Resource<Thread>;
  transcript: TranscriptState;
  resume: ResumeState;
  importRun: ImportRunState;
  /** Sessions EMP holds (driver or takeover terminal), by thread id. */
  live: Record<string, LiveThread>;
  liveView: LiveView;
  composer: ComposerState;
  /** Models the owner switched chats to in their composers, by thread id; sent with each
   * message (the daemon keeps the pick, so it survives a reload as the thread's model). */
  threadModels: Record<string, string>;
  terminals: Record<string, TerminalInfo>;
  /** Terminal shown in the thread's terminal pane (side shell or takeover), if any. */
  paneTerminalId: string | null;
  /** In-chat terminal cards for commands that need a harness's own UI: scope (a thread id,
   *  `new` or `suki`) → terminal id. */
  commandCards: Record<string, string>;
  /** Codex goals of threads that aren't live (`threads.goal`); live ones use `live[id].goal`. */
  goals: Record<string, ThreadGoal | null>;
  /** Processes outside EMP with the thread's session open (`threads.holders`). */
  holders: Record<string, SessionHolder[]>;
  /** Terminal shown full-size in the Terminals section. */
  focusedTerminalId: string | null;
  newThread: NewThreadState;
  agentEdit: AgentEditState;
  tabs: TabId[];
  activeTab: TabId;
  /** Titles for thread tabs, filled as threads are opened/loaded. */
  tabTitles: Record<string, string>;
  sidebarTab: SidebarTab;
  /** Thread details panel (metadata + resume command) is open. */
  detailsOpen: boolean;
  /** Agent picked in the new-thread composer. */
  newThreadAgentId: string | null;
  /** Model override picked in the new-thread composer; null = the agent's own model. */
  newThreadModel: string | null;
  /** Project picked in the composer: undefined follows the sidebar, null means no project. */
  newThreadProjectId: string | null | undefined;
  /** Model catalogues by `harness:profile`. */
  models: Record<string, Resource<ModelOption[]>>;
  /** Slash commands per `harness:agentId`. */
  commands: Record<string, Resource<SlashCommand[]>>;
  /** Files attached to a message being written, per composer (`new` or a thread id). */
  drafts: Record<string, DraftFile[]>;
  /** Subagent threads under each expanded parent. */
  children: Record<string, Resource<Thread[]>>;
  expandedThreads: string[];
  projectCreate: { saving: boolean; error: string | null };
  /** Expanded rows on the Projects page: recent threads and the folder's top level. */
  projectThreads: Record<string, Resource<{ items: Thread[]; total: number }>>;
  /** Theme colour and layout, persisted in host.prefs. */
  settings: UiSettings;
  /** Open Settings overlay page (e.g. "appearance/theme"); null = closed. */
  settingsPage: string | null;
  /** Hermes-style layout edit mode over the workspace. */
  layoutEditing: boolean;
  /** Where the Layouts card was dragged to (this window only); null = centred. */
  layoutCardPos: { x: number; y: number } | null;
  /** Narrow (phone) layout: the sidebar is a drawer; this says whether it's out. */
  drawerOpen: boolean;
  projectFiles: Record<string, Resource<ProjectFiles>>;
  /** Bumped to ask the search box to take focus (⌘K). */
  searchFocusTick: number;
  /** Coarse clock (1 min) so relative times refresh. */
  now: number;
}

export interface WorkspaceActions {
  /** Subscribes to the client and loads data. Returns the matching teardown. Re-entrant. */
  start(): () => void;
  showSection(section: Section): void;
  selectProject(projectId: string | null): void;
  setHarness(harness: HarnessFilter | null): void;
  setQuery(query: string): void;
  setIncludeAutomated(include: boolean): void;
  focusSearch(): void;
  loadProjects(): Promise<void>;
  loadAgents(): Promise<void>;
  loadDaemonSettings(): Promise<void>;
  loadGroups(): Promise<void>;
  /** Each group action resolves to an error message, or null when it worked. */
  createGroup(name: string, opts?: { threadIds?: string[]; pinned?: boolean }): Promise<string | null>;
  renameGroup(groupId: string, name: string): Promise<string | null>;
  setGroupPinned(groupId: string, pinned: boolean): Promise<string | null>;
  deleteGroup(groupId: string): Promise<string | null>;
  addToGroup(groupId: string, threadIds: string[]): Promise<string | null>;
  removeFromGroup(threadId: string): Promise<string | null>;
  /** Expands or collapses a group in the sidebar (saved per device). */
  toggleGroup(groupId: string): void;
  /** Saves daemon-wide settings; resolves to an error message, or null when saved. */
  saveDaemonSettings(patch: Partial<DaemonSettings>): Promise<string | null>;
  loadDoctor(): Promise<void>;
  loadThreads(): Promise<void>;
  loadMoreThreads(): Promise<void>;
  openThread(threadId: string): Promise<void>;
  loadEarlier(): Promise<void>;
  loadLater(): Promise<void>;
  fetchResume(): Promise<void>;
  runImport(): Promise<void>;
  reconnect(): void;
  // live sessions
  loadLive(): Promise<void>;
  /** Sends to the open thread. While a turn runs, `prompt` queues as a follow-up. */
  send(text: string, mode?: SendMode, force?: boolean, attachments?: string[]): Promise<boolean>;
  /** Switches a chat's model from its next message on. */
  setThreadModel(threadId: string, model: string): void;
  interrupt(): Promise<void>;
  takeover(force?: boolean): Promise<void>;
  release(): Promise<void>;
  startThread(
    agentId: string,
    text: string,
    cwd: string | null,
    opts?: { model?: string | null; projectId?: string | null; attachments?: string[] },
  ): Promise<boolean>;
  /** Uploads prepared files into a composer's draft. */
  attachFiles(draft: string, files: PreparedFile[]): Promise<void>;
  removeDraftFile(draft: string, key: string): void;
  /** Adds dropped folders; empd finds where they are. */
  attachFolders(draft: string, folders: DroppedFolder[], use: DraftFolder["use"]): Promise<void>;
  setDraftFolder(draft: string, key: string, patch: Partial<Pick<DraftFolder, "path" | "use">>): void;
  /** Opens New thread working in the folder of `key` (taken out of `draft`). */
  newThreadInFolder(draft: string, key: string, agentId: string | null): void;
  clearDraft(draft: string): void;
  loadCommands(harness: Harness, agentId: string | null): Promise<void>;
  /** Runs a slash command in the harness's terminal UI: the open thread's session, or a fresh one of `agentId`. */
  runCommand(text: string, target: { threadId: string } | { agentId: string }, scope?: string): Promise<boolean>;
  /** Closes an in-chat terminal card: returns a taken-over session to chat, else kills it. */
  closeCommandCard(scope: string): Promise<void>;
  /** A `native` slash command, run by empd in the chat (`threads.command`). */
  runNative(threadId: string, text: string): Promise<boolean>;
  loadGoal(threadId: string): Promise<void>;
  /** Attaches to a Codex thread Codex is working on without EMP; true if it did. */
  watch(threadId: string): Promise<boolean>;
  loadHolders(threadId: string): Promise<void>;
  /** Closes the session's other window (the owner asked); true when none is left. */
  closeElsewhere(threadId: string): Promise<boolean>;
  answerQuestion(threadId: string, questionId: string, answer: { value?: string; confirmed?: boolean; cancelled?: boolean }): Promise<void>;
  toggleChildren(threadId: string): void;
  setNewThreadOpen(open: boolean): void;
  // terminals
  loadTerminals(): Promise<void>;
  /** A shell in the open thread's directory (`forThread`), or a standalone one in home. */
  openShell(forThread?: boolean): Promise<void>;
  setPaneTerminal(terminalId: string | null): void;
  focusTerminal(terminalId: string | null): void;
  killTerminal(terminalId: string): Promise<void>;
  // Suki
  setSukiOpen(open: boolean): void;
  loadSukiChats(): Promise<void>;
  selectSukiChat(threadId: string | null): void;
  setSukiModel(model: string | null): void;
  /** Sends to the shown conversation (or starts one), with the open page as context. */
  /** `attachments` are uploaded ids (the `SUKI_DRAFT` files). */
  sendToSuki(text: string, attachments?: string[]): Promise<boolean>;
  interruptSuki(): Promise<void>;
  // scheduled tasks
  loadSchedules(): Promise<void>;
  loadExternalSchedules(): Promise<void>;
  /** Opens the editor on the Scheduled page: a new schedule (`draft` without id) or an edit. */
  editSchedule(draft: ScheduleSaveParams | null): void;
  /** Saves the editor's draft; closes it on success. */
  saveSchedule(draft: ScheduleSaveParams): Promise<boolean>;
  deleteSchedule(scheduleId: string): Promise<void>;
  enableSchedule(scheduleId: string, enabled: boolean): Promise<void>;
  runSchedule(scheduleId: string): Promise<void>;
  /** The timetable in words and its next runs; null when the daemon can't be asked. */
  previewSchedule(when: ScheduleWhen): Promise<SchedulePreview | null>;
  // dev servers
  loadServers(): Promise<void>;
  /** SIGTERM (with `force`, SIGKILL); a stopped server leaves the list. */
  killServer(pid: number, force?: boolean): Promise<void>;
  // tabs
  activateTab(tab: TabId): void;
  closeTab(tab: TabId): void;
  setSidebarTab(tab: SidebarTab): void;
  setNewThreadAgent(agentId: string | null): void;
  setNewThreadModel(model: string | null): void;
  setNewThreadError(error: string | null): void;
  setComposerError(error: string | null): void;
  setNewThreadProject(projectId: string | null): void;
  loadModels(harness: Harness, profile?: string | null, refresh?: boolean): Promise<void>;
  /** Re-reads the thread's context usage; `null` clears it. */
  loadContext(threadId: string | null): Promise<void>;
  createProject(path: string, name: string | null, createFolder: boolean): Promise<boolean>;
  /** Loads (or refreshes) one project's threads and files. */
  loadProjectDetail(projectId: string): Promise<void>;
  /** Applies a settings change; `persist: false` for live drags (save on release). */
  updateSettings(patch: Partial<UiSettings>, opts?: { persist?: boolean }): void;
  resetLayout(): void;
  openSettings(page?: string): void;
  closeSettings(): void;
  setLayoutEditing(on: boolean): void;
  setLayoutCardPos(pos: { x: number; y: number }): void;
  setDrawerOpen(open: boolean): void;
  applyArrangement(arrangement: Arrangement): void;
  saveLayout(name: string): void;
  deleteLayout(id: string): void;
  /** Opens a terminal running `program` (agents EMP can't chat with yet). */
  runInTerminal(program: string, args: string[], cwd: string | null, title: string): Promise<void>;
  toggleDetails(): void;
  // agent editor
  editAgent(draft: AgentSaveParams | null): void;
  saveAgent(draft: AgentSaveParams): Promise<boolean>;
  deleteAgent(agentId: string): Promise<void>;
}

export type Workspace = WorkspaceState & WorkspaceActions;
export type WorkspaceStore = StoreApi<Workspace>;

const MAX_IMPORT_LINES = 500;
const PREF_HARNESS = "threads.harness";
const PREF_SETTINGS = "ui.settings";

/** Threads shown in an expanded Projects row; the rest are one click away in the sidebar. */
const PROJECT_THREADS = 12;

const idle = <T>(): Resource<T> => ({ status: "idle", data: null, error: null });
const loading = <T>(prev: Resource<T>): Resource<T> => ({ ...prev, status: "loading", error: null });

function emptyTranscript(threadId: string | null): TranscriptState {
  return { threadId, items: [], start: 0, total: 0, status: threadId ? "loading" : "idle", error: null, loadingEarlier: false, loadingLater: false };
}

export function initialWorkspaceState(connection: ConnectionStatus, now = Date.now()): WorkspaceState {
  return {
    connection,
    section: "threads",
    filter: { projectId: null, harness: null, query: "", includeAutomated: false },
    selectedThreadId: null,
    projects: idle(),
    agents: idle(),
    daemonSettings: idle(),
    groups: idle(),
    schedules: idle(),
    scheduleEditor: null,
    externalSchedules: idle(),
    context: { threadId: null, data: null },
    doctor: idle(),
    servers: idle(),
    serverOps: {},
    suki: { open: false, threadId: null, items: [], tail: [], partial: null, status: "idle", error: null, sending: false, pending: null, model: null },
    sukiChats: idle(),
    threads: { items: [], total: 0, status: "idle", error: null, loadingMore: false },
    thread: idle(),
    transcript: emptyTranscript(null),
    resume: { threadId: null, status: "idle", spec: null, error: null },
    importRun: { running: false, lines: [], report: null, error: null },
    live: {},
    liveView: { threadId: null, tail: [], partial: null },
    composer: { sending: false, error: null },
    threadModels: {},
    terminals: {},
    paneTerminalId: null,
    commandCards: {},
    goals: {},
    holders: {},
    focusedTerminalId: null,
    newThread: { open: false, sending: false, error: null },
    agentEdit: { draft: null, saving: false, error: null },
    tabs: ["new"],
    activeTab: "new",
    tabTitles: {},
    sidebarTab: "threads",
    detailsOpen: false,
    newThreadAgentId: null,
    newThreadModel: null,
    newThreadProjectId: undefined,
    models: {},
    commands: {},
    drafts: {},
    children: {},
    expandedThreads: [],
    projectCreate: { saving: false, error: null },
    projectThreads: {},
    settings: DEFAULT_SETTINGS,
    settingsPage: null,
    layoutEditing: false,
    layoutCardPos: null,
    drawerOpen: false,
    projectFiles: {},
    searchFocusTick: 0,
    now,
  };
}

/** Groups show (and their threads leave the main list) only when nothing narrows the list. */
export function showsGroups(filter: ThreadFilter): boolean {
  return !filter.projectId && !filter.harness && !filter.query.trim();
}

export function threadListParams(filter: ThreadFilter, offset: number): ThreadsListParams {
  // Suki's own conversations live in its panel.
  const params: ThreadsListParams = { limit: THREAD_PAGE_SIZE, offset, coordinator: false };
  if (showsGroups(filter)) params.ungrouped = true;
  if (filter.projectId) params.projectId = filter.projectId;
  if (filter.harness) params.harness = filter.harness;
  const query = filter.query.trim();
  if (query) params.query = query;
  // Subagents sit under their parent, except in search results.
  else params.topLevel = true;
  if (filter.includeAutomated) params.includeAutomated = true;
  return params;
}

export function coordinatorAgent(state: WorkspaceState): AgentProfile | null {
  return state.agents.data?.find((a) => a.coordinator) ?? null;
}

/** What the owner is looking at, for Suki ("this thread", "this project"). */
export function pageContext(state: WorkspaceState): string {
  const tab = state.activeTab;
  if (tab.startsWith("t:")) {
    const id = tab.slice(2);
    const t = state.thread.data?.id === id ? state.thread.data : (state.threads.items.find((x) => x.id === id) ?? null);
    if (!t) return `thread ${id}`;
    const agent = state.agents.data?.find((a) => a.id === t.agentId)?.name;
    const status = state.live[t.id]?.status ?? t.status;
    return `thread "${cleanTitle(t.title)}" [${t.id}] (${[agent, t.harness, status, t.cwd].filter(Boolean).join(", ")})`;
  }
  if (tab === "new") return "the New thread page";
  const page = tab.slice(5);
  const project = selectedProject(state);
  if (page === "projects" && project) return `project "${project.name}" (${project.rootPath})`;
  const names: Record<string, string> = { agents: "Agents", terminals: "Terminals", servers: "Dev servers", schedules: "Scheduled", import: "Sources", projects: "Projects" };
  return `the ${names[page] ?? page} page`;
}

export function hasMoreThreads(list: ThreadListState): boolean {
  return list.items.length < list.total;
}

export function selectedProject(state: WorkspaceState): Project | null {
  const id = state.filter.projectId;
  return id ? (state.projects.data?.find((p) => p.id === id) ?? null) : null;
}

export function newThreadProject(state: WorkspaceState): Project | null {
  const id = state.newThreadProjectId === undefined ? state.filter.projectId : state.newThreadProjectId;
  return state.projects.data?.find((p) => p.id === id) ?? null;
}

export function projectName(state: WorkspaceState, projectId: string | null): string | null {
  if (!projectId) return null;
  return state.projects.data?.find((p) => p.id === projectId)?.name ?? null;
}

export function appendImportLine(lines: ImportLine[], line: ImportLine): ImportLine[] {
  const next = lines.length >= MAX_IMPORT_LINES ? lines.slice(lines.length - MAX_IMPORT_LINES + 1) : lines.slice();
  next.push(line);
  return next;
}

export function summarizeImport(report: ImportReport): string {
  let added = 0;
  let updated = 0;
  let failed = 0;
  for (const s of report.sources) {
    added += s.stats.threadsAdded;
    updated += s.stats.threadsUpdated;
    if (s.error) failed += 1;
  }
  const parts = [`${added} new`, `${updated} updated`];
  if (failed) parts.push(`${failed} source${failed === 1 ? "" : "s"} failed`);
  return parts.join(", ");
}

export function createWorkspaceStore(client: WorkspaceClient, host?: Pick<EmpHost, "prefs" | "notify">): WorkspaceStore {
  /** Settings reopens on the page it was closed on. */
  let lastSettingsPage = "appearance/theme";
  /** Newest assistant text per thread, for notification bodies. */
  const lastReply = new Map<string, string>();

  /** Notify when a turn ends (or needs input / fails) and the reader isn't looking at that thread. */
  let getState: () => Workspace = () => {
    throw new Error("store not ready");
  };
  const notifyTurn = (prev: LiveThread | undefined, next: LiveThread) => {
    const st = getState();
    if (!host || !st.settings.notifications || prev?.status !== "running" || next.status === "running") return;
    const looking = typeof document !== "undefined" && document.visibilityState === "visible" && document.hasFocus();
    if (looking && st.selectedThreadId === next.threadId && st.activeTab === `t:${next.threadId}`) return;
    const known =
      st.threads.items.find((t) => t.id === next.threadId) ??
      st.groups.data?.flatMap((g) => g.threads).find((t) => t.id === next.threadId);
    const title = cleanTitle(st.tabTitles[next.threadId] ?? known?.title ?? "Thread");
    const reply = (lastReply.get(next.threadId) ?? "").replace(/\s+/g, " ").trim();
    const body =
      next.status === "failed"
        ? `Failed: ${next.error ?? "the turn stopped with an error"}`
        : next.status === "needs-input"
          ? "Needs your input"
          : reply
            ? reply.length > 140
              ? `${reply.slice(0, 139)}…`
              : reply
            : "Finished";
    host.notify(title, body, () => void getState().openThread(next.threadId));
  };
  // Monotonic request tokens: a response is applied only if it is still the latest of its kind.
  let threadsSeq = 0;
  let threadSeq = 0;
  let resumeSeq = 0;
  let lineId = 0;
  let importFinishedSeen = false;

  return createStore<Workspace>()((set, get) => {
    getState = get;
    /** Applies a group change's result (the full list) and refreshes the main list. */
    const groupOp = async (req: Promise<ThreadGroup[]>): Promise<string | null> => {
      try {
        const data = await req;
        set({ groups: { status: "ready", data, error: null } });
        void get().loadThreads();
        return null;
      } catch (err) {
        return errorMessage(err);
      }
    };

    /** A schedule change: the daemon pushes the new list; a refusal shows on the page. */
    const scheduleOp = async (req: Promise<unknown>) => {
      try {
        await req;
        set((s) => ({ schedules: { ...s.schedules, error: null } }));
      } catch (err) {
        set((s) => ({ schedules: { ...s.schedules, error: errorMessage(err) } }));
      }
    };

    const refreshAll = () => {
      void get().loadProjects();
      void get().loadGroups();
      void get().loadAgents();
      void get().loadThreads();
      void get().loadLive();
      void get().loadTerminals();
      void get().loadServers();
      void get().loadSchedules();
      void get().loadSukiChats();
      if (get().section === "import" || get().doctor.status !== "idle") void get().loadDoctor();
      const id = get().selectedThreadId;
      if (id) {
        const seq = threadSeq;
        client.request("threads.get", { threadId: id }).then(
          (data) => {
            if (seq === threadSeq) set({ thread: { status: "ready", data, error: null } });
          },
          () => {},
        );
      }
    };

    /** Adds `tab` (after the active one) if missing and makes it active. */
    // Any navigation also puts the phone drawer away.
    const showTab = (tab: TabId) =>
      set((s) => {
        if (s.tabs.includes(tab)) return { activeTab: tab, drawerOpen: false };
        const at = s.tabs.indexOf(s.activeTab);
        const tabs = s.tabs.slice();
        tabs.splice(at < 0 ? tabs.length : at + 1, 0, tab);
        return { tabs, activeTab: tab, drawerOpen: false };
      });

    // Re-read the open thread's newest transcript page; live items it now contains are dropped.
    const reloadTranscript = async (threadId: string) => {
      const seq = threadSeq;
      try {
        const head = await client.request("threads.transcript", { threadId, offset: 0, limit: 1 });
        const page = await client.request("threads.transcript", { threadId, ...tailRange(head.total) });
        if (seq !== threadSeq) return;
        set((s) => ({
          transcript: { ...emptyTranscript(threadId), items: page.items, start: page.offset, total: page.total, status: "ready" },
          liveView: { threadId, tail: [], partial: s.liveView.partial },
        }));
        const t = await client.request("threads.get", { threadId });
        if (seq === threadSeq) set({ thread: { status: "ready", data: t, error: null } });
      } catch {
        // Keep what's on screen; the next change retries.
      }
    };

    /** Suki's transcript: the newest page, with the streamed tail dropped once it's on disk. */
    let sukiSeq = 0;
    let sukiOpened = false;
    const loadSuki = async (threadId: string, quiet = false) => {
      const seq = ++sukiSeq;
      if (!quiet) set((s) => ({ suki: { ...s.suki, status: "loading", error: null } }));
      try {
        const head = await client.request("threads.transcript", { threadId, offset: 0, limit: 1 });
        const page = await client.request("threads.transcript", { threadId, ...tailRange(head.total) });
        if (seq !== sukiSeq || get().suki.threadId !== threadId) return;
        set((s) => ({ suki: { ...s.suki, items: page.items, tail: [], status: "ready", error: null, pending: null } }));
      } catch (err) {
        if (seq === sukiSeq && !quiet) set((s) => ({ suki: { ...s.suki, status: "error", error: errorMessage(err) } }));
      }
    };

    let listTimer: ReturnType<typeof setTimeout> | null = null;
    const loadChildren = async (parentId: string) => {
      set((s) => ({ children: { ...s.children, [parentId]: loading(s.children[parentId] ?? idle()) } }));
      try {
        const page = await client.request("threads.list", { parentId, limit: 200 });
        set((s) => ({ children: { ...s.children, [parentId]: { status: "ready", data: page.items, error: null } } }));
      } catch (err) {
        set((s) => ({ children: { ...s.children, [parentId]: { status: "error", data: null, error: errorMessage(err) } } }));
      }
    };

    const scheduleListRefresh = () => {
      if (listTimer) return;
      listTimer = setTimeout(() => {
        listTimer = null;
        void get().loadThreads();
        void get().loadProjects();
        void get().loadGroups();
      }, 1500);
    };

    const afterImport = (report: ImportReport) => {
      set((s) => ({ importRun: { ...s.importRun, report } }));
      refreshAll();
    };

    return {
      ...initialWorkspaceState(client.status),

      start() {
        const offs: Array<() => void> = [];
        let wasReady = false;
        const onStatus = (connection: ConnectionStatus) => {
          set({ connection });
          const ready = connection.state === "ready";
          if (ready && !wasReady) refreshAll();
          wasReady = ready;
        };
        offs.push(client.onStateChange(onStatus));
        onStatus(client.status);

        offs.push(
          client.on("import.progress", (p) => {
            set((s) => ({
              importRun: {
                ...s.importRun,
                lines: appendImportLine(s.importRun.lines, { id: ++lineId, source: p.source, message: p.message, at: Date.now() }),
              },
            }));
          }),
          client.on("import.finished", (report) => {
            importFinishedSeen = true;
            afterImport(report);
          }),
          client.on("thread.event", (ev) => {
            switch (ev.type) {
              case "state": {
                const live = ev.live;
                const before = get().live[live.threadId];
                notifyTurn(before, live);
                if (live.threadId === get().context.threadId && before?.status !== live.status) void get().loadContext(live.threadId);
                set((s) => {
                  const next = { ...s.live };
                  if (live.owner === "none" && live.status !== "failed") delete next[live.threadId];
                  else next[live.threadId] = live;
                  const selected = s.selectedThreadId === live.threadId;
                  // Takeover opened a terminal for the open thread: show it.
                  const inCard = !!live.terminalId && Object.values(s.commandCards).includes(live.terminalId);
                  const pane = selected && !inCard && live.owner === "terminal" && live.terminalId ? live.terminalId : s.paneTerminalId;
                  return { live: next, paneTerminalId: pane };
                });
                break;
              }
              case "item":
                if (ev.item.kind === "assistant" && ev.item.text.trim()) lastReply.set(ev.threadId, ev.item.text);
                if (ev.threadId === get().suki.threadId) {
                  set((s) => ({ suki: { ...s.suki, tail: [...s.suki.tail, ev.item], partial: null } }));
                }
                if (ev.threadId === get().selectedThreadId) {
                  set((s) => ({ liveView: { ...s.liveView, tail: [...s.liveView.tail, ev.item], partial: null } }));
                }
                break;
              case "partial":
                if (ev.threadId === get().suki.threadId) {
                  set((s) => ({ suki: { ...s.suki, partial: ev.text ? { kind: ev.kind, text: ev.text } : null } }));
                }
                if (ev.threadId === get().selectedThreadId) {
                  set((s) => ({ liveView: { ...s.liveView, partial: ev.text ? { kind: ev.kind, text: ev.text } : null } }));
                }
                break;
              case "transcriptChanged":
                if (ev.threadId === get().selectedThreadId) void reloadTranscript(ev.threadId);
                if (ev.threadId === get().suki.threadId) void loadSuki(ev.threadId, true);
                if (get().sukiChats.status !== "idle" && get().sukiChats.data?.some((t) => t.id === ev.threadId)) void get().loadSukiChats();
                if (ev.threadId === get().context.threadId) void get().loadContext(ev.threadId);
                scheduleListRefresh();
                break;
            }
          }),
          client.on("terminal.state", (info) => {
            set((s) => ({ terminals: { ...s.terminals, [info.id]: info } }));
          }),
          client.on("settings.changed", (data) => {
            set({ daemonSettings: { status: "ready", data, error: null } });
          }),
          client.on("ui.openThread", ({ threadId }) => {
            void get().openThread(threadId);
          }),
          client.on("schedules.changed", (data) => {
            set({ schedules: { status: "ready", data, error: null } });
          }),
          client.on("groups.changed", (data) => {
            set({ groups: { status: "ready", data, error: null } });
            void get().loadThreads();
          }),
        );

        const clock = setInterval(() => set({ now: Date.now() }), 60_000);
        offs.push(() => clearInterval(clock));
        offs.push(() => {
          if (listTimer) clearTimeout(listTimer);
          listTimer = null;
        });

        let alive = true;
        offs.push(() => {
          alive = false;
        });
        host?.prefs.get(PREF_SETTINGS).then(
          (v) => {
            if (alive && v !== undefined) set({ settings: normalizeSettings(v) });
          },
          () => {},
        );
        host?.prefs.get(PREF_HARNESS).then(
          (v) => {
            if (alive && typeof v === "string" && (HARNESS_FILTERS as readonly string[]).includes(v) && get().filter.harness !== v) {
              get().setHarness(v as HarnessFilter);
            }
          },
          () => {},
        );

        return () => {
          for (const off of offs.splice(0)) off();
        };
      },

      showSection(section) {
        if (section === "settings") {
          get().openSettings();
          return;
        }
        set({ section });
        if (section !== "threads") showTab(`page:${section}`);
        if (section === "import" && get().doctor.status !== "loading") void get().loadDoctor();
        if (section === "agents" && get().agents.status === "idle") void get().loadAgents();
        if (section === "servers") void get().loadServers();
        if (section === "schedules") {
          if (get().schedules.status === "idle") void get().loadSchedules();
          void get().loadExternalSchedules();
        }
      },

      selectProject(projectId) {
        set((s) => ({ section: "threads", filter: { ...s.filter, projectId } }));
        void get().loadThreads();
      },

      setHarness(harness) {
        set((s) => ({ filter: { ...s.filter, harness } }));
        host?.prefs.set(PREF_HARNESS, harness).catch(() => {});
        void get().loadThreads();
      },

      setQuery(query) {
        if (query === get().filter.query) return;
        set((s) => ({ filter: { ...s.filter, query } }));
        void get().loadThreads();
      },

      setIncludeAutomated(includeAutomated) {
        if (includeAutomated === get().filter.includeAutomated) return;
        set((s) => ({ filter: { ...s.filter, includeAutomated } }));
        void get().loadThreads();
      },

      focusSearch() {
        set((s) => ({ section: "threads", searchFocusTick: s.searchFocusTick + 1 }));
      },

      async loadProjects() {
        set((s) => ({ projects: loading(s.projects) }));
        try {
          const data = await client.request("projects.list");
          set({ projects: { status: "ready", data, error: null } });
        } catch (err) {
          set((s) => ({ projects: { ...s.projects, status: "error", error: errorMessage(err) } }));
        }
      },

      async loadGroups() {
        set((s) => ({ groups: loading(s.groups) }));
        try {
          const data = await client.request("groups.list", undefined);
          set({ groups: { status: "ready", data, error: null } });
        } catch (err) {
          set((s) => ({ groups: { ...s.groups, status: "error", error: errorMessage(err) } }));
        }
      },

      createGroup(name, opts = {}) {
        return groupOp(client.request("groups.create", { name, pinned: opts.pinned ?? false, threadIds: opts.threadIds ?? [] }));
      },

      renameGroup(groupId, name) {
        return groupOp(client.request("groups.update", { groupId, name }));
      },

      setGroupPinned(groupId, pinned) {
        return groupOp(client.request("groups.update", { groupId, pinned }));
      },

      deleteGroup(groupId) {
        return groupOp(client.request("groups.delete", { groupId }));
      },

      addToGroup(groupId, threadIds) {
        // Open the group so the moved thread stays in view.
        if (!get().settings.expandedGroups.includes(groupId)) get().toggleGroup(groupId);
        return groupOp(client.request("groups.add", { groupId, threadIds }));
      },

      removeFromGroup(threadId) {
        return groupOp(client.request("groups.remove", { threadId }));
      },

      toggleGroup(groupId) {
        const open = get().settings.expandedGroups;
        get().updateSettings({ expandedGroups: open.includes(groupId) ? open.filter((g) => g !== groupId) : [...open, groupId] });
      },

      async loadDaemonSettings() {
        set((s) => ({ daemonSettings: loading(s.daemonSettings) }));
        try {
          const data = await client.request("settings.get", undefined);
          set({ daemonSettings: { status: "ready", data, error: null } });
        } catch (err) {
          set((s) => ({ daemonSettings: { ...s.daemonSettings, status: "error", error: errorMessage(err) } }));
        }
      },

      async saveDaemonSettings(patch) {
        const current = get().daemonSettings.data ?? { compactAt: null };
        try {
          const data = await client.request("settings.set", { ...current, ...patch });
          set({ daemonSettings: { status: "ready", data, error: null } });
          return null;
        } catch (err) {
          return errorMessage(err);
        }
      },

      async loadAgents() {
        set((s) => ({ agents: loading(s.agents) }));
        try {
          const data = await client.request("agents.list");
          set({ agents: { status: "ready", data, error: null } });
        } catch (err) {
          set((s) => ({ agents: { ...s.agents, status: "error", error: errorMessage(err) } }));
        }
      },

      setSukiOpen(open) {
        set((s) => ({ suki: { ...s.suki, open }, drawerOpen: open ? false : s.drawerOpen }));
        if (!open) return;
        if (get().agents.status === "idle") void get().loadAgents();
        // First open: continue the latest conversation if it's from the last 72 hours.
        if (!sukiOpened) {
          sukiOpened = true;
          void get()
            .loadSukiChats()
            .then(() => {
              const s = get();
              const latest = s.sukiChats.data?.[0];
              if (!s.suki.threadId && !s.suki.pending && latest && Date.now() - latest.updatedAt < 72 * 3_600_000) s.selectSukiChat(latest.id);
            });
        }
      },

      async loadSukiChats() {
        set((s) => ({ sukiChats: loading(s.sukiChats) }));
        try {
          const page = await client.request("threads.list", { coordinator: true, limit: 50 });
          set({ sukiChats: { status: "ready", data: page.items, error: null } });
        } catch (err) {
          set((s) => ({ sukiChats: { ...s.sukiChats, status: "error", error: errorMessage(err) } }));
        }
      },

      selectSukiChat(threadId) {
        sukiSeq++;
        set((s) => ({ suki: { ...s.suki, threadId, items: [], tail: [], partial: null, status: threadId ? "loading" : "idle", error: null, pending: null, model: null } }));
        if (threadId) void loadSuki(threadId);
      },

      setSukiModel(model) {
        set((s) => ({ suki: { ...s.suki, model } }));
      },

      async sendToSuki(text, attachments = []) {
        const st = get();
        const agent = coordinatorAgent(st);
        const body = text.trim();
        if ((!body && attachments.length === 0) || st.suki.sending) return false;
        const files = attachments.length ? { attachments } : {};
        if (!agent) {
          set((s) => ({ suki: { ...s.suki, error: "Suki isn't set up yet: restart empd so it creates the profile." } }));
          return false;
        }
        const context = pageContext(st);
        const threadId = st.suki.threadId;
        set((s) => ({ suki: { ...s.suki, sending: true, error: null, pending: threadId ? null : body } }));
        try {
          if (threadId) {
            await client.request("threads.send", { threadId, text: body, context, ...files, ...(st.suki.model ? { model: st.suki.model } : {}) });
            set((s) => ({ suki: { ...s.suki, sending: false } }));
          } else {
            const t = await client.request("threads.start", { agentId: agent.id, text: body, context, ...files, ...(st.suki.model ? { model: st.suki.model } : {}) });
            sukiSeq++;
            set((s) => ({ suki: { ...s.suki, threadId: t.id, sending: false } }));
            void loadSuki(t.id, true);
            void get().loadSukiChats();
          }
          return true;
        } catch (err) {
          set((s) => ({ suki: { ...s.suki, sending: false, pending: null, error: errorMessage(err) } }));
          return false;
        }
      },

      async interruptSuki() {
        const id = get().suki.threadId;
        if (!id) return;
        try {
          await client.request("threads.interrupt", { threadId: id });
        } catch (err) {
          set((s) => ({ suki: { ...s.suki, error: errorMessage(err) } }));
        }
      },

      async loadSchedules() {
        set((s) => ({ schedules: loading(s.schedules) }));
        try {
          const data = await client.request("schedules.list", undefined);
          set({ schedules: { status: "ready", data, error: null } });
        } catch (err) {
          set((s) => ({ schedules: { ...s.schedules, status: "error", error: errorMessage(err) } }));
        }
      },

      async loadExternalSchedules() {
        set((s) => ({ externalSchedules: loading(s.externalSchedules) }));
        try {
          const data = await client.request("schedules.external", undefined);
          set({ externalSchedules: { status: "ready", data, error: null } });
        } catch (err) {
          set((s) => ({ externalSchedules: { ...s.externalSchedules, status: "error", error: errorMessage(err) } }));
        }
      },

      editSchedule(draft) {
        set({ scheduleEditor: draft ? { draft, error: null, saving: false } : null });
        if (draft) get().showSection("schedules");
      },

      async saveSchedule(draft) {
        set({ scheduleEditor: { draft, error: null, saving: true } });
        try {
          await client.request("schedules.save", draft);
          set({ scheduleEditor: null });
          return true;
        } catch (err) {
          set({ scheduleEditor: { draft, error: errorMessage(err), saving: false } });
          return false;
        }
      },

      async deleteSchedule(scheduleId) {
        await scheduleOp(client.request("schedules.delete", { scheduleId }));
        if (get().scheduleEditor?.draft.id === scheduleId) set({ scheduleEditor: null });
      },

      async enableSchedule(scheduleId, enabled) {
        await scheduleOp(client.request("schedules.enable", { scheduleId, enabled }));
      },

      async runSchedule(scheduleId) {
        await scheduleOp(client.request("schedules.run", { scheduleId }));
      },

      async previewSchedule(when) {
        try {
          return await client.request("schedules.preview", { when });
        } catch {
          return null;
        }
      },

      async loadServers() {
        set((s) => ({ servers: loading(s.servers) }));
        try {
          const data = await client.request("servers.list");
          const pids = new Set(data.map((d) => d.pid));
          set((s) => ({
            servers: { status: "ready", data, error: null },
            // Ops for processes that are gone are finished.
            serverOps: Object.fromEntries(Object.entries(s.serverOps).filter(([pid]) => pids.has(Number(pid)))),
          }));
        } catch (err) {
          set((s) => ({ servers: { ...s.servers, status: "error", error: errorMessage(err) } }));
        }
      },

      async killServer(pid, force = false) {
        const op = (o: ServerOp | null) =>
          set((s) => {
            const next = { ...s.serverOps };
            if (o) next[pid] = o;
            else delete next[pid];
            return { serverOps: next };
          });
        op({ state: "stopping" });
        try {
          const { stopped } = await client.request("servers.kill", { pid, ...(force ? { force: true } : {}) });
          if (stopped) {
            op(null);
            set((s) => (s.servers.data ? { servers: { ...s.servers, data: s.servers.data.filter((d) => d.pid !== pid) } } : {}));
          } else {
            op({ state: "stuck" });
          }
        } catch (err) {
          op({ state: "error", error: errorMessage(err) });
          void get().loadServers();
        }
      },

      async loadDoctor() {
        set((s) => ({ doctor: loading(s.doctor) }));
        try {
          const data = await client.request("doctor");
          set({ doctor: { status: "ready", data, error: null } });
        } catch (err) {
          set((s) => ({ doctor: { ...s.doctor, status: "error", error: errorMessage(err) } }));
        }
      },

      async attachFiles(draft, files) {
        const add: DraftFile[] = files.map((f) => ({
          key: `f${++draftSeq}`,
          name: f.name,
          size: f.size,
          preview: f.preview,
          status: "uploading",
          id: null,
          error: null,
        }));
        set((s) => ({ drafts: { ...s.drafts, [draft]: [...(s.drafts[draft] ?? []), ...add] } }));
        const update = (key: string, patch: Partial<DraftFile>) =>
          set((s) => ({ drafts: { ...s.drafts, [draft]: (s.drafts[draft] ?? []).map((d) => (d.key === key ? { ...d, ...patch } : d)) } }));
        await Promise.all(
          files.map(async (f, i) => {
            const key = add[i]!.key;
            try {
              if (f.find) {
                const found = await client.request("attachments.locateFile", { name: f.name, size: f.find.size, modified: f.find.modified });
                if (found) {
                  update(key, { status: "ready", id: found.id });
                  return;
                }
                if (!f.upload) {
                  const mb = Math.round(f.size / 1048576);
                  update(key, { status: "error", error: `Couldn't find ${f.name} on this Mac, and uploads can be at most 20 MB (it's ${mb} MB)` });
                  return;
                }
                f = await f.upload();
              }
              if (f.error) {
                update(key, { status: "error", error: f.error });
                return;
              }
              const saved: Attachment = await client.request("attachments.upload", { name: f.name, data: f.data, ...(f.mime ? { mime: f.mime } : {}) });
              update(key, { status: "ready", id: saved.id });
            } catch (err) {
              update(key, { status: "error", error: errorMessage(err) });
            }
          }),
        );
      },

      async attachFolders(draft, folders, use) {
        // One working folder at a time: a newer one makes earlier ones context.
        const add: DraftFile[] = folders.map((f, i) => ({
          key: `f${++draftSeq}`,
          name: f.name,
          size: 0,
          preview: null,
          status: "uploading",
          id: null,
          error: null,
          folder: { path: null, options: [], use: use === "workdir" && i === 0 ? "workdir" : "context" },
        }));
        set((s) => {
          const prev = (s.drafts[draft] ?? []).map((d) =>
            d.folder?.use === "workdir" && add.some((a) => a.folder?.use === "workdir") ? { ...d, folder: { ...d.folder, use: "context" as const } } : d,
          );
          return { drafts: { ...s.drafts, [draft]: [...prev, ...add] } };
        });
        await Promise.all(
          folders.map(async (f, i) => {
            const key = add[i]!.key;
            let patch: Partial<DraftFile>;
            try {
              const options = await client.request("attachments.locateFolder", { name: f.name, entries: f.entries });
              patch = options.length
                ? { status: "ready", folder: { ...add[i]!.folder!, path: options[0]!, options } }
                : { status: "error", error: `Couldn't find ${f.name} on this Mac. Drop it from the Finder on the Mac EMP runs on.` };
            } catch (err) {
              patch = { status: "error", error: errorMessage(err) };
            }
            set((s) => ({
              drafts: {
                ...s.drafts,
                [draft]: (s.drafts[draft] ?? []).map((d) => (d.key === key ? { ...d, ...patch, folder: patch.folder ? { ...patch.folder, use: d.folder!.use } : d.folder } : d)),
              },
            }));
          }),
        );
      },

      setDraftFolder(draft, key, patch) {
        set((s) => ({
          drafts: {
            ...s.drafts,
            [draft]: (s.drafts[draft] ?? []).map((d) => {
              if (d.key === key && d.folder) return { ...d, folder: { ...d.folder, ...patch } };
              // Choosing a working folder makes any other one context.
              if (patch.use === "workdir" && d.folder?.use === "workdir") return { ...d, folder: { ...d.folder, use: "context" as const } };
              return d;
            }),
          },
        }));
      },

      newThreadInFolder(draft, key, agentId) {
        const chip = get().drafts[draft]?.find((d) => d.key === key);
        if (!chip?.folder) return;
        get().removeDraftFile(draft, key);
        set((s) => {
          const others = (s.drafts[NEW_DRAFT] ?? []).map((d) => (d.folder?.use === "workdir" ? { ...d, folder: { ...d.folder, use: "context" as const } } : d));
          return { drafts: { ...s.drafts, [NEW_DRAFT]: [...others, { ...chip, folder: { ...chip.folder!, use: "workdir" } }] } };
        });
        if (agentId) get().setNewThreadAgent(agentId);
        get().setNewThreadOpen(true);
      },

      removeDraftFile(draft, key) {
        set((s) => ({ drafts: { ...s.drafts, [draft]: (s.drafts[draft] ?? []).filter((d) => d.key !== key) } }));
      },

      clearDraft(draft) {
        if (!get().drafts[draft]?.length) return;
        set((s) => {
          const next = { ...s.drafts };
          delete next[draft];
          return { drafts: next };
        });
      },

      async loadCommands(harness, agentId) {
        const key = `${harness}:${agentId ?? ""}`;
        const prev = get().commands[key];
        if (prev && (prev.status === "loading" || prev.status === "ready")) return;
        set((s) => ({ commands: { ...s.commands, [key]: { status: "loading", data: prev?.data ?? null, error: null } } }));
        try {
          const data = await client.request("commands.list", { harness, ...(agentId ? { agentId } : {}) });
          set((s) => ({ commands: { ...s.commands, [key]: { status: "ready", data, error: null } } }));
        } catch (err) {
          set((s) => ({ commands: { ...s.commands, [key]: { status: "error", data: null, error: errorMessage(err) } } }));
        }
      },

      async runCommand(text, target, scope) {
        const forThread = "threadId" in target;
        const where = scope ?? ("threadId" in target ? target.threadId : "new");
        const fail = (err: unknown) =>
          where === "suki"
            ? set((s) => ({ suki: { ...s.suki, error: errorMessage(err) } }))
            : forThread
              ? set({ composer: { sending: false, error: errorMessage(err) } })
              : set({ newThread: { open: true, sending: false, error: errorMessage(err) } });
        try {
          const info = await client.request("commands.run", { ...target, text, ...COMMAND_CARD_SIZE });
          // Shown in the chat as a card, not on the Terminals page.
          set((s) => ({
            terminals: { ...s.terminals, [info.id]: info },
            commandCards: { ...s.commandCards, [where]: info.id },
            ...(forThread ? { composer: { sending: false, error: null } } : { newThread: { ...s.newThread, error: null } }),
          }));
          return true;
        } catch (err) {
          fail(err);
          return false;
        }
      },

      async closeCommandCard(scope) {
        const terminalId = get().commandCards[scope];
        if (!terminalId) return;
        set((s) => {
          const next = { ...s.commandCards };
          delete next[scope];
          return { commandCards: next };
        });
        const takeover = Object.values(get().live).find((l) => l.owner === "terminal" && l.terminalId === terminalId);
        try {
          if (takeover) await client.request("threads.release", { threadId: takeover.threadId });
          else await get().killTerminal(terminalId);
        } catch {
          // Gone already.
        }
        // A sign-in may have changed which models each harness offers.
        const stale = Object.keys(get().models);
        set({ models: {} });
        for (const key of stale) {
          const [harness, profile] = key.split(":");
          void get().loadModels(harness as Harness, profile || null);
        }
      },

      async runNative(threadId, text) {
        try {
          await client.request("threads.command", { threadId, text });
          set({ composer: { sending: false, error: null } });
          return true;
        } catch (err) {
          set({ composer: { sending: false, error: errorMessage(err) } });
          return false;
        }
      },

      async loadGoal(threadId) {
        try {
          const goal = await client.request("threads.goal", { threadId });
          set((s) => ({ goals: { ...s.goals, [threadId]: goal } }));
        } catch {
          // Not a Codex thread, or Codex's store isn't readable: no goal bar.
        }
      },

      async watch(threadId) {
        try {
          return await client.request("threads.watch", { threadId });
        } catch {
          return false;
        }
      },

      async loadHolders(threadId) {
        try {
          const list = await client.request("threads.holders", { threadId });
          set((s) => ({ holders: { ...s.holders, [threadId]: list } }));
        } catch {
          set((s) => ({ holders: { ...s.holders, [threadId]: [] } }));
        }
      },

      async closeElsewhere(threadId) {
        try {
          const { remaining } = await client.request("threads.closeElsewhere", { threadId });
          set((s) => ({
            holders: { ...s.holders, [threadId]: remaining },
            composer: remaining.length ? s.composer : { sending: false, error: null },
          }));
          return remaining.length === 0;
        } catch (err) {
          set({ composer: { sending: false, error: errorMessage(err) } });
          return false;
        }
      },

      async answerQuestion(threadId, questionId, answer) {
        try {
          const live = await client.request("threads.answer", { threadId, questionId, ...answer });
          set((s) => ({ live: { ...s.live, [threadId]: live } }));
        } catch (err) {
          set({ composer: { sending: false, error: errorMessage(err) } });
          void get().loadLive();
        }
      },

      toggleChildren(threadId) {
        const open = get().expandedThreads.includes(threadId);
        set((s) => ({ expandedThreads: open ? s.expandedThreads.filter((id) => id !== threadId) : [...s.expandedThreads, threadId] }));
        if (!open) void loadChildren(threadId);
      },

      async loadThreads() {
        const seq = ++threadsSeq;
        set((s) => ({ threads: { ...s.threads, status: "loading", error: null, loadingMore: false } }));
        try {
          const page = await client.request("threads.list", threadListParams(get().filter, 0));
          if (seq !== threadsSeq) return;
          set({ threads: { items: page.items, total: page.total, status: "ready", error: null, loadingMore: false } });
          for (const id of get().expandedThreads) void loadChildren(id);
        } catch (err) {
          if (seq !== threadsSeq) return;
          set((s) => ({ threads: { ...s.threads, status: "error", error: errorMessage(err) } }));
        }
      },

      async loadMoreThreads() {
        const list = get().threads;
        if (list.loadingMore || list.status !== "ready" || !hasMoreThreads(list)) return;
        const seq = threadsSeq;
        set({ threads: { ...list, loadingMore: true } });
        try {
          const page = await client.request("threads.list", threadListParams(get().filter, list.items.length));
          if (seq !== threadsSeq) return;
          set((s) => ({
            threads: { ...s.threads, items: appendUnique(s.threads.items, page.items), total: page.total, loadingMore: false },
          }));
        } catch (err) {
          if (seq !== threadsSeq) return;
          set((s) => ({ threads: { ...s.threads, loadingMore: false, error: errorMessage(err) } }));
        }
      },

      async openThread(threadId) {
        const seq = ++threadSeq;
        const cached = get().threads.items.find((t) => t.id === threadId) ?? null;
        showTab(`t:${threadId}`);
        if (cached) set((s) => ({ tabTitles: { ...s.tabTitles, [threadId]: cached.title } }));
        set({
          section: "threads",
          selectedThreadId: threadId,
          thread: { status: cached ? "ready" : "loading", data: cached, error: null },
          transcript: emptyTranscript(threadId),
          resume: { threadId, status: "idle", spec: null, error: null },
          liveView: { threadId, tail: [], partial: null },
          composer: { sending: false, error: null },
          paneTerminalId: liveFor(get(), threadId)?.terminalId ?? null,
          newThread: { open: false, sending: false, error: null },
        });

        client.request("threads.get", { threadId }).then(
          (data) => {
            set((s) => ({ tabTitles: { ...s.tabTitles, [threadId]: data.title } }));
            if (seq === threadSeq) set({ thread: { status: "ready", data, error: null } });
          },
          (err: unknown) => {
            if (seq === threadSeq) set((s) => ({ thread: { ...s.thread, status: "error", error: errorMessage(err) } }));
          },
        );

        try {
          let page = await client.request("threads.transcript", { threadId, offset: 0, limit: TRANSCRIPT_PAGE_SIZE });
          if (page.total > page.items.length) {
            page = await client.request("threads.transcript", { threadId, ...tailRange(page.total) });
          }
          if (seq !== threadSeq) return;
          set({
            transcript: { ...emptyTranscript(threadId), items: page.items, start: page.offset, total: page.total, status: "ready" },
          });
        } catch (err) {
          if (seq !== threadSeq) return;
          set((s) => ({ transcript: { ...s.transcript, status: "error", error: errorMessage(err) } }));
        }
      },

      async loadEarlier() {
        const tr = get().transcript;
        const range = earlierRange(tr);
        if (!range || !tr.threadId || tr.loadingEarlier || tr.status !== "ready") return;
        const seq = threadSeq;
        set({ transcript: { ...tr, loadingEarlier: true } });
        try {
          const page = await client.request("threads.transcript", { threadId: tr.threadId, ...range });
          if (seq !== threadSeq) return;
          set((s) => ({ transcript: { ...s.transcript, ...mergePage(s.transcript, page), loadingEarlier: false } }));
        } catch (err) {
          if (seq !== threadSeq) return;
          set((s) => ({ transcript: { ...s.transcript, loadingEarlier: false, error: errorMessage(err) } }));
        }
      },

      async loadLater() {
        const tr = get().transcript;
        const range = laterRange(tr);
        if (!range || !tr.threadId || tr.loadingLater || tr.status !== "ready") return;
        const seq = threadSeq;
        set({ transcript: { ...tr, loadingLater: true } });
        try {
          const page = await client.request("threads.transcript", { threadId: tr.threadId, ...range });
          if (seq !== threadSeq) return;
          set((s) => ({ transcript: { ...s.transcript, ...mergePage(s.transcript, page), loadingLater: false } }));
        } catch (err) {
          if (seq !== threadSeq) return;
          set((s) => ({ transcript: { ...s.transcript, loadingLater: false, error: errorMessage(err) } }));
        }
      },

      async fetchResume() {
        const threadId = get().selectedThreadId;
        if (!threadId) return;
        const seq = ++resumeSeq;
        set({ resume: { threadId, status: "loading", spec: null, error: null } });
        try {
          const spec = await client.request("threads.resumeCommand", { threadId });
          if (seq !== resumeSeq) return;
          set({ resume: { threadId, status: "ready", spec, error: null } });
        } catch (err) {
          if (seq !== resumeSeq) return;
          set({ resume: { threadId, status: "error", spec: null, error: errorMessage(err) } });
        }
      },

      async runImport() {
        if (get().importRun.running) return;
        importFinishedSeen = false;
        set({ importRun: { running: true, lines: [], report: null, error: null } });
        try {
          const report = await client.request("import.run", { sources: [] });
          set((s) => ({ importRun: { ...s.importRun, running: false, report } }));
          if (!importFinishedSeen) afterImport(report);
          host?.notify("EMP import finished", summarizeImport(report));
        } catch (err) {
          set((s) => ({ importRun: { ...s.importRun, running: false, error: errorMessage(err) } }));
        }
      },

      reconnect() {
        client.reconnect().catch(() => {});
      },

      async loadLive() {
        try {
          const list = await client.request("threads.live");
          set({ live: Object.fromEntries(list.map((l) => [l.threadId, l])) });
        } catch {
          // Older daemons have no live sessions.
        }
      },

      async send(text, mode = "prompt", force = false, attachments = []) {
        const threadId = get().selectedThreadId;
        if (!threadId || (!text.trim() && attachments.length === 0)) return false;
        set({ composer: { sending: true, error: null } });
        const model = get().threadModels[threadId];
        try {
          const live = await client.request("threads.send", {
            threadId,
            text,
            mode,
            ...(model ? { model } : {}),
            ...(force ? { force } : {}),
            ...(attachments.length ? { attachments } : {}),
          });
          set((s) => ({ composer: { sending: false, error: null }, live: { ...s.live, [threadId]: live } }));
          return true;
        } catch (err) {
          set({ composer: { sending: false, error: errorMessage(err) } });
          void get().loadHolders(threadId);
          return false;
        }
      },

      setThreadModel(threadId, model) {
        set((s) => ({ threadModels: { ...s.threadModels, [threadId]: model } }));
      },

      async interrupt() {
        const threadId = get().selectedThreadId;
        if (!threadId) return;
        try {
          await client.request("threads.interrupt", { threadId });
        } catch (err) {
          set({ composer: { sending: false, error: errorMessage(err) } });
        }
      },

      async takeover(force = false) {
        const threadId = get().selectedThreadId;
        if (!threadId) return;
        set({ composer: { sending: false, error: null } });
        try {
          const info = await client.request("threads.takeover", { threadId, ...DEFAULT_TERMINAL_SIZE, ...(force ? { force } : {}) });
          set((s) => ({ terminals: { ...s.terminals, [info.id]: info }, paneTerminalId: info.id }));
        } catch (err) {
          set({ composer: { sending: false, error: errorMessage(err) } });
        }
      },

      async release() {
        const threadId = get().selectedThreadId;
        if (!threadId) return;
        try {
          await client.request("threads.release", { threadId });
          set({ paneTerminalId: null });
        } catch (err) {
          set({ composer: { sending: false, error: errorMessage(err) } });
        }
      },

      setNewThreadOpen(open) {
        set({ newThread: { open, sending: false, error: null }, section: "threads" });
        if (open) showTab("new");
        if (open && get().agents.status === "idle") void get().loadAgents();
      },

      activateTab(tab) {
        if (tab === "new") get().setNewThreadOpen(true);
        else if (tab.startsWith("t:")) void get().openThread(tab.slice(2));
        else get().showSection(tab.slice(5) as Section);
      },

      closeTab(tab) {
        const s = get();
        const idx = s.tabs.indexOf(tab);
        if (idx < 0) return;
        const tabs = s.tabs.filter((t) => t !== tab);
        if (tabs.length === 0) tabs.push("new");
        set({ tabs });
        if (s.activeTab === tab) get().activateTab(tabs[Math.max(0, idx - 1)]!);
      },

      setSidebarTab(sidebarTab) {
        set({ sidebarTab });
        if (sidebarTab === "agents" && get().agents.status === "idle") void get().loadAgents();
      },

      setNewThreadAgent(newThreadAgentId) {
        // A model picked for one agent rarely fits another harness.
        if (newThreadAgentId !== get().newThreadAgentId) set({ newThreadAgentId, newThreadModel: null });
      },

      setComposerError(error) {
        set({ composer: { sending: false, error } });
      },

      setNewThreadError(error) {
        set((s) => ({ newThread: { ...s.newThread, error } }));
      },

      setNewThreadModel(newThreadModel) {
        set({ newThreadModel });
      },

      setNewThreadProject(newThreadProjectId) {
        set({ newThreadProjectId });
      },

      async loadModels(harness, profile = null, refresh = false) {
        const key = `${harness}:${profile ?? ""}`;
        const prev = get().models[key];
        if (prev && (prev.status === "loading" || (prev.status === "ready" && !refresh))) return;
        set((s) => ({ models: { ...s.models, [key]: { status: "loading", data: prev?.data ?? null, error: null } } }));
        try {
          const data = (await client.request("models.list", { harness, ...(profile ? { profile } : {}) }))
            .filter((m) => !isCopilotModel(m.id, m.group));
          set((s) => ({ models: { ...s.models, [key]: { status: "ready", data, error: null } } }));
        } catch (err) {
          set((s) => ({ models: { ...s.models, [key]: { status: "error", data: null, error: errorMessage(err) } } }));
        }
      },

      async loadContext(threadId) {
        if (!threadId) {
          set({ context: { threadId: null, data: null } });
          return;
        }
        // Keep the old numbers on screen for the same thread while refreshing.
        if (get().context.threadId !== threadId) set({ context: { threadId, data: null } });
        try {
          const data = await client.request("threads.context", { threadId });
          if (get().context.threadId === threadId) set({ context: { threadId, data } });
        } catch {
          if (get().context.threadId === threadId) set({ context: { threadId, data: null } });
        }
      },

      async createProject(path, name, createFolder) {
        set({ projectCreate: { saving: true, error: null } });
        try {
          const project = await client.request("projects.create", { path, createFolder, ...(name ? { name } : {}) });
          set({ projectCreate: { saving: false, error: null } });
          await get().loadProjects();
          set({ newThreadProjectId: project.id });
          return true;
        } catch (err) {
          set({ projectCreate: { saving: false, error: errorMessage(err) } });
          return false;
        }
      },

      async loadProjectDetail(projectId) {
        set((s) => ({
          projectThreads: { ...s.projectThreads, [projectId]: loading(s.projectThreads[projectId] ?? idle()) },
          projectFiles: { ...s.projectFiles, [projectId]: loading(s.projectFiles[projectId] ?? idle()) },
        }));
        const threads = client.request("threads.list", { projectId, limit: PROJECT_THREADS }).then(
          (page) => set((s) => ({ projectThreads: { ...s.projectThreads, [projectId]: { status: "ready", data: { items: page.items, total: page.total }, error: null } } })),
          (err: unknown) => set((s) => ({ projectThreads: { ...s.projectThreads, [projectId]: { status: "error", data: null, error: errorMessage(err) } } })),
        );
        const files = client.request("projects.files", { projectId }).then(
          (data) => set((s) => ({ projectFiles: { ...s.projectFiles, [projectId]: { status: "ready", data, error: null } } })),
          (err: unknown) => set((s) => ({ projectFiles: { ...s.projectFiles, [projectId]: { status: "error", data: null, error: errorMessage(err) } } })),
        );
        await Promise.all([threads, files]);
      },

      updateSettings(patch, opts = {}) {
        const settings = normalizeSettings({ ...get().settings, ...patch });
        set({ settings });
        if (patch.defaultAgentId) get().setNewThreadAgent(patch.defaultAgentId);
        if (opts.persist !== false) host?.prefs.set(PREF_SETTINGS, settings).catch(() => {});
      },

      resetLayout() {
        get().updateSettings({ ...DEFAULT_LAYOUT, nav: DEFAULT_LAYOUT.nav.map((e) => ({ ...e })) });
      },

      openSettings(page) {
        set({ settingsPage: page ?? get().settingsPage ?? lastSettingsPage, layoutEditing: false, drawerOpen: false });
      },

      closeSettings() {
        const page = get().settingsPage;
        if (page) lastSettingsPage = page;
        set({ settingsPage: null });
      },

      setLayoutEditing(layoutEditing) {
        if (layoutEditing) get().closeSettings();
        set({ layoutEditing });
      },

      setLayoutCardPos(layoutCardPos) {
        set({ layoutCardPos });
      },

      setDrawerOpen(drawerOpen) {
        set({ drawerOpen });
      },

      applyArrangement(arrangement) {
        get().updateSettings({ ...arrangement });
      },

      saveLayout(name) {
        const s = get().settings;
        const id = `layout-${Date.now().toString(36)}`;
        const arrangement = { sidebarSide: s.sidebarSide, sidebarScale: s.sidebarScale, threadWidth: s.threadWidth };
        get().updateSettings({ savedLayouts: [...s.savedLayouts, { id, name: name.trim(), arrangement }] });
      },

      deleteLayout(id) {
        get().updateSettings({ savedLayouts: get().settings.savedLayouts.filter((l) => l.id !== id) });
      },

      async runInTerminal(program, args, cwd, title) {
        try {
          const info = await client.request("terminals.open", {
            kind: "command",
            spec: { program, args, cwd, env: [], display: [program, ...args].join(" ") },
            ...DEFAULT_TERMINAL_SIZE,
          });
          set((s) => ({ terminals: { ...s.terminals, [info.id]: { ...info, title } }, focusedTerminalId: info.id }));
          get().showSection("terminals");
        } catch (err) {
          set({ newThread: { open: true, sending: false, error: errorMessage(err) } });
        }
      },

      toggleDetails() {
        const open = !get().detailsOpen;
        set({ detailsOpen: open });
        if (open && get().resume.threadId !== get().selectedThreadId) void get().fetchResume();
      },

      async startThread(agentId, text, cwd, opts = {}) {
        if (!text.trim() && !opts.attachments?.length) return false;
        set({ newThread: { open: true, sending: true, error: null } });
        try {
          const projectId = opts.projectId === undefined ? get().filter.projectId : opts.projectId;
          const thread = await client.request("threads.start", {
            agentId,
            text,
            ...(cwd ? { cwd } : {}),
            ...(projectId ? { projectId } : {}),
            ...(opts.model ? { model: opts.model } : {}),
            ...(opts.attachments?.length ? { attachments: opts.attachments } : {}),
          });
          // The New thread tab becomes the thread's tab.
          set((s) => ({
            newThread: { open: false, sending: false, error: null },
            tabs: s.tabs.map((t) => (t === "new" ? (`t:${thread.id}` as TabId) : t)),
            tabTitles: { ...s.tabTitles, [thread.id]: thread.title },
          }));
          void get().loadThreads();
          void get().loadProjects();
          await get().openThread(thread.id);
          return true;
        } catch (err) {
          set({ newThread: { open: true, sending: false, error: errorMessage(err) } });
          return false;
        }
      },

      async loadTerminals() {
        try {
          const list = await client.request("terminals.list");
          set({ terminals: Object.fromEntries(list.map((t) => [t.id, t])) });
        } catch {
          // Older daemons have no terminals.
        }
      },

      async openShell(forThread = true) {
        const t = forThread ? get().thread.data : null;
        try {
          const info = await client.request("terminals.open", {
            kind: "shell",
            ...(t ? { threadId: t.id } : {}),
            ...DEFAULT_TERMINAL_SIZE,
          });
          set((s) => ({
            terminals: { ...s.terminals, [info.id]: info },
            ...(t ? { paneTerminalId: info.id } : { focusedTerminalId: info.id, section: "terminals" as const }),
          }));
        } catch (err) {
          set({ composer: { sending: false, error: errorMessage(err) } });
        }
      },

      setPaneTerminal(terminalId) {
        set({ paneTerminalId: terminalId });
      },

      focusTerminal(terminalId) {
        set({ focusedTerminalId: terminalId, section: "terminals" });
      },

      async killTerminal(terminalId) {
        // Killing an exited terminal forgets it; decide from the state *before* the request,
        // since the exit push can arrive ahead of the response.
        const wasExited = get().terminals[terminalId]?.state === "exited";
        try {
          await client.request("terminals.kill", { terminalId });
          if (wasExited) {
            set((s) => {
              const next = { ...s.terminals };
              delete next[terminalId];
              return {
                terminals: next,
                paneTerminalId: s.paneTerminalId === terminalId ? null : s.paneTerminalId,
                focusedTerminalId: s.focusedTerminalId === terminalId ? null : s.focusedTerminalId,
              };
            });
          }
        } catch {
          void get().loadTerminals();
        }
      },

      editAgent(draft) {
        set({ agentEdit: { draft, saving: false, error: null }, section: "agents" });
      },

      async saveAgent(draft) {
        set((s) => ({ agentEdit: { ...s.agentEdit, saving: true, error: null } }));
        try {
          await client.request("agents.save", draft);
          set({ agentEdit: { draft: null, saving: false, error: null } });
          await get().loadAgents();
          return true;
        } catch (err) {
          set((s) => ({ agentEdit: { ...s.agentEdit, saving: false, error: errorMessage(err) } }));
          return false;
        }
      },

      async deleteAgent(agentId) {
        try {
          await client.request("agents.delete", { agentId });
          set({ agentEdit: { draft: null, saving: false, error: null } });
          await get().loadAgents();
        } catch (err) {
          set((s) => ({ agentEdit: { ...s.agentEdit, error: errorMessage(err) } }));
        }
      },
    };
  });
}

/** Live state for a thread, if EMP holds its session. */
export function liveFor(state: WorkspaceState, threadId: string | null): LiveThread | null {
  return threadId ? (state.live[threadId] ?? null) : null;
}

/** A draft for editing `agent`, or a blank one for a new agent. */
export function agentDraft(agent?: AgentProfile): AgentSaveParams {
  return agent
    ? {
        id: agent.id,
        name: agent.name,
        harness: agent.harness,
        model: agent.model,
        effort: agent.effort,
        role: agent.role,
        instructions: agent.instructions,
        cwd: agent.cwd,
        autonomy: agent.autonomy,
        harnessProfile: agent.harnessProfile,
        compactAt: agent.compactAt,
      }
    : {
        name: "",
        harness: "pi",
        model: null,
        effort: null,
        role: null,
        instructions: null,
        cwd: null,
        autonomy: 3,
        harnessProfile: null,
        compactAt: null,
      };
}

/** Harnesses EMP can chat with (structured drivers). Others open in a terminal. */
export const CHAT_HARNESSES: readonly Harness[] = ["pi", "claude", "hermes", "codex", "antigravity"];

/** CLI that opens an interactive session for harnesses EMP can't chat with yet. */
export const TERMINAL_PROGRAMS: Partial<Record<Harness, string>> = { gemini: "gemini", cursor: "cursor-agent" };

/** A file read for upload (`lib/attach.ts`). */
export interface PreparedFile {
  name: string;
  mime: string | null;
  /** Base64 bytes. */
  data: string;
  size: number;
  /** Image preview URL (blob/data), if it's an image. */
  preview: string | null;
  /** Set when the file can't be attached (too big, a folder); nothing is uploaded. */
  error?: string;
  /** Look for it on the Mac first (`attachments.locateFile`); `data` is empty then. */
  find?: { size: number; modified: number };
  /** Reads it for upload when it isn't on the Mac; absent when it's too big to upload. */
  upload?: () => Promise<PreparedFile>;
}

/** A folder in a draft: where it is, and whether to work in it or hand it over as context. */
export interface DraftFolder {
  path: string | null;
  /** Other same-named folders it may be, likeliest first (`path` is one of them). */
  options: string[];
  use: "context" | "workdir";
}

/** A file (or folder) attached to a message that hasn't been sent yet. */
export interface DraftFile {
  key: string;
  folder?: DraftFolder;
  name: string;
  size: number;
  preview: string | null;
  status: "uploading" | "ready" | "error";
  /** Attachment id once uploaded. */
  id: string | null;
  error: string | null;
}

let draftSeq = 0;
/** The New thread composer's draft key. */
export const NEW_DRAFT = "new";
/** Suki's composer draft. */
export const SUKI_DRAFT = "suki";
