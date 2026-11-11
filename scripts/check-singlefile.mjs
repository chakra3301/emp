#!/usr/bin/env node
// Verifies packages/ui/dist-singlefile/ui.html against the Orion plugin sandbox
// (FOUNDATION §4a): one self-contained file ≤ 4 MiB, classic inline script only,
// no external resources, no dynamic import, workers or sockets.

import { readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";

const file = fileURLToPath(new URL("../packages/ui/dist-singlefile/ui.html", import.meta.url));
const BUDGET = 4 * 1024 * 1024;

let html;
let size;
try {
  html = readFileSync(file, "utf8");
  size = statSync(file).size;
} catch {
  console.error("check-singlefile: ui.html not found. Run: npm run build:singlefile -w @emp/ui");
  process.exit(1);
}

const problems = [];
if (size > BUDGET) problems.push(`ui.html is ${size} bytes, over the 4 MiB budget (${BUDGET})`);

const checks = [
  [/type\s*=\s*["']?module/i, 'type="module" (Orion rejects module scripts)'],
  [/<script\b[^>]*\bsrc\s*=/i, "<script src=…> (external scripts are stripped)"],
  [/<link\b[^>]*\bhref\s*=\s*["']?(?!data:)/i, "<link href=…> to a non-data URL"],
  [/\bimport\s*\(/, "dynamic import()"],
  [/\bnew\s+(?:Shared)?Worker\b/, "new Worker (worker-src 'none')"],
  [/\bnew\s+WebSocket\b/, "new WebSocket (connect-src 'none')"],
  [/\bnew\s+EventSource\b/, "new EventSource (connect-src 'none')"],
  // CSS url(); case-sensitive so JavaScript's `new URL(…)` doesn't match, and a JS method
  // named `url` (`url(e){…}`, in marked's tokenizer) isn't a resource either.
  [/(?<![\w$.])url\(\s*["']?(?!data:|#|["']?\)|[\w$]+\)\s*\{)/, "url() to a non-data resource (font-src/img-src data: only)"],
];
for (const [re, msg] of checks) {
  const m = re.exec(html);
  if (m) {
    const line = html.slice(0, m.index).split("\n").length;
    problems.push(`${msg} at line ${line}: …${html.slice(Math.max(0, m.index - 40), m.index + 60).replace(/\s+/g, " ")}…`);
  }
}

const body = /<body\b[^>]*>/i.exec(html)?.[0] ?? "";
if (!/data-orion-appearance="v1"/.test(body) || !/data-orion-skin="standard"/.test(body)) {
  problems.push('<body> must carry data-orion-appearance="v1" data-orion-skin="standard"');
}

const scripts = html.match(/<script\b[^>]*>/gi) ?? [];
if (!scripts.some((s) => /^<script>$/i.test(s))) problems.push("no classic inline <script> found");

if (problems.length) {
  console.error("check-singlefile: FAILED");
  for (const p of problems) console.error(`  ${p}`);
  process.exit(1);
}
console.log(`check-singlefile: ok — ui.html ${(size / 1024).toFixed(1)} KiB (${((size / BUDGET) * 100).toFixed(1)}% of 4 MiB budget)`);
