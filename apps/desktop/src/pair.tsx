// Pairing screen for EMP served to a phone: redeem a code (from the QR link's `#pair=` or typed)
// for this device's own token, kept in localStorage. Host-side, so browser APIs are fine here.
import { useEffect, useRef, useState } from "react";
import "./pair.css";

const DEVICE_KEY = "emp:device-token";

export function readDeviceToken(): string | null {
  try {
    return localStorage.getItem(DEVICE_KEY);
  } catch {
    return null;
  }
}

export function clearDeviceToken(): void {
  localStorage.removeItem(DEVICE_KEY);
}

function deviceName(): string {
  const ua = navigator.userAgent;
  const base = /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android phone" : "Browser";
  const standalone = (navigator as Navigator & { standalone?: boolean }).standalone || matchMedia("(display-mode: standalone)").matches;
  return standalone ? `${base} (home screen)` : base;
}

function codeFromHash(): string | null {
  const m = /[#&]pair=([A-Za-z0-9-]+)/.exec(location.hash);
  return m ? m[1]! : null;
}

async function redeem(code: string): Promise<void> {
  const res = await fetch("/api/pair", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ code, name: deviceName() }),
  });
  const body = (await res.json().catch(() => ({}))) as { token?: string; error?: string };
  if (!res.ok || !body.token) throw new Error(body.error ?? `Pairing failed (HTTP ${res.status})`);
  localStorage.setItem(DEVICE_KEY, body.token);
}

export function PairScreen({ notice, onPaired }: { notice: string | null; onPaired(): void }) {
  const [code, setCode] = useState(() => codeFromHash() ?? "");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const tried = useRef(false);

  const submit = async (value: string) => {
    if (!value.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await redeem(value.trim());
      history.replaceState(null, "", location.pathname);
      onPaired();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  // Opened from the QR code: pair straight away.
  useEffect(() => {
    const fromLink = codeFromHash();
    if (fromLink && !tried.current) {
      tried.current = true;
      void submit(fromLink);
    }
    // Runs once on load.
  }, []);

  return (
    <main className="pair">
      <div className="pair__mark">EMP</div>
      <h1 className="pair__title">Pair this device</h1>
      <p className="pair__text">
        On your Mac, open EMP → Settings → Phone and press <b>Pair a phone</b>. Scan the QR code, or type the code here.
      </p>
      {notice && <p className="pair__notice">{notice}</p>}
      <form
        className="pair__form"
        onSubmit={(e) => {
          e.preventDefault();
          void submit(code);
        }}
      >
        <input
          className="pair__input"
          aria-label="Pairing code"
          placeholder="ABCD-EFGH"
          autoCapitalize="characters"
          autoComplete="one-time-code"
          spellCheck={false}
          value={code}
          onChange={(e) => setCode(e.target.value.toUpperCase())}
        />
        <button className="pair__button" type="submit" disabled={busy || !code.trim()}>
          {busy ? "Pairing…" : "Pair"}
        </button>
      </form>
      {error && <p className="pair__error">{error}</p>}
      <p className="pair__fine">Only devices on your Tailscale network can reach this page. Each paired device can be revoked from the Mac.</p>
    </main>
  );
}
