// Library build for EMP.app: ESM, peers and @emp/* external, CSS emitted as dist/emp-ui.css.
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const external = /^(?:react|react-dom|zustand)(?:\/|$)|^@emp\/|^@xterm\//;

export default defineConfig({
  plugins: [react()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
    lib: {
      entry: "src/index.ts",
      formats: ["es"],
      fileName: () => "emp-ui.js",
      cssFileName: "emp-ui",
    },
    rollupOptions: {
      external: (id) => external.test(id),
    },
  },
});
