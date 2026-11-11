// Display helpers for titles, paths and list grouping. Pure functions, unit-tested.

/** Readable one-line title: markdown links/images and inline code markers become plain text. */
export function cleanTitle(raw: string | null | undefined): string {
  if (!raw) return "Untitled thread";
  const text = raw
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[`*_]{1,3}([^`*_]+)[`*_]{1,3}/g, "$1")
    .replace(/<[^>]{1,40}>/g, "")
    .replace(/\s+/g, " ")
    .trim();
  return text || "Untitled thread";
}

const HOME = /^\/Users\/[^/]+(?=\/|$)/;

/** `/Users/me/code/app` → `~/code/app`; long paths keep their head and tail. */
export function shortPath(path: string | null | undefined, max = 48): string {
  if (!path) return "";
  const p = path.replace(HOME, "~").replace(/^\/private(?=\/(?:tmp|var)\b)/, "");
  if (p.length <= max) return p;
  const parts = p.split("/");
  const head = parts[0] === "~" ? "~" : parts.slice(0, 2).join("/");
  let tail = parts[parts.length - 1]!;
  for (let i = parts.length - 2; i > 0; i -= 1) {
    const next = `${parts[i]}/${tail}`;
    if (head.length + next.length + 3 > max) break;
    tail = next;
  }
  return `${head}/…/${tail}`;
}

/** Two-letter monogram: "Research Captain" → "RC", "pi (default)" → "PI". */
export function initials(name: string): string {
  const words = name.replace(/\(.*?\)/g, " ").match(/[A-Za-z0-9]+/g) ?? [];
  if (words.length === 0) return "?";
  if (words.length === 1) return words[0]!.slice(0, 2).toUpperCase();
  return (words[0]![0]! + words[1]![0]!).toUpperCase();
}

export type DateGroup = "Today" | "Yesterday" | "This week" | "This month" | "Earlier";

/** Bucket for list section headers, by local calendar day. */
export function dateGroup(ts: number, now: number): DateGroup {
  const day = (t: number) => {
    const d = new Date(t);
    return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  };
  const days = Math.round((day(now) - day(ts)) / 86_400_000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "This week";
  if (days < 31) return "This month";
  return "Earlier";
}

/** 2048 → "2 KB"; folders and unknown sizes pass null. */
export function formatBytes(n: number | null | undefined): string {
  if (n == null) return "";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1).replace(/\.0$/, "") : Math.round(v)} ${units[i]}`;
}

/** Markdown reduced to its words, for one-line previews ("**Planning** the `fix`" → "Planning the fix"). */
// eslint-disable-next-line no-control-regex
const ANSI = /\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>78]/g;

/** The last sign-in link a program printed (OAuth / device pages), escape codes removed. */
export function lastLink(output: string): string | null {
  const plain = output.replace(ANSI, "");
  const links = (plain.match(/https?:\/\/[^\s"'<>\x00-\x1f]+/g) ?? [])
    .map((l) => l.replace(/[).,;:]+$/, ""))
    .filter((l) => /oauth|authori[sz]e|\/login|signin|sign-in|\/device|\/auth\b|[?&](code|state|client_id)=/i.test(l));
  return links[links.length - 1] ?? null;
}

/** Drops the `<emp-context>` block EMP appends to messages for Suki. */
/** The line an agent ends a scheduled run's reply with when the schedule's stop condition holds (empd `DONE_MARK`). */
export const SCHEDULE_DONE_MARK = "[[schedule complete]]";

/** A reply without the stop-condition line, and whether it had one. */
export function scheduleDone(text: string): { text: string; done: boolean } {
  if (!text.includes(SCHEDULE_DONE_MARK)) return { text, done: false };
  const kept = text.split("\n").filter((l) => l.trim() !== SCHEDULE_DONE_MARK);
  return { text: kept.join("\n").replaceAll(SCHEDULE_DONE_MARK, "").trimEnd(), done: true };
}

/** The schedule's name when a scheduled run sent this message, not the owner. */
export function scheduledBy(text: string): string | null {
  return /<emp-context>\s*Scheduled run of "([^"]*)"/.exec(text)?.[1] ?? null;
}

export function stripContext(text: string): string {
  const i = text.indexOf("<emp-context>");
  return i < 0 ? text : text.slice(0, i).trimEnd();
}

export function stripMarkdown(text: string): string {
  return text
    .replace(/```[^\n]*\n?/g, "")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/(\*\*|__)(?=\S)([\s\S]*?\S)\1/g, "$2")
    .replace(/(^|[\s(])[*_](?=\S)([^*_\n]*?\S)[*_](?=[\s).,:;!?]|$)/g, "$1$2")
    .replace(/`([^`\n]*)`/g, "$1")
    .replace(/^[ \t]{0,3}(?:#{1,6}|>|[-*+]|\d+\.)[ \t]+/gm, "");
}
