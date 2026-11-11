import { describe, expect, it } from "vitest";
import { formatTokens, parseTokens } from "./compaction";
import { cleanTitle, formatBytes, dateGroup, initials, lastLink, scheduleDone, scheduledBy, shortPath, stripContext, stripMarkdown } from "./format";
import { formatShort } from "./time";

describe("format helpers", () => {
  it("cleans markdown out of titles", () => {
    expect(cleanTitle("[@Notion](plugin://notion@openai-curated-remote) summarize")).toBe("@Notion summarize");
    expect(cleanTitle("Fix `parse()` in **store**")).toBe("Fix parse() in store");
    expect(cleanTitle("  \n ")).toBe("Untitled thread");
    expect(cleanTitle(null)).toBe("Untitled thread");
  });

  it("shortens paths around the home dir and keeps the tail", () => {
    expect(shortPath("/Users/me/Desktop/EMP")).toBe("~/Desktop/EMP");
    expect(shortPath("/private/tmp/emp-smoke")).toBe("/tmp/emp-smoke");
    const long = "/Users/me/Library/Application Support/com.example.app/command-center/divisions/dev/wiki";
    const s = shortPath(long, 40);
    expect(s.startsWith("~/…/")).toBe(true);
    expect(s.endsWith("dev/wiki")).toBe(true);
    expect(s.length).toBeLessThanOrEqual(40);
  });

  it("makes monograms", () => {
    expect(initials("Research Captain")).toBe("RC");
    expect(initials("pi (default)")).toBe("PI");
    expect(initials("Desk")).toBe("DE");
  });

  it("groups by calendar day", () => {
    const now = new Date(2026, 9, 19, 9, 0).getTime();
    expect(dateGroup(new Date(2026, 9, 19, 0, 5).getTime(), now)).toBe("Today");
    expect(dateGroup(new Date(2026, 9, 18, 23, 59).getTime(), now)).toBe("Yesterday");
    expect(dateGroup(new Date(2026, 9, 14).getTime(), now)).toBe("This week");
    expect(dateGroup(new Date(2026, 9, 1).getTime(), now)).toBe("This month");
    expect(dateGroup(new Date(2026, 5, 1).getTime(), now)).toBe("Earlier");
  });
});

describe("formatShort", () => {
  it("is compact", () => {
    const now = new Date(2026, 9, 19, 12).getTime();
    expect(formatShort(now - 20_000, now)).toBe("now");
    expect(formatShort(now - 35 * 60_000, now)).toBe("35m");
    expect(formatShort(now - 4 * 3_600_000, now)).toBe("4h");
    expect(formatShort(now - 2 * 86_400_000, now)).toBe("2d");
    expect(formatShort(now - 21 * 86_400_000, now)).toBe("3w");
  });
});

describe("formatBytes", () => {
  it("scales and trims", () => {
    expect(formatBytes(null)).toBe("");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(50 * 1024 * 1024)).toBe("50 MB");
  });
});

describe("compaction limits", () => {
  it("formats and parses token counts", () => {
    expect(formatTokens(150_000)).toBe("150k");
    expect(formatTokens(1_000_000)).toBe("1M");
    expect(parseTokens("150")).toBe(150_000);
    expect(parseTokens("150k")).toBe(150_000);
    expect(parseTokens("250000")).toBe(250_000);
    expect(parseTokens("1m")).toBe(1_000_000);
    expect(parseTokens("20k")).toBeNull();
    expect(parseTokens("2m")).toBeNull();
    expect(parseTokens("lots")).toBeNull();
  });
});

describe("lastLink", () => {
  it("finds the last link a program printed, without escape codes", () => {
    const out = "Opening \x1b[1mbrowser\x1b[0m…\r\nIf it didn't open, visit: \x1b[4mhttps://claude.ai/oauth/authorize?code=true&client_id=abc\x1b[24m\r\n";
    expect(lastLink(out)).toBe("https://claude.ai/oauth/authorize?code=true&client_id=abc");
    expect(lastLink("open https://auth.openai.com/oauth/authorize?x=1. then https://b.dev/login).")).toBe("https://b.dev/login");
    // Other links (pi's billing notice) aren't sign-in pages.
    expect(lastLink("Manage extra usage at https://claude.ai/settings/usage.")).toBeNull();
    expect(lastLink("no links")).toBeNull();
  });
});

describe("stripContext", () => {
  it("drops the block EMP appends for Suki", () => {
    expect(stripContext("hi\n\n<emp-context>\nRunning: nothing\n</emp-context>")).toBe("hi");
    expect(stripContext("plain")).toBe("plain");
    expect(scheduledBy('stretch\n\n<emp-context>\nScheduled run of "Stretch reminder" (Once, Tue Oct 20 at 3:58 PM).\n</emp-context>')).toBe("Stretch reminder");
    expect(scheduledBy("hi\n\n<emp-context>\nRunning: nothing\n</emp-context>")).toBeNull();
    expect(scheduleDone("It's live.\n\n[[schedule complete]]\n")).toEqual({ text: "It's live.", done: true });
    expect(scheduleDone("Not yet.")).toEqual({ text: "Not yet.", done: false });
  });
});

describe("stripMarkdown", () => {
  it("keeps the words and drops the markup", () => {
    expect(stripMarkdown("**Planning** the `fix` in _two_ steps")).toBe("Planning the fix in two steps");
    expect(stripMarkdown("## Heading\n- item [docs](https://x.io)\n> quote")).toBe("Heading\nitem docs\nquote");
    expect(stripMarkdown("a * b * c and snake_case_name")).toBe("a * b * c and snake_case_name");
  });
});
