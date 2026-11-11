import type {
  AgentProfile,
  DoctorReport,
  Harness,
  Project,
  Thread,
  TranscriptItem,
} from "@emp/protocol";

export interface MockFixture {
  projects: Project[];
  threads: Thread[];
  /** Transcript per thread id. Threads without an entry have an empty transcript. */
  transcripts: Record<string, TranscriptItem[]>;
  agents: AgentProfile[];
  doctor: DoctorReport;
}

export interface DemoFixtureOptions {
  /** Reference time for timestamps. Default `Date.now()`. */
  now?: number;
  /** Items in the one very long transcript (exercises paging). Default 3200. */
  longTranscriptItems?: number;
}

const MIN = 60_000;
const HOUR = 60 * MIN;
const HOME = "/Users/demo";

/** Deterministic PRNG so the demo looks the same on every load. */
function mulberry32(seed: number): () => number {
  let a = seed;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const PROJECTS: Array<{ id: string; name: string; path: string }> = [
  { id: "p-emp", name: "EMP", path: `${HOME}/Desktop/EMP` },
  { id: "p-orion", name: "Orion Terminal", path: `${HOME}/Orion_Terminal` },
  { id: "p-designer", name: "PI DESIGNER", path: `${HOME}/Desktop/PI DESIGNER` },
  { id: "p-site", name: "website", path: `${HOME}/code/website` },
];

const MODELS: Record<Harness, string | null> = {
  pi: "openai/gpt-6.1-sol",
  hermes: "anthropic/claude-opus-5-5",
  claude: "claude-opus-5-5",
  codex: "gpt-6.1-sol",
  gemini: "gemini-3.6-pro",
  cursor: null,
  antigravity: "gemini-4-argon-medium",
  shell: null,
};

const TITLES = [
  "Scaffold the empd daemon and SQLite store",
  "Import Claude sessions from ~/.claude/projects",
  "Fix flaky PTY resize test",
  "Design tokens for the thread list",
  "Review plugin sandbox CSP",
  "Add Codex importer",
  "Why does hermes acp drop tool_call_update?",
  "Refactor transcript normalizer",
  "Write the localService broker proposal",
  "Speed up threads.list with an index",
  "Landing page hero copy",
  "Port xterm.js 6 addon setup",
  "Investigate websocket reconnect storm",
  "Doctor: detect cursor-agent version",
  "Hermes profile import",
  "Bump React to 19.2",
];

const HARNESSES = ["pi", "hermes", "claude", "codex"] as const;

function sessionPath(h: Harness, id: string, cwd: string): string | null {
  switch (h) {
    case "pi":
      return `${HOME}/.pi/agent/sessions/${cwd.replaceAll("/", "-")}/${id}.jsonl`;
    case "claude":
      return `${HOME}/.claude/projects/${cwd.replaceAll("/", "-")}/${id}.jsonl`;
    case "codex":
      return `${HOME}/.codex/sessions/2026/10/${id}.jsonl`;
    default:
      return null;
  }
}

function originPath(h: Harness, path: string | null): string {
  if (path) return path;
  return h === "hermes" ? `${HOME}/.hermes/state.db` : `${HOME}/.${h}`;
}

function makeTranscript(rand: () => number, count: number, start: number, model: string | null): TranscriptItem[] {
  const items: TranscriptItem[] = [];
  let ts = start;
  let turn = 0;
  while (items.length < count) {
    turn += 1;
    ts += Math.round(rand() * 5 * MIN);
    items.push({
      kind: "user",
      text: `Step ${turn}: please continue with the next part of the task.\nKeep the change minimal and run the tests afterwards.`,
      ts,
    });
    if (rand() < 0.6) {
      items.push({
        kind: "thinking",
        text: `The user wants step ${turn}. I should read the relevant files first, then make a focused edit.\nRisks: breaking the existing tests; keep scope tight.`,
        ts: ts + 1_000,
      });
    }
    const tools = Math.floor(rand() * 3);
    for (let t = 0; t < tools; t += 1) {
      const callId = `call_${turn}_${t}`;
      const isError = rand() < 0.1;
      items.push({
        kind: "toolCall",
        callId,
        name: t % 2 === 0 ? "bash" : "read",
        input: t % 2 === 0 ? JSON.stringify({ command: "npm test" }, null, 2) : JSON.stringify({ path: "src/index.ts" }, null, 2),
        ts: ts + 2_000,
      });
      items.push({
        kind: "toolResult",
        callId,
        output: isError ? "Error: command exited with code 1\n  at run (src/run.ts:12:5)" : "✓ 42 tests passed (1.2s)",
        isError,
        ts: ts + 3_000,
      });
    }
    items.push({
      kind: "assistant",
      text: `Done with step ${turn}. I changed two files and the tests pass.\n\n- src/index.ts: exported the new helper\n- src/run.ts: handled the empty case`,
      model,
      ts: ts + 4_000,
    });
    if (turn % 25 === 0) items.push({ kind: "system", text: "Context compacted (summary kept, 182k → 24k tokens)", ts: ts + 5_000 });
  }
  return items.slice(0, count);
}

/** A believable, deterministic data set: 4 projects, ~48 threads, agents and a doctor report. */
export function createDemoFixture(options: DemoFixtureOptions = {}): MockFixture {
  const now = options.now ?? Date.now();
  const longItems = options.longTranscriptItems ?? 3200;
  const rand = mulberry32(47);

  const threads: Thread[] = [];
  const transcripts: Record<string, TranscriptItem[]> = {};

  for (let i = 0; i < 48; i += 1) {
    const harness = HARNESSES[i % HARNESSES.length]!;
    const project = PROJECTS[i % PROJECTS.length]!;
    const id = `t-${String(i + 1).padStart(3, "0")}`;
    const sessionId = `${harness}-${(0x1000 + i * 7919).toString(16)}`;
    const updatedAt = now - Math.round(i * i * 0.7 * HOUR + rand() * 30 * MIN);
    const createdAt = updatedAt - Math.round((1 + rand() * 6) * HOUR);
    const path = sessionPath(harness, sessionId, project.path);
    const count = i === 0 ? longItems : 4 + Math.floor(rand() * 40);
    const model = MODELS[harness];
    transcripts[id] = makeTranscript(rand, count, createdAt, model);
    threads.push({
      id,
      harness,
      title: TITLES[i % TITLES.length]! + (i >= TITLES.length ? ` (${Math.floor(i / TITLES.length) + 1})` : ""),
      projectId: i % 11 === 10 ? null : project.id,
      cwd: i % 11 === 10 ? HOME : project.path,
      model,
      agentId: null,
      parentId: null,
      childCount: 0,
      assignedBy: null,
      status: "idle",
      // Every eighth Codex-style run stands in for a machine-started `codex exec` call.
      automated: i % 8 === 7,
      session: { harness, sessionId, path, home: null, profile: harness === "hermes" ? "studio" : null },
      messageCount: count,
      createdAt,
      updatedAt,
      importedFrom: { source: harness, key: `${harness}:${sessionId}`, path: originPath(harness, path) },
    });
  }

  // Subagents: the newest Codex thread spawned three, the second-newest Claude thread one.
  const spawn = (parent: Thread, names: string[]) => {
    names.forEach((name, n) => {
      const id = `${parent.id}-sub-${n + 1}`;
      const sessionId = `${parent.session.sessionId}-sub-${n + 1}`;
      const updatedAt = parent.updatedAt - (n + 1) * 4 * MIN;
      transcripts[id] = makeTranscript(rand, 6, updatedAt - 20 * MIN, parent.model);
      threads.push({
        ...parent,
        id,
        title: `Subagent: ${name}`,
        parentId: parent.id,
        childCount: 0,
        automated: false,
        session: { ...parent.session, sessionId },
        messageCount: 6,
        createdAt: updatedAt - 20 * MIN,
        updatedAt,
        importedFrom: parent.importedFrom && { ...parent.importedFrom, key: `${parent.harness}:${sessionId}` },
      });
    });
    parent.childCount = names.length;
  };
  spawn(threads.find((t) => t.harness === "codex")!, ["Pasteur", "Descartes", "Laplace"]);
  spawn(threads.filter((t) => t.harness === "claude")[1]!, ["Explore auth flow"]);

  // A conversation with Suki, the coordinator, which started the newest Claude thread.
  const helped = threads.find((t) => t.harness === "claude" && !t.parentId)!;
  const sukiAt = helped.createdAt - 2 * MIN;
  threads.push({
    ...helped,
    id: "t-suki-1",
    harness: "pi",
    title: "Get someone on the flaky login test",
    projectId: null,
    cwd: `${HOME}/Library/Application Support/EMP/suki`,
    model: "openai/gpt-6.1-sol",
    agentId: "a-suki",
    parentId: null,
    childCount: 0,
    assignedBy: null,
    session: { harness: "pi", sessionId: "pi-suki-1", path: `${HOME}/.pi/agent/sessions/suki/pi-suki-1.jsonl`, home: null, profile: null },
    messageCount: 7,
    createdAt: sukiAt,
    updatedAt: helped.updatedAt + MIN,
    importedFrom: { source: "pi", key: "pi:pi-suki-1", path: `${HOME}/.pi/agent/sessions/suki/pi-suki-1.jsonl` },
  });
  helped.assignedBy = "t-suki-1";
  transcripts["t-suki-1"] = [
    { kind: "user", text: "Get someone on the flaky login test\n\n<emp-context>\nOwner is viewing: Dev servers page\nRunning: nothing\n</emp-context>", ts: sukiAt },
    { kind: "toolCall", callId: "c1", name: "mcp__emp__list_agents", input: "{}", ts: sukiAt + 2000 },
    { kind: "toolResult", callId: "c1", output: '[{"name":"Reviewer","harness":"claude"}]', isError: false, ts: sukiAt + 2500 },
    { kind: "toolCall", callId: "c2", name: "mcp__emp__start_thread", input: `{"agent":"Reviewer","prompt":"The login test fails about one run in five…"}`, ts: sukiAt + 4000 },
    { kind: "toolResult", callId: "c2", output: `{"thread_id":"${helped.id}","title":"${helped.title}","status":"running"}`, isError: false, ts: sukiAt + 9000 },
    { kind: "assistant", text: `Started **Reviewer** on it: *${helped.title}*. I'll check back when it finishes, or ask me to wait for it.`, model: "gpt-6.1-sol", ts: sukiAt + 11000 },
    { kind: "user", text: "thanks, wait for it", ts: sukiAt + 60000 },
  ];

  const projects: Project[] = PROJECTS.map((p) => {
    const own = threads.filter((t) => t.projectId === p.id);
    return {
      id: p.id,
      name: p.name,
      rootPath: p.path,
      threadCount: own.filter((t) => !t.automated).length,
      automatedCount: own.filter((t) => t.automated).length,
      lastActivityAt: own.length ? Math.max(...own.map((t) => t.updatedAt)) : null,
    };
  });

  const agents: AgentProfile[] = [
    {
      id: "a-suki",
      name: "Suki",
      harness: "pi",
      model: "openai/gpt-6.1-sol",
      effort: "high",
      role: "Coordinator: starts, finds, steers and checks on your agents",
      instructions: null,
      cwd: `${HOME}/Library/Application Support/EMP/suki`,
      autonomy: 2,
      harnessProfile: null,
      compactAt: null,
      importedFrom: null,
      coordinator: true,
    },
    {
      id: "a-studio",
      name: "studio",
      harness: "hermes",
      model: "anthropic/claude-opus-5-5",
      effort: "xhigh",
      role: "Design owner: DESIGN.md and tokens.",
      instructions: "Own the visual direction. Write DESIGN.md and tokens.json; never let code agents invent tokens.",
      cwd: null,
      autonomy: 1,
      harnessProfile: "studio",
      compactAt: null,
      importedFrom: { source: "hermes", key: "hermes:profile:studio", path: `${HOME}/.hermes/profiles/studio/SOUL.md` },
      coordinator: false,
    },
    {
      id: "a-reviewer",
      name: "reviewer",
      harness: "claude",
      model: "claude-opus-5-5",
      effort: null,
      role: "Read-only code review.",
      instructions: null,
      cwd: null,
      autonomy: 0,
      harnessProfile: null,
      compactAt: null,
      importedFrom: { source: "orion", key: "orion:cc:reviewer", path: `${HOME}/Library/Application Support/Orion/orion.db` },
      coordinator: false,
    },
    {
      id: "a-worker",
      name: "worker",
      harness: "codex",
      model: "gpt-6.1-sol",
      effort: "medium",
      role: "Implements scoped changes.",
      instructions: "Make exactly the requested change, run the tests, report back.",
      cwd: `${HOME}/Desktop/EMP`,
      autonomy: 3,
      harnessProfile: null,
      compactAt: null,
      importedFrom: null,
      coordinator: false,
    },
    {
      id: "a-antigravity",
      name: "antigravity (default)",
      harness: "antigravity",
      model: "gemini-4-argon-medium",
      effort: null,
      role: null,
      instructions: null,
      cwd: null,
      autonomy: 1,
      harnessProfile: null,
      compactAt: null,
      importedFrom: { source: "antigravity", key: "antigravity:default", path: `${HOME}/.gemini/antigravity-cli/settings.json` },
      coordinator: false,
    },
  ];

  const countBy = (s: string) => threads.filter((t) => t.importedFrom?.source === s).length;
  const doctor: DoctorReport = {
    daemonVersion: "0.1.0-mock",
    dataDir: `${HOME}/Library/Application Support/EMP`,
    harnesses: [
      { harness: "pi", installed: true, path: "/opt/homebrew/bin/pi", version: "0.71.2" },
      { harness: "hermes", installed: true, path: `${HOME}/.local/bin/hermes`, version: "1.8.0" },
      { harness: "claude", installed: true, path: `${HOME}/.claude/local/claude`, version: "3.4.1" },
      { harness: "codex", installed: true, path: "/opt/homebrew/bin/codex", version: "0.140.0" },
      { harness: "gemini", installed: false, path: null, version: null },
      { harness: "cursor", installed: false, path: null, version: null },
      { harness: "antigravity", installed: true, path: `${HOME}/.local/bin/agy`, version: "1.4.3" },
      { harness: "shell", installed: true, path: "/bin/zsh", version: "5.9" },
    ],
    sources: [
      { source: "pi", found: true, paths: [`${HOME}/.pi/agent/sessions`], threadCount: countBy("pi"), agentCount: 0, lastImportAt: now - 2 * HOUR },
      { source: "hermes", found: true, paths: [`${HOME}/.hermes/state.db`, `${HOME}/.hermes/profiles`], threadCount: countBy("hermes"), agentCount: 1, lastImportAt: now - 2 * HOUR },
      { source: "claude", found: true, paths: [`${HOME}/.claude/projects`], threadCount: countBy("claude"), agentCount: 0, lastImportAt: now - 2 * HOUR },
      { source: "codex", found: true, paths: [`${HOME}/.codex/sessions`], threadCount: countBy("codex"), agentCount: 0, lastImportAt: now - 2 * HOUR },
      { source: "orion", found: false, paths: [], threadCount: 0, agentCount: 1, lastImportAt: null },
      { source: "antigravity", found: true, paths: [`${HOME}/.gemini/antigravity-cli`], threadCount: 0, agentCount: 1, lastImportAt: now - 2 * HOUR },
    ],
  };

  return { projects, threads, transcripts, agents, doctor };
}
