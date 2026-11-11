import { useEffect, type CSSProperties } from "react";
import { StatusDot, connectionLabel } from "../components/common";
import { IconSpark } from "../components/icons";
import { formatTokens } from "../lib/compaction";
import { useWorkspace } from "../store/context";

/** Slim bottom bar, Hermes-style: daemon state on the left, live counts on the right. */
export function StatusBar() {
  const connection = useWorkspace((s) => s.connection);
  const reconnect = useWorkspace((s) => s.reconnect);
  const live = useWorkspace((s) => s.live);
  const terminals = useWorkspace((s) => s.terminals);
  const liveList = Object.values(live);
  const running = liveList.filter((l) => l.status === "running").length;
  const runningTerminals = Object.values(terminals).filter((t) => t.state === "running").length;
  const ready = connection.state === "ready";
  const openThreadId = useWorkspace((s) => (s.activeTab.startsWith("t:") ? s.selectedThreadId : null));
  const loadContext = useWorkspace((s) => s.loadContext);
  useEffect(() => {
    if (ready) void loadContext(openThreadId);
  }, [ready, openThreadId, loadContext]);

  return (
    <footer className="emp-statusbar" data-orion-ui="toolbar">
      <span className="emp-statusbar__item" title={connection.error ?? undefined}>
        <StatusDot status={connection} />
        {ready ? "empd ready" : connectionLabel(connection)}
      </span>
      {(connection.state === "error" || connection.state === "closed") && (
        <button type="button" className="emp-statusbar__link" onClick={reconnect}>
          Reconnect
        </button>
      )}
      <span className="emp-row__spacer" />
      <ContextMeter />
      {liveList.length > 0 && (
        <span className="emp-statusbar__item">
          <IconSpark />
          {running > 0 ? `${running} running` : `${liveList.length} live`}
        </span>
      )}
      {runningTerminals > 0 && <span className="emp-statusbar__item">{runningTerminals} terminal{runningTerminals === 1 ? "" : "s"}</span>}
      {ready && connection.daemonVersion && <span className="emp-statusbar__item emp-statusbar__accent"># v{connection.daemonVersion}</span>}
    </footer>
  );
}

/** "Context 45k / 200k" for the open thread, with a small fill bar; red past 80%. */
function ContextMeter() {
  const context = useWorkspace((s) => s.context);
  const thread = useWorkspace((s) => s.thread.data);
  const agent = useWorkspace((s) => s.agents.data?.find((a) => a.id === thread?.agentId) ?? null);
  const globalLimit = useWorkspace((s) => s.daemonSettings.data?.compactAt ?? null);
  const c = context.data;
  if (!context.threadId || !c) return null;
  const share = c.window ? Math.min(1, c.used / c.window) : null;
  const percent = share === null ? null : Math.round(share * 100);
  const limit = agent?.compactAt ?? globalLimit;
  const title = [
    `Context: ${c.used.toLocaleString()}${c.window ? ` of ${c.window.toLocaleString()}` : ""} tokens${percent !== null ? ` (${percent}%)` : ""}`,
    c.model ? `Model: ${c.model}` : null,
    limit ? `EMP compacts this chat at ${formatTokens(limit)}` : null,
    "Measured at the agent's last model call.",
  ]
    .filter(Boolean)
    .join("\n");
  return (
    <span className="emp-statusbar__item emp-context" title={title} data-level={share !== null && share >= 0.8 ? "high" : undefined}>
      {share !== null && (
        <span className="emp-context__meter" aria-hidden="true" style={{ "--emp-context-fill": `${share * 100}%` } as CSSProperties} />
      )}
      <span>
        Context {formatCount(c.used)}
        {c.window ? ` / ${formatCount(c.window)}` : ""}
      </span>
      {percent !== null && <span className="emp-context__percent">{percent}%</span>}
    </span>
  );
}

function formatCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
  return formatTokens(n);
}
