#!/usr/bin/env node
// Enforces the Orion-sandbox rules on packages/ui/src (AGENTS.md "Plugin-readiness rules",
// FOUNDATION §4a). Fails with file:line for:
//   - browser I/O and globals the sandbox forbids (fetch, WebSocket, Worker, storage, …)
//   - host APIs that must go through EmpHost (clipboard, window.open, Tauri, Node)
//   - viewport units, document.title, window/document-level listeners, URL routing
//   - CSS selectors not scoped under .emp-root, and literal colors/sizes outside tokens.css

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const uiSrc = join(root, "packages/ui/src");

const CODE_RULES = [
  [/\bfetch\s*\(/, "fetch() — use a daemon call"],
  [/\bWebSocket\b/, "WebSocket — transports live outside @emp/ui"],
  [/@emp\/client\/ws\b/, "@emp/client/ws — transports live outside @emp/ui"],
  [/\bXMLHttpRequest\b/, "XMLHttpRequest"],
  [/\b(?:SharedWorker|ServiceWorker|Worker)\b|\bserviceWorker\b/, "Workers (worker-src 'none')"],
  [/\bEventSource\b/, "EventSource"],
  [/\bsendBeacon\b/, "navigator.sendBeacon"],
  [/(?<![\w$.])import\s*\(/, "dynamic import()"],
  [/\b(?:localStorage|sessionStorage|indexedDB)\b/, "browser storage — use host.prefs"],
  [/\bwindow\.open\b/, "window.open — use host.openExternal"],
  [/\bdocument\.cookie\b/, "document.cookie"],
  [/\bnavigator\.clipboard\b/, "navigator.clipboard — use host.clipboard"],
  [/\bdocument\.title\b/, "document.title"],
  [/\b100v[wh]\b/, "100vw/100vh — the workspace fills its container"],
  [/\b(?:window|document|globalThis|self)\s*\.\s*(?:addEventListener|onkeydown|onkeyup|onkeypress)\b/, "window/document-level listener — scope to the root element"],
  [/\b(?:history\.(?:pushState|replaceState)|location\.(?:href|assign|replace|hash|search))\b/, "URL routing — navigation state is in-memory"],
  [/@tauri-apps\/|__TAURI__/, "Tauri API — go through EmpHost"],
  [/from\s+["']node:|require\s*\(/, "Node API"],
  [/import\s*\{[^}]*\bcreate\b[^}]*\}\s*from\s*["']zustand["']/, "module-level zustand store — use createStore per instance"],
];

const LITERAL_RULES = [
  [/#[0-9a-fA-F]{3,8}\b/, "literal color — use a --emp-color-* token"],
  [/\b(?:rgba?|hsla?|oklch|oklab|lab|lch|color)\(/, "literal color — use a --emp-color-* token"],
  [/(?<![\w-])(?!1px\b)\d*\.?\d+(?:px|rem|em|pt|vh|vw|vmin|vmax|ch|ms|s)\b/, "literal size/duration — use a token (only 0 and 1px are allowed)"],
  [/@import\b/, "@import"],
  [/url\(\s*["']?(?!data:)/, "url() to a non-data resource"],
];

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...walk(p));
    else out.push(p);
  }
  return out;
}

/** Blank out comments but keep line structure, so reported line numbers stay right. */
function stripComments(text, lineComments) {
  let s = text.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
  if (lineComments) s = s.replace(/(^|[^:"'\\])\/\/[^\n]*/g, (_m, pre) => pre);
  return s;
}

const lineOf = (text, index) => text.slice(0, index).split("\n").length;

const problems = [];
const report = (file, line, msg) => problems.push(`${relative(root, file)}:${line}: ${msg}`);

function checkCode(file) {
  const lines = stripComments(readFileSync(file, "utf8"), true).split("\n");
  lines.forEach((line, i) => {
    for (const [re, msg] of CODE_RULES) if (re.test(line)) report(file, i + 1, msg);
  });
}

const SCOPED = /^\.emp-root(?=$|[\s.:#[>+~,])/;

function checkCss(file, isGenerated) {
  const text = stripComments(readFileSync(file, "utf8"), false);
  // The generated fonts.css may declare @font-face, with data: sources only.
  const isFonts = file.endsWith("fonts.css");
  if (isFonts && /url\(\s*["']?(?!data:)/.test(text)) report(file, 1, "font source must be a data: URL");

  // Selectors: every rule outside @keyframes must be scoped under .emp-root.
  let depth = 0;
  let skipUntilDepth = -1;
  let start = 0;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (ch === "{") {
      const prelude = text.slice(start, i).trim();
      if (skipUntilDepth < 0) {
        if (prelude.startsWith("@")) {
          if (/^@(?:-\w+-)?keyframes\b/.test(prelude)) skipUntilDepth = depth;
          else if (isFonts && prelude === "@font-face") skipUntilDepth = depth;
          else if (!/^@(?:media|supports|container|layer)\b/.test(prelude)) {
            report(file, lineOf(text, start + text.slice(start).search(/\S/)), `at-rule not allowed: ${prelude.split(/\s/)[0]}`);
          }
        } else {
          const selectors = prelude.split(/,(?![^(]*\))/).map((s) => s.trim());
          for (const sel of selectors) {
            if (!SCOPED.test(sel)) {
              report(file, lineOf(text, text.indexOf(sel, start)), `selector not scoped under .emp-root: ${sel}`);
            }
          }
        }
      }
      depth += 1;
      start = i + 1;
    } else if (ch === "}") {
      depth -= 1;
      if (skipUntilDepth === depth) skipUntilDepth = -1;
      start = i + 1;
    } else if (ch === ";" ) {
      start = i + 1;
    }
  }

  const lines = text.split("\n");
  lines.forEach((line, i) => {
    if (/\b100v[wh]\b/.test(line)) report(file, i + 1, "100vw/100vh — the workspace fills its container");
    if (isGenerated) return;
    // Values only: skip the selector part of a line.
    const value = line.includes(":") && !line.trimEnd().endsWith("{") ? line.slice(line.indexOf(":") + 1) : line.includes("@") ? line : "";
    for (const [re, msg] of LITERAL_RULES) if (re.test(value)) report(file, i + 1, `${msg}: ${line.trim()}`);
  });
}

const files = walk(uiSrc);
for (const f of files) {
  if (/\.(?:ts|tsx|js|jsx|mjs)$/.test(f)) checkCode(f);
  else if (f.endsWith(".css")) checkCss(f, f.endsWith("tokens.css") || f.endsWith("fonts.css"));
}

if (problems.length) {
  console.error(`check-sandbox: ${problems.length} problem(s) in packages/ui/src\n`);
  for (const p of problems) console.error(`  ${p}`);
  process.exit(1);
}
console.log(`check-sandbox: ok (${files.length} files in packages/ui/src)`);
