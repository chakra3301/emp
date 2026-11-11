// Desktop/web host: mounts <EmpWorkspace> full-window. Three ways to reach the daemon:
// - EMP.app (Tauri): `emp_connect` from the Rust side, which also starts empd via launchd.
// - Vite dev server: its /__emp/connect endpoint (or the in-memory mock with `?mock`).
// - Served by empd itself (a phone over Tailscale): same-origin /ws with this device's own
//   token, which it gets by pairing (QR link `#pair=CODE`, or typing the code).
// Connection info is re-read on every reconnect, since empd picks a new port and token when it
// restarts.
import { EmpClient, type TransportFactory } from "@emp/client";
import { MockServer } from "@emp/client/mock";
import { WebSocketTransport } from "@emp/client/ws";
import { EmpWorkspace } from "@emp/ui";
import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { createDefaultHost, inTauri } from "./host";
import { PairScreen, clearDeviceToken, readDeviceToken } from "./pair";

interface ConnectInfo {
  wsUrl: string;
  token: string;
}

const mode: "tauri" | "dev" | "web" = inTauri ? "tauri" : import.meta.env.DEV ? "dev" : "web";
const useMock = mode === "dev" && new URLSearchParams(location.search).has("mock");
const host = createDefaultHost();

async function tauriConnect(): Promise<ConnectInfo> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<ConnectInfo>("emp_connect");
}

async function devConnect(): Promise<ConnectInfo> {
  const res = await fetch("/__emp/connect", { cache: "no-store" });
  const body = (await res.json().catch(() => ({}))) as Partial<ConnectInfo> & { error?: string };
  if (!res.ok || !body.wsUrl || !body.token) throw new Error(body.error ?? `Daemon discovery failed (HTTP ${res.status})`);
  return { wsUrl: body.wsUrl, token: body.token };
}

function webConnect(): ConnectInfo {
  const token = readDeviceToken();
  if (!token) throw new Error("This device isn't paired");
  return { wsUrl: `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`, token };
}

function makeClient(): EmpClient {
  let token = "";
  const daemonTransport: TransportFactory = async () => {
    const info = mode === "tauri" ? await tauriConnect() : mode === "dev" ? await devConnect() : webConnect();
    token = info.token;
    return WebSocketTransport.open(info.wsUrl);
  };
  const client = new EmpClient(useMock ? new MockServer({ latencyMs: 30 }).factory : daemonTransport, {
    token: () => (useMock ? "mock" : token),
    client: { kind: mode === "tauri" ? "desktop" : mode === "web" ? "web" : "browser", version: mode === "dev" ? "0.1.0-dev" : "0.1.0" },
  });
  client.connect().catch(() => {
    // Shown in the UI via the connection state.
  });
  return client;
}

/** Desktop and dev: one client for the page's lifetime. */
const desktopClient = mode === "web" ? null : makeClient();
import.meta.hot?.dispose(() => desktopClient?.dispose());

/** Served to a phone: pair first, then connect; a revoked device goes back to pairing. */
function WebRoot() {
  const [paired, setPaired] = useState(() => !!readDeviceToken());
  const [notice, setNotice] = useState<string | null>(null);
  const [client, setClient] = useState<EmpClient | null>(null);

  useEffect(() => {
    if (!paired) return;
    const c = makeClient();
    setClient(c);
    const off = c.onStateChange((s) => {
      if (s.state === "error" && /invalid token/i.test(s.error ?? "")) {
        clearDeviceToken();
        setNotice("This device was signed out. Pair it again from Settings → Phone on your Mac.");
        setPaired(false);
      }
    });
    return () => {
      off();
      c.dispose();
      setClient(null);
    };
  }, [paired]);

  if (!paired) return <PairScreen notice={notice} onPaired={() => setPaired(true)} />;
  return client ? <EmpWorkspace client={client} host={host} /> : null;
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>{desktopClient ? <EmpWorkspace client={desktopClient} host={host} /> : <WebRoot />}</StrictMode>,
);
