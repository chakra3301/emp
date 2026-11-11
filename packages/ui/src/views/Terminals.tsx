import type { TerminalInfo } from "@emp/protocol";
import { EmptyState } from "../components/common";
import { IconClose, IconPlus, IconTerminal } from "../components/icons";
import { TerminalView } from "../components/TerminalView";
import { shortPath } from "../lib/format";
import { formatRelative } from "../lib/time";
import { useWorkspace } from "../store/context";

const KIND_LABEL: Record<TerminalInfo["kind"], string> = { shell: "Shell", resume: "Session", command: "Command" };

/** Every daemon-owned terminal: side shells, takeovers, commands. They keep running when hidden. */
export function Terminals() {
  const terminals = useWorkspace((s) => s.terminals);
  const focused = useWorkspace((s) => s.focusedTerminalId);
  const openShell = useWorkspace((s) => s.openShell);
  const list = Object.values(terminals).sort((a, b) => b.createdAt - a.createdAt);
  const current = (focused && terminals[focused]) || list.find((t) => t.state === "running") || list[0] || null;
  const running = list.filter((t) => t.state === "running");
  const finished = list.filter((t) => t.state !== "running");

  return (
    <section className="emp-page emp-page--terminals" data-orion-ui="panel" aria-label="Terminals">
      <div className="emp-terminals">
        <aside className="emp-terminals__list" aria-label="Terminal list">
          <div className="emp-terminals__head">
            <span>Terminals</span>
            <button type="button" className="emp-icon-button" title="New shell" aria-label="New shell" onClick={() => void openShell(false)}>
              <IconPlus />
            </button>
          </div>
          {list.length === 0 && <EmptyState title="No terminals" hint="Open a shell, or take over a thread's session from its page." />}
          <TerminalGroup label="Running" items={running} currentId={current?.id ?? null} />
          <TerminalGroup label="Finished" items={finished} currentId={current?.id ?? null} />
        </aside>
        <div className="emp-terminals__main">
          {current ? (
            <TerminalDetail key={current.id} info={current} />
          ) : (
            <EmptyState title="Nothing running" hint="Shells and agent sessions you open show up here and keep running in the background." centered>
              <button type="button" className="emp-button" data-orion-ui="button" onClick={() => void openShell(false)}>
                New shell
              </button>
            </EmptyState>
          )}
        </div>
      </div>
    </section>
  );
}

function TerminalGroup({ label, items, currentId }: { label: string; items: TerminalInfo[]; currentId: string | null }) {
  const focus = useWorkspace((s) => s.focusTerminal);
  const kill = useWorkspace((s) => s.killTerminal);
  const now = useWorkspace((s) => s.now);
  if (items.length === 0) return null;
  return (
    <div className="emp-terminals__group" role="list" aria-label={label}>
      <div className="emp-terminals__label">
        {label}
        <span>{items.length}</span>
      </div>
      {items.map((t) => (
        <div
          key={t.id}
          role="listitem"
          tabIndex={0}
          className="emp-term-row"
          data-active={t.id === currentId || undefined}
          data-state={t.state}
          onClick={() => focus(t.id)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              focus(t.id);
            }
          }}
        >
          <span className="emp-term-row__icon" aria-hidden="true">
            <IconTerminal />
          </span>
          <span className="emp-term-row__text">
            <span className="emp-term-row__title emp-truncate">{t.title}</span>
            <span className="emp-term-row__meta emp-truncate">
              {KIND_LABEL[t.kind]}
              {t.state === "exited" && ` · exited${t.exitCode !== null ? ` ${t.exitCode}` : ""}`}
              {t.cwd && ` · ${shortPath(t.cwd, 28)}`}
            </span>
          </span>
          <span className="emp-term-row__time">{formatRelative(t.createdAt, now)}</span>
          <button
            type="button"
            className="emp-term-row__close"
            title={t.state === "exited" ? "Close" : "Kill"}
            aria-label={t.state === "exited" ? `Close ${t.title}` : `Kill ${t.title}`}
            onClick={(e) => {
              e.stopPropagation();
              void kill(t.id);
            }}
          >
            <IconClose />
          </button>
        </div>
      ))}
    </div>
  );
}

function TerminalDetail({ info }: { info: TerminalInfo }) {
  const kill = useWorkspace((s) => s.killTerminal);
  const openThread = useWorkspace((s) => s.openThread);
  const exited = info.state === "exited";
  return (
    <>
      <header className="emp-terminals__bar" data-orion-ui="toolbar">
        <div className="emp-terminals__title">
          <span className="emp-truncate">{info.title}</span>
          <span className="emp-terminals__command emp-mono emp-truncate" title={info.command}>
            {info.cwd ? `${shortPath(info.cwd)}  ·  ${info.command}` : info.command}
          </span>
        </div>
        <span className="emp-terminals__state" data-state={info.state}>
          <span className="emp-dot" aria-hidden="true" />
          {exited ? `Exited${info.exitCode !== null ? ` ${info.exitCode}` : ""}` : "Running"}
        </span>
        {info.threadId && (
          <button type="button" className="emp-button emp-button--small" data-orion-ui="button" onClick={() => void openThread(info.threadId!)}>
            Open thread
          </button>
        )}
        <button type="button" className="emp-button emp-button--small" data-orion-ui="button" onClick={() => void kill(info.id)}>
          {exited ? "Close" : "Kill"}
        </button>
      </header>
      <div className="emp-terminals__screen" data-state={info.state}>
        <TerminalView terminalId={info.id} />
      </div>
    </>
  );
}
