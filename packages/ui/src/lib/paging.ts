import type { Page } from "@emp/protocol";

export const THREAD_PAGE_SIZE = 100;
export const TRANSCRIPT_PAGE_SIZE = 200;

export interface Range {
  offset: number;
  limit: number;
}

/**
 * A contiguous slice [start, start + items.length) of a server-side list of `total` items.
 * Transcripts open at the tail and grow in both directions.
 */
export interface Window<T> {
  items: T[];
  start: number;
  total: number;
}

export function windowEnd(w: Window<unknown>): number {
  return w.start + w.items.length;
}

/** The last page, given the total from a first probe. */
export function tailRange(total: number, pageSize = TRANSCRIPT_PAGE_SIZE): Range {
  const offset = Math.max(0, total - pageSize);
  return { offset, limit: pageSize };
}

/** The page just before the window, or null when the window starts at 0. */
export function earlierRange(w: Window<unknown>, pageSize = TRANSCRIPT_PAGE_SIZE): Range | null {
  if (w.start <= 0) return null;
  const offset = Math.max(0, w.start - pageSize);
  return { offset, limit: w.start - offset };
}

/** The page just after the window, or null when everything up to `total` is loaded. */
export function laterRange(w: Window<unknown>, pageSize = TRANSCRIPT_PAGE_SIZE): Range | null {
  const end = windowEnd(w);
  if (end >= w.total) return null;
  return { offset: end, limit: pageSize };
}

/**
 * Merge a fetched page into the window. Pages that are not adjacent to the window
 * (stale responses) are ignored, so out-of-order replies can never corrupt the list.
 */
export function mergePage<T>(w: Window<T>, page: Page<T>): Window<T> {
  const end = windowEnd(w);
  const pageEnd = page.offset + page.items.length;
  if (page.offset === end) {
    return { items: w.items.concat(page.items), start: w.start, total: page.total };
  }
  if (pageEnd === w.start) {
    return { items: page.items.concat(w.items), start: page.offset, total: page.total };
  }
  return { ...w, total: page.total };
}

/** Append a thread-list page, dropping ids already present (lists can shift between pages). */
export function appendUnique<T extends { id: string }>(prev: T[], next: T[]): T[] {
  const seen = new Set(prev.map((t) => t.id));
  return prev.concat(next.filter((t) => !seen.has(t.id)));
}
