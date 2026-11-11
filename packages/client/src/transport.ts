/**
 * The only capability @emp/client needs: a bidirectional channel of JSON-RPC messages.
 *
 * Desktop: a WebSocket (`@emp/client/ws`). Orion plugin: a `localService` channel.
 * Tests and offline demos: `@emp/client/mock`. Messages are plain JSON-compatible values;
 * the transport owns (de)serialization.
 */
export interface Transport {
  send(msg: unknown): void;
  /** Subscribe to incoming messages. Returns an unsubscribe function. */
  onMessage(cb: (msg: unknown) => void): () => void;
  /** Called once when the channel closes, from either side. Returns an unsubscribe function. */
  onClose(cb: (info: TransportCloseInfo) => void): () => void;
  close(): void;
}

export interface TransportCloseInfo {
  code?: number;
  reason?: string;
}

/** Opens a fresh, connected transport. Called again for every reconnect attempt. */
export type TransportFactory = () => Promise<Transport>;
