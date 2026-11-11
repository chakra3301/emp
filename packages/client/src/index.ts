// @emp/client — browser-safe. No Node, Tauri, or WebSocket APIs here; see ./ws and ./mock.
export {
  EmpClient,
  type ConnectionState,
  type ConnectionStatus,
  type EmpClientOptions,
  type ReconnectOptions,
  type RequestOptions,
} from "./client";
export {
  EmpConnectionError,
  EmpError,
  EmpRpcError,
  EmpTimeoutError,
  errorMessage,
  isFatalHelloError,
} from "./errors";
export type { Transport, TransportCloseInfo, TransportFactory } from "./transport";
