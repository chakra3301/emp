import { describe, expect, it } from "vitest";
import { DEFAULT_SETTINGS, LAYOUT_TEMPLATES, NAV_IDS, moveNav, normalizeSettings, sameArrangement, settingsAttrs, settingsStyle, visible } from "./settings";

describe("normalizeSettings", () => {
  it("falls back to defaults for junk", () => {
    expect(normalizeSettings(undefined)).toEqual(DEFAULT_SETTINGS);
    expect(normalizeSettings({ accent: "url(x)", sidebarScale: 99, sidebarSide: "top", threadWidth: 3 })).toMatchObject({
      accent: "default",
      sidebarScale: 1.8,
      sidebarSide: "left",
      threadWidth: "full",
    });
  });

  it("keeps saved nav order, drops unknown ids, slots in new ones and never hides Settings", () => {
    const s = normalizeSettings({
      nav: [
        { id: "sources", visible: false },
        { id: "bogus", visible: true },
        { id: "settings", visible: false },
        { id: "sources", visible: true },
      ],
    });
    expect(s.nav.map((e) => e.id)).toEqual(["suki", "new", "terminals", "servers", "schedules", "projects", "agents", "sources", "settings"]);
    expect(s.nav.find((e) => e.id === "sources")).toEqual({ id: "sources", visible: false });
    expect(s.nav.find((e) => e.id === "settings")).toEqual({ id: "settings", visible: true });
  });

  it("puts a nav item added since the save next to its neighbour", () => {
    const saved = ["settings", "terminals", "new", "projects", "agents", "sources"].map((id) => ({ id, visible: true }));
    expect(normalizeSettings({ nav: saved }).nav.map((e) => e.id)).toEqual(["suki", "settings", "terminals", "servers", "schedules", "new", "projects", "agents", "sources"]);
    expect(NAV_IDS).toContain("servers");
  });

  it("accepts #rrggbb colours, maps old preset names, and rejects junk", () => {
    expect(normalizeSettings({ accent: "#12ab9F" }).accent).toBe("#12ab9f");
    expect(normalizeSettings({ accent: "violet" }).accent).toBe("#8250df");
    expect(normalizeSettings({ accent: "#12ab9" }).accent).toBe("default");
    expect(normalizeSettings({ background: "#0D1117" }).background).toBe("#0d1117");
    expect(normalizeSettings({ background: "black" }).background).toBeNull();
  });
});

describe("settings → root styles", () => {
  it("adds nothing for the defaults", () => {
    expect(settingsStyle(DEFAULT_SETTINGS)).toEqual({});
    expect(settingsAttrs(DEFAULT_SETTINGS)["data-accent"]).toBeUndefined();
  });

  it("passes wheel colours through and flags dark backgrounds", () => {
    expect(settingsStyle({ ...DEFAULT_SETTINGS, background: "#0d1117" })).toEqual({ "--emp-color-background": "#0d1117" });
    expect(settingsAttrs({ ...DEFAULT_SETTINGS, background: "#0d1117" })).toMatchObject({ "data-bg": "custom", "data-tone": "dark" });
    expect(settingsAttrs({ ...DEFAULT_SETTINGS, background: "#fdf6e3" })["data-tone"]).toBeUndefined();
    expect(settingsStyle({ ...DEFAULT_SETTINGS, accent: "#112233", sidebarScale: 1.2 })).toEqual({
      "--emp-color-accent": "#112233",
      "--emp-sidebar-scale": "1.2",
    });
    expect(settingsAttrs({ ...DEFAULT_SETTINGS, statusBar: false, sidebarSide: "right" })).toMatchObject({
      "data-statusbar": "off",
      "data-sidebar": "right",
    });
  });
});

describe("moveNav", () => {
  it("moves within bounds only", () => {
    const nav = DEFAULT_SETTINGS.nav;
    expect(moveNav(nav, "terminals", -1).map((e) => e.id).slice(0, 3)).toEqual(["suki", "terminals", "new"]);
    expect(moveNav(nav, "suki", -1)).toBe(nav);
  });
});

describe("interface mode and arrangements", () => {
  it("Simple hides search, Live and the status bar whatever the toggles say", () => {
    const simple = { ...DEFAULT_SETTINGS, interfaceMode: "simple" as const };
    expect(visible(simple, "search") || visible(simple, "live") || visible(simple, "statusBar")).toBe(false);
    expect(settingsAttrs(simple)["data-statusbar"]).toBe("off");
    expect(visible({ ...DEFAULT_SETTINGS, showLive: false }, "live")).toBe(false);
    expect(visible(DEFAULT_SETTINGS, "statusBar")).toBe(true);
  });

  it("keeps valid saved layouts and drops junk", () => {
    const s = normalizeSettings({
      savedLayouts: [
        { id: "a", name: " Mine ", arrangement: { sidebarSide: "right", sidebarScale: 9, threadWidth: "readable" } },
        { id: "b", name: "" },
        "junk",
      ],
    });
    expect(s.savedLayouts).toEqual([{ id: "a", name: "Mine", arrangement: { sidebarSide: "right", sidebarScale: 1.8, threadWidth: "readable" } }]);
  });

  it("the defaults match the Default template", () => {
    expect(sameArrangement(DEFAULT_SETTINGS, LAYOUT_TEMPLATES[0]!.arrangement)).toBe(true);
    expect(sameArrangement(DEFAULT_SETTINGS, LAYOUT_TEMPLATES[1]!.arrangement)).toBe(false);
  });
});
