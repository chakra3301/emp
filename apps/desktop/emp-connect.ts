// Dev-server endpoint GET /__emp/connect → { wsUrl, token, daemonVersion, protocolVersion }.
//
// Reads empd's discovery file (`$EMP_DATA_DIR` or ~/Library/Application Support/EMP/daemon.json)
// and the token it points to, so the browser dev host can open the daemon WebSocket. Only answers
// same-machine requests: Host must be 127.0.0.1/localhost (blocks DNS rebinding) and any Origin
// must be local too. The token is never logged.

import { readFile } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import type { Plugin } from "vite";

const LOCAL_HOSTS = new Set(["127.0.0.1", "localhost"]);
const LOCAL_ORIGIN = /^http:\/\/(?:127\.0\.0\.1|localhost)(?::\d+)?$/;

export function empDataDir(): string {
  return process.env.EMP_DATA_DIR || join(homedir(), "Library", "Application Support", "EMP");
}

function hostname(hostHeader: string | undefined): string {
  return (hostHeader ?? "").replace(/:\d+$/, "").toLowerCase();
}

function send(res: ServerResponse, status: number, body: unknown): void {
  res.statusCode = status;
  res.setHeader("Content-Type", "application/json");
  res.setHeader("Cache-Control", "no-store");
  res.end(JSON.stringify(body));
}

interface DaemonJson {
  daemonVersion?: string;
  protocolVersion?: number;
  wsUrl?: string;
  tokenPath?: string;
}

async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
  if (!LOCAL_HOSTS.has(hostname(req.headers.host))) return send(res, 403, { error: "forbidden host" });
  const origin = req.headers.origin;
  if (origin && !LOCAL_ORIGIN.test(origin)) return send(res, 403, { error: "forbidden origin" });
  if (req.method !== "GET") return send(res, 405, { error: "GET only" });

  const dir = empDataDir();
  const discovery = join(dir, "daemon.json");
  let info: DaemonJson;
  try {
    info = JSON.parse(await readFile(discovery, "utf8")) as DaemonJson;
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    return send(res, 503, {
      error: code === "ENOENT" ? `empd is not running (no ${discovery})` : `Could not read ${discovery}: ${code ?? "invalid JSON"}`,
    });
  }
  if (!info.wsUrl || !info.tokenPath) return send(res, 503, { error: `${discovery} has no wsUrl/tokenPath` });

  const tokenPath = isAbsolute(info.tokenPath) ? info.tokenPath : join(dir, info.tokenPath);
  let token: string;
  try {
    token = (await readFile(tokenPath, "utf8")).trim();
  } catch (err) {
    return send(res, 503, { error: `Could not read the daemon token (${(err as NodeJS.ErrnoException).code ?? "error"})` });
  }
  send(res, 200, { wsUrl: info.wsUrl, token, daemonVersion: info.daemonVersion, protocolVersion: info.protocolVersion });
}

export function empConnectPlugin(): Plugin {
  return {
    name: "emp:connect",
    apply: "serve",
    configureServer(server) {
      server.middlewares.use("/__emp/connect", (req, res) => {
        handle(req, res).catch(() => send(res, 500, { error: "internal error" }));
      });
    },
  };
}
