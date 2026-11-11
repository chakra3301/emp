import type { ITheme } from "@xterm/xterm";

/*
 * Terminal colours. The ANSI tables are VS Code's default integrated-terminal palettes (as in
 * Hermes desktop): a fixed table per light/dark, so yellow stays legible on white. They colour
 * program output, not EMP's chrome, so they aren't design tokens. Surface, ink, cursor and
 * selection come from the workspace tokens.
 */
const ANSI_DARK: ITheme = {
  black: "#000000",
  red: "#cd3131",
  green: "#0dbc79",
  yellow: "#e5e510",
  blue: "#2472c8",
  magenta: "#bc3fbc",
  cyan: "#11a8cd",
  white: "#e5e5e5",
  brightBlack: "#666666",
  brightRed: "#f14c4c",
  brightGreen: "#23d18b",
  brightYellow: "#f5f543",
  brightBlue: "#3b8eea",
  brightMagenta: "#d670d6",
  brightCyan: "#29b8db",
  brightWhite: "#e5e5e5",
};

const ANSI_LIGHT: ITheme = {
  black: "#000000",
  red: "#cd3131",
  green: "#00bc00",
  yellow: "#949800",
  blue: "#0451a5",
  magenta: "#bc05bc",
  cyan: "#0598bc",
  white: "#555555",
  brightBlack: "#666666",
  brightRed: "#cd3131",
  brightGreen: "#14ce14",
  brightYellow: "#b5ba00",
  brightBlue: "#0451a5",
  brightMagenta: "#bc05bc",
  brightCyan: "#0598bc",
  brightWhite: "#a5a5a5",
};

/**
 * The xterm theme for the terminal mounted at `el`. Token values are often `color-mix()`es,
 * which custom properties hand back unresolved, so each one is painted on a probe and read
 * back as a concrete colour, flattened over the surface (xterm wants opaque colours).
 */
export function terminalTheme(el: HTMLElement): ITheme {
  const probe = document.createElement("span");
  probe.style.position = "absolute";
  probe.style.visibility = "hidden";
  el.appendChild(probe);
  const read = (expr: string) => {
    probe.style.color = expr;
    return getComputedStyle(probe).color;
  };
  const surface = flatten(read("var(--emp-color-background)"), null) ?? "#ffffff";
  const fg = flatten(read("var(--emp-color-text)"), surface);
  const accent = flatten(read("var(--emp-color-accent)"), surface);
  const selection = flatten(read("var(--emp-color-accent-soft)"), surface);
  probe.remove();
  const dark = luma(surface) < 0.5;
  return {
    ...(dark ? ANSI_DARK : ANSI_LIGHT),
    background: surface,
    foreground: fg ?? (dark ? "#cccccc" : "#333333"),
    cursor: accent ?? fg ?? "#333333",
    cursorAccent: surface,
    selectionBackground: selection ?? (dark ? "#264f78" : "#add6ff"),
  };
}

let canvas: HTMLCanvasElement | null = null;

/** `css` painted over `under` (or alone), as `#rrggbb`; null if the browser can't paint it. */
function flatten(css: string, under: string | null): string | null {
  if (!css) return null;
  canvas ??= document.createElement("canvas");
  canvas.width = canvas.height = 1;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) return null;
  ctx.clearRect(0, 0, 1, 1);
  if (under) {
    ctx.fillStyle = under;
    ctx.fillRect(0, 0, 1, 1);
  }
  ctx.fillStyle = css;
  ctx.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
  if (a === 0) return null;
  return `#${[r, g, b].map((n) => (n ?? 0).toString(16).padStart(2, "0")).join("")}`;
}

function luma(hex: string): number {
  const n = Number.parseInt(hex.slice(1), 16);
  return (0.2126 * ((n >> 16) & 255) + 0.7152 * ((n >> 8) & 255) + 0.0722 * (n & 255)) / 255;
}

/** Waits (briefly) for the terminal's font, so xterm measures cells with the face it draws. */
export async function loadTerminalFont(family: string, size: number): Promise<void> {
  const fonts = typeof document !== "undefined" ? document.fonts : undefined;
  if (!fonts?.load) return;
  const loads = ["400", "700"].map((w) => fonts.load(`${w} ${size}px ${family}`).catch(() => []));
  await Promise.race([Promise.all(loads), new Promise((r) => setTimeout(r, 1500))]);
}
