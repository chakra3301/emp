import { describe, expect, it } from "vitest";
import { appendUnique, earlierRange, laterRange, mergePage, tailRange, type Window } from "./paging";
import { formatDuration, formatRelative } from "./time";

describe("formatRelative", () => {
  const now = Date.UTC(2026, 9, 19, 12, 0, 0);
  it.each([
    [now - 10_000, "just now"],
    [now + 60_000, "just now"],
    [now - 5 * 60_000, "5m ago"],
    [now - 3 * 3_600_000, "3h ago"],
    [now - 2 * 86_400_000, "2d ago"],
    [now - 15 * 86_400_000, "2w ago"],
  ])("%s → %s", (ts, expected) => {
    expect(formatRelative(ts, now)).toBe(expected);
  });

  it("falls back to a date, with the year only when it differs", () => {
    const sameYear = formatRelative(Date.UTC(2026, 2, 4, 12), now);
    const otherYear = formatRelative(Date.UTC(2024, 2, 4, 12), now);
    expect(sameYear).not.toMatch(/2026/);
    expect(otherYear).toMatch(/2024/);
  });

  it("formats durations", () => {
    expect(formatDuration(420)).toBe("420 ms");
    expect(formatDuration(1500)).toBe("1.5 s");
    expect(formatDuration(125_000)).toBe("2m 5s");
  });
});

describe("transcript paging", () => {
  const items = (from: number, to: number) => Array.from({ length: to - from }, (_, i) => from + i);
  const page = (offset: number, limit: number, total: number) => ({ items: items(offset, Math.min(total, offset + limit)), offset, total });

  it("opens at the tail", () => {
    expect(tailRange(3200, 200)).toEqual({ offset: 3000, limit: 200 });
    expect(tailRange(50, 200)).toEqual({ offset: 0, limit: 200 });
  });

  it("walks earlier pages down to 0 without overlap", () => {
    let w: Window<number> = { items: items(3000, 3200), start: 3000, total: 3200 };
    let requests = 0;
    for (let r = earlierRange(w, 200); r; r = earlierRange(w, 200)) {
      w = mergePage(w, page(r.offset, r.limit, 3200));
      requests += 1;
    }
    expect(requests).toBe(15);
    expect(w.start).toBe(0);
    expect(w.items).toEqual(items(0, 3200));
    expect(laterRange(w, 200)).toBeNull();
  });

  it("clamps the first page when start is not a page multiple", () => {
    expect(earlierRange({ items: items(150, 350), start: 150, total: 350 }, 200)).toEqual({ offset: 0, limit: 150 });
  });

  it("loads later items when the total grew", () => {
    const w: Window<number> = { items: items(0, 10), start: 0, total: 25 };
    const r = laterRange(w, 200)!;
    expect(r).toEqual({ offset: 10, limit: 200 });
    expect(mergePage(w, page(r.offset, r.limit, 25)).items).toEqual(items(0, 25));
  });

  it("ignores non-adjacent (stale) pages but keeps the newer total", () => {
    const w: Window<number> = { items: items(400, 600), start: 400, total: 600 };
    const merged = mergePage(w, page(0, 200, 700));
    expect(merged.items).toBe(w.items);
    expect(merged.total).toBe(700);
  });

  it("dedupes thread pages by id", () => {
    expect(appendUnique([{ id: "a" }, { id: "b" }], [{ id: "b" }, { id: "c" }]).map((t) => t.id)).toEqual(["a", "b", "c"]);
  });
});
