import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import type { EmpHost } from "./host";
import { WorkspaceProvider, useWorkspace } from "./store/context";
import { settingsAttrs, settingsStyle, visible } from "./store/settings";
import { createWorkspaceStore, type WorkspaceClient } from "./store/workspace";
import { Agents } from "./views/Agents";
import { ImportDoctor } from "./views/ImportDoctor";
import { NewThread } from "./views/NewThread";
import { Projects } from "./views/Projects";
import { LayoutEditor } from "./views/LayoutEditor";
import { SettingsOverlay } from "./views/Settings";
import { Sidebar } from "./views/Sidebar";
import { StatusBar } from "./views/StatusBar";
import { TabBar } from "./views/TabBar";
import { Schedules } from "./views/Schedules";
import { Servers } from "./views/Servers";
import { SukiPanel } from "./views/SukiPanel";
import { Terminals } from "./views/Terminals";
import { ThreadView } from "./views/ThreadView";
import "./styles/fonts.css";
import "./styles/tokens.css";
import "./styles/workspace.css";

/** Below this width (px) the workspace uses the phone layout. */
const COMPACT_BELOW = 720;

export interface EmpWorkspaceProps {
  /** A connected (or connecting) client. The workspace never disposes it; its owner does. */
  client: WorkspaceClient;
  host: EmpHost;
  className?: string;
}

/** Keep only `--emp-*` overrides; hosts must not restyle anything else through the theme. */
function themeStyle(vars: Record<string, string>): Record<string, string> {
  const style: Record<string, string> = {};
  for (const [k, v] of Object.entries(vars)) if (k.startsWith("--emp-")) style[k] = v;
  return style;
}

/**
 * The whole EMP UI. Fills its container (the host sizes it), keeps all state in a
 * per-instance store, and releases every subscription on unmount.
 */
export function EmpWorkspace({ client, host, className }: EmpWorkspaceProps) {
  const [store] = useState(() => createWorkspaceStore(client, host));
  const [theme, setTheme] = useState(() => themeStyle(host.theme.get()));
  const idPrefix = useId();

  useEffect(() => store.getState().start(), [store]);

  useEffect(() => {
    setTheme(themeStyle(host.theme.get()));
    return host.theme.subscribe((vars) => setTheme(themeStyle(vars)));
  }, [host]);

  useEffect(() => {
    if (!host.registerCommand) return;
    const s = store.getState();
    const disposables = [
      host.registerCommand({ id: "emp.focusSearch", title: "EMP: Search threads", keybinding: "Mod+K", run: () => s.focusSearch() }),
      host.registerCommand({ id: "emp.importNow", title: "EMP: Import now", run: () => void s.runImport() }),
      host.registerCommand({ id: "emp.suki", title: "EMP: Ask Suki", keybinding: "Mod+/", run: () => store.getState().setSukiOpen(!store.getState().suki.open) }),
      host.registerCommand({ id: "emp.newThread", title: "EMP: New thread", keybinding: "Mod+N", run: () => s.setNewThreadOpen(true) }),
    ];
    return () => {
      for (const d of disposables) d.dispose();
    };
  }, [host, store]);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    // Terminals get every key, including ⌘K.
    if (e.target instanceof Element && e.target.closest(".xterm")) return;
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") {
      e.preventDefault();
      store.getState().setSidebarTab("threads");
      store.getState().focusSearch();
    }
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "n") {
      e.preventDefault();
      store.getState().setNewThreadOpen(true);
    }
    const s = store.getState();
    // ⌘/ opens Suki over the current page (Amp's Puck binding).
    if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key === "/") {
      e.preventDefault();
      s.setSukiOpen(!s.suki.open);
    }
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key === ",") {
      e.preventDefault();
      if (s.settingsPage) s.closeSettings();
      else s.openSettings();
    }
    // ⌘⇧\ toggles the layout editor (Hermes's binding).
    if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.code === "Backslash") {
      e.preventDefault();
      s.setLayoutEditing(!s.layoutEditing);
    }
    if (e.key === "Escape" && !e.defaultPrevented) {
      if (s.settingsPage) {
        e.preventDefault();
        s.closeSettings();
      } else if (s.layoutEditing) {
        e.preventDefault();
        s.setLayoutEditing(false);
      }
    }
  };

  return (
    <WorkspaceProvider store={store} host={host} client={client}>
      <Root className={className} hostKind={host.kind} theme={theme} idPrefix={idPrefix} onKeyDown={onKeyDown} />
    </WorkspaceProvider>
  );
}

/** The root element: host theme first, then the user's own colour and layout on top. */
function Root(props: {
  className?: string;
  hostKind: string;
  theme: Record<string, string>;
  idPrefix: string;
  onKeyDown(e: KeyboardEvent<HTMLDivElement>): void;
}) {
  const settings = useWorkspace((s) => s.settings);
  const settingsOpen = useWorkspace((s) => s.settingsPage !== null);
  const editing = useWorkspace((s) => s.layoutEditing);
  const drawer = useWorkspace((s) => s.drawerOpen);
  const setDrawer = useWorkspace((s) => s.setDrawerOpen);
  const style = { ...props.theme, ...settingsStyle(settings) } as CSSProperties;
  const rootRef = useRef<HTMLDivElement>(null);
  // null until measured: children render only once the layout is known (still before paint).
  const [measured, setCompact] = useState<boolean | null>(null);
  const compact = measured === true;

  // Phone layout by the workspace's own width (works in a narrow Orion panel too). Measured
  // before the first paint so nothing lays itself out in the desktop grid on a phone.
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    setCompact(el.getBoundingClientRect().width < COMPACT_BELOW);
    const ro = new ResizeObserver(([entry]) => setCompact((entry?.contentRect.width ?? COMPACT_BELOW) < COMPACT_BELOW));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  return (
    <div
      ref={rootRef}
      className={props.className ? `emp-root ${props.className}` : "emp-root"}
      data-orion-ui="panel"
      data-host={props.hostKind}
      data-editing={(editing && !compact) || undefined}
      data-compact={compact || undefined}
      data-drawer={(compact && drawer) || undefined}
      // Focusable so clicks anywhere keep keyboard focus inside the workspace (shortcuts are
      // handled here; window-level listeners aren't allowed in the plugin sandbox).
      tabIndex={-1}
      {...settingsAttrs(settings)}
      style={style}
      onKeyDown={props.onKeyDown}
    >
      {measured !== null && <Sidebar idPrefix={props.idPrefix} />}
      {measured !== null && <Main />}
      {visible(settings, "statusBar") && !compact && <StatusBar />}
      {compact && drawer && <button type="button" className="emp-drawer-scrim" aria-label="Close menu" onClick={() => setDrawer(false)} />}
      {editing && !compact && <LayoutEditor />}
      {settingsOpen && <SettingsOverlay />}
    </div>
  );
}

function Main() {
  const active = useWorkspace((s) => s.activeTab);
  const sukiOpen = useWorkspace((s) => s.suki.open);
  const connection = useWorkspace((s) => s.connection);
  const reconnect = useWorkspace((s) => s.reconnect);

  return (
    <main className="emp-main">
      <TabBar />
      {connection.state === "error" && (
        <div className="emp-banner" role="alert">
          <span>Can't connect to empd: {connection.error ?? "unknown error"}</span>
          <button type="button" className="emp-button" data-orion-ui="button" onClick={reconnect}>
            Retry
          </button>
        </div>
      )}
      {active === "new" && <NewThread />}
      {active.startsWith("t:") && <ThreadView />}
      {active === "page:import" && <ImportDoctor />}
      {active === "page:agents" && <Agents />}
      {active === "page:terminals" && <Terminals />}
      {active === "page:servers" && <Servers />}
      {active === "page:schedules" && <Schedules />}
      {active === "page:projects" && <Projects />}
      {sukiOpen && <SukiPanel />}
    </main>
  );
}
