// Single-file demo entry: <EmpWorkspace> on the in-memory MockServer, no daemon and no network.
// The Orion plugin (integrations/orion/) will swap the mock for a localService transport and an
// EmpHost over window.orion; everything else in this file stays the same.
import { EmpClient } from "@emp/client";
import { MockServer } from "@emp/client/mock";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { EmpWorkspace, type EmpHost } from "../src";

const prefs = new Map<string, unknown>();
const host: EmpHost = {
  kind: "browser",
  notify: (title, body) => console.info(`[emp] ${title}${body ? `: ${body}` : ""}`),
  prefs: {
    get: async (key) => prefs.get(key),
    set: async (key, value) => void prefs.set(key, value),
  },
  theme: { get: () => ({}), subscribe: () => () => {} },
};

const server = new MockServer({ latencyMs: 30 });
const client = new EmpClient(server.factory, { token: "mock", client: { kind: "browser", version: "0.1.0-singlefile" } });
void client.connect();

createRoot(document.getElementById("emp")!).render(
  <StrictMode>
    <EmpWorkspace client={client} host={host} />
  </StrictMode>,
);
