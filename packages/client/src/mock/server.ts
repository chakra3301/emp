import {
  PROTOCOL_VERSION,
  RPC_ERROR_CODES,
  type EmpNotificationName,
  type EmpNotifications,
  type Harness,
  type ImportReport,
  type ImportSourceKind,
  type AgentProfile,
  type AgentSaveParams,
  type DaemonSettings,
  type DevServer,
  type LiveThread,
  type SessionHolder,
  type SpawnSpec,
  type TerminalInfo,
  type Thread,
  type ThreadGoal,
  type ThreadGroup,
  type Schedule,
  type ScheduleWhen,
} from "@emp/protocol";
import type { Transport, TransportCloseInfo, TransportFactory } from "../transport";
import { createDemoFixture, type MockFixture } from "./fixture";

export interface MockServerOptions {
  fixture?: MockFixture;
  /** When set, `hello` must carry this token. */
  token?: string;
  daemonVersion?: string;
  /** Delay per message in each direction. 0 (default) delivers on the next microtask. */
  latencyMs?: number;
  /** Delay between `import.progress` notifications. Default 120 ms. */
  importStepMs?: number;
}

type Json = Record<string, unknown>;

class RpcFailure extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(message);
  }
}

/** JSON round-trip: the mock never shares object identity with the client, like a real wire. */
function wire<T>(v: T): T {
  return v === undefined ? v : (JSON.parse(JSON.stringify(v)) as T);
}

function shellQuote(s: string): string {
  return /^[\w@%+=:,./-]+$/.test(s) ? s : `'${s.replaceAll("'", `'\\''`)}'`;
}

/** One client↔server channel. The client side is the `Transport` interface. */
export class MockTransport implements Transport {
  readonly #server: MockServer;
  readonly #messageListeners = new Set<(msg: unknown) => void>();
  readonly #closeListeners = new Set<(info: TransportCloseInfo) => void>();
  #closed = false;
  authenticated = false;

  constructor(server: MockServer) {
    this.#server = server;
  }

  get closed(): boolean {
    return this.#closed;
  }

  send(msg: unknown): void {
    if (this.#closed) throw new Error("MockTransport is closed");
    const copy = wire(msg);
    this.#server.schedule(() => {
      if (!this.#closed) this.#server.receive(this, copy);
    });
  }

  onMessage(cb: (msg: unknown) => void): () => void {
    this.#messageListeners.add(cb);
    return () => {
      this.#messageListeners.delete(cb);
    };
  }

  onClose(cb: (info: TransportCloseInfo) => void): () => void {
    this.#closeListeners.add(cb);
    return () => {
      this.#closeListeners.delete(cb);
    };
  }

  close(): void {
    this.#shutdown({ code: 1000, reason: "client closed" });
  }

  /** Server → client. */
  deliver(msg: unknown): void {
    if (this.#closed) return;
    const copy = wire(msg);
    this.#server.schedule(() => {
      if (this.#closed) return;
      for (const cb of [...this.#messageListeners]) cb(copy);
    });
  }

  /** Server-side drop, as if the daemon went away. */
  drop(reason = "server closed"): void {
    this.#shutdown({ code: 1006, reason });
  }

  #shutdown(info: TransportCloseInfo): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#server.forget(this);
    const listeners = [...this.#closeListeners];
    this.#closeListeners.clear();
    this.#messageListeners.clear();
    for (const cb of listeners) cb(info);
  }
}

/**
 * In-memory stand-in for `empd`. Answers every protocol v1 method from a fixture and fakes
 * `import.run` with progress notifications. Browser- and Node-safe; no I/O.
 */
export class MockServer {
  readonly fixture: MockFixture;
  readonly options: MockServerOptions;
  readonly connections = new Set<MockTransport>();
  /** Every received request, for test assertions. */
  readonly received: Array<{ method: string; params: unknown }> = [];
  connectCount = 0;
  /** The next N factory calls reject, as if the daemon were down. */
  failNextConnects = 0;

  constructor(options: MockServerOptions = {}) {
    this.options = options;
    this.fixture = options.fixture ?? createDemoFixture();
    this.#seedCodex();
  }

  /** Pass to `EmpClient` / `new EmpClient(server.factory, …)`. */
  readonly factory: TransportFactory = async () => {
    if (this.failNextConnects > 0) {
      this.failNextConnects -= 1;
      throw new Error("mock daemon unavailable");
    }
    this.connectCount += 1;
    const t = new MockTransport(this);
    this.connections.add(t);
    return t;
  };

  /** Drop every open connection from the server side. */
  dropAll(reason?: string): void {
    for (const t of [...this.connections]) t.drop(reason);
  }

  notifyAll<N extends EmpNotificationName>(method: N, params: EmpNotifications[N]): void {
    for (const t of this.connections) {
      if (t.authenticated) t.deliver({ jsonrpc: "2.0", method, params });
    }
  }

  /** @internal */
  schedule(fn: () => void): void {
    const latency = this.options.latencyMs ?? 0;
    if (latency > 0) setTimeout(fn, latency);
    else queueMicrotask(fn);
  }

  /** @internal */
  forget(t: MockTransport): void {
    this.connections.delete(t);
  }

  /** @internal */
  receive(conn: MockTransport, raw: unknown): void {
    if (typeof raw !== "object" || raw === null) return;
    const msg = raw as Json;
    if (typeof msg.method !== "string" || !("id" in msg)) return;
    const id = msg.id;
    const method = msg.method;
    this.received.push({ method, params: msg.params });
    const reply = (result: unknown) => conn.deliver({ jsonrpc: "2.0", id, result: result ?? null });
    const fail = (err: unknown) => {
      const code = err instanceof RpcFailure ? err.code : RPC_ERROR_CODES.INTERNAL;
      const message = err instanceof Error ? err.message : String(err);
      conn.deliver({ jsonrpc: "2.0", id, error: { code, message } });
    };
    try {
      const result = this.#dispatch(conn, method, msg.params);
      if (result instanceof Promise) result.then(reply, fail);
      else reply(result);
    } catch (err) {
      fail(err);
    }
  }

  #dispatch(conn: MockTransport, method: string, params: unknown): unknown {
    const p = (params ?? {}) as Json;
    if (method === "hello") {
      if (p.protocolVersion !== PROTOCOL_VERSION) {
        throw new RpcFailure(RPC_ERROR_CODES.INCOMPATIBLE_VERSION, `Unsupported protocol version ${String(p.protocolVersion)}`);
      }
      if (this.options.token !== undefined && p.token !== this.options.token) {
        throw new RpcFailure(RPC_ERROR_CODES.NOT_AUTHENTICATED, "Invalid token");
      }
      conn.authenticated = true;
      return { protocolVersion: PROTOCOL_VERSION, daemonVersion: this.options.daemonVersion ?? this.fixture.doctor.daemonVersion };
    }
    if (!conn.authenticated) throw new RpcFailure(RPC_ERROR_CODES.NOT_AUTHENTICATED, "hello required");

    const f = this.fixture;
    switch (method) {
      case "doctor":
        return f.doctor;
      case "projects.list":
        return f.projects;
      case "agents.list":
        return f.agents;
      case "threads.list":
        return this.#threadsList(p);
      case "threads.get":
        return this.#thread(p);
      case "threads.transcript": {
        const t = this.#thread(p);
        const items = f.transcripts[t.id] ?? [];
        const offset = this.#uint(p.offset, 0);
        const limit = Math.min(this.#uint(p.limit, 200), 1000);
        return { items: items.slice(offset, offset + limit), total: items.length, offset };
      }
      case "threads.resumeCommand":
        return resumeSpec(this.#thread(p));
      case "attachments.locateFolder": {
        // Like empd's Spotlight search, minus the disk: a folder is wherever its name says.
        const name = String(p.name ?? "");
        if (!name || name === "Nowhere") return [];
        return name === "Site" ? ["/Users/demo/Desktop/Site", "/Users/demo/Backup/Site"] : [`/Users/demo/Desktop/${name}`];
      }
      case "attachments.locateFile": {
        // Files named like "elsewhere.*" aren't on the mock Mac; the rest are on its Desktop.
        const name = String(p.name ?? "");
        if (!name || name.startsWith("elsewhere.")) return null;
        const id = `att-${++this.#nextId}`;
        const a = { id, name, mime: null, size: Number(p.size ?? 0), path: `/Users/demo/Desktop/${name}` };
        this.#attachments.set(id, a);
        return a;
      }
      case "attachments.upload": {
        const name = String(p.name ?? "file").split("/").pop() || "file";
        const data = String(p.data ?? "");
        if (!data) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, `${name} is empty`);
        const id = `att-${++this.#nextId}`;
        const a = { id, name, mime: typeof p.mime === "string" ? p.mime : null, size: Math.floor((data.length * 3) / 4), path: `/mock/attachments/${id}/${name}` };
        this.#attachments.set(id, a);
        return a;
      }
      case "commands.list":
        return MOCK_COMMANDS[String(p.harness) as Harness] ?? [];
      case "commands.run": {
        const text = String(p.text ?? "").trim();
        if (!text.startsWith("/")) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "a slash command starts with /");
        const info = typeof p.threadId === "string" ? this.#takeover(p) : this.#openTerminal("shell", null);
        const entry = this.#terms.get(info.id)!;
        this.#termOut(entry, `${text}\r\n(mock) ${text} would run here.\r\n$ `);
        return info;
      }
      case "threads.context": {
        // Fake but stable: a few thousand tokens per transcript item.
        const t = this.#thread(p);
        const items = f.transcripts[t.id]?.length ?? 0;
        if (items === 0) return null;
        const window = t.harness === "codex" ? 500_000 : 200_000;
        return { used: Math.min(window, 6_000 + items * 4_200), window, model: t.model };
      }
      case "import.run":
        return this.#importRun(p);
      case "threads.live":
        return [...this.#live.values()].filter((l) => l.owner !== "none");
      case "threads.send":
        return this.#send(p);
      case "threads.goal": {
        const t = this.#thread(p);
        return this.#live.get(t.id)?.goal ?? this.#goals.get(t.id) ?? null;
      }
      case "threads.watch":
        return false;
      case "threads.holders":
        return this.#holders.get(this.#thread(p).id) ?? [];
      case "threads.closeElsewhere": {
        const t = this.#thread(p);
        const closed = this.#holders.get(t.id)?.length ?? 0;
        this.#holders.delete(t.id);
        return { closed, remaining: [] };
      }
      case "threads.command": {
        // Like empd for pi: `/model` alone asks which; other commands answer with a line.
        const t = this.#thread(p);
        const text = String(p.text ?? "").trim();
        const [name = "", ...rest] = text.slice(1).split(/\s+/);
        const say = (msg: string) => this.notifyAll("thread.event", { type: "item", threadId: t.id, item: { kind: "system", text: msg, ts: Date.now() } });
        say(text);
        if (name === "goal" && t.harness === "codex") {
          const arg = rest.join(" ");
          const prev = this.#live.get(t.id)?.goal ?? this.#goals.get(t.id) ?? null;
          const goal: ThreadGoal | null =
            arg === "clear" ? null
            : arg === "pause" && prev ? { ...prev, status: "paused" }
            : arg === "resume" && prev ? { ...prev, status: "active" }
            : arg && arg !== "pause" && arg !== "resume" ? { objective: arg, status: "active", tokensUsed: 0, tokenBudget: null, timeUsedSeconds: 0 }
            : prev;
          this.#goals.set(t.id, goal);
          say(goal === null ? "Goal cleared." : goal.status === "active" ? (arg === "resume" ? "Goal resumed." : `Goal set: ${goal.objective}`) : "Goal paused. /goal resume continues it.");
          return this.#setLive(t.id, { goal, status: goal?.status === "active" ? "running" : "idle" });
        }
        if (name === "model" && rest.length === 0) {
          const prev = this.#live.get(t.id)?.questions ?? [];
          const q = { id: "emp:model", kind: "select" as const, title: "Switch this chat to", message: null, options: ["anthropic/claude-sonnet-5-5", "openai/gpt-6.1-sol"], placeholder: null, prefill: null };
          return this.#setLive(t.id, { status: "needs-input", questions: [...prev.filter((x) => x.id !== q.id), q] });
        }
        if (name === "model") t.model = rest.join(" ");
        say(name === "compact" ? "Compacted: 48k → about 9k tokens." : name === "model" ? `Model: ${t.model}` : `(mock) /${name} done.`);
        return this.#setLive(t.id, {});
      }
      case "threads.answer": {
        const t = this.#thread(p);
        const live = this.#live.get(t.id);
        const q = live?.questions.find((x) => x.id === p.questionId);
        if (!live || !q) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, "that question was already answered");
        const questions = live.questions.filter((x) => x.id !== q.id);
        if (q.id === "emp:model" && typeof p.value === "string" && p.cancelled !== true) {
          t.model = p.value;
          this.notifyAll("thread.event", { type: "item", threadId: t.id, item: { kind: "system", text: `Model: ${p.value}`, ts: Date.now() } });
        }
        return this.#setLive(t.id, { questions, status: questions.length ? "needs-input" : "idle" });
      }
      case "threads.interrupt":
        return this.#setLive(this.#thread(p).id, { status: "idle" });
      case "threads.start":
        return this.#start(p);
      case "threads.takeover":
        return this.#takeover(p);
      case "threads.release": {
        const id = this.#thread(p).id;
        const l = this.#live.get(id);
        if (l?.terminalId) this.#exitTerminal(l.terminalId);
        return this.#setLive(id, { owner: "none", terminalId: null });
      }
      case "terminals.list":
        return [...this.#terms.values()].map((t) => t.info).reverse();
      case "terminals.open":
        return this.#openTerminal(
          String(p.kind ?? "shell") === "resume" ? "resume" : "shell",
          typeof p.threadId === "string" ? this.#thread(p) : null,
        );
      case "terminals.attach": {
        const t = this.#term(p);
        return { info: t.info, scrollback: btoa(t.text), seq: t.seq };
      }
      case "terminals.write": {
        const t = this.#term(p);
        const text = atob(String(p.data ?? "")).replaceAll("\r", "\r\n$ ");
        this.#termOut(t, text);
        return null;
      }
      case "terminals.resize": {
        const t = this.#term(p);
        t.info = { ...t.info, cols: this.#uint(p.cols, 80), rows: this.#uint(p.rows, 24) };
        return null;
      }
      case "servers.list":
        return this.#servers;
      case "servers.kill": {
        const pid = Number(p.pid);
        const s = this.#servers.find((x) => x.pid === pid);
        if (!s) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, "that process isn't listening on a port any more");
        if (s.protected) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "EMP's own daemon can't be stopped from here");
        this.#servers = this.#servers.filter((x) => x.pid !== pid);
        return { stopped: true };
      }
      case "terminals.kill": {
        const t = this.#term(p);
        if (t.info.state === "exited") this.#terms.delete(t.info.id);
        else this.#exitTerminal(t.info.id);
        return null;
      }
      case "models.list": {
        const harness = String(p.harness ?? "");
        const catalogue: Record<string, Array<[string, string, string]>> = {
          pi: [
            ["anthropic/claude-fable-5-1", "claude-fable-5-1", "anthropic"],
            ["openai/gpt-6.1-sol", "gpt-6.1-sol", "openai-codex"],
          ],
          codex: [["gpt-6.1-sol", "GPT-6.1-Sol", "openai"]],
          claude: [["opus", "Opus (latest)", "anthropic"]],
          antigravity: [
            ["gemini-4-argon-medium", "Gemini 4 Argon (Medium)", "google"],
            ["claude-opus-5-5-high", "Claude Opus 5.5 (High)", "anthropic"],
          ],
        };
        return (catalogue[harness] ?? []).map(([id, label, group]) => ({ id, label, group }));
      }
      case "projects.create": {
        const path = String(p.path ?? "").trim();
        if (!path.startsWith("/") && !path.startsWith("~")) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "use an absolute path or ~/…");
        const root = path.replace(/\/$/, "");
        const existing = f.projects.find((x) => x.rootPath === root);
        if (existing) return existing;
        const project = {
          id: `p-new-${++this.#nextId}`,
          name: typeof p.name === "string" && p.name.trim() ? p.name.trim() : (root.split("/").pop() ?? root),
          rootPath: root,
          threadCount: 0,
          automatedCount: 0,
          lastActivityAt: null,
        };
        f.projects.unshift(project);
        return project;
      }
      case "projects.files": {
        const project = f.projects.find((x) => x.id === p.projectId);
        if (!project) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, `no project ${String(p.projectId)}`);
        const entries = [
          { name: "src", isDir: true, size: null, modifiedAt: null },
          { name: "README.md", isDir: false, size: 2048, modifiedAt: null },
          { name: "package.json", isDir: false, size: 512, modifiedAt: null },
        ];
        return { rootPath: project.rootPath, exists: true, entries, truncated: false };
      }
      case "groups.list":
        return this.#groupList();
      case "groups.create": {
        const g = { id: `g-${++this.#nextId}`, name: this.#groupName(p.name), pinned: p.pinned === true, createdAt: Date.now() + this.#nextId, threadIds: [] };
        this.#groups.push(g);
        this.#moveInto(g.id, p.threadIds);
        return this.#groupsChanged();
      }
      case "groups.update": {
        const g = this.#group(p);
        if (p.name !== undefined) g.name = this.#groupName(p.name);
        if (typeof p.pinned === "boolean") g.pinned = p.pinned;
        return this.#groupsChanged();
      }
      case "groups.delete": {
        const g = this.#group(p);
        this.#groups = this.#groups.filter((x) => x !== g);
        return this.#groupsChanged();
      }
      case "groups.add":
        this.#moveInto(this.#group(p).id, p.threadIds);
        return this.#groupsChanged();
      case "groups.remove":
        for (const g of this.#groups) g.threadIds = g.threadIds.filter((id) => id !== p.threadId);
        return this.#groupsChanged();
      case "schedules.list":
        return this.#scheduleList();
      case "schedules.save":
        return this.#saveSchedule(p);
      case "schedules.delete": {
        const before = this.#schedules.length;
        this.#schedules = this.#schedules.filter((x) => x.id !== p.scheduleId);
        if (this.#schedules.length === before) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, `no schedule ${String(p.scheduleId)}`);
        this.#schedulesChanged();
        return null;
      }
      case "schedules.enable": {
        const s = this.#schedule(p);
        s.enabled = p.enabled === true;
        s.nextRunAt = s.enabled ? mockNext(s.when) : null;
        if (s.enabled) {
          s.ended = null;
          s.failStreak = 0;
        }
        this.#schedulesChanged();
        return { ...s };
      }
      case "schedules.run": {
        const s = this.#schedule(p);
        if (s.lastStatus === "running") throw new RpcFailure(RPC_ERROR_CODES.BUSY, "it's running already");
        Object.assign(s, { lastStatus: "running", lastRunAt: Date.now(), runCount: s.runCount + 1, lastThreadId: s.threadId ?? this.fixture.threads[0]?.id ?? null });
        this.#schedulesChanged();
        setTimeout(() => {
          s.lastStatus = "done";
          this.#schedulesChanged();
        }, 1500);
        return { ...s };
      }
      case "schedules.external":
        return [
          {
            source: "hermes",
            profile: "studio",
            id: "3773f1ccbd2a",
            name: "monthly-skill-review",
            summary: "Monthly on the 1st at 10:00 AM",
            enabled: true,
            state: "scheduled",
            nextRunAt: Date.now() + 11 * 86_400_000,
            lastRunAt: Date.now() - 11 * 86_400_000,
            lastStatus: "ok",
            lastError: null,
            task: "Runs monthly-skill-review.sh",
            deliver: "telegram",
          },
          {
            source: "hermes",
            profile: "default",
            id: "a1",
            name: "standup notes",
            summary: "every 1d",
            enabled: false,
            state: "paused",
            nextRunAt: null,
            lastRunAt: null,
            lastStatus: null,
            lastError: null,
            task: "Summarise what changed in my repos yesterday",
            deliver: "local",
          },
        ];
      case "schedules.preview": {
        const when = p.when as ScheduleWhen;
        if (when.kind === "cron" && when.expr.trim().split(/\s+/).length !== 5) {
          return { error: "a cron expression has 5 fields (minute hour day month weekday)", summary: "", next: [] };
        }
        if (when.kind === "once" && when.at <= Date.now()) return { error: "That time has already passed.", summary: "", next: [] };
        const first = mockNext(when)!;
        const step = when.kind === "every" ? when.minutes * 60_000 : 86_400_000;
        return { error: null, summary: mockSummary(when), next: when.kind === "once" ? [first] : [first, first + step, first + 2 * step] };
      }
      case "settings.get":
        return { ...this.#settings };
      case "settings.set": {
        const n = p.compactAt;
        if (n !== null && n !== undefined && (typeof n !== "number" || n < 50_000 || n > 1_000_000)) {
          throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "compactAt must be between 50000 and 1000000 tokens");
        }
        this.#settings = { compactAt: typeof n === "number" ? n : null };
        this.notifyAll("settings.changed", { ...this.#settings });
        return { ...this.#settings };
      }
      case "agents.save":
        return this.#saveAgent(p as unknown as AgentSaveParams);
      case "agents.delete": {
        const before = f.agents.length;
        f.agents = f.agents.filter((a) => a.id !== p.agentId);
        if (f.agents.length === before) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, "no such agent");
        return null;
      }
      default:
        throw new RpcFailure(RPC_ERROR_CODES.METHOD_NOT_FOUND, `Unknown method ${method}`);
    }
  }

  // ---- simulated live sessions and terminals (demo + tests) ----

  #settings: DaemonSettings = { compactAt: null };
  #schedules: Schedule[] = [];

  #scheduleList(): Schedule[] {
    return this.#schedules
      .map((s) => ({ ...s, threadTitle: s.threadId ? (this.fixture.threads.find((t) => t.id === s.threadId)?.title ?? null) : null }))
      .sort((a, b) => Number(b.enabled) - Number(a.enabled) || (a.nextRunAt ?? Infinity) - (b.nextRunAt ?? Infinity));
  }

  #schedulesChanged() {
    this.notifyAll("schedules.changed", this.#scheduleList());
  }

  #schedule(p: Json): Schedule {
    const s = this.#schedules.find((x) => x.id === p.scheduleId);
    if (!s) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, `no schedule ${String(p.scheduleId)}`);
    return s;
  }

  #saveSchedule(p: Json): Schedule {
    const prompt = typeof p.prompt === "string" ? p.prompt.trim() : "";
    if (!prompt) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "say what the agent should do each run");
    const threadId = typeof p.threadId === "string" && p.threadId ? p.threadId : null;
    const agentId = threadId ? null : typeof p.agentId === "string" && p.agentId ? p.agentId : null;
    if (!threadId && !agentId) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "pick the agent that runs it");
    const when = p.when as ScheduleWhen;
    const old = typeof p.id === "string" ? this.#schedule({ scheduleId: p.id }) : null;
    const enabled = p.enabled !== false;
    const s: Schedule = {
      ...(old ?? { lastRunAt: null, lastStatus: null, lastError: null, lastThreadId: null, runCount: 0, createdAt: Date.now() }),
      endsAt: when.kind !== "once" && typeof p.endsAt === "number" ? p.endsAt : null,
      maxRuns: when.kind !== "once" && typeof p.maxRuns === "number" ? p.maxRuns : null,
      stopWhen: when.kind !== "once" && typeof p.stopWhen === "string" && p.stopWhen.trim() ? p.stopWhen.trim() : null,
      ended: enabled ? null : (old?.ended ?? null),
      failStreak: 0,
      id: old?.id ?? `s-${++this.#nextId}`,
      title: typeof p.title === "string" && p.title.trim() ? p.title.trim() : prompt.split("\n")[0]!.slice(0, 60),
      prompt,
      when,
      summary: mockSummary(when),
      agentId,
      cwd: typeof p.cwd === "string" && p.cwd ? p.cwd : null,
      projectId: typeof p.projectId === "string" && p.projectId ? p.projectId : null,
      threadId,
      threadTitle: null,
      enabled,
      nextRunAt: enabled ? mockNext(when) : null,
    };
    this.#schedules = old ? this.#schedules.map((x) => (x === old ? s : x)) : [...this.#schedules, s];
    this.#schedulesChanged();
    return s;
  }
  /** Groups as stored: thread ids, not threads. */
  #groups: Array<{ id: string; name: string; pinned: boolean; createdAt: number; threadIds: string[] }> = [];

  #groupList(): ThreadGroup[] {
    const threads = new Map(this.fixture.threads.map((t) => [t.id, t]));
    return [...this.#groups]
      .sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.createdAt - b.createdAt)
      .map(({ threadIds, ...g }) => ({
        ...g,
        threads: threadIds
          .map((id) => threads.get(id))
          .filter((t): t is Thread => !!t)
          .sort((a, b) => b.updatedAt - a.updatedAt),
      }));
  }

  #groupsChanged(): ThreadGroup[] {
    const list = this.#groupList();
    this.notifyAll("groups.changed", list);
    return list;
  }

  #group(p: Json) {
    const g = this.#groups.find((x) => x.id === p.groupId);
    if (!g) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, `no group ${String(p.groupId)}`);
    return g;
  }

  #groupName(raw: unknown): string {
    const name = typeof raw === "string" ? raw.trim().slice(0, 60) : "";
    if (!name) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "a group needs a name");
    return name;
  }

  #moveInto(groupId: string, threadIds: unknown) {
    const ids = (Array.isArray(threadIds) ? threadIds : []).filter((id): id is string => typeof id === "string" && this.fixture.threads.some((t) => t.id === id));
    for (const g of this.#groups) g.threadIds = g.threadIds.filter((id) => !ids.includes(id));
    this.#groups.find((g) => g.id === groupId)!.threadIds.push(...ids);
  }
  readonly #live = new Map<string, LiveThread>();
  /** Codex goals and the Terminal window holding one session, like the owner's XDesign thread. */
  readonly #goals = new Map<string, ThreadGoal | null>();
  readonly #holders = new Map<string, SessionHolder[]>();
  #seedCodex() {
    const t = this.fixture.threads.find((x) => x.harness === "codex" && !x.parentId);
    this.#seedSchedules(t?.id ?? null);
    if (!t) return;
    this.#goals.set(t.id, {
      objective: "Complete XDesign inside Orion Terminal as the unified Photoshop/Figma replacement, covering the remaining prototyping, tool-option and RAW work.",
      status: "paused",
      tokensUsed: 57_981_379,
      tokenBudget: null,
      timeUsedSeconds: 294_614,
    });
    this.#holders.set(t.id, [{ pid: 41923, app: "Terminal", command: `codex resume ${t.session.sessionId}` }]);
  }
  #seedSchedules(codexThread: string | null) {
    const now = Date.now();
    const base = {
      cwd: null,
      projectId: null,
      threadTitle: null,
      lastError: null,
      lastThreadId: null,
      endsAt: null,
      maxRuns: null,
      stopWhen: null,
      ended: null,
      failStreak: 0,
      createdAt: now - 7 * 86_400_000,
    };
    this.#schedules = [
      {
        ...base,
        id: "s-build",
        title: "Morning build check",
        prompt: "Run the test suite on main. Fix anything simple; list what's still broken. If everything passes, just say so.",
        when: { kind: "cron", expr: "0 9 * * 1-5" },
        summary: "Weekdays at 9:00 AM",
        agentId: "a-reviewer",
        threadId: null,
        enabled: true,
        nextRunAt: now + 5 * 3_600_000,
        lastRunAt: now - 19 * 3_600_000,
        lastStatus: "done",
        lastThreadId: this.fixture.threads[0]?.id ?? null,
        runCount: 12,
      },
      ...(codexThread
        ? [
            {
              ...base,
              id: "s-goal",
              title: "Nudge the XDesign goal",
              prompt: "Pick the goal back up where you left it and report progress in three lines.",
              when: { kind: "cron", expr: "0 18 * * *" } as ScheduleWhen,
              summary: "Daily at 6:00 PM",
              agentId: null,
              threadId: codexThread,
              stopWhen: "the goal is complete",
              enabled: true,
              nextRunAt: now + 2 * 3_600_000,
              lastRunAt: null,
              lastStatus: null,
              runCount: 0,
            },
          ]
        : []),
      {
        ...base,
        id: "s-deps",
        title: "Weekly dependency review",
        prompt: "Check for outdated dependencies and open a short report.",
        when: { kind: "cron", expr: "0 10 * * 1" },
        summary: "Mondays at 10:00 AM",
        agentId: "a-worker",
        threadId: null,
        enabled: false,
        nextRunAt: null,
        lastRunAt: now - 3 * 86_400_000,
        lastStatus: "failed",
        lastError: "codex exited before opening a session; see Import & Doctor",
        runCount: 3,
        failStreak: 3,
      },
      {
        ...base,
        id: "s-deploy",
        title: "Watch the staging deploy",
        prompt: "Check whether the staging deploy finished and the smoke tests passed.",
        when: { kind: "every", minutes: 15 },
        summary: "Every 15 minutes",
        agentId: "a-reviewer",
        threadId: null,
        enabled: false,
        nextRunAt: null,
        lastRunAt: now - 26 * 3_600_000,
        lastStatus: "done",
        lastThreadId: this.fixture.threads[1]?.id ?? null,
        runCount: 6,
        stopWhen: "the deploy is live and the smoke tests pass",
        ended: "Condition met",
      },
    ];
  }

  readonly #terms = new Map<string, { info: TerminalInfo; text: string; seq: number }>();
  #servers: DevServer[] = mockServers(Date.now());
  #nextId = 0;

  #setLive(threadId: string, patch: Partial<LiveThread>): LiveThread {
    const prev = this.#live.get(threadId) ?? { threadId, status: "idle", owner: "driver", terminalId: null, queued: [], error: null, questions: [], goal: this.#goals.get(threadId) ?? null };
    const live: LiveThread = { ...prev, ...patch };
    this.#live.set(threadId, live);
    this.notifyAll("thread.event", { type: "state", live });
    return live;
  }

  /** The text plus one `[Attached: path]` line per uploaded file, like empd. */
  #withAttachments(p: Json): string {
    const ids = Array.isArray(p.attachments) ? p.attachments.map(String) : [];
    const lines = ids.map((id) => {
      const a = this.#attachments.get(id);
      if (!a) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, `unknown attachment ${id}`);
      return `[Attached: ${a.path}]`;
    });
    const text = String(p.text ?? "").trim();
    return [text, lines.join("\n")].filter(Boolean).join("\n\n");
  }

  #attachments = new Map<string, { id: string; name: string; mime: string | null; size: number; path: string }>();

  #send(p: Json): LiveThread {
    const t = this.#thread(p);
    const text = this.#withAttachments(p);
    if (!text) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "empty message");
    if (this.#live.get(t.id)?.owner === "terminal") throw new RpcFailure(RPC_ERROR_CODES.BUSY, "this session is open in a terminal");
    const live = this.#setLive(t.id, { owner: "driver", status: "running" });
    const items = (this.fixture.transcripts[t.id] ??= []);
    // Like empd: a model pick switches the chat and is kept as its model.
    if (typeof p.model === "string" && p.model && p.model !== t.model) {
      t.model = p.model;
      const note = { kind: "system" as const, text: `Model: ${p.model}`, ts: Date.now() };
      items.push(note);
      this.notifyAll("thread.event", { type: "item", threadId: t.id, item: note });
    }
    // Like empd: the coordinator gets the owner's page appended.
    const page = this.#isCoordinatorThread(t) && typeof p.context === "string" ? `\n\n<emp-context>\nOwner is viewing: ${p.context}\n</emp-context>` : "";
    const user = { kind: "user" as const, text: text + page, ts: Date.now() };
    items.push(user);
    this.notifyAll("thread.event", { type: "item", threadId: t.id, item: user });
    const reply = `(mock ${t.harness}) Got it: "${text.slice(0, 80)}". This is the demo daemon; nothing ran.`;
    const step = this.options.importStepMs ?? 120;
    setTimeout(() => this.notifyAll("thread.event", { type: "partial", threadId: t.id, kind: "assistant", text: reply.slice(0, 20) }), step);
    setTimeout(() => {
      const item = { kind: "assistant" as const, text: reply, model: t.model, ts: Date.now() };
      items.push(item);
      this.notifyAll("thread.event", { type: "item", threadId: t.id, item });
      this.#setLive(t.id, { status: "idle" });
      t.updatedAt = Date.now();
      t.messageCount = items.length;
      this.notifyAll("thread.event", { type: "transcriptChanged", threadId: t.id });
    }, 2 * step);
    return live;
  }

  #start(p: Json): Thread {
    const agent = this.fixture.agents.find((a) => a.id === p.agentId);
    if (!agent) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, "no such agent");
    const now = Date.now();
    const id = `t-new-${++this.#nextId}`;
    const cwd = agent.coordinator ? (agent.cwd ?? "~") : typeof p.cwd === "string" ? p.cwd : (agent.cwd ?? "~");
    const thread: Thread = {
      id,
      harness: agent.harness,
      title: String(p.text ?? "").slice(0, 80) || "New thread",
      projectId: typeof p.projectId === "string" ? p.projectId : null,
      cwd,
      model: agent.model,
      agentId: agent.id,
      parentId: null,
      childCount: 0,
      assignedBy: null,
      status: "idle",
      automated: false,
      session: { harness: agent.harness, sessionId: `mock-${id}`, path: null, home: null, profile: agent.harnessProfile },
      messageCount: 0,
      createdAt: now,
      updatedAt: now,
      importedFrom: null,
    };
    this.fixture.threads.unshift(thread);
    this.fixture.transcripts[id] = [];
    this.#send({ threadId: id, text: p.text, attachments: p.attachments, context: p.context });
    return thread;
  }

  #takeover(p: Json): TerminalInfo {
    const t = this.#thread(p);
    if (this.#live.get(t.id)?.status === "running") throw new RpcFailure(RPC_ERROR_CODES.BUSY, "a turn is running");
    const info = this.#openTerminal("resume", t);
    this.#setLive(t.id, { owner: "terminal", terminalId: info.id, status: "idle" });
    return info;
  }

  #openTerminal(kind: "shell" | "resume", thread: Thread | null): TerminalInfo {
    const id = `term-${++this.#nextId}`;
    const command = kind === "resume" && thread ? (resumeSpec(thread)?.display ?? "resume") : "/bin/zsh -l";
    const info: TerminalInfo = {
      id,
      kind,
      title: kind === "resume" && thread ? `${thread.harness} · ${thread.title.slice(0, 40)}` : "Shell · demo",
      threadId: thread?.id ?? null,
      cwd: thread?.cwd ?? "~",
      command,
      pid: 4242,
      state: "running",
      exitCode: null,
      cols: 120,
      rows: 36,
      createdAt: Date.now(),
    };
    const entry = { info, text: "", seq: 0 };
    this.#terms.set(id, entry);
    this.notifyAll("terminal.state", info);
    this.#termOut(entry, `EMP demo terminal: no process runs here.\r\n${command}\r\n$ `);
    return info;
  }

  #term(p: Json) {
    const t = typeof p.terminalId === "string" ? this.#terms.get(p.terminalId) : undefined;
    if (!t) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, "no such terminal");
    return t;
  }

  #termOut(t: { info: TerminalInfo; text: string; seq: number }, text: string) {
    t.text += text;
    t.seq += 1;
    this.notifyAll("terminal.output", { terminalId: t.info.id, data: btoa(text), seq: t.seq });
  }

  #exitTerminal(id: string) {
    const t = this.#terms.get(id);
    if (!t || t.info.state === "exited") return;
    t.info = { ...t.info, state: "exited", exitCode: 0 };
    this.notifyAll("terminal.state", t.info);
  }

  #saveAgent(d: AgentSaveParams): AgentProfile {
    if (!d.name?.trim()) throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "name is required");
    const agents = this.fixture.agents;
    const existing = d.id ? agents.find((a) => a.id === d.id) : undefined;
    if (d.id && !existing) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, "no such agent");
    const saved: AgentProfile = {
      id: existing?.id ?? `a-new-${++this.#nextId}`,
      name: d.name.trim(),
      harness: d.harness,
      model: d.model,
      effort: d.effort,
      role: d.role,
      instructions: d.instructions,
      cwd: d.cwd,
      autonomy: Math.min(3, d.autonomy),
      harnessProfile: d.harnessProfile,
      compactAt: d.compactAt ?? null,
      importedFrom: existing?.importedFrom ?? null,
      coordinator: existing?.coordinator ?? false,
    };
    if (existing) agents[agents.indexOf(existing)] = saved;
    else agents.push(saved);
    return saved;
  }

  #uint(v: unknown, fallback: number): number {
    if (v === undefined || v === null) return fallback;
    if (typeof v !== "number" || !Number.isInteger(v) || v < 0) {
      throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "Expected a non-negative integer");
    }
    return v;
  }

  #thread(p: Json): Thread {
    if (typeof p.threadId !== "string") throw new RpcFailure(RPC_ERROR_CODES.INVALID_PARAMS, "threadId required");
    const t = this.fixture.threads.find((x) => x.id === p.threadId);
    if (!t) throw new RpcFailure(RPC_ERROR_CODES.NOT_FOUND, `Thread ${p.threadId} not found`);
    return t;
  }

  #isCoordinatorThread(t: Thread): boolean {
    return this.fixture.agents.some((a) => a.coordinator && a.id === t.agentId);
  }

  #threadsList(p: Json) {
    const query = typeof p.query === "string" ? p.query.trim().toLowerCase() : "";
    const offset = this.#uint(p.offset, 0);
    const limit = Math.min(this.#uint(p.limit, 100), 500);
    const parent = typeof p.parentId === "string" ? p.parentId : null;
    const items = this.fixture.threads
      .filter((t) => (parent ? t.parentId === parent : p.includeAutomated === true || !t.automated))
      .filter((t) => (p.topLevel === true ? !t.parentId : true))
      .filter((t) => (typeof p.coordinator === "boolean" ? this.#isCoordinatorThread(t) === p.coordinator : true))
      .filter((t) => (p.projectId ? t.projectId === p.projectId : true))
      .filter((t) => (p.harness ? t.harness === (p.harness as Harness) : true))
      .filter((t) => (p.ungrouped === true ? !this.#groups.some((g) => g.threadIds.includes(t.id)) : true))
      .filter((t) => !query || t.title.toLowerCase().includes(query) || (t.cwd ?? "").toLowerCase().includes(query))
      .sort((a, b) => b.updatedAt - a.updatedAt);
    return { items: items.slice(offset, offset + limit), total: items.length, offset };
  }

  async #importRun(p: Json): Promise<ImportReport> {
    const requested = Array.isArray(p.sources) ? (p.sources as ImportSourceKind[]) : [];
    const sources = requested.length ? requested : this.fixture.doctor.sources.map((s) => s.source);
    const step = this.options.importStepMs ?? 120;
    const wait = () => new Promise<void>((r) => setTimeout(r, step));
    const report: ImportReport = { sources: [] };
    for (const source of sources) {
      const status = this.fixture.doctor.sources.find((s) => s.source === source);
      this.notifyAll("import.progress", { source, message: status?.found ? `scanning ${status.paths.join(", ")}` : "not found, skipping" });
      await wait();
      const seen = status?.found ? status.threadCount : 0;
      if (status?.found) {
        this.notifyAll("import.progress", { source, message: `${seen} threads indexed` });
        status.lastImportAt = Date.now();
        await wait();
      }
      report.sources.push({
        source,
        stats: {
          threadsSeen: seen,
          threadsAdded: 0,
          threadsUpdated: 0,
          threadsUnchanged: seen,
          agentsSeen: status?.agentCount ?? 0,
          skipped: 0,
          warnings: [],
        },
        error: null,
        durationMs: status?.found ? 2 * step : step,
      });
    }
    this.notifyAll("import.finished", report);
    return report;
  }
}

function resumeSpec(t: Thread): SpawnSpec | null {
  const id = t.session.sessionId;
  let program: string;
  let args: string[];
  switch (t.harness) {
    case "pi":
      program = "pi";
      args = t.session.path ? ["--session", t.session.path] : ["--resume"];
      break;
    case "claude":
      program = "claude";
      args = ["--resume", id];
      break;
    case "codex":
      program = "codex";
      args = ["resume", id];
      break;
    case "hermes":
      program = "hermes";
      args = [...(t.session.profile ? ["-p", t.session.profile] : []), "--resume", id];
      break;
    default:
      return null;
  }
  const cmd = [program, ...args].map(shellQuote).join(" ");
  return {
    program,
    args,
    cwd: t.cwd,
    env: [],
    display: t.cwd ? `cd ${shellQuote(t.cwd)} && ${cmd}` : cmd,
  };
}

/** Shorthand: a fresh server's factory. */
export function createMockTransportFactory(options?: MockServerOptions): TransportFactory {
  return new MockServer(options).factory;
}

const cmd = (name: string, run: "chat" | "terminal" | "native", description: string | null, source: string) => ({ name, run, description, source });

/** A few of each kind, like `commands.list` returns. */
const MOCK_COMMANDS: Partial<Record<Harness, Array<ReturnType<typeof cmd>>>> = {
  pi: [
    cmd("compact", "native", "Compact the context now", "built-in"),
    cmd("copy", "native", "Copy the last reply", "built-in"),
    cmd("fix-tests", "chat", "Fix failing tests", "prompt"),
    cmd("login", "terminal", "Sign in to a provider", "built-in"),
    cmd("model", "native", "Select a model", "built-in"),
    cmd("new", "native", "Start a new session", "built-in"),
    cmd("settings", "terminal", "Open settings", "built-in"),
    cmd("skill:frontend-design", "chat", "Distinctive, intentional visual design", "skill"),
    cmd("skill:wiki", "chat", "Build and maintain the LLM wiki", "skill"),
  ],
  claude: [
    cmd("compact", "chat", null, "command"),
    cmd("login", "terminal", "Sign in to your Anthropic account", "built-in"),
    cmd("review", "chat", null, "command"),
  ],
  codex: [
    cmd("logout", "terminal", "Sign out of Codex", "built-in"),
    cmd("model", "terminal", "Choose the model and reasoning effort", "built-in"),
  ],
};

/** A few listeners of each kind, like `servers.list` on a busy Mac. */
function mockServers(now: number): DevServer[] {
  const HOUR = 3_600_000;
  const base = { ppid: 1, exposed: false, connections: 0, memory: 48 * 1024 * 1024, terminalId: null, protected: false };
  return [
    { ...base, pid: 4101, name: "vite", command: "node /Users/demo/code/emp/node_modules/.bin/vite", cwd: "/Users/demo/code/emp", ports: [5173], kind: "dev", connections: 2, startedAt: now - 2 * HOUR },
    { ...base, pid: 4102, name: "next-server (v16.3.4)", command: "next-server (v16.3.4)", cwd: "/Users/demo/code/site", ports: [3000], kind: "dev", startedAt: now - 30 * HOUR, memory: 410 * 1024 * 1024 },
    { ...base, pid: 4103, name: "vite preview", command: "node /Users/demo/code/site/node_modules/.bin/vite preview --port 4173", cwd: "/Users/demo/code/site", ports: [4173], kind: "dev", startedAt: now - 96 * HOUR },
    { ...base, pid: 4104, name: "python -m http.server", command: "python3 -m http.server 8000", cwd: "/tmp/preview", ports: [8000], kind: "dev", exposed: true, startedAt: now - 5 * 24 * HOUR, memory: 12 * 1024 * 1024 },
    { ...base, pid: 4201, name: "postgres", command: "/opt/homebrew/opt/postgresql@16/bin/postgres -D /opt/homebrew/var/postgresql@16", cwd: "/opt/homebrew/var/postgresql@16", ports: [5432], kind: "service", startedAt: now - 20 * 24 * HOUR },
    { ...base, pid: 4301, name: "codex", command: "codex", cwd: "/Users/demo/code/emp", ports: [60222], kind: "agent", startedAt: now - 3 * HOUR },
    { ...base, pid: 4302, name: "empd serve", command: "empd serve", cwd: "/", ports: [47357], kind: "agent", connections: 1, startedAt: now - 3 * HOUR, protected: true },
    { ...base, pid: 4401, name: "ControlCenter", command: "/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter", cwd: "/", ports: [5000, 7000], kind: "app", exposed: true, startedAt: now - 40 * 24 * HOUR },
  ];
}

/** The mock's stand-in for empd's timetable words. */
function mockSummary(when: ScheduleWhen): string {
  if (when.kind === "once") return `Once, ${new Date(when.at).toLocaleString()}`;
  if (when.kind === "every") return when.minutes % 60 === 0 ? `Every ${when.minutes / 60} hours` : `Every ${when.minutes} minutes`;
  const known: Record<string, string> = { "0 9 * * 1-5": "Weekdays at 9:00 AM", "0 9 * * *": "Daily at 9:00 AM", "0 18 * * *": "Daily at 6:00 PM" };
  return known[when.expr.trim()] ?? `Cron ${when.expr.trim()}`;
}

function mockNext(when: ScheduleWhen): number | null {
  const now = Date.now();
  if (when.kind === "once") return when.at > now ? when.at : null;
  if (when.kind === "every") return now + when.minutes * 60_000;
  return now + 3_600_000;
}
