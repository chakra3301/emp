// Orion-sandbox build: one self-contained dist-singlefile/ui.html with a single classic inline
// script (no type="module", no external src/href, no dynamic import, no workers). The entry
// mounts <EmpWorkspace> on the in-memory MockServer so the file renders fully offline.
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";
import { viteSingleFile } from "vite-plugin-singlefile";

/**
 * Runs after vite-plugin-singlefile inlined everything: turns the inline module script into a
 * classic one and moves it to the end of <body>, where the DOM it mounts into already exists.
 */
function classicInlineScript(): Plugin {
  return {
    name: "emp:classic-inline-script",
    enforce: "post",
    generateBundle: {
      order: "post",
      handler(_options, bundle) {
        for (const file of Object.values(bundle)) {
          if (file.type !== "asset" || !file.fileName.endsWith(".html")) continue;
          let html = String(file.source);
          const open = /<script\b[^>]*\btype="module"[^>]*>/.exec(html);
          if (!open) continue;
          const end = html.indexOf("</script>", open.index);
          const code = html.slice(open.index + open[0].length, end);
          html = html.slice(0, open.index) + html.slice(end + "</script>".length);
          html = html.replace("</body>", `<script>${code}</script>\n</body>`);
          file.source = html;
        }
      },
    },
  };
}

export default defineConfig({
  root: "singlefile",
  base: "./",
  plugins: [react(), viteSingleFile(), classicInlineScript()],
  build: {
    outDir: "../dist-singlefile",
    emptyOutDir: true,
    modulePreload: false,
    cssCodeSplit: false,
    assetsInlineLimit: () => true,
    rollupOptions: {
      input: fileURLToPath(new URL("./singlefile/ui.html", import.meta.url)),
      output: {
        format: "iife",
        inlineDynamicImports: true,
      },
    },
  },
});
