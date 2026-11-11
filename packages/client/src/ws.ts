// WebSocket transport. The ONLY module in @emp/* that touches `WebSocket`; never import it from
// @emp/ui (the Orion sandbox has `connect-src 'none'`). Hosts that can open sockets pass
// `webSocketTransportFactory(...)` to `EmpClient`.

import { EmpConnectionError } from "./errors";
import type { Transport, TransportCloseInfo, TransportFactory } from "./transport";

export interface WebSocketTransportOptions {
  /** Give up if the socket is not open within this time. Default 5 s. */
  openTimeoutMs?: number;
}

export class WebSocketTransport implements Transport {
  readonly #ws: WebSocket;
  readonly #messageListeners = new Set<(msg: unknown) => void>();
  readonly #closeListeners = new Set<(info: TransportCloseInfo) => void>();
  #closeInfo: TransportCloseInfo | null = null;

  private constructor(ws: WebSocket) {
    this.#ws = ws;
    ws.addEventListener("message", (ev: MessageEvent) => {
      if (typeof ev.data !== "string") return;
      let msg: unknown;
      try {
        msg = JSON.parse(ev.data);
      } catch {
        return;
      }
      for (const cb of [...this.#messageListeners]) cb(msg);
    });
    ws.addEventListener("close", (ev: CloseEvent) => {
      this.#emitClose({ code: ev.code, reason: ev.reason });
    });
  }

  /** Opens a socket and resolves once it is connected. */
  static open(url: string, options: WebSocketTransportOptions = {}): Promise<WebSocketTransport> {
    const openTimeoutMs = options.openTimeoutMs ?? 5_000;
    return new Promise((resolve, reject) => {
      let ws: WebSocket;
      try {
        ws = new WebSocket(url);
      } catch (err) {
        reject(err);
        return;
      }
      const timer = setTimeout(() => {
        cleanup();
        ws.close();
        reject(new EmpConnectionError(`Timed out connecting to ${url}`));
      }, openTimeoutMs);
      const onOpen = () => {
        cleanup();
        resolve(new WebSocketTransport(ws));
      };
      const onFail = () => {
        cleanup();
        reject(new EmpConnectionError(`Could not connect to ${url}`));
      };
      const cleanup = () => {
        clearTimeout(timer);
        ws.removeEventListener("open", onOpen);
        ws.removeEventListener("error", onFail);
        ws.removeEventListener("close", onFail);
      };
      ws.addEventListener("open", onOpen);
      ws.addEventListener("error", onFail);
      ws.addEventListener("close", onFail);
    });
  }

  send(msg: unknown): void {
    if (this.#ws.readyState !== WebSocket.OPEN) {
      throw new EmpConnectionError("WebSocket is not open");
    }
    this.#ws.send(JSON.stringify(msg));
  }

  onMessage(cb: (msg: unknown) => void): () => void {
    this.#messageListeners.add(cb);
    return () => {
      this.#messageListeners.delete(cb);
    };
  }

  onClose(cb: (info: TransportCloseInfo) => void): () => void {
    const info = this.#closeInfo;
    if (info) {
      queueMicrotask(() => cb(info));
      return () => {};
    }
    this.#closeListeners.add(cb);
    return () => {
      this.#closeListeners.delete(cb);
    };
  }

  close(): void {
    if (this.#ws.readyState === WebSocket.CONNECTING || this.#ws.readyState === WebSocket.OPEN) {
      this.#ws.close(1000, "client closed");
    }
    this.#emitClose({ code: 1000, reason: "client closed" });
  }

  #emitClose(info: TransportCloseInfo): void {
    if (this.#closeInfo) return;
    this.#closeInfo = info;
    const listeners = [...this.#closeListeners];
    this.#closeListeners.clear();
    this.#messageListeners.clear();
    for (const cb of listeners) cb(info);
  }
}

/**
 * A factory for `EmpClient`. `url` may be a function so every reconnect re-reads discovery
 * (the daemon picks a new port when it restarts).
 */
export function webSocketTransportFactory(
  url: string | (() => string | Promise<string>),
  options?: WebSocketTransportOptions,
): TransportFactory {
  return async () => WebSocketTransport.open(typeof url === "function" ? await url() : url, options);
}
