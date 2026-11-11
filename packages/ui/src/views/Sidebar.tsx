// Hermes-style sidebar: Threads / Agents tabs, primary nav, search, Live, then date-grouped
// threads. It replaces the separate thread-list column.

import { Fragment, memo, useCallback, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { LiveThread, Thread } from "@emp/protocol";
import { ErrorState, HarnessBadge, Kbd } from "../components/common";
import { IconAgents, IconChevron, IconClock, IconFilter, IconFolder, IconFolderPlus, IconPlus, IconSearch, IconServer, IconSettings, IconSuki, IconSources, IconTerminal } from "../components/icons";
import { cleanTitle, dateGroup } from "../lib/format";
import { marqueeHandlers } from "../lib/marquee";
import { formatShort } from "../lib/time";
import { useWorkspace } from "../store/context";
import { NAV_LABELS, clampScale, visible, type NavId } from "../store/settings";
import { CHAT_HARNESSES, HARNESS_FILTERS, TERMINAL_PROGRAMS, hasMoreThreads, selectedProject, showsGroups } from "../store/workspace";
import { GroupRow, NewGroupField, ThreadMenu, dragThread, groupThreadTitle, useThreadDrop } from "./ThreadGroups";

const SEARCH_DEBOUNCE_MS = 250;

export function Sidebar({ idPrefix }: { idPrefix: string }) {
  const tab = useWorkspace((s) => s.sidebarTab);
  const setTab = useWorkspace((s) => s.setSidebarTab);
  const activeTab = useWorkspace((s) => s.activeTab);
  const openNew = useWorkspace((s) => s.setNewThreadOpen);
  const showSection = useWorkspace((s) => s.showSection);
  const terminals = useWorkspace((s) => s.terminals);
  const nav = useWorkspace((s) => s.settings.nav);
  const settingsOpen = useWorkspace((s) => s.settingsPage !== null);
  const runningTerminals = Object.values(terminals).filter((t) => t.state === "running").length;
  const sukiOpen = useWorkspace((s) => s.suki.open);
  const setSukiOpen = useWorkspace((s) => s.setSukiOpen);
  const devServers = useWorkspace((s) => s.servers.data?.filter((d) => d.kind === "dev").length ?? 0);
  const activeSchedules = useWorkspace((s) => s.schedules.data?.filter((x) => x.enabled).length ?? 0);

  const item = (id: NavId) => {
    switch (id) {
      case "suki":
        return (
          <NavItem key={id} icon={<IconSuki />} current={sukiOpen} onClick={() => setSukiOpen(!sukiOpen)} trailing={<KeyCombo keys={["⌘", "/"]} />}>
            {NAV_LABELS.suki}
          </NavItem>
        );
      case "new":
        return (
          <NavItem key={id} icon={<IconPlus />} current={activeTab === "new"} onClick={() => openNew(true)} trailing={<KeyCombo keys={["⌘", "N"]} />}>
            {NAV_LABELS.new}
          </NavItem>
        );
      case "terminals":
        return (
          <NavItem
            key={id}
            icon={<IconTerminal />}
            current={activeTab === "page:terminals"}
            onClick={() => showSection("terminals")}
            trailing={runningTerminals > 0 ? <span className="emp-count">{runningTerminals}</span> : undefined}
          >
            {NAV_LABELS.terminals}
          </NavItem>
        );
      case "servers":
        return (
          <NavItem
            key={id}
            icon={<IconServer />}
            current={activeTab === "page:servers"}
            onClick={() => showSection("servers")}
            trailing={devServers > 0 ? <span className="emp-count">{devServers}</span> : undefined}
          >
            {NAV_LABELS.servers}
          </NavItem>
        );
      case "schedules":
        return (
          <NavItem
            key={id}
            icon={<IconClock />}
            current={activeTab === "page:schedules"}
            onClick={() => showSection("schedules")}
            trailing={activeSchedules > 0 ? <span className="emp-count">{activeSchedules}</span> : undefined}
          >
            {NAV_LABELS.schedules}
          </NavItem>
        );
      case "projects":
        return (
          <NavItem key={id} icon={<IconFolder />} current={activeTab === "page:projects"} onClick={() => showSection("projects")}>
            {NAV_LABELS.projects}
          </NavItem>
        );
      case "agents":
        return (
          <NavItem key={id} icon={<IconAgents />} current={activeTab === "page:agents"} onClick={() => showSection("agents")}>
            {NAV_LABELS.agents}
          </NavItem>
        );
      case "sources":
        return (
          <NavItem key={id} icon={<IconSources />} current={activeTab === "page:import"} onClick={() => showSection("import")}>
            {NAV_LABELS.sources}
          </NavItem>
        );
      case "settings":
        return (
          <NavItem key={id} icon={<IconSettings />} current={settingsOpen} onClick={() => showSection("settings")} trailing={<KeyCombo keys={["⌘", ","]} />}>
            {NAV_LABELS.settings}
          </NavItem>
        );
    }
  };

  return (
    <nav className="emp-sidebar" data-orion-ui="panel" aria-label="EMP">
      <ResizeHandle />
      <div className="emp-sb-head">
        <span className="emp-brand" data-orion-ui="title">
          EMP
        </span>
      </div>

      <div className="emp-tabs emp-tabs--sidebar" role="tablist" aria-label="Sidebar">
        <button type="button" role="tab" className="emp-tab" aria-selected={tab === "threads"} onClick={() => setTab("threads")}>
          Threads
        </button>
        <button type="button" role="tab" className="emp-tab" aria-selected={tab === "agents"} onClick={() => setTab("agents")}>
          Agents
        </button>
      </div>

      <div className="emp-nav">{nav.filter((e) => e.visible).map((e) => item(e.id))}</div>

      {tab === "threads" ? <ThreadsPane idPrefix={idPrefix} /> : <AgentsPane />}
    </nav>
  );
}

/** Drag the sidebar's inner edge to resize it; double-click resets. Saves on release. */
function ResizeHandle() {
  const side = useWorkspace((s) => s.settings.sidebarSide);
  const scale = useWorkspace((s) => s.settings.sidebarScale);
  const update = useWorkspace((s) => s.updateSettings);
  const drag = useRef<{ x: number; width: number; base: number; scale: number } | null>(null);

  return (
    <div
      className="emp-sb-resize"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize sidebar"
      aria-valuenow={Math.round(scale * 100)}
      title="Drag to resize · double-click to reset"
      onPointerDown={(e) => {
        const sidebar = e.currentTarget.parentElement;
        if (!sidebar || e.button !== 0) return;
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        const width = sidebar.getBoundingClientRect().width;
        drag.current = { x: e.clientX, width, base: width / scale, scale };
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (!d) return;
        const dx = (e.clientX - d.x) * (side === "right" ? -1 : 1);
        d.scale = clampScale((d.width + dx) / d.base);
        update({ sidebarScale: d.scale }, { persist: false });
      }}
      onPointerUp={(e) => {
        const d = drag.current;
        drag.current = null;
        e.currentTarget.releasePointerCapture(e.pointerId);
        if (d) update({ sidebarScale: d.scale });
      }}
      onDoubleClick={() => update({ sidebarScale: 1 })}
    />
  );
}

function KeyCombo({ keys }: { keys: string[] }) {
  return (
    <span className="emp-keys" aria-hidden="true">
      {keys.map((k) => (
        <Kbd key={k}>{k}</Kbd>
      ))}
    </span>
  );
}

function NavItem(props: { icon: ReactNode; current: boolean; onClick(): void; trailing?: ReactNode; children: ReactNode }) {
  return (
    <button type="button" className="emp-nav__item" aria-current={props.current ? "page" : undefined} onClick={props.onClick}>
      <span className="emp-nav__icon">{props.icon}</span>
      <span className="emp-truncate">{props.children}</span>
      {props.trailing}
    </button>
  );
}

/** Blue tracked label with Hermes's dither square. */
function SectionLabel({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div className="emp-label">
      <span className="emp-dither" aria-hidden="true" />
      <span className="emp-truncate">{children}</span>
      <span className="emp-row__spacer" />
      {action}
    </div>
  );
}

function liveStatus(l: LiveThread | undefined): string | null {
  if (!l) return null;
  return l.owner === "terminal" ? "terminal" : l.status;
}

function ThreadsPane({ idPrefix }: { idPrefix: string }) {
  const filter = useWorkspace((s) => s.filter);
  const threads = useWorkspace((s) => s.threads);
  const live = useWorkspace((s) => s.live);
  const selectedId = useWorkspace((s) => s.selectedThreadId);
  const activeTab = useWorkspace((s) => s.activeTab);
  const project = useWorkspace(selectedProject);
  const projects = useWorkspace((s) => s.projects);
  const now = useWorkspace((s) => s.now);
  const focusTick = useWorkspace((s) => s.searchFocusTick);
  const setQuery = useWorkspace((s) => s.setQuery);
  const setHarness = useWorkspace((s) => s.setHarness);
  const selectProject = useWorkspace((s) => s.selectProject);
  const setIncludeAutomated = useWorkspace((s) => s.setIncludeAutomated);
  const loadThreads = useWorkspace((s) => s.loadThreads);
  const loadMore = useWorkspace((s) => s.loadMoreThreads);
  const openThread = useWorkspace((s) => s.openThread);

  const [draft, setDraft] = useState(filter.query);
  const [activeId, setActiveId] = useState<string | null>(selectedId);
  const [showFilters, setShowFilters] = useState(false);
  // ⌘K reveals a hidden search box until it's empty and loses focus.
  const [revealed, setRevealed] = useState(false);
  const wantFocus = useRef(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const timer = setTimeout(() => setQuery(draft), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [draft, setQuery]);

  useEffect(() => {
    if (focusTick === 0) return;
    setRevealed(true);
    wantFocus.current = true;
  }, [focusTick]);

  // Focus after the reveal has rendered (a hidden input can't take focus).
  useEffect(() => {
    if (!revealed || !wantFocus.current) return;
    wantFocus.current = false;
    searchRef.current?.focus();
    searchRef.current?.select();
  }, [revealed, focusTick]);

  useEffect(() => {
    if (!activeId) return;
    listRef.current?.querySelector<HTMLElement>(`[data-thread-id="${CSS.escape(activeId)}"]`)?.scrollIntoView({ block: "nearest" });
  }, [activeId]);

  const onOpen = useCallback(
    (id: string) => {
      setActiveId(id);
      void openThread(id);
    },
    [openThread],
  );

  const onListKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const items = threads.items;
    if (!items.length) return;
    const idx = items.findIndex((t) => t.id === activeId);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const next = e.key === "ArrowDown" ? Math.min(items.length - 1, idx + 1) : Math.max(0, idx < 0 ? 0 : idx - 1);
      setActiveId(items[next]!.id);
    } else if (e.key === "Enter" && idx >= 0) {
      e.preventDefault();
      void openThread(items[idx]!.id);
    }
  };

  // Suki's own conversations show in its panel, not here.
  const sukiIds = useWorkspace((s) => s.sukiChats.data);
  const sukiThread = useWorkspace((s) => s.suki.threadId);
  const liveList = (useWorkspace((s) => visible(s.settings, "live")) ? Object.values(live) : []).filter(
    (l) => l.threadId !== sukiThread && !sukiIds?.some((t) => t.id === l.threadId),
  );
  const groups = useWorkspace((s) => s.groups.data);
  const removeFromGroup = useWorkspace((s) => s.removeFromGroup);
  const [naming, setNaming] = useState(false);
  const ungroupDrop = useThreadDrop((id) => void removeFromGroup(id));
  const grouping = showsGroups(filter);
  const pinnedGroups = grouping ? (groups ?? []).filter((g) => g.pinned) : [];
  const otherGroups = grouping ? (groups ?? []).filter((g) => !g.pinned) : [];
  const expanded = useWorkspace((s) => s.expandedThreads);
  const children = useWorkspace((s) => s.children);
  const toggleChildren = useWorkspace((s) => s.toggleChildren);
  // A thread row, plus its subagents underneath when expanded (searches list them flat).
  const renderThread = (t: Thread, groupId: string | null, depth = 0): ReactNode => {
    const open = t.childCount > 0 && expanded.includes(t.id) && !filter.query.trim();
    const kids = open ? children[t.id] : undefined;
    return (
      <Fragment key={t.id}>
        <ThreadRow
          domId={`${idPrefix}-t-${t.id}`}
          thread={t}
          now={now}
          selected={threadOpen && t.id === selectedId}
          active={t.id === activeId}
          liveStatus={liveStatus(live[t.id])}
          groupId={groupId}
          depth={depth}
          expanded={open}
          onToggle={t.childCount > 0 && !filter.query.trim() ? toggleChildren : undefined}
          onOpen={onOpen}
        />
        {open && (
          <div className="emp-sb-children" role="group" aria-label={`Subagents of ${cleanTitle(t.title)}`}>
            {kids?.status === "loading" && !kids.data && <div className="emp-sb-note">Loading…</div>}
            {kids?.status === "error" && <div className="emp-sb-note">{kids.error}</div>}
            {kids?.data?.map((c) => renderThread(c, null, depth + 1))}
          </div>
        )}
      </Fragment>
    );
  };
  const showSearch = useWorkspace((s) => visible(s.settings, "search"));
  const filtered = !!(filter.harness || filter.projectId || filter.includeAutomated);
  const threadOpen = activeTab.startsWith("t:");

  return (
    <>
      <label className="emp-sb-search" hidden={!showSearch && !revealed && !draft}>
        <IconSearch />
        <input
          ref={searchRef}
          className="emp-sb-search__input"
          data-orion-ui="input"
          type="search"
          placeholder="Search threads…"
          aria-label="Search threads"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => setRevealed(false)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              listRef.current?.focus();
              if (!activeId && threads.items[0]) setActiveId(threads.items[0].id);
            }
          }}
        />
      </label>

      <div
        ref={listRef}
        className="emp-sb-scroll"
        role="listbox"
        tabIndex={0}
        aria-label="Threads"
        aria-activedescendant={activeId ? `${idPrefix}-t-${activeId}` : undefined}
        onKeyDown={onListKeyDown}
      >
        {pinnedGroups.length > 0 && (
          <div className="emp-sb-section" role="presentation">
            <SectionLabel>Pinned</SectionLabel>
            {pinnedGroups.map((g) => (
              <GroupRow key={g.id} group={g} renderThread={renderThread} />
            ))}
          </div>
        )}

        {liveList.length > 0 && (
          <div className="emp-sb-section" role="presentation">
            <SectionLabel>Live</SectionLabel>
            {liveList.map((l) => {
              const t = threads.items.find((x) => x.id === l.threadId);
              return (
                <button
                  key={l.threadId}
                  type="button"
                  className="emp-sb-row"
                  aria-current={threadOpen && selectedId === l.threadId ? "true" : undefined}
                  title={l.error ?? undefined}
                  data-working={l.status === "running" || undefined}
                  onClick={() => void openThread(l.threadId)}
                  {...marqueeHandlers}
                >
                  <span className="emp-sb-bullet" data-status={liveStatus(l) ?? undefined} />
                  <span className="emp-sb-row__title emp-truncate emp-marquee">
                    <span className="emp-marquee__inner">
                      {t ? cleanTitle(t.title) : (groupThreadTitle(groups, l.threadId) ?? `Thread ${l.threadId.slice(-6)}`)}
                    </span>
                  </span>
                  <span className="emp-sb-row__time">{l.owner === "terminal" ? "term" : l.status === "running" ? "run" : ""}</span>
                </button>
              );
            })}
          </div>
        )}

        <div className="emp-sb-section" role="presentation" data-drop={ungroupDrop.over || undefined} {...ungroupDrop.props}>
          <SectionLabel
            action={
              <>
                <button
                  type="button"
                  className="emp-icon-button"
                  aria-label="New group"
                  title="New group"
                  onClick={() => setNaming(true)}
                >
                  <IconFolderPlus />
                </button>
                <button
                  type="button"
                  className="emp-icon-button"
                  aria-label="Filter threads"
                  aria-expanded={showFilters}
                  data-on={filtered || undefined}
                  title="Filter"
                  onClick={() => setShowFilters((v) => !v)}
                >
                  <IconFilter />
                </button>
              </>
            }
          >
            {project ? project.name : "Threads"}
          </SectionLabel>

          {showFilters && (
            <div className="emp-sb-filters" role="presentation">
              <div className="emp-chips" role="group" aria-label="Harness">
                <button type="button" className="emp-chip" aria-pressed={filter.harness === null} onClick={() => setHarness(null)}>
                  All
                </button>
                {HARNESS_FILTERS.map((h) => (
                  <button key={h} type="button" className="emp-chip" aria-pressed={filter.harness === h} onClick={() => setHarness(filter.harness === h ? null : h)}>
                    {h}
                  </button>
                ))}
              </div>
              <select
                className="emp-select"
                aria-label="Project"
                value={filter.projectId ?? ""}
                onChange={(e) => selectProject(e.target.value || null)}
              >
                <option value="">All projects</option>
                {projects.data?.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name} ({p.threadCount || p.automatedCount})
                  </option>
                ))}
              </select>
              <label className="emp-check">
                <input type="checkbox" checked={filter.includeAutomated} onChange={(e) => setIncludeAutomated(e.target.checked)} />
                Show automated runs
              </label>
            </div>
          )}

          {naming && <NewGroupField onDone={() => setNaming(false)} />}
          {otherGroups.map((g) => (
            <GroupRow key={g.id} group={g} renderThread={renderThread} />
          ))}
          {otherGroups.length > 0 && threads.items.length > 0 && <div className="emp-sb-groupgap" role="presentation" />}

          {threads.status === "idle" && <div className="emp-sb-note">Waiting for empd…</div>}
          {threads.status === "loading" && threads.items.length === 0 && <div className="emp-sb-note">Loading…</div>}
          {threads.status === "error" && threads.items.length === 0 && (
            <ErrorState message={threads.error ?? "Could not load threads"} onRetry={() => void loadThreads()} />
          )}
          {threads.status === "ready" && threads.items.length === 0 && (
            <div className="emp-sb-note">{filter.query || filtered ? "No matching threads" : "No threads yet. Open Sources to import."}</div>
          )}

          {threads.items.map((t, i) => {
            const group = dateGroup(t.updatedAt, now);
            const prev = i > 0 ? dateGroup(threads.items[i - 1]!.updatedAt, now) : null;
            return (
              <div key={t.id} role="presentation">
                {group !== prev && group !== "Today" && (
                  <div className="emp-sb-group" role="presentation">
                    <span>{group}</span>
                  </div>
                )}
                {renderThread(t, null)}
              </div>
            );
          })}
          {hasMoreThreads(threads) && threads.status === "ready" && (
            <button type="button" className="emp-sb-more" disabled={threads.loadingMore} onClick={() => void loadMore()}>
              {threads.loadingMore ? "Loading…" : `Show more (${(threads.total - threads.items.length).toLocaleString()})`}
            </button>
          )}
        </div>
      </div>
    </>
  );
}

interface ThreadRowProps {
  domId: string;
  thread: Thread;
  now: number;
  selected: boolean;
  active: boolean;
  liveStatus: string | null;
  /** The group this row is shown under, if any. */
  groupId: string | null;
  /** 0 for top-level threads, 1+ for subagents. */
  depth?: number;
  expanded?: boolean;
  /** Set when the thread has subagents: shows the expander. */
  onToggle?(id: string): void;
  onOpen(id: string): void;
}

const ThreadRow = memo(function ThreadRow({ domId, thread, now, selected, active, liveStatus, groupId, depth = 0, expanded, onToggle, onOpen }: ThreadRowProps) {
  return (
    <div
      id={domId}
      data-thread-id={thread.id}
      className="emp-sb-row"
      role="option"
      aria-selected={selected}
      aria-expanded={onToggle ? !!expanded : undefined}
      data-active={active || undefined}
      data-working={liveStatus === "running" || undefined}
      data-depth={depth || undefined}
      draggable
      onDragStart={(e) => dragThread(e, thread.id)}
      onClick={() => onOpen(thread.id)}
      {...marqueeHandlers}
    >
      <span className="emp-sb-bullet" data-status={liveStatus ?? undefined} data-harness={thread.harness} />
      <span className="emp-sb-row__title emp-truncate emp-marquee">
        <span className="emp-marquee__inner">{cleanTitle(thread.title)}</span>
      </span>
      {onToggle && (
        <button
          type="button"
          className="emp-sb-sub"
          aria-label={`${expanded ? "Hide" : "Show"} ${thread.childCount} subagent${thread.childCount === 1 ? "" : "s"}`}
          title={`${thread.childCount} subagent${thread.childCount === 1 ? "" : "s"}`}
          onClick={(e) => {
            e.stopPropagation();
            onToggle(thread.id);
          }}
        >
          <IconChevron open={expanded} />
          <span>{thread.childCount}</span>
        </button>
      )}
      <span className="emp-sb-row__time">{formatShort(thread.updatedAt, now)}</span>
      <ThreadMenu threadId={thread.id} groupId={groupId} />
    </div>
  );
});

function AgentsPane() {
  const agents = useWorkspace((s) => s.agents);
  const openNew = useWorkspace((s) => s.setNewThreadOpen);
  const setAgent = useWorkspace((s) => s.setNewThreadAgent);
  const showSection = useWorkspace((s) => s.showSection);
  const runInTerminal = useWorkspace((s) => s.runInTerminal);

  return (
    <div className="emp-sb-scroll">
      <div className="emp-sb-section">
        <SectionLabel
          action={
            <button type="button" className="emp-sb-link" onClick={() => showSection("agents")}>
              Manage
            </button>
          }
        >
          Agents
        </SectionLabel>
        {agents.status === "loading" && !agents.data && <div className="emp-sb-note">Loading…</div>}
        {agents.data?.map((a) => {
          const chat = CHAT_HARNESSES.includes(a.harness);
          return (
            <button
              key={a.id}
              type="button"
              className="emp-sb-row emp-sb-row--agent"
              title={chat ? `New thread with ${a.name}` : `Open ${a.name} in a terminal (chat with ${a.harness} isn't supported yet)`}
              onClick={() => {
                if (chat) {
                  setAgent(a.id);
                  openNew(true);
                } else if (a.harness === "shell") void runInTerminal("zsh", ["-l"], a.cwd, a.name);
                else void runInTerminal(TERMINAL_PROGRAMS[a.harness] ?? a.harness, [], a.cwd, a.name);
              }}
            >
              <span className="emp-sb-row__title emp-truncate">{a.name}</span>
              <HarnessBadge harness={a.harness} />
            </button>
          );
        })}
      </div>
    </div>
  );
}
