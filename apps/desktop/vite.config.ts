import react from "@vitejs/plugin-react";
import { defaultClientConditions, defineConfig } from "vite";
import { empConnectPlugin } from "./emp-connect";

export default defineConfig({
  plugins: [react(), empConnectPlugin()],
  // Resolve @emp/ui to its TypeScript source (package.json "source" condition) for HMR.
  resolve: { conditions: ["source", ...defaultClientConditions] },
  server: {
    host: "127.0.0.1",
    port: 5174,
    strictPort: true,
  },
});
