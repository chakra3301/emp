// Top tab strip, Hermes-style: uppercase tabs with an accent underline, "+" for a new thread,
// and the open thread's actions as quiet icons on the right.

import type { Thread } from "@emp/protocol";
import { IconClose, IconExpand, IconInfo, IconLayout, IconMenu, IconPlus, IconReturn, IconSettings, IconSwap, IconTerminal } from "../components/icons";
import { cleanTitle } from "../lib/format";
import { marqueeHandlers } from "../lib/marquee";
import { useWorkspace } from "../store/context";
import { liveFor, type TabId } from "../store/workspace";

const PAGE_TITLES: Record<string, string> = { agents: "Agents", terminals: "Terminals", servers: "Dev servers", schedules: "Scheduled", import: "Sources", projects: "Projects" };

function tabTitle(tab: TabId, titles: Record<string, string>): string {
  if (tab === "new") return "New thread";
  if (tab.startsWith("t:")) return cleanTitle(titles[tab.slice(2)] ?? "Thread");
  return PAGE_TITLES[tab.slice(5)] ?? tab.slice(5);
}

/** Orion-only chats live in orion.db; Gemini and plain shells can't be reopened by id. */
export function canTerminal(t: Thread): boolean {
  return !(t.session.profile?.startsWith("orion:") ?? false) && t.harness !== "gemini" && t.harness !== "shell";
}

export function TabBar() {
  const tabs = useWorkspace((s) => s.tabs);
  const active = useWorkspace((s) => s.activeTab);
  const titles = useWorkspace((s) => s.tabTitles);
  const activate = useWorkspace((s) => s.activateTab);
  const close = useWorkspace((s) => s.closeTab);
  const openNew = useWorkspace((s) => s.setNewThreadOpen);
  const live = useWorkspace((s) => s.live);
  const drawer = useWorkspace((s) => s.drawerOpen);
  const setDrawer = useWorkspace((s) => s.setDrawerOpen);
  const status = (tab: TabId) => {
    const l = tab.startsWith("t:") ? live[tab.slice(2)] : undefined;
    return l ? (l.owner === "terminal" ? "terminal" : l.status) : undefined;
  };

  return (
    <div className="emp-tabbar" data-orion-ui="toolbar">
      <button type="button" className="emp-icon-button emp-tabbar__menu" aria-label="Menu" aria-expanded={drawer} onClick={() => setDrawer(!drawer)}>
        <IconMenu />
      </button>
      <div className="emp-tabbar__tabs" role="tablist" aria-label="Open">
        {tabs.map((tab) => (
          <div key={tab} className="emp-tabbar__tab" data-active={tab === active || undefined}>
            <button type="button" role="tab" aria-selected={tab === active} className="emp-tabbar__label" onClick={() => activate(tab)} {...marqueeHandlers}>
              <span className="emp-tabbar__marker" data-new={tab === "new" || undefined} data-status={status(tab)} aria-hidden="true" />
              <span className="emp-truncate emp-marquee">
                <span className="emp-marquee__inner">{tabTitle(tab, titles)}</span>
              </span>
            </button>
            {(tabs.length > 1 || tab !== "new") && (
              <button type="button" className="emp-tabbar__close" aria-label={`Close ${tabTitle(tab, titles)}`} onClick={() => close(tab)}>
                <IconClose />
              </button>
            )}
          </div>
        ))}
        <button type="button" className="emp-icon-button emp-tabbar__add" aria-label="New thread" title="New thread" onClick={() => openNew(true)}>
          <IconPlus />
        </button>
      </div>
      {active.startsWith("t:") && <ThreadActions />}
      <AppActions />
    </div>
  );
}

/** Hermes's titlebar app controls: settings, layout editor, swap sidebar sides. */
function AppActions() {
  const settingsOpen = useWorkspace((s) => s.settingsPage !== null);
  const openSettings = useWorkspace((s) => s.openSettings);
  const editing = useWorkspace((s) => s.layoutEditing);
  const setEditing = useWorkspace((s) => s.setLayoutEditing);
  const reset = useWorkspace((s) => s.resetLayout);
  const side = useWorkspace((s) => s.settings.sidebarSide);
  const update = useWorkspace((s) => s.updateSettings);

  return (
    <div className="emp-tabbar__actions emp-tabbar__app" data-orion-ui="toolbar">
      <button type="button" className="emp-icon-button" aria-label="Open settings" title="Settings (⌘,)" aria-pressed={settingsOpen} onClick={() => openSettings()}>
        <IconSettings />
      </button>
      <button
        type="button"
        className="emp-icon-button"
        aria-label="Layout editor"
        title={"Layout editor (⌘⇧\\) — ⌘-click resets the layout"}
        aria-pressed={editing}
        onClick={(e) => {
          if (e.metaKey || e.ctrlKey) reset();
          else setEditing(!editing);
        }}
      >
        <IconLayout />
      </button>
      <button
        type="button"
        className="emp-icon-button"
        aria-label="Swap sidebar sides"
        title="Swap sidebar sides"
        onClick={() => update({ sidebarSide: side === "left" ? "right" : "left" })}
      >
        <IconSwap />
      </button>
    </div>
  );
}

function ThreadActions() {
  const t = useWorkspace((s) => s.thread.data);
  const live = useWorkspace((s) => liveFor(s, s.selectedThreadId));
  const takeover = useWorkspace((s) => s.takeover);
  const release = useWorkspace((s) => s.release);
  const openShell = useWorkspace((s) => s.openShell);
  const details = useWorkspace((s) => s.detailsOpen);
  const toggleDetails = useWorkspace((s) => s.toggleDetails);
  if (!t) return null;
  const inTerminal = live?.owner === "terminal";
  const running = live?.status === "running";

  return (
    <div className="emp-tabbar__actions" data-orion-ui="toolbar">
      {canTerminal(t) &&
        (inTerminal ? (
          <button type="button" className="emp-icon-button" aria-label="Return to chat" title="Return to chat" onClick={() => void release()}>
            <IconReturn />
          </button>
        ) : (
          <button
            type="button"
            className="emp-icon-button"
            aria-label="Open in terminal"
            title={running ? "Wait for the turn to finish, or stop it" : "Open this session in its own terminal UI"}
            disabled={running}
            onClick={() => void takeover()}
          >
            <IconExpand />
          </button>
        ))}
      <button type="button" className="emp-icon-button" aria-label="Shell" title="Shell in this thread's folder" onClick={() => void openShell()}>
        <IconTerminal />
      </button>
      <button type="button" className="emp-icon-button" aria-label="Details" title="Details" aria-pressed={details} onClick={toggleDetails}>
        <IconInfo />
      </button>
    </div>
  );
}
