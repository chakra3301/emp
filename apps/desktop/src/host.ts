// EmpHost for EMP.app (Tauri) and a plain browser tab (the dev host). Browser and Tauri APIs
// (localStorage, navigator.clipboard, Notification, @tauri-apps/*) are allowed here, never in @emp/ui.
import type { EmpHost } from "@emp/ui";

const PREFS_PREFIX = "emp:prefs:";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** A notification click focuses EMP; the first focus soon after runs that notification's action. */
const CLICK_WINDOW_MS = 30_000;
let pendingClick: { run: () => void; at: number } | null = null;
if (inTauri) {
  window.addEventListener("focus", () => {
    const p = pendingClick;
    pendingClick = null;
    if (p && Date.now() - p.at < CLICK_WINDOW_MS) p.run();
  });
}

async function tauriNotify(title: string, body?: string) {
  const n = await import("@tauri-apps/plugin-notification");
  if (!(await n.isPermissionGranted()) && (await n.requestPermission()) !== "granted") return;
  n.sendNotification({ title, body });
}

export function createDefaultHost(): EmpHost {
  const clipboard = navigator.clipboard;
  return {
    kind: inTauri ? "emp-desktop" : "browser",
    notify(title, body, onClick) {
      if (inTauri) {
        pendingClick = onClick ? { run: onClick, at: Date.now() } : null;
        void tauriNotify(title, body).catch((err: unknown) => console.warn("[emp] notification failed", err));
      } else if ("Notification" in window && Notification.permission === "granted") {
        const n = new Notification(title, { body, tag: `emp:${title}`, silent: false });
        n.onclick = () => {
          window.focus();
          n.close();
          onClick?.();
        };
      } else {
        console.info(`[emp] ${title}${body ? `: ${body}` : ""}`);
      }
    },
    // EMP.app asks macOS on the first notification; System Settings → Notifications manages it.
    notificationPermission: () => (inTauri ? "granted" : "Notification" in window ? Notification.permission : "denied"),
    requestNotifications: async () => inTauri || ("Notification" in window && (await Notification.requestPermission()) === "granted"),
    prefs: {
      async get(key) {
        const raw = localStorage.getItem(PREFS_PREFIX + key);
        if (raw === null) return undefined;
        try {
          return JSON.parse(raw) as unknown;
        } catch {
          return undefined;
        }
      },
      async set(key, value) {
        if (value === undefined || value === null) localStorage.removeItem(PREFS_PREFIX + key);
        else localStorage.setItem(PREFS_PREFIX + key, JSON.stringify(value));
      },
    },
    theme: {
      get: () => ({}),
      subscribe: () => () => {},
    },
    openExternal(url) {
      window.open(url, "_blank", "noopener,noreferrer");
    },
    clipboard: clipboard
      ? {
          read: () => clipboard.readText(),
          write: (text) => clipboard.writeText(text),
        }
      : undefined,
  };
}
