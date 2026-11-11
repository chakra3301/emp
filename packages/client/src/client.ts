import {
  PROTOCOL_VERSION,
  RPC_ERROR_CODES,
  type ClientInfo,
  type EmpMethodName,
  type EmpMethods,
  type EmpNotificationName,
  type EmpNotifications,
  type HelloResult,
} from "@emp/protocol";
import {
  EmpConnectionError,
  EmpRpcError,
  EmpTimeoutError,
  errorMessage,
  isFatalHelloError,
} from "./errors";
import type { Transport, TransportFactory } from "./transport";

export type ConnectionState = "connecting" | "ready" | "closed" | "error";

export interface ConnectionStatus {
  /**
   * `connecting`: opening a transport / waiting for hello.
   * `ready`: hello done, requests flow.
   * `closed`: disconnected; `retryAt` is set when a reconnect is scheduled, unset after dispose.
   * `error`: fatal (bad token, incompatible version, or reconnect disabled). Call `reconnect()`.
   */
  state: ConnectionState;
  /** Failed attempts since the last successful hello. */
  attempt: number;
  error?: string;
  /** Epoch ms of the next reconnect attempt. */
  retryAt?: number;
  daemonVersion?: string;
}

export interface ReconnectOptions {
  initialDelayMs?: number;
  maxDelayMs?: number;
  factor?: number;
}

export interface EmpClientOptions {
  /** Daemon token. A function is re-evaluated on every (re)connect. */
  token: string | (() => string | Promise<string>);
  client: ClientInfo;
  /** Default per-request timeout. 0 disables. Default 30 s. */
  requestTimeoutMs?: number;
  /** Timeout for the hello handshake. Default 10 s. */
  helloTimeoutMs?: number;
  /** Backoff settings, or `false` to never reconnect. */
  reconnect?: ReconnectOptions | false;
}

export interface RequestOptions {
  /** Overrides the default timeout. 0 disables. Counts time spent waiting for `ready`. */
  timeoutMs?: number;
}

type RequestArgs<M extends EmpMethodName> = undefined extends EmpMethods[M]["params"]
  ? [params?: EmpMethods[M]["params"], options?: RequestOptions]
  : [params: EmpMethods[M]["params"], options?: RequestOptions];

/** Methods that legitimately run for minutes; no timeout unless the caller sets one. */
const NO_DEFAULT_TIMEOUT: ReadonlySet<string> = new Set<EmpMethodName>(["import.run"]);

interface Pending {
  id: number;
  method: string;
  params: unknown;
  /** Sent on the current transport. Unsent requests wait for `ready`. */
  sent: boolean;
  /** Bound to one transport (hello); never queued for a later connection. */
  transport?: Transport;
  timer?: ReturnType<typeof setTimeout>;
  resolve(value: unknown): void;
  reject(err: unknown): void;
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
  reject(err: unknown): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  // Nobody is required to await readiness; avoid unhandled-rejection noise.
  promise.catch(() => {});
  return { promise, resolve, reject };
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Typed JSON-RPC client for `empd`. Owns one transport at a time, performs `hello` on every
 * connection, correlates responses by id, and reconnects with exponential backoff.
 */
export class EmpClient {
  readonly #factory: TransportFactory;
  readonly #options: EmpClientOptions;
  readonly #requestTimeoutMs: number;
  readonly #helloTimeoutMs: number;
  readonly #reconnect: Required<ReconnectOptions> | false;

  #transport: Transport | null = null;
  #detach: Array<() => void> = [];
  #ready = false;
  #started = false;
  #disposed = false;
  #nextId = 1;
  #attempt = 0;
  #retryTimer: ReturnType<typeof setTimeout> | undefined;
  #hello: HelloResult | null = null;
  #readyDeferred = deferred<HelloResult>();
  #status: ConnectionStatus = { state: "closed", attempt: 0 };

  readonly #pending = new Map<number, Pending>();
  readonly #notificationListeners = new Map<string, Set<(params: unknown) => void>>();
  readonly #stateListeners = new Set<(status: ConnectionStatus) => void>();

  constructor(factory: TransportFactory, options: EmpClientOptions) {
    this.#factory = factory;
    this.#options = options;
    this.#requestTimeoutMs = options.requestTimeoutMs ?? 30_000;
    this.#helloTimeoutMs = options.helloTimeoutMs ?? 10_000;
    this.#reconnect =
      options.reconnect === false
        ? false
        : {
            initialDelayMs: options.reconnect?.initialDelayMs ?? 500,
            maxDelayMs: options.reconnect?.maxDelayMs ?? 10_000,
            factor: options.reconnect?.factor ?? 2,
          };
  }

  /** Creates a client and resolves once the first hello succeeds. Disposes it on failure. */
  static async connect(factory: TransportFactory, options: EmpClientOptions): Promise<EmpClient> {
    const client = new EmpClient(factory, options);
    try {
      await client.connect();
      return client;
    } catch (err) {
      client.dispose();
      throw err;
    }
  }

  get status(): ConnectionStatus {
    return this.#status;
  }

  get daemonVersion(): string | undefined {
    return this.#hello?.daemonVersion;
  }

  /**
   * Starts connecting (idempotent) and resolves on the next successful hello. Transient
   * failures keep retrying; rejects only on a fatal error, or on any failure when
   * reconnect is disabled, or on dispose.
   */
  connect(): Promise<HelloResult> {
    if (this.#disposed) return Promise.reject(new EmpConnectionError("Client disposed"));
    if (!this.#started) {
      this.#started = true;
      void this.#attemptConnect();
    }
    return this.#ready && this.#hello ? Promise.resolve(this.#hello) : this.#readyDeferred.promise;
  }

  /** Drops the current connection (if any) and connects again immediately. */
  reconnect(): Promise<HelloResult> {
    if (this.#disposed) return Promise.reject(new EmpConnectionError("Client disposed"));
    clearTimeout(this.#retryTimer);
    this.#retryTimer = undefined;
    const transport = this.#transport;
    if (transport) this.#dropTransport(transport, new EmpConnectionError("Reconnecting"));
    if (this.#status.state === "error") this.#readyDeferred = deferred();
    this.#attempt = 0;
    this.#started = true;
    void this.#attemptConnect();
    return this.#readyDeferred.promise;
  }

  request<M extends EmpMethodName>(
    method: M,
    ...args: RequestArgs<M>
  ): Promise<EmpMethods[M]["result"]> {
    const [params, options] = args as [unknown, RequestOptions | undefined];
    if (this.#disposed) return Promise.reject(new EmpConnectionError("Client disposed"));
    if (this.#status.state === "error") {
      return Promise.reject(new EmpConnectionError(this.#status.error ?? "Not connected"));
    }
    const timeoutMs =
      options?.timeoutMs ?? (NO_DEFAULT_TIMEOUT.has(method) ? 0 : this.#requestTimeoutMs);
    return this.#enqueue(method, params, timeoutMs) as Promise<EmpMethods[M]["result"]>;
  }

  /** Subscribe to a daemon notification. Returns an unsubscribe function. */
  on<N extends EmpNotificationName>(
    notification: N,
    cb: (params: EmpNotifications[N]) => void,
  ): () => void {
    let set = this.#notificationListeners.get(notification);
    if (!set) {
      set = new Set();
      this.#notificationListeners.set(notification, set);
    }
    const listener = cb as (params: unknown) => void;
    set.add(listener);
    return () => {
      set.delete(listener);
    };
  }

  /** Subscribe to connection-state changes. Returns an unsubscribe function. */
  onStateChange(cb: (status: ConnectionStatus) => void): () => void {
    this.#stateListeners.add(cb);
    return () => {
      this.#stateListeners.delete(cb);
    };
  }

  /** Closes the transport, rejects everything pending, stops reconnecting. Idempotent. */
  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    clearTimeout(this.#retryTimer);
    this.#retryTimer = undefined;
    const err = new EmpConnectionError("Client disposed");
    const transport = this.#transport;
    if (transport) this.#dropTransport(transport, err);
    this.#rejectAll(err);
    this.#readyDeferred.reject(err);
    this.#setStatus({ state: "closed", attempt: this.#attempt });
    this.#notificationListeners.clear();
    this.#stateListeners.clear();
  }

  // ---- connection lifecycle -------------------------------------------------------------

  async #attemptConnect(): Promise<void> {
    if (this.#disposed) return;
    this.#setStatus({ state: "connecting", attempt: this.#attempt });

    let transport: Transport;
    try {
      transport = await this.#factory();
    } catch (err) {
      this.#attemptFailed(err);
      return;
    }
    if (this.#disposed || this.#transport) {
      transport.close();
      return;
    }

    this.#transport = transport;
    this.#detach = [
      transport.onMessage((msg) => this.#handleMessage(transport, msg)),
      transport.onClose((info) => this.#handleClose(transport, info.reason)),
    ];

    try {
      const tokenOpt = this.#options.token;
      const token = typeof tokenOpt === "function" ? await tokenOpt() : tokenOpt;
      if (this.#transport !== transport) return;
      const result = (await this.#enqueue(
        "hello",
        { protocolVersion: PROTOCOL_VERSION, token, client: this.#options.client },
        this.#helloTimeoutMs,
        transport,
      )) as HelloResult;
      if (this.#transport !== transport) return;
      if (result?.protocolVersion !== PROTOCOL_VERSION) {
        throw new EmpRpcError(
          RPC_ERROR_CODES.INCOMPATIBLE_VERSION,
          `Daemon speaks protocol ${String(result?.protocolVersion)}, client speaks ${PROTOCOL_VERSION}`,
          undefined,
          "hello",
        );
      }
      this.#attempt = 0;
      this.#hello = result;
      this.#ready = true;
      this.#setStatus({ state: "ready", attempt: 0, daemonVersion: result.daemonVersion });
      this.#readyDeferred.resolve(result);
      this.#flushQueued();
    } catch (err) {
      // A transport close already handled cleanup and scheduled the retry.
      if (this.#transport !== transport) return;
      this.#dropTransport(transport, err instanceof Error ? err : new EmpConnectionError(String(err)));
      if (isFatalHelloError(err)) this.#fail(err);
      else this.#attemptFailed(err);
    }
  }

  #handleClose(transport: Transport, reason: string | undefined): void {
    if (this.#transport !== transport) return;
    const wasReady = this.#ready;
    this.#dropTransport(transport, new EmpConnectionError(reason || "Connection closed"));
    if (this.#disposed) return;
    if (wasReady) this.#attempt = 0;
    this.#attemptFailed(new EmpConnectionError(reason || "Connection closed"));
  }

  /** Detaches and closes `transport`, rejecting the requests that were in flight on it. */
  #dropTransport(transport: Transport, err: Error): void {
    for (const off of this.#detach) off();
    this.#detach = [];
    this.#transport = null;
    if (this.#ready) {
      this.#ready = false;
      this.#readyDeferred = deferred();
    }
    try {
      transport.close();
    } catch {
      // Already closed.
    }
    for (const p of [...this.#pending.values()]) {
      if (p.sent || p.transport === transport) this.#settle(p, undefined, err);
    }
  }

  #attemptFailed(err: unknown): void {
    if (this.#disposed) return;
    this.#attempt += 1;
    if (this.#reconnect === false) {
      this.#fail(err);
      return;
    }
    const { initialDelayMs, maxDelayMs, factor } = this.#reconnect;
    const delay = Math.min(maxDelayMs, initialDelayMs * factor ** (this.#attempt - 1));
    this.#setStatus({
      state: "closed",
      attempt: this.#attempt,
      error: errorMessage(err),
      retryAt: Date.now() + delay,
    });
    clearTimeout(this.#retryTimer);
    this.#retryTimer = setTimeout(() => {
      this.#retryTimer = undefined;
      void this.#attemptConnect();
    }, delay);
  }

  #fail(err: unknown): void {
    this.#setStatus({ state: "error", attempt: this.#attempt, error: errorMessage(err) });
    this.#rejectAll(err instanceof Error ? err : new EmpConnectionError(String(err)));
    this.#readyDeferred.reject(err);
  }

  #setStatus(status: ConnectionStatus): void {
    this.#status = status;
    for (const cb of [...this.#stateListeners]) {
      try {
        cb(status);
      } catch (err) {
        console.error("[emp] state listener failed", err);
      }
    }
  }

  // ---- requests -------------------------------------------------------------------------

  #enqueue(method: string, params: unknown, timeoutMs: number, transport?: Transport): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const p: Pending = { id: this.#nextId++, method, params, sent: false, transport, resolve, reject };
      if (timeoutMs > 0) {
        p.timer = setTimeout(() => this.#settle(p, undefined, new EmpTimeoutError(method, timeoutMs)), timeoutMs);
      }
      this.#pending.set(p.id, p);
      if (transport) this.#sendPending(transport, p);
      else if (this.#ready && this.#transport) this.#sendPending(this.#transport, p);
    });
  }

  #flushQueued(): void {
    const transport = this.#transport;
    if (!transport) return;
    for (const p of [...this.#pending.values()]) {
      if (!p.sent && !p.transport) this.#sendPending(transport, p);
    }
  }

  #sendPending(transport: Transport, p: Pending): void {
    const msg: Record<string, unknown> = { jsonrpc: "2.0", id: p.id, method: p.method };
    if (p.params !== undefined) msg.params = p.params;
    p.sent = true;
    try {
      transport.send(msg);
    } catch (err) {
      this.#settle(p, undefined, new EmpConnectionError(errorMessage(err)));
    }
  }

  #settle(p: Pending, value: unknown, err?: unknown): void {
    if (!this.#pending.delete(p.id)) return;
    clearTimeout(p.timer);
    if (err !== undefined) p.reject(err);
    else p.resolve(value);
  }

  #rejectAll(err: Error): void {
    for (const p of [...this.#pending.values()]) this.#settle(p, undefined, err);
  }

  #handleMessage(transport: Transport, msg: unknown): void {
    if (this.#transport !== transport || !isObject(msg)) return;

    if (typeof msg.method === "string" && !("id" in msg)) {
      const listeners = this.#notificationListeners.get(msg.method);
      if (!listeners) return;
      for (const cb of [...listeners]) {
        try {
          cb(msg.params);
        } catch (err) {
          console.error(`[emp] ${msg.method} listener failed`, err);
        }
      }
      return;
    }

    if ("id" in msg && typeof msg.method !== "string") {
      const p = typeof msg.id === "number" ? this.#pending.get(msg.id) : undefined;
      if (!p) return;
      if (isObject(msg.error)) {
        const e = msg.error;
        const code = typeof e.code === "number" ? e.code : RPC_ERROR_CODES.INTERNAL;
        const message = typeof e.message === "string" ? e.message : "Unknown error";
        this.#settle(p, undefined, new EmpRpcError(code, message, e.data, p.method));
      } else {
        this.#settle(p, msg.result ?? null);
      }
    }
    // Daemon → client requests are not part of protocol v1; ignore them.
  }
}
