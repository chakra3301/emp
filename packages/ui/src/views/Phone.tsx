// Settings → Phone: turn on phone access (tailscale serve), pair a phone with a code / QR,
// and manage paired devices. Everything authoritative runs in the daemon.

import { useCallback, useEffect, useState } from "react";
import type { PairingCode, PhoneStatus } from "@emp/protocol";
import { errorMessage } from "@emp/client";
import { useClient, useHost, useWorkspace } from "../store/context";
import { formatRelative } from "../lib/time";
import { ListRow } from "./Settings";

const POLL_MS = 3000;

function svgDataUrl(svg: string): string {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

function remaining(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export function PhonePage() {
  const client = useClient();
  const host = useHost();
  const now = useWorkspace((s) => s.now);
  const [status, setStatus] = useState<PhoneStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [pairing, setPairing] = useState<PairingCode | null>(null);
  const [clock, setClock] = useState(() => Date.now());
  const [pairedBefore, setPairedBefore] = useState<Set<string>>(new Set());

  const refresh = useCallback(async () => {
    try {
      setStatus(await client.request("phone.status", undefined));
    } catch (err) {
      setError(errorMessage(err));
    }
  }, [client]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // While a code is showing: tick the countdown and watch for the phone to appear.
  useEffect(() => {
    if (!pairing) return;
    const tick = setInterval(() => setClock(Date.now()), 1000);
    const poll = setInterval(() => void refresh(), POLL_MS);
    return () => {
      clearInterval(tick);
      clearInterval(poll);
    };
  }, [pairing, refresh]);

  const act = async (fn: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await fn();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  const enable = () => act(async () => setStatus(await client.request("phone.enable", undefined)));
  const disable = () =>
    act(async () => {
      setStatus(await client.request("phone.disable", undefined));
      setPairing(null);
    });
  const pair = () =>
    act(async () => {
      setPairedBefore(new Set(status?.devices.map((d) => d.id) ?? []));
      setPairing(await client.request("phone.pair", undefined));
      setClock(Date.now());
    });
  const revoke = (deviceId: string) => act(async () => setStatus(await client.request("devices.revoke", { deviceId })));

  const ts = status?.tailscale;
  const on = !!status?.url;
  const expired = pairing ? clock > pairing.expiresAt : false;
  const fresh = pairing ? (status?.devices ?? []).filter((d) => !pairedBefore.has(d.id)) : [];

  return (
    <>
      <ListRow
        row="phoneAccess"
        action={
          !status ? (
            <span className="emp-list-row__value">Checking…</span>
          ) : on ? (
            <button type="button" className="emp-button" data-orion-ui="button" disabled={busy} onClick={() => void disable()}>
              Turn off
            </button>
          ) : (
            <>
              <button type="button" className="emp-button emp-button--ghost emp-button--small" data-orion-ui="button" disabled={busy} onClick={() => void refresh()}>
                Refresh
              </button>
              <button
                type="button"
                className="emp-button emp-button--primary"
                data-orion-ui="button"
                disabled={busy || ts !== "running"}
                onClick={() => void enable()}
              >
                {busy ? "Turning on…" : "Turn on"}
              </button>
            </>
          )
        }
        below={
          status && (
            <div className="emp-phone-state">
              <span className="emp-phone-dot" data-on={on || undefined} data-warn={(ts !== "running" && ts !== "unknown") || undefined} />
              {on ? (
                <span>
                  On at <span className="emp-mono emp-phone-url">{status.url}</span>
                </span>
              ) : ts === "missing" ? (
                <span>Tailscale isn't installed. Get it at tailscale.com/download, on this Mac and on your phone.</span>
              ) : ts === "needs-login" ? (
                <span>Sign in to Tailscale on this Mac (open the Tailscale app).</span>
              ) : ts === "stopped" ? (
                <span>Tailscale is off on this Mac. Open the Tailscale app and connect, then press Refresh.</span>
              ) : ts === "running" ? (
                <span>
                  Off. This Mac is <span className="emp-mono">{status.dnsName ?? "on Tailscale"}</span>.
                </span>
              ) : (
                <span>Couldn't read Tailscale's state.</span>
              )}
              {!status.portFixed && (
                <span className="emp-phone-note">
                  This daemon was started by hand on a random port (dev). Phone access needs EMP's own daemon: quit the hand-started one and reopen EMP.
                </span>
              )}
              {status.servingOther && <span className="emp-phone-note">Tailscale already serves {status.servingOther} on this Mac; EMP leaves it alone.</span>}
              {error && <span className="emp-phone-error">{error}</span>}
            </div>
          )
        }
      />

      <ListRow
        row="pairPhone"
        action={
          <button type="button" className="emp-button" data-orion-ui="button" disabled={busy || !on} onClick={() => void pair()}>
            {pairing ? "New code" : "Pair a phone"}
          </button>
        }
        below={
          pairing && (
            <div className="emp-pair">
              {pairing.qrSvg && <img className="emp-pair__qr" src={svgDataUrl(pairing.qrSvg)} alt={`QR code for ${pairing.url ?? "pairing"}`} />}
              <div className="emp-pair__text">
                <div className="emp-pair__code emp-mono" data-expired={expired || undefined}>
                  {pairing.code}
                </div>
                <div className="emp-list-row__desc">{expired ? "Expired. Press New code." : `Works for ${remaining(pairing.expiresAt - clock)}`}</div>
                <ol className="emp-pair__steps">
                  <li>On your phone, install Tailscale and sign in with the same account as this Mac.</li>
                  <li>
                    Scan the code with the camera, or open <span className="emp-mono">{status?.url ?? "the address above"}</span> and type the code.
                  </li>
                  <li>In Safari: Share → Add to Home Screen. If the home-screen app asks for a code, type this one.</li>
                </ol>
                {pairing.url && host.clipboard && (
                  <button type="button" className="emp-text-button" onClick={() => void host.clipboard?.write(pairing.url!)}>
                    Copy link
                  </button>
                )}
                {fresh.length > 0 && <div className="emp-pair__done">Paired: {fresh.map((d) => d.name).join(", ")}</div>}
              </div>
            </div>
          )
        }
      />

      <ListRow
        row="devices"
        wide
        below={
          <ul className="emp-devices">
            {(status?.devices ?? []).length === 0 && <li className="emp-list-row__desc">No phones paired yet.</li>}
            {status?.devices.map((d) => (
              <li key={d.id} className="emp-device">
                <div className="emp-device__text">
                  <span className="emp-list-row__title">{d.name}</span>
                  <span className="emp-list-row__desc">
                    Paired {formatRelative(d.createdAt, now)}
                    {d.lastSeenAt ? ` · last used ${formatRelative(d.lastSeenAt, now)}` : " · not used yet"}
                  </span>
                </div>
                <button type="button" className="emp-button emp-button--ghost emp-button--small" data-orion-ui="button" disabled={busy} onClick={() => void revoke(d.id)}>
                  Revoke
                </button>
              </li>
            ))}
          </ul>
        }
      />
    </>
  );
}
