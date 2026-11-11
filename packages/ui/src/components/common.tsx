import { useState, type ReactNode } from "react";
import type { ConnectionStatus } from "@emp/client";
import type { Harness } from "@emp/protocol";

export function connectionLabel(status: ConnectionStatus): string {
  switch (status.state) {
    case "ready":
      return status.daemonVersion ? `Connected · empd ${status.daemonVersion}` : "Connected";
    case "connecting":
      return "Connecting…";
    case "closed":
      return status.retryAt ? "Disconnected · retrying" : "Disconnected";
    case "error":
      return "Connection failed";
  }
}

export function StatusDot({ status }: { status: ConnectionStatus }) {
  return <span className="emp-dot" data-state={status.state} aria-hidden="true" />;
}

/** Harness identity: a tinted dot and the harness name. */
export function HarnessBadge({ harness, label }: { harness: Harness; label?: string }) {
  return (
    <span className="emp-harness" data-harness={harness}>
      <span className="emp-harness__dot" aria-hidden="true" />
      {label ?? harness}
    </span>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="emp-kbd">{children}</kbd>;
}

export function LoadingState({ label = "Loading…" }: { label?: string }) {
  return (
    <div className="emp-state" role="status" data-orion-ui="muted">
      {label}
    </div>
  );
}

export function ErrorState({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <div className="emp-state emp-state--error" role="alert">
      <span>{message}</span>
      {onRetry && (
        <button type="button" className="emp-button" data-orion-ui="button" onClick={onRetry}>
          Retry
        </button>
      )}
    </div>
  );
}

export function EmptyState({ title, hint, children, centered }: { title: string; hint?: ReactNode; children?: ReactNode; centered?: boolean }) {
  return (
    <div className="emp-state" data-centered={centered || undefined}>
      <div className="emp-state__title">{title}</div>
      {hint && (
        <div className="emp-state__hint" data-orion-ui="muted">
          {hint}
        </div>
      )}
      {children}
    </div>
  );
}

/** Long text, truncated until expanded. Keeps huge tool outputs out of the DOM by default. */
export function ClampedText({ text, limit, className }: { text: string; limit: number; className?: string }) {
  const [full, setFull] = useState(false);
  const clipped = !full && text.length > limit;
  return (
    <>
      <div className={className}>{clipped ? `${text.slice(0, limit)}…` : text}</div>
      {text.length > limit && (
        <button type="button" className="emp-link" onClick={() => setFull((v) => !v)}>
          {full ? "Show less" : `Show all (${text.length.toLocaleString()} chars)`}
        </button>
      )}
    </>
  );
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="emp-field">
      <dt data-orion-ui="muted">{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}
