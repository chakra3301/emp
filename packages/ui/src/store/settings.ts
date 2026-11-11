// UI settings: theme colour and layout. Stored as one JSON value in host.prefs, so they follow
// the host (desktop app or Orion plugin) and never touch the daemon.

import { isDark, isHex } from "../lib/color";

/** The default accent: the token value (`color.accent`), i.e. no override. ("nous" is an older name for it.) */
export const DEFAULT_ACCENT = "default";

/** Presets from an earlier build, kept so a saved choice still loads (as its hex). */
const LEGACY_ACCENTS: Record<string, string> = {
  violet: "#8250df",
  green: "#1a7f37",
  teal: "#0f7b83",
  orange: "#bc4c00",
  red: "#cf222e",
  pink: "#bf3989",
  graphite: "#1f2328",
};

export const NAV_IDS = ["suki", "new", "terminals", "servers", "schedules", "projects", "agents", "sources", "settings"] as const;
export type NavId = (typeof NAV_IDS)[number];

export const NAV_LABELS: Record<NavId, string> = {
  suki: "Suki",
  new: "New thread",
  terminals: "Terminals",
  servers: "Dev servers",
  schedules: "Scheduled",
  projects: "Projects",
  agents: "Agents",
  sources: "Sources",
  settings: "Settings",
};

/** Settings can't be hidden: it's the way back to unhiding things. */
export const PINNED_NAV: readonly NavId[] = ["settings"];

export interface NavEntry {
  id: NavId;
  visible: boolean;
}

/** The arrangement part of the layout: what a template or a saved layout captures. */
export interface Arrangement {
  sidebarSide: "left" | "right";
  sidebarScale: number;
  threadWidth: "full" | "readable";
}

export interface SavedLayout {
  id: string;
  name: string;
  arrangement: Arrangement;
}

/** Hermes's two interface modes: Simple hides the extra chrome, Advanced shows it as set. */
export type InterfaceMode = "simple" | "advanced";

export const INTERFACE_MODES: { id: InterfaceMode; label: string; description: string }[] = [
  { id: "simple", label: "Simple", description: "For talking to your agents. Sidebar and chat; no Live section, search box or status bar." },
  { id: "advanced", label: "Advanced", description: "Everything: Live, search and the status bar, as you set them." },
];

export const LAYOUT_TEMPLATES: { id: string; name: string; arrangement: Arrangement }[] = [
  { id: "default", name: "Default", arrangement: { sidebarSide: "left", sidebarScale: 1, threadWidth: "full" } },
  { id: "mirrored", name: "Mirrored", arrangement: { sidebarSide: "right", sidebarScale: 1, threadWidth: "full" } },
  { id: "focus", name: "Focus", arrangement: { sidebarSide: "left", sidebarScale: 0.8, threadWidth: "readable" } },
  { id: "wide", name: "Wide list", arrangement: { sidebarSide: "left", sidebarScale: 1.4, threadWidth: "full" } },
];

export function sameArrangement(s: Arrangement, a: Arrangement): boolean {
  return s.sidebarSide === a.sidebarSide && Math.abs(s.sidebarScale - a.sidebarScale) < 0.001 && s.threadWidth === a.threadWidth;
}

export interface UiSettings {
  /** Highlight colour: `"default"` (the token value) or a `#rrggbb` from the colour wheel. */
  accent: string;
  /** Background `#rrggbb` from the colour wheel; null = the token default. Dark ones flip the ink to light. */
  background: string | null;
  interfaceMode: InterfaceMode;
  /** Arrangements saved from the layout editor ("Save current arrangement as a template"). */
  savedLayouts: SavedLayout[];
  sidebarSide: "left" | "right";
  /** Multiplies the `layout.sidebar` token. */
  sidebarScale: number;
  nav: NavEntry[];
  showSearch: boolean;
  showLive: boolean;
  /** Thread groups opened in the sidebar (groups start collapsed). */
  expandedGroups: string[];
  statusBar: boolean;
  /** "readable" centres the transcript and composer at `layout.content`. */
  threadWidth: "full" | "readable";
  /** Picked first in the New thread composer; null = the first agent. */
  defaultAgentId: string | null;
  /** System notifications when a reply finishes, fails or needs input while you're elsewhere. */
  notifications: boolean;
}

export const SIDEBAR_SCALE_MIN = 0.8;
export const SIDEBAR_SCALE_MAX = 1.8;

export const DEFAULT_LAYOUT: Omit<UiSettings, "accent" | "background" | "defaultAgentId" | "savedLayouts" | "notifications" | "expandedGroups"> = {
  interfaceMode: "advanced",
  sidebarSide: "left",
  sidebarScale: 1,
  nav: NAV_IDS.map((id) => ({ id, visible: true })),
  showSearch: true,
  showLive: true,
  statusBar: true,
  threadWidth: "full",
};

export const DEFAULT_SETTINGS: UiSettings = {
  accent: DEFAULT_ACCENT,
  background: null,
  defaultAgentId: null,
  savedLayouts: [],
  notifications: true,
  expandedGroups: [],
  ...DEFAULT_LAYOUT,
};

/** What's actually on screen: Simple mode hides the extra chrome whatever the toggles say. */
export function visible(s: UiSettings, part: "search" | "live" | "statusBar"): boolean {
  if (s.interfaceMode === "simple") return false;
  return part === "search" ? s.showSearch : part === "live" ? s.showLive : s.statusBar;
}

export function isCustomAccent(accent: string): boolean {
  return isHex(accent);
}

export function clampScale(n: number): number {
  return Math.round(Math.min(SIDEBAR_SCALE_MAX, Math.max(SIDEBAR_SCALE_MIN, n)) * 100) / 100;
}

/** Accepts whatever host.prefs returned (older shapes, junk) and fills in defaults. */
export function normalizeSettings(raw: unknown): UiSettings {
  const r = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const d = DEFAULT_SETTINGS;
  const rawAccent = typeof r.accent === "string" ? (LEGACY_ACCENTS[r.accent] ?? r.accent) : "";
  const accent = isHex(rawAccent) ? rawAccent.toLowerCase() : d.accent;
  return {
    accent,
    background: typeof r.background === "string" && isHex(r.background) ? r.background.toLowerCase() : null,
    interfaceMode: r.interfaceMode === "simple" ? "simple" : "advanced",
    savedLayouts: normalizeSaved(r.savedLayouts),
    sidebarSide: r.sidebarSide === "right" ? "right" : "left",
    sidebarScale: typeof r.sidebarScale === "number" && Number.isFinite(r.sidebarScale) ? clampScale(r.sidebarScale) : d.sidebarScale,
    nav: normalizeNav(r.nav),
    showSearch: typeof r.showSearch === "boolean" ? r.showSearch : d.showSearch,
    showLive: typeof r.showLive === "boolean" ? r.showLive : d.showLive,
    expandedGroups: Array.isArray(r.expandedGroups) ? [...new Set(r.expandedGroups.filter((g): g is string => typeof g === "string"))].slice(0, 200) : [],
    statusBar: typeof r.statusBar === "boolean" ? r.statusBar : d.statusBar,
    threadWidth: r.threadWidth === "readable" ? "readable" : "full",
    defaultAgentId: typeof r.defaultAgentId === "string" && r.defaultAgentId ? r.defaultAgentId : null,
    notifications: typeof r.notifications === "boolean" ? r.notifications : d.notifications,
  };
}

function normalizeSaved(raw: unknown): SavedLayout[] {
  const out: SavedLayout[] = [];
  for (const e of Array.isArray(raw) ? raw : []) {
    if (!e || typeof e !== "object") continue;
    const r = e as Record<string, unknown>;
    const a = (r.arrangement && typeof r.arrangement === "object" ? r.arrangement : {}) as Record<string, unknown>;
    if (typeof r.id !== "string" || typeof r.name !== "string" || !r.name.trim()) continue;
    out.push({
      id: r.id,
      name: r.name.trim().slice(0, 40),
      arrangement: {
        sidebarSide: a.sidebarSide === "right" ? "right" : "left",
        sidebarScale: typeof a.sidebarScale === "number" && Number.isFinite(a.sidebarScale) ? clampScale(a.sidebarScale) : 1,
        threadWidth: a.threadWidth === "readable" ? "readable" : "full",
      },
    });
  }
  return out.slice(0, 12);
}

/** Keeps the saved order, drops unknown ids and duplicates; items added since go after their
 *  neighbour in `NAV_IDS` (Dev servers lands under Terminals, not at the bottom). */
function normalizeNav(raw: unknown): NavEntry[] {
  const seen = new Set<NavId>();
  const out: NavEntry[] = [];
  for (const e of Array.isArray(raw) ? raw : []) {
    const id = e && typeof e === "object" ? (e as Record<string, unknown>).id : undefined;
    if (typeof id !== "string" || !(NAV_IDS as readonly string[]).includes(id) || seen.has(id as NavId)) continue;
    seen.add(id as NavId);
    const visible = (e as Record<string, unknown>).visible !== false || PINNED_NAV.includes(id as NavId);
    out.push({ id: id as NavId, visible });
  }
  NAV_IDS.forEach((id, i) => {
    if (seen.has(id)) return;
    const prev = i > 0 ? out.findIndex((e) => e.id === NAV_IDS[i - 1]) : -1;
    out.splice(prev + 1, 0, { id, visible: true });
    seen.add(id);
  });
  return out;
}

/** Moves nav item `id` by `delta` places. */
export function moveNav(nav: NavEntry[], id: NavId, delta: number): NavEntry[] {
  const from = nav.findIndex((e) => e.id === id);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= nav.length) return nav;
  const next = nav.slice();
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item!);
  return next;
}

/** CSS variables for the workspace root. The default accent adds nothing (exact Hermes values). */
export function settingsStyle(s: UiSettings): Record<string, string> {
  const vars: Record<string, string> = {};
  if (isCustomAccent(s.accent)) vars["--emp-color-accent"] = s.accent;
  if (s.background) vars["--emp-color-background"] = s.background;
  if (s.sidebarScale !== 1) vars["--emp-sidebar-scale"] = String(s.sidebarScale);
  return vars;
}

/** Data attributes for the workspace root, read by workspace.css. */
export function settingsAttrs(s: UiSettings): Record<string, string | undefined> {
  return {
    "data-accent": isCustomAccent(s.accent) ? "custom" : undefined,
    "data-bg": s.background ? "custom" : undefined,
    "data-tone": s.background && isDark(s.background) ? "dark" : undefined,
    "data-sidebar": s.sidebarSide,
    "data-statusbar": visible(s, "statusBar") ? undefined : "off",
    "data-thread-width": s.threadWidth,
  };
}
