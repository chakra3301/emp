// Settings, copied from Hermes desktop's settings overlay: a raised card over a dimmed
// workspace, a left rail of sections (with indented sub-pages), a breadcrumb header and
// ListRows (title + description left, control right). A search pill rides the card's top edge.

import { Fragment, useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { Kbd } from "../components/common";
import { Segmented, Switch } from "../components/controls";
import {
  IconChat,
  IconChevron,
  IconChevronRight,
  IconChevronUp,
  IconClose,
  IconGrip,
  IconInfo,
  IconKeyboard,
  IconLayout,
  IconMonitor,
  IconPalette,
  IconPhone,
  IconRefresh,
  IconSearch,
} from "../components/icons";
import { Picker } from "../components/Picker";
import { PhonePage } from "./Phone";
import { useHost, useWorkspace } from "../store/context";
import { ColorWheel } from "../components/ColorWheel";
import { COMPACT_MAX, COMPACT_MIN, COMPACT_PRESETS, formatTokens, parseTokens } from "../lib/compaction";
import {
  DEFAULT_SETTINGS,
  NAV_LABELS,
  PINNED_NAV,
  SIDEBAR_SCALE_MAX,
  SIDEBAR_SCALE_MIN,
  isCustomAccent,
  moveNav,
  type NavEntry,
} from "../store/settings";
import { CHAT_HARNESSES } from "../store/workspace";

type PageId = "appearance/theme" | "appearance/window-layout" | "threads" | "phone" | "keyboard" | "about";

interface NavLink {
  id: PageId | "appearance";
  label: string;
  icon: () => ReactNode;
  children?: NavLink[];
  gapBefore?: boolean;
}

const NAV: NavLink[] = [
  {
    id: "appearance",
    label: "Appearance",
    icon: IconPalette,
    children: [
      { id: "appearance/theme", label: "Theme", icon: IconPalette },
      { id: "appearance/window-layout", label: "Window layout", icon: IconMonitor },
    ],
  },
  { id: "threads", label: "Threads", icon: IconChat },
  { id: "phone", label: "Phone", icon: IconPhone },
  { id: "keyboard", label: "Keyboard shortcuts", icon: IconKeyboard },
  { id: "about", label: "About", icon: IconInfo, gapBefore: true },
];

const PAGE_IDS: PageId[] = ["appearance/theme", "appearance/window-layout", "threads", "phone", "keyboard", "about"];

/** Every row, for the pages and for search. */
const ROWS: Record<string, { page: PageId; title: string; description?: string; keywords?: string }> = {
  accent: {
    page: "appearance/theme",
    title: "Highlight colour",
    description: "Links, buttons, selected rows, focus rings, text selection and running work.",
    keywords: "colour color accent highlight selection theme wheel",
  },
  background: {
    page: "appearance/theme",
    title: "Background",
    description: "Panels, cards and lines are mixed from it. Dark backgrounds switch the text to light ink.",
    keywords: "colour color background dark light theme wheel",
  },
  layoutEditor: {
    page: "appearance/window-layout",
    title: "Layout editor",
    description: "Rearrange the workspace in place: drag zones across, pick a template, save your own.",
    keywords: "arrange zones template",
  },
  interfaceMode: {
    page: "appearance/window-layout",
    title: "Interface mode",
    description: "Simple hides the Live section, search box and status bar. Changes what is shown, not what EMP can do.",
    keywords: "simple advanced",
  },
  sidebarSide: { page: "appearance/window-layout", title: "Sidebar side", description: "Which side of the window the sidebar sits on.", keywords: "left right swap" },
  sidebarWidth: {
    page: "appearance/window-layout",
    title: "Sidebar width",
    description: "You can also drag the sidebar's edge. Double-click the edge to reset.",
    keywords: "resize size",
  },
  threadWidth: {
    page: "appearance/window-layout",
    title: "Thread width",
    description: "Readable keeps the transcript and composer in a centred column.",
    keywords: "column readable full",
  },
  search: { page: "appearance/window-layout", title: "Search box", description: "Above the thread list. ⌘K opens it even when it's hidden.", keywords: "find" },
  live: { page: "appearance/window-layout", title: "Live section", description: "Running and terminal-owned threads above the thread list." },
  statusBar: { page: "appearance/window-layout", title: "Status bar", description: "Daemon state, live counts and the version along the bottom." },
  navItems: {
    page: "appearance/window-layout",
    title: "Sidebar items",
    description: "Drag to reorder; switch off to hide. Settings always stays.",
    keywords: "nav navigation order hide",
  },
  compaction: {
    page: "threads",
    title: "Compact chats at",
    description:
      "When a chat's context grows past this, the agent summarises the older part and keeps going. Long chats then cost far less per message than running to the model's full window (up to 1M).",
    keywords: "compaction compact context tokens window cost credits summarise summarize auto-compact",
  },
  notifications: {
    page: "threads",
    title: "Notifications",
    description: "A system notification when a reply finishes, fails or needs your input while you're looking elsewhere. Click it to open the chat.",
    keywords: "notify alert done finished sound",
  },
  defaultAgent: {
    page: "threads",
    title: "Default agent",
    description: "Picked first in the New thread composer.",
    keywords: "desk new thread composer",
  },
  phoneAccess: {
    page: "phone",
    title: "Phone access",
    description: "Use EMP from your phone over Tailscale. Only devices signed in to your Tailscale account can reach it; nothing is on the public internet.",
    keywords: "mobile iphone android tailscale remote web",
  },
  pairPhone: {
    page: "phone",
    title: "Pair a phone",
    description: "Each phone gets its own key. A pairing code works for 10 minutes.",
    keywords: "qr code mobile iphone pair",
  },
  devices: {
    page: "phone",
    title: "Paired devices",
    description: "Revoke a device to sign it out at once.",
    keywords: "phone revoke unpair",
  },
  daemon: { page: "about", title: "EMP daemon", description: "empd keeps agents running when the window is closed." },
  host: { page: "about", title: "Host", description: "Where this EMP window runs." },
  storage: { page: "about", title: "Settings storage", description: "Settings are saved on this device by the host." },
};

const SHORTCUTS: [string, string[]][] = [
  ["New thread", ["⌘", "N"]],
  ["Search threads", ["⌘", "K"]],
  ["Settings", ["⌘", ","]],
  ["Layout editor", ["⌘", "⇧", "\\"]],
  ["Send (steers a running turn)", ["↵"]],
  ["Queue a follow-up", ["⌘", "↵"]],
  ["New line", ["⇧", "↵"]],
  ["Stop the running turn", ["Esc", "Esc"]],
  ["Close Settings / leave the layout editor", ["Esc"]],
];

function asPage(page: string | null): PageId {
  return (PAGE_IDS as string[]).includes(page ?? "") ? (page as PageId) : "appearance/theme";
}

export function SettingsOverlay() {
  const pageRaw = useWorkspace((s) => s.settingsPage);
  const open = useWorkspace((s) => s.openSettings);
  const close = useWorkspace((s) => s.closeSettings);
  const page = asPage(pageRaw);
  const [query, setQuery] = useState<string | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  const mainRef = useRef<HTMLDivElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);

  // Modal focus: start inside the dialog so Esc and ⌘, reach the workspace root.
  useEffect(() => {
    const el = cardRef.current;
    const root = el?.closest<HTMLElement>(".emp-root");
    el?.focus();
    return () => root?.focus({ preventScroll: true });
  }, []);

  // Deep link from search: scroll the row into view and flash it.
  useEffect(() => {
    if (!flash) return;
    const row = mainRef.current?.querySelector<HTMLElement>(`[data-row="${flash}"]`);
    row?.scrollIntoView({ block: "center" });
    row?.setAttribute("data-flash", "");
    const t = setTimeout(() => {
      row?.removeAttribute("data-flash");
      setFlash(null);
    }, 1600);
    return () => clearTimeout(t);
  }, [flash, page]);

  const group = NAV.find((g) => g.id === page || g.children?.some((c) => c.id === page))!;
  const child = group.children?.find((c) => c.id === page);

  return (
    <div
      className="emp-overlay"
      role="presentation"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div ref={cardRef} className="emp-overlay__card" role="dialog" aria-modal="true" aria-label="Settings" tabIndex={-1}>
        <SearchPill query={query} onQuery={setQuery} />
        <button type="button" className="emp-icon-button emp-overlay__close" aria-label="Close settings" title="Close (Esc)" onClick={close}>
          <IconClose />
        </button>
        <div className="emp-overlay__split">
          <SettingsNav page={page} onOpen={(p) => (setQuery(null), open(p))} />
          <div className="emp-overlay__main" ref={mainRef}>
            {query !== null && query.trim() ? (
              <SearchResults
                query={query}
                onPick={(row) => {
                  setQuery(null);
                  open(ROWS[row]!.page);
                  setFlash(row);
                  // The clicked hit unmounts; keep focus in the dialog so Esc still closes it.
                  cardRef.current?.focus();
                }}
              />
            ) : (
              <>
                <nav className="emp-breadcrumb" aria-label="Breadcrumb">
                  <span>Settings</span>
                  <IconChevronRight />
                  {child ? (
                    <>
                      <button type="button" className="emp-breadcrumb__link" onClick={() => open(group.children![0]!.id as PageId)}>
                        {group.label}
                      </button>
                      <IconChevronRight />
                      <span aria-current="page" className="emp-breadcrumb__current">
                        {child.label}
                      </span>
                    </>
                  ) : (
                    <span aria-current="page" className="emp-breadcrumb__current">
                      {group.label}
                    </span>
                  )}
                </nav>
                <div className="emp-settings-content">
                  {page === "appearance/theme" && <ThemePage />}
                  {page === "appearance/window-layout" && <WindowLayoutPage />}
                  {page === "threads" && <ThreadsPage />}
                  {page === "phone" && <PhonePage />}
                  {page === "keyboard" && <KeyboardPage />}
                  {page === "about" && <AboutPage />}
                </div>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function SettingsNav({ page, onOpen }: { page: PageId; onOpen(page: PageId): void }) {
  const reset = useWorkspace((s) => s.updateSettings);
  const [confirming, setConfirming] = useState(false);

  return (
    <aside className="emp-overlay__rail" aria-label="Settings sections">
      {NAV.map((g) => {
        const active = g.id === page || !!g.children?.some((c) => c.id === page);
        return (
          <Fragment key={g.id}>
            {g.gapBefore && <div className="emp-overlay__gap" aria-hidden="true" />}
            <RailItem
              icon={g.icon}
              label={g.label}
              active={active}
              current={g.id === page}
              onClick={() => onOpen((g.children ? g.children[0]!.id : g.id) as PageId)}
            />
            {active &&
              g.children?.map((c) => (
                <RailItem key={c.id} nested icon={c.icon} label={c.label} active={c.id === page} current={c.id === page} onClick={() => onOpen(c.id as PageId)} />
              ))}
          </Fragment>
        );
      })}
      <span className="emp-row__spacer" />
      <div className="emp-overlay__rail-footer">
        <button
          type="button"
          className="emp-icon-button"
          data-danger={confirming || undefined}
          aria-label={confirming ? "Click again to reset every setting" : "Reset to defaults"}
          title={confirming ? "Click again to reset every setting" : "Reset to defaults"}
          onBlur={() => setConfirming(false)}
          onClick={() => {
            if (!confirming) return setConfirming(true);
            reset({ ...DEFAULT_SETTINGS, nav: DEFAULT_SETTINGS.nav.map((e) => ({ ...e })), savedLayouts: [] });
            setConfirming(false);
          }}
        >
          <IconRefresh />
        </button>
        {confirming && <span className="emp-overlay__confirm">Click again to reset</span>}
      </div>
    </aside>
  );
}

function RailItem(props: { icon: () => ReactNode; label: string; active: boolean; current: boolean; nested?: boolean; onClick(): void }) {
  const Icon = props.icon;
  return (
    <button
      type="button"
      className="emp-rail-item"
      data-nested={props.nested || undefined}
      data-active={props.active || undefined}
      aria-current={props.current ? "page" : undefined}
      onClick={props.onClick}
    >
      <Icon />
      <span className="emp-truncate">{props.label}</span>
    </button>
  );
}

/** The pill riding the card's top edge; it grows into a search field. */
function SearchPill({ query, onQuery }: { query: string | null; onQuery(q: string | null): void }) {
  const input = useRef<HTMLInputElement>(null);
  const isOpen = query !== null;
  useEffect(() => {
    if (isOpen) input.current?.focus();
  }, [isOpen]);

  if (query === null) {
    return (
      <button type="button" className="emp-search-pill" onClick={() => onQuery("")}>
        <IconSearch />
        <span>Search settings</span>
      </button>
    );
  }
  return (
    <label className="emp-search-pill emp-search-pill--open">
      <IconSearch />
      <input
        ref={input}
        aria-label="Search settings"
        placeholder="Search settings…"
        value={query}
        onChange={(e) => onQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onQuery(null);
          }
        }}
        onBlur={() => !query.trim() && onQuery(null)}
      />
    </label>
  );
}

function SearchResults({ query, onPick }: { query: string; onPick(row: string): void }) {
  const q = query.trim().toLowerCase();
  // Title matches first, then description/keyword matches.
  const hits = Object.entries(ROWS)
    .filter(([, r]) => `${r.title} ${r.description ?? ""} ${r.keywords ?? ""}`.toLowerCase().includes(q))
    .sort(([, a], [, b]) => Number(!a.title.toLowerCase().includes(q)) - Number(!b.title.toLowerCase().includes(q)));
  const pageLabel = (p: PageId) => {
    const g = NAV.find((x) => x.id === p || x.children?.some((c) => c.id === p))!;
    const c = g.children?.find((x) => x.id === p);
    return c ? `${g.label} › ${c.label}` : g.label;
  };
  return (
    <div className="emp-settings-content">
      <div className="emp-breadcrumb">
        <span>Search</span>
        <IconChevronRight />
        <span className="emp-breadcrumb__current">
          {hits.length} result{hits.length === 1 ? "" : "s"}
        </span>
      </div>
      {hits.length === 0 && <p className="emp-list-row__desc">No settings match “{query.trim()}”.</p>}
      {hits.map(([id, r]) => (
        <button key={id} type="button" className="emp-search-hit" onClick={() => onPick(id)}>
          <span className="emp-list-row__title">{r.title}</span>
          <span className="emp-list-row__desc">{pageLabel(r.page)}</span>
        </button>
      ))}
    </div>
  );
}

/** Hermes's ListRow: title + description on the left, the control on the right. `wide` puts the content below. */
export function ListRow({ row, action, below, wide }: { row: keyof typeof ROWS; action?: ReactNode; below?: ReactNode; wide?: boolean }) {
  const r = ROWS[row]!;
  return (
    <div className="emp-list-row" data-row={row} data-wide={wide || undefined}>
      <div className="emp-list-row__text">
        <div className="emp-list-row__title">
          <span>{r.title}</span>
          {wide && action && <div className="emp-list-row__inline">{action}</div>}
        </div>
        {r.description && <div className="emp-list-row__desc">{r.description}</div>}
        {below}
      </div>
      {!wide && action && <div className="emp-list-row__action">{action}</div>}
    </div>
  );
}

function SectionHeading({ icon: Icon, title }: { icon: () => ReactNode; title: string }) {
  return (
    <div className="emp-settings-heading">
      <Icon />
      <span>{title}</span>
    </div>
  );
}

/** Token defaults (design/tokens.json `color.accent` / `color.background`), shown when nothing is picked. */
const TOKEN_ACCENT = "#e8173f";
const TOKEN_BACKGROUND = "#ffffff";

function ThemePage() {
  const accent = useWorkspace((s) => s.settings.accent);
  const background = useWorkspace((s) => s.settings.background);
  const update = useWorkspace((s) => s.updateSettings);
  const custom = isCustomAccent(accent);

  return (
    <>
      <ListRow
        row="accent"
        wide
        action={
          <button type="button" className="emp-button emp-button--ghost emp-button--small" data-orion-ui="button" disabled={!custom} onClick={() => update({ accent: DEFAULT_SETTINGS.accent })}>
            Reset
          </button>
        }
        below={<ColorWheel label="Highlight colour" value={custom ? accent : TOKEN_ACCENT} onChange={(hex, final) => update({ accent: hex }, { persist: final })} />}
      />
      <ListRow
        row="background"
        wide
        action={
          <button type="button" className="emp-button emp-button--ghost emp-button--small" data-orion-ui="button" disabled={!background} onClick={() => update({ background: null })}>
            Reset
          </button>
        }
        below={<ColorWheel label="Background" value={background ?? TOKEN_BACKGROUND} onChange={(hex, final) => update({ background: hex }, { persist: final })} />}
      />
    </>
  );
}

function WindowLayoutPage() {
  const s = useWorkspace((st) => st.settings);
  const update = useWorkspace((st) => st.updateSettings);
  const edit = useWorkspace((st) => st.setLayoutEditing);
  const simple = s.interfaceMode === "simple";

  return (
    <>
      <ListRow
        row="layoutEditor"
        action={
          <button type="button" className="emp-button" data-orion-ui="button" onClick={() => edit(true)}>
            <IconLayout />
            Open layout editor
            <span className="emp-keys">
              <Kbd>⌘</Kbd>
              <Kbd>⇧</Kbd>
              <Kbd>\</Kbd>
            </span>
          </button>
        }
      />
      <ListRow
        row="interfaceMode"
        action={
          <Segmented
            label="Interface mode"
            value={s.interfaceMode}
            options={[
              ["simple", "Simple"],
              ["advanced", "Advanced"],
            ]}
            onChange={(interfaceMode) => update({ interfaceMode })}
          />
        }
      />
      <ListRow
        row="sidebarSide"
        action={
          <Segmented
            label="Sidebar side"
            value={s.sidebarSide}
            options={[
              ["left", "Left"],
              ["right", "Right"],
            ]}
            onChange={(sidebarSide) => update({ sidebarSide })}
          />
        }
      />
      <ListRow
        row="sidebarWidth"
        action={
          <div className="emp-slider">
            <input
              type="range"
              aria-label="Sidebar width"
              min={SIDEBAR_SCALE_MIN * 100}
              max={SIDEBAR_SCALE_MAX * 100}
              step={5}
              value={Math.round(s.sidebarScale * 100)}
              onChange={(e) => update({ sidebarScale: Number(e.target.value) / 100 }, { persist: false })}
              onPointerUp={() => update({})}
              onKeyUp={() => update({})}
            />
            <span className="emp-slider__value">{Math.round(s.sidebarScale * 100)}%</span>
          </div>
        }
      />
      <ListRow
        row="threadWidth"
        action={
          <Segmented
            label="Thread width"
            value={s.threadWidth}
            options={[
              ["full", "Full width"],
              ["readable", "Readable"],
            ]}
            onChange={(threadWidth) => update({ threadWidth })}
          />
        }
      />
      <ListRow row="search" action={<Switch label="Search box" checked={s.showSearch && !simple} disabled={simple} onChange={(showSearch) => update({ showSearch })} />} />
      <ListRow row="live" action={<Switch label="Live section" checked={s.showLive && !simple} disabled={simple} onChange={(showLive) => update({ showLive })} />} />
      <ListRow row="statusBar" action={<Switch label="Status bar" checked={s.statusBar && !simple} disabled={simple} onChange={(statusBar) => update({ statusBar })} />} />
      <ListRow row="navItems" wide below={<NavOrder nav={s.nav} onChange={(nav) => update({ nav })} />} />
    </>
  );
}

/** Reorder (drag, or the arrow buttons) and show/hide the sidebar's nav items. */
function NavOrder({ nav, onChange }: { nav: NavEntry[]; onChange(nav: NavEntry[]): void }) {
  const [dragId, setDragId] = useState<string | null>(null);
  const [overIndex, setOverIndex] = useState<number | null>(null);

  const drop = (to: number) => {
    const from = nav.findIndex((e) => e.id === dragId);
    if (from >= 0 && to !== from) onChange(moveNav(nav, nav[from]!.id, to > from ? to - from - 1 : to - from));
    setDragId(null);
    setOverIndex(null);
  };

  return (
    <ol className="emp-nav-order">
      {nav.map((e, i) => {
        const pinned = PINNED_NAV.includes(e.id);
        return (
          <li
            key={e.id}
            className="emp-nav-order__item"
            draggable
            data-dragging={dragId === e.id || undefined}
            data-drop={overIndex === i ? "before" : overIndex === i + 1 && i === nav.length - 1 ? "after" : undefined}
            onDragStart={(ev) => {
              ev.dataTransfer.effectAllowed = "move";
              ev.dataTransfer.setData("text/plain", e.id);
              setDragId(e.id);
            }}
            onDragOver={(ev) => {
              if (!dragId) return;
              ev.preventDefault();
              const r = ev.currentTarget.getBoundingClientRect();
              setOverIndex(ev.clientY > r.top + r.height / 2 ? i + 1 : i);
            }}
            onDrop={(ev) => {
              ev.preventDefault();
              drop(overIndex ?? i);
            }}
            onDragEnd={() => {
              setDragId(null);
              setOverIndex(null);
            }}
          >
            <span className="emp-nav-order__grip" aria-hidden="true">
              <IconGrip />
            </span>
            <span className="emp-nav-order__label" data-hidden={!e.visible || undefined}>
              {NAV_LABELS[e.id]}
            </span>
            <button type="button" className="emp-icon-button" aria-label={`Move ${NAV_LABELS[e.id]} up`} disabled={i === 0} onClick={() => onChange(moveNav(nav, e.id, -1))}>
              <IconChevronUp />
            </button>
            <button
              type="button"
              className="emp-icon-button"
              aria-label={`Move ${NAV_LABELS[e.id]} down`}
              disabled={i === nav.length - 1}
              onClick={() => onChange(moveNav(nav, e.id, 1))}
            >
              <IconChevron open />
            </button>
            <Switch
              label={`Show ${NAV_LABELS[e.id]}`}
              checked={e.visible}
              disabled={pinned}
              onChange={(visible) => onChange(nav.map((x) => (x.id === e.id ? { ...x, visible } : x)))}
            />
          </li>
        );
      })}
    </ol>
  );
}

function ThreadsPage() {
  const agents = useWorkspace((s) => s.agents);
  const loadAgents = useWorkspace((s) => s.loadAgents);
  const defaultAgentId = useWorkspace((s) => s.settings.defaultAgentId);
  const update = useWorkspace((s) => s.updateSettings);
  const usable = (agents.data ?? []).filter((a) => CHAT_HARNESSES.includes(a.harness) && !a.coordinator);
  const picked = usable.find((a) => a.id === defaultAgentId);

  useEffect(() => {
    if (agents.status === "idle") void loadAgents();
  }, [agents.status, loadAgents]);

  return (
    <>
      <CompactionRow />
      <NotificationsRow />
      <ListRow
        row="defaultAgent"
        action={
          <Picker
            title="Default agent"
            label={picked?.name ?? "Automatic"}
            value={defaultAgentId ?? ""}
            onChange={(id) => update({ defaultAgentId: id || null })}
            options={[{ id: "", label: "Automatic", detail: "The first agent" }, ...usable.map((a) => ({ id: a.id, label: a.name, detail: a.harness }))]}
          />
        }
      />
    </>
  );
}

function CompactionRow() {
  const settings = useWorkspace((s) => s.daemonSettings);
  const load = useWorkspace((s) => s.loadDaemonSettings);
  const save = useWorkspace((s) => s.saveDaemonSettings);
  const value = settings.data?.compactAt ?? null;
  const [custom, setCustom] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (settings.status === "idle") void load();
  }, [settings.status, load]);

  const pick = async (n: number | null) => {
    setError(await save({ compactAt: n }));
  };
  const preset = value === null ? "off" : (COMPACT_PRESETS as readonly number[]).includes(value) ? String(value) : "custom";

  return (
    <ListRow
      row="compaction"
      wide
      below={
        <div className="emp-compaction">
          <div className="emp-compaction__controls">
            <Segmented
              label="Compact chats at"
              value={preset}
              options={[["off", "Tool default"], ...COMPACT_PRESETS.map((n) => [String(n), formatTokens(n)] as const), ["custom", "Custom"]]}
              disabled={settings.status !== "ready"}
              onChange={(v) => {
                if (v === "off") void pick(null);
                else if (v !== "custom") void pick(Number(v));
                else setCustom(value ? String(Math.round(value / 1000)) : "");
              }}
            />
            {(preset === "custom" || custom) && (
              <form
                className="emp-compaction__custom"
                onSubmit={(e) => {
                  e.preventDefault();
                  const n = parseTokens(custom);
                  if (n === null) setError(`Enter ${formatTokens(COMPACT_MIN)}–${formatTokens(COMPACT_MAX)}, e.g. 250k`);
                  else void pick(n).then(() => setCustom(""));
                }}
              >
                <input
                  className="emp-input emp-mono"
                  data-orion-ui="input"
                  aria-label="Custom limit"
                  placeholder={value ? formatTokens(value) : "250k"}
                  value={custom}
                  onChange={(e) => setCustom(e.target.value)}
                />
                <button type="submit" className="emp-button emp-button--small" data-orion-ui="button" disabled={!custom.trim()}>
                  Set
                </button>
              </form>
            )}
          </div>
          <div className="emp-list-row__desc">
            {value === null
              ? "Off: each tool compacts on its own, often near the model's full window."
              : `On at ${formatTokens(value)} for every agent, unless an agent sets its own (Agents → Edit).`}{" "}
            Claude Code and Codex apply it themselves, also mid-turn (Claude from the chat's next start). For pi and Hermes, EMP checks after each reply
            and compacts before the next.
          </div>
          {(error ?? settings.error) && <div className="emp-phone-error">{error ?? settings.error}</div>}
        </div>
      }
    />
  );
}

function NotificationsRow() {
  const host = useHost();
  const on = useWorkspace((s) => s.settings.notifications);
  const update = useWorkspace((s) => s.updateSettings);
  const [permission, setPermission] = useState(() => host.notificationPermission?.() ?? "granted");

  return (
    <ListRow
      row="notifications"
      action={
        <>
          {on && permission === "default" && host.requestNotifications && (
            <button
              type="button"
              className="emp-button emp-button--small"
              data-orion-ui="button"
              onClick={async () => {
                await host.requestNotifications?.();
                setPermission(host.notificationPermission?.() ?? "granted");
              }}
            >
              Allow notifications
            </button>
          )}
          {on && permission === "denied" && <span className="emp-list-row__value">Blocked by the browser or system settings</span>}
          <Switch label="Notifications" checked={on} onChange={(notifications) => update({ notifications })} />
        </>
      }
    />
  );
}

function KeyboardPage() {
  return (
    <>
      <SectionHeading icon={IconKeyboard} title="Shortcuts" />
      {SHORTCUTS.map(([what, combo]) => (
        <div key={what} className="emp-list-row">
          <div className="emp-list-row__text">
            <div className="emp-list-row__title">{what}</div>
          </div>
          <div className="emp-list-row__action emp-keys">
            {combo.map((k, i) => (
              <Kbd key={i}>{k}</Kbd>
            ))}
          </div>
        </div>
      ))}
    </>
  );
}

function AboutPage() {
  const connection = useWorkspace((s) => s.connection);
  const host = useHost();
  const hostLabel = { "emp-desktop": "EMP desktop", "orion-plugin": "Orion plugin", browser: "Browser" }[host.kind];
  return (
    <>
      <ListRow
        row="daemon"
        action={
          <span className="emp-mono emp-list-row__value">
            {connection.state === "ready" ? `empd v${connection.daemonVersion ?? "?"}` : connection.state}
          </span>
        }
      />
      <ListRow row="host" action={<span className="emp-list-row__value">{hostLabel}</span>} />
      <ListRow row="storage" action={<span className="emp-mono emp-list-row__value">ui.settings</span>} />
    </>
  );
}
