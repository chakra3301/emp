// The one injected interface between @emp/ui and whatever hosts it (FOUNDATION §4a, rule 1).
// Desktop implements it with browser/Tauri APIs; the Orion plugin over `window.orion`.
// The transport is NOT part of the host: the host builds an `EmpClient` from its own
// `TransportFactory` and passes the client to <EmpWorkspace> next to the host.

export type EmpHostKind = "emp-desktop" | "orion-plugin" | "browser";

export interface Disposable {
  dispose(): void;
}

/** Small persistent key/value store for UI preferences. Values must be JSON-serializable. */
export interface KeyValueStore {
  get(key: string): Promise<unknown>;
  set(key: string, value: unknown): Promise<void>;
}

/** Overrides for `--emp-*` CSS variables, e.g. mapped from Orion's appearance snapshot. */
export interface EmpThemeSource {
  get(): Record<string, string>;
  /** Returns an unsubscribe function. */
  subscribe(cb: (vars: Record<string, string>) => void): () => void;
}

export interface EmpCommand {
  id: string;
  title: string;
  /** Display hint, e.g. "Mod+K". The host owns actual key handling. */
  keybinding?: string;
  run(): void;
}

export interface EmpClipboard {
  read(): Promise<string>;
  write(text: string): Promise<void>;
}

export interface EmpHost {
  kind: EmpHostKind;
  /** System notification. `onClick` runs when the user clicks it (hosts that can route clicks). */
  notify(title: string, body?: string, onClick?: () => void): void;
  /** Notification permission: "granted" | "denied" | "default" (not asked yet). Absent = always allowed. */
  notificationPermission?(): "granted" | "denied" | "default";
  /** Ask for notification permission; must be called from a click. */
  requestNotifications?(): Promise<boolean>;
  prefs: KeyValueStore;
  theme: EmpThemeSource;
  /** Desktop only; the Orion plugin declares commands in its manifest instead. */
  registerCommand?(cmd: EmpCommand): Disposable;
  setBadge?(count: number): void;
  openExternal?(url: string): void;
  clipboard?: EmpClipboard;
}
