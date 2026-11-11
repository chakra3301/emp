import { useEffect, useState } from "react";
import type { DevServer, ServerKind } from "@emp/protocol";
import { EmptyState, ErrorState, LoadingState } from "../components/common";
import { ConfirmButton, Segmented } from "../components/controls";
import { IconRefresh, IconTerminal } from "../components/icons";
import { formatBytes, shortPath } from "../lib/format";
import { formatShort } from "../lib/time";
import { useHost, useWorkspace } from "../store/context";

const POLL_MS = 5000;

const KIND_TITLES: Record<ServerKind, string> = { dev: "Dev servers", service: "Databases and services", agent: "Agents", app: "Apps and system" };
const KIND_ORDER: ServerKind[] = ["dev", "service", "agent", "app"];

type View = "dev" | "all";

/** Processes listening on a port, so forgotten dev servers can be found and stopped. */
export function Servers() {
  const servers = useWorkspace((s) => s.servers);
  const ops = useWorkspace((s) => s.serverOps);
  const load = useWorkspace((s) => s.loadServers);
  const kill = useWorkspace((s) => s.killServer);
  const [view, setView] = useState<View>("dev");

  useEffect(() => {
    const t = setInterval(() => void load(), POLL_MS);
    return () => clearInterval(t);
  }, [load]);

  const all = servers.data ?? [];
  const dev = all.filter((d) => d.kind === "dev");
  const shown = view === "dev" ? dev : all;
  const idle = dev.filter((d) => d.connections === 0 && !d.protected && !ops[d.pid]);
  const groups = view === "dev" ? byUse(shown) : byKind(shown);

  return (
    <section className="emp-page" data-orion-ui="panel" aria-label="Dev servers">
      <div className="emp-toolbar emp-toolbar--page" data-orion-ui="toolbar">
        <div className="emp-toolbar__row emp-servers__bar">
          <h2 className="emp-heading emp-heading--lg" data-orion-ui="title">
            Dev servers
          </h2>
          {servers.data && (
            <span className="emp-count" data-orion-ui="muted">
              {dev.length}
            </span>
          )}
          <span className="emp-row__spacer" />
          <Segmented<View>
            label="Show"
            value={view}
            options={[
              ["dev", "Dev servers"],
              ["all", `All ports${servers.data ? ` · ${all.length}` : ""}`],
            ]}
            onChange={setView}
          />
          <button type="button" className="emp-icon-button" title="Refresh" aria-label="Refresh" onClick={() => void load()}>
            <IconRefresh />
          </button>
          <ConfirmButton
            className="emp-button"
            disabled={idle.length === 0}
            label={`Stop idle${idle.length ? ` (${idle.length})` : ""}`}
            confirm={`Stop ${idle.length} idle server${idle.length === 1 ? "" : "s"}?`}
            onConfirm={() => idle.forEach((d) => void kill(d.pid))}
          />
        </div>
      </div>
      <div className="emp-page__body emp-servers">
        {servers.status === "idle" && <LoadingState label="Waiting for empd…" />}
        {servers.status === "loading" && !servers.data && <LoadingState label="Looking for listening ports…" />}
        {servers.status === "error" && !servers.data && <ErrorState message={servers.error ?? "Could not list ports"} onRetry={() => void load()} />}
        {servers.data && shown.length === 0 && (
          <EmptyState
            title={view === "dev" ? "No dev servers running" : "Nothing is listening"}
            hint={view === "dev" ? "Servers started from a project folder (vite, next, python -m http.server, …) show up here." : undefined}
          />
        )}
        {groups.map((g) => (
          <ServerGroup key={g.key} title={g.title} hint={g.hint} items={g.items} />
        ))}
        {servers.data && shown.length > 0 && (
          <p className="emp-servers__note" data-orion-ui="muted">
            A static file server can look idle while you use it: it only holds a connection while it sends a page.
          </p>
        )}
      </div>
    </section>
  );
}

interface Group {
  key: string;
  title: string;
  hint?: string;
  items: DevServer[];
}

const byFolderThenPort = (a: DevServer, b: DevServer) => (a.cwd ?? "").localeCompare(b.cwd ?? "") || (a.ports[0] ?? 0) - (b.ports[0] ?? 0);

function byUse(list: DevServer[]): Group[] {
  const sorted = list.slice().sort(byFolderThenPort);
  return [
    { key: "use", title: "In use", hint: "Something is connected, usually a browser tab.", items: sorted.filter((d) => d.connections > 0) },
    { key: "idle", title: "Idle", hint: "Nothing connected right now.", items: sorted.filter((d) => d.connections === 0) },
  ].filter((g) => g.items.length > 0);
}

function byKind(list: DevServer[]): Group[] {
  return KIND_ORDER.map((k) => ({ key: k, title: KIND_TITLES[k], items: list.filter((d) => d.kind === k).sort(byFolderThenPort) })).filter((g) => g.items.length > 0);
}

function ServerGroup({ title, hint, items }: { title: string; hint?: string; items: DevServer[] }) {
  return (
    <div className="emp-servers__group" role="group" aria-label={title}>
      <div className="emp-servers__head">
        <span className="emp-servers__title">{title}</span>
        <span className="emp-count" data-orion-ui="muted">
          {items.length}
        </span>
        {hint && <span className="emp-servers__hint emp-truncate">{hint}</span>}
      </div>
      <div className="emp-servers__list" role="list">
        {items.map((d) => (
          <ServerRow key={d.pid} server={d} />
        ))}
      </div>
    </div>
  );
}

/** A folder worth naming a row after: not `/`, not home itself. */
function projectFolder(cwd: string | null): string | null {
  if (!cwd || cwd === "/") return null;
  const name = cwd.split("/").filter(Boolean).pop() ?? null;
  return shortPath(cwd) === "~" ? null : name;
}

function ServerRow({ server: d }: { server: DevServer }) {
  const host = useHost();
  const now = useWorkspace((s) => s.now);
  const op = useWorkspace((s) => s.serverOps[d.pid]);
  const kill = useWorkspace((s) => s.killServer);
  const showSection = useWorkspace((s) => s.showSection);
  const focusTerminal = useWorkspace((s) => s.focusTerminal);
  const inUse = d.connections > 0;
  const openable = d.kind === "dev" && host.openExternal;
  const stopping = op?.state === "stopping";
  const folder = d.kind === "dev" ? projectFolder(d.cwd) : null;
  const detail = [folder ? d.name : null, d.cwd && d.cwd !== "/" ? shortPath(d.cwd, 56) : null].filter(Boolean).join("  ·  ");

  return (
    <div className="emp-server" role="listitem" data-idle={!inUse || undefined} data-stopping={stopping || undefined}>
      <span className="emp-server__state" role="img" aria-label={inUse ? "In use" : "Idle"} title={inUse ? `${d.connections} connection${d.connections === 1 ? "" : "s"} open` : "Nothing connected"}>
        <span className="emp-dot" aria-hidden="true" />
      </span>
      <span className="emp-server__text">
        <span className="emp-server__name emp-truncate" title={d.command}>
          {folder ?? d.name}
          {d.exposed && (
            <span className="emp-server__badge" title="Listens on every network interface, so other devices on your network can reach it">
              Network
            </span>
          )}
        </span>
        <span className="emp-server__command emp-truncate" title={d.command}>
          {detail || d.command}
        </span>
      </span>
      <span className="emp-server__ports">
        {d.ports.slice(0, 3).map((port) =>
          openable ? (
            <button key={port} type="button" className="emp-server__port" title={`Open http://localhost:${port}`} onClick={() => host.openExternal?.(`http://localhost:${port}`)}>
              :{port}
            </button>
          ) : (
            <span key={port} className="emp-server__port">
              :{port}
            </span>
          ),
        )}
        {d.ports.length > 3 && <span className="emp-server__more">+{d.ports.length - 3}</span>}
      </span>
      {d.terminalId && (
        <button
          type="button"
          className="emp-icon-button"
          title="Show its terminal"
          aria-label={`Show the terminal running ${d.name}`}
          onClick={() => {
            showSection("terminals");
            focusTerminal(d.terminalId);
          }}
        >
          <IconTerminal />
        </button>
      )}
      <span className="emp-server__meta emp-server__age" title={`Started ${new Date(d.startedAt).toLocaleString()} · pid ${d.pid}`}>
        {formatShort(d.startedAt, now)}
      </span>
      <span className="emp-server__meta emp-server__mem">{formatBytes(d.memory)}</span>
      <span className="emp-server__action">
        {d.protected ? (
          <span className="emp-server__protected" title="EMP's own daemon">
            EMP
          </span>
        ) : op?.state === "stuck" ? (
          <button type="button" className="emp-button emp-button--small emp-button--danger" data-orion-ui="button" onClick={() => void kill(d.pid, true)}>
            Force quit
          </button>
        ) : (
          <ConfirmButton
            className="emp-button emp-button--small"
            disabled={stopping}
            label={stopping ? "Stopping…" : "Stop"}
            confirm="Stop it?"
            skipConfirm={d.kind === "dev" && !inUse}
            onConfirm={() => void kill(d.pid)}
          />
        )}
      </span>
      {op?.state === "error" && (
        <span className="emp-server__error" role="alert">
          {op.error}
        </span>
      )}
      {op?.state === "stuck" && (
        <span className="emp-server__error" role="status">
          Still running 3 seconds after being asked to stop.
        </span>
      )}
    </div>
  );
}
