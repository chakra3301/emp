import { RPC_ERROR_CODES } from "@emp/protocol";

export class EmpError extends Error {
  override name = "EmpError";
}

/** The daemon answered with a JSON-RPC error object. */
export class EmpRpcError extends EmpError {
  override name = "EmpRpcError";
  constructor(
    readonly code: number,
    message: string,
    readonly data?: unknown,
    readonly method?: string,
  ) {
    super(message);
  }
}

/** The connection closed (or the client was disposed) before a response arrived. */
export class EmpConnectionError extends EmpError {
  override name = "EmpConnectionError";
}

export class EmpTimeoutError extends EmpError {
  override name = "EmpTimeoutError";
  constructor(
    readonly method: string,
    readonly timeoutMs: number,
  ) {
    super(`${method} timed out after ${timeoutMs} ms`);
  }
}

/** Errors that retrying cannot fix: bad token or incompatible protocol version. */
export function isFatalHelloError(err: unknown): boolean {
  return (
    err instanceof EmpRpcError &&
    (err.code === RPC_ERROR_CODES.NOT_AUTHENTICATED ||
      err.code === RPC_ERROR_CODES.INCOMPATIBLE_VERSION)
  );
}

export function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}
