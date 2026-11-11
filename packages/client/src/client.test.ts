import { PROTOCOL_VERSION, RPC_ERROR_CODES, type ImportProgress } from "@emp/protocol";
import { afterEach, describe, expect, it, vi } from "vitest";
import { EmpClient, type EmpClientOptions } from "./client";
import { EmpConnectionError, EmpRpcError, EmpTimeoutError } from "./errors";
import { MockServer } from "./mock";
import type { Transport, TransportCloseInfo } from "./transport";

const OPTS: EmpClientOptions = { token: "secret", client: { kind: "browser", version: "test" } };

/** A transport the test drives by hand: inspect what was sent, reply in any order. */
class ScriptedTransport implements Transport {
  sent: Array<{ id: number; method: string; params?: unknown }> = [];
  #onMessage = new Set<(m: unknown) => void>();
  #onClose = new Set<(i: TransportCloseInfo) => void>();
  constructor(readonly autoHello = true) {}
  send(msg: unknown) {
    const m = msg as { id: number; method: string; params?: unknown };
    this.sent.push(m);
    if (this.autoHello && m.method === "hello") {
      this.push({ jsonrpc: "2.0", id: m.id, result: { protocolVersion: PROTOCOL_VERSION, daemonVersion: "x" } });
    }
  }
  onMessage(cb: (m: unknown) => void) {
    this.#onMessage.add(cb);
    return () => this.#onMessage.delete(cb) as unknown as void;
  }
  onClose(cb: (i: TransportCloseInfo) => void) {
    this.#onClose.add(cb);
    return () => this.#onClose.delete(cb) as unknown as void;
  }
  close() {
    for (const cb of [...this.#onClose]) cb({ reason: "closed by test" });
    this.#onClose.clear();
  }
  push(msg: unknown) {
    queueMicrotask(() => {
      for (const cb of [...this.#onMessage]) cb(msg);
    });
  }
  lastId(method: string) {
    return [...this.sent].reverse().find((m) => m.method === method)!.id;
  }
}

const flush = () => new Promise((r) => setTimeout(r, 0));

let client: EmpClient | undefined;
afterEach(() => {
  client?.dispose();
  client = undefined;
  vi.useRealTimers();
});

describe("EmpClient", () => {
  it("performs hello with token, client info and protocol version", async () => {
    const server = new MockServer({ token: "secret" });
    client = await EmpClient.connect(server.factory, OPTS);
    expect(client.status.state).toBe("ready");
    expect(client.daemonVersion).toBe("0.1.0-mock");
    expect(server.received[0]).toEqual({
      method: "hello",
      params: { protocolVersion: PROTOCOL_VERSION, token: "secret", client: { kind: "browser", version: "test" } },
    });
  });

  it("evaluates a token function on connect", async () => {
    const server = new MockServer({ token: "fresh" });
    client = await EmpClient.connect(server.factory, { ...OPTS, token: async () => "fresh" });
    expect(client.status.state).toBe("ready");
  });

  it("treats a bad token as fatal: rejects, state error, no reconnect", async () => {
    const server = new MockServer({ token: "secret" });
    const c = new EmpClient(server.factory, { ...OPTS, token: "wrong" });
    client = c;
    const err = await c.connect().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(EmpRpcError);
    expect((err as EmpRpcError).code).toBe(RPC_ERROR_CODES.NOT_AUTHENTICATED);
    expect(c.status.state).toBe("error");
    await flush();
    expect(server.connectCount).toBe(1);
    await expect(c.request("doctor")).rejects.toBeInstanceOf(EmpConnectionError);
  });

  it("correlates responses by id regardless of arrival order", async () => {
    const t = new ScriptedTransport();
    client = await EmpClient.connect(async () => t, OPTS);
    const a = client.request("threads.get", { threadId: "a" });
    const b = client.request("threads.get", { threadId: "b" });
    const [idA, idB] = t.sent.slice(1).map((m) => m.id);
    expect(idA).not.toBe(idB);
    t.push({ jsonrpc: "2.0", id: idB, result: { id: "b" } });
    t.push({ jsonrpc: "2.0", id: idA, result: { id: "a" } });
    expect(await a).toEqual({ id: "a" });
    expect(await b).toEqual({ id: "b" });
  });

  it("omits params for parameterless methods and passes them otherwise", async () => {
    const t = new ScriptedTransport();
    client = await EmpClient.connect(async () => t, OPTS);
    void client.request("projects.list").catch(() => {});
    void client.request("threads.list", { limit: 5 }).catch(() => {});
    expect(t.sent[1]).toEqual({ jsonrpc: "2.0", id: t.sent[1]!.id, method: "projects.list" });
    expect(t.sent[2]).toMatchObject({ method: "threads.list", params: { limit: 5 } });
  });

  it("maps RpcError responses to EmpRpcError with code, data and method", async () => {
    const server = new MockServer();
    client = await EmpClient.connect(server.factory, OPTS);
    const err = await client.request("threads.get", { threadId: "missing" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(EmpRpcError);
    expect(err).toMatchObject({ code: RPC_ERROR_CODES.NOT_FOUND, method: "threads.get" });

    const t = new ScriptedTransport();
    const c2 = await EmpClient.connect(async () => t, OPTS);
    const p = c2.request("doctor");
    t.push({ jsonrpc: "2.0", id: t.lastId("doctor"), error: { code: -32603, message: "boom", data: { why: 1 } } });
    await expect(p).rejects.toMatchObject({ code: -32603, message: "boom", data: { why: 1 } });
    c2.dispose();
  });

  it("delivers notifications to subscribers until they unsubscribe", async () => {
    const server = new MockServer({ importStepMs: 0 });
    client = await EmpClient.connect(server.factory, OPTS);
    const progress: ImportProgress[] = [];
    const off = client.on("import.progress", (p) => progress.push(p));
    const finished = vi.fn();
    client.on("import.finished", finished);
    const report = await client.request("import.run", { sources: ["pi", "orion"] });
    await flush();
    expect(report.sources.map((s) => s.source)).toEqual(["pi", "orion"]);
    expect(progress.map((p) => p.source)).toEqual(["pi", "pi", "orion"]);
    expect(finished).toHaveBeenCalledWith(report);

    off();
    await client.request("import.run", { sources: ["pi"] });
    await flush();
    expect(progress).toHaveLength(3);
    expect(finished).toHaveBeenCalledTimes(2);
  });

  it("rejects in-flight requests when the connection closes", async () => {
    const t = new ScriptedTransport();
    client = new EmpClient(async () => t, { ...OPTS, reconnect: false });
    await client.connect();
    const p = client.request("doctor");
    t.close();
    await expect(p).rejects.toBeInstanceOf(EmpConnectionError);
    expect(client.status.state).toBe("error");
  });

  it("times out requests that get no answer", async () => {
    vi.useFakeTimers();
    const t = new ScriptedTransport();
    client = new EmpClient(async () => t, OPTS);
    await client.connect();
    const p = client.request("doctor", undefined, { timeoutMs: 1000 });
    const assertion = expect(p).rejects.toBeInstanceOf(EmpTimeoutError);
    await vi.advanceTimersByTimeAsync(1000);
    await assertion;
  });

  it("does not time out import.run by default", async () => {
    vi.useFakeTimers();
    const t = new ScriptedTransport();
    client = new EmpClient(async () => t, { ...OPTS, requestTimeoutMs: 100 });
    await client.connect();
    let settled = false;
    void client.request("import.run", { sources: [] }).finally(() => (settled = true)).catch(() => {});
    await vi.advanceTimersByTimeAsync(10_000);
    expect(settled).toBe(false);
  });

  it("reconnects with backoff, re-does hello, and sends requests queued while down", async () => {
    vi.useFakeTimers();
    const server = new MockServer();
    client = new EmpClient(server.factory, { ...OPTS, reconnect: { initialDelayMs: 100, factor: 2, maxDelayMs: 1000 } });
    const states: string[] = [];
    client.onStateChange((s) => states.push(s.state));
    await client.connect();

    server.failNextConnects = 1;
    server.dropAll();
    expect(client.status.state).toBe("closed");
    expect(client.status.retryAt).toBeDefined();

    const queued = client.request("projects.list");
    await vi.advanceTimersByTimeAsync(100); // attempt 1 fails → retry in 200 ms
    expect(client.status).toMatchObject({ state: "closed", attempt: 2 });
    await vi.advanceTimersByTimeAsync(200);
    expect(client.status.state).toBe("ready");
    expect(server.connectCount).toBe(2);
    expect(server.received.filter((r) => r.method === "hello")).toHaveLength(2);
    expect(await queued).toHaveLength(server.fixture.projects.length);
    expect(states).toEqual(["connecting", "ready", "closed", "connecting", "closed", "connecting", "ready"]);
  });

  it("dispose rejects pending work, closes the transport and stops reconnecting", async () => {
    vi.useFakeTimers();
    const t = new ScriptedTransport();
    let opens = 0;
    const c = new EmpClient(async () => (opens++, t), OPTS);
    await c.connect();
    const p = c.request("doctor");
    c.dispose();
    await expect(p).rejects.toBeInstanceOf(EmpConnectionError);
    await expect(c.request("doctor")).rejects.toBeInstanceOf(EmpConnectionError);
    expect(c.status).toMatchObject({ state: "closed" });
    expect(c.status.retryAt).toBeUndefined();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(opens).toBe(1);
  });
});

describe("MockServer", () => {
  it("pages and filters threads and transcripts", async () => {
    const server = new MockServer();
    client = await EmpClient.connect(server.factory, OPTS);
    const all = await client.request("threads.list", { limit: 10 });
    expect(all.items).toHaveLength(10);
    const visible = server.fixture.threads.filter((t) => !t.automated).length;
    expect(visible).toBeLessThan(server.fixture.threads.length);
    expect(all.total).toBe(visible);
    const withAutomated = await client.request("threads.list", { limit: 10, includeAutomated: true });
    expect(withAutomated.total).toBe(server.fixture.threads.length);
    const sorted = [...all.items].sort((a, b) => b.updatedAt - a.updatedAt);
    expect(all.items).toEqual(sorted);

    const claude = await client.request("threads.list", { harness: "claude", limit: 100 });
    expect(claude.items.every((t) => t.harness === "claude")).toBe(true);

    const page = await client.request("threads.transcript", { threadId: "t-001", offset: 200, limit: 200 });
    expect(page).toMatchObject({ offset: 200, total: 3200 });
    expect(page.items).toHaveLength(200);

    const resume = await client.request("threads.resumeCommand", { threadId: claude.items[0]!.id });
    expect(resume?.display).toContain("claude --resume");
  });

  it("refuses methods before hello", async () => {
    const server = new MockServer();
    const t = await server.factory();
    const got = new Promise((r) => t.onMessage(r));
    t.send({ jsonrpc: "2.0", id: 1, method: "doctor" });
    expect(await got).toMatchObject({ id: 1, error: { code: RPC_ERROR_CODES.NOT_AUTHENTICATED } });
  });
});
