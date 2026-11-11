import { useEffect, useRef } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { errorMessage } from "@emp/client";
import type { TerminalOutput } from "@emp/protocol";
import { base64ToBytes, bytesToBase64 } from "../lib/base64";
import { loadTerminalFont, terminalTheme } from "../lib/terminalTheme";
import { useClient, useWorkspace } from "../store/context";

/** Daemon-owned terminal rendered with xterm.js. Output streams straight from the client. */
/** `onText` gets what the program prints (decoded, escape codes included), e.g. to find links. */
export function TerminalView({ terminalId, autoFocus = true, onText }: { terminalId: string; autoFocus?: boolean; onText?: (text: string) => void }) {
  const onTextRef = useRef(onText);
  onTextRef.current = onText;
  const client = useClient();
  const ref = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  // Re-colour when the workspace theme changes (Settings → Background / Accent).
  const look = useWorkspace((s) => `${s.settings.background}|${s.settings.accent}`);

  useEffect(() => {
    const el = ref.current;
    let disposed = false;
    let teardown: (() => void) | null = null;
    if (!el) return;
    const css = getComputedStyle(el);
    const family = css.getPropertyValue("--emp-type-family-mono").trim() || "monospace";
    const size = Number.parseFloat(css.getPropertyValue("--emp-type-size-sm")) || 12;
    // Measure cells only once the font is in, or box-drawing lines overrun the pane.
    void loadTerminalFont(family, size).then(() => {
      if (!disposed) teardown = mount(el, family, size);
    });
    return () => {
      disposed = true;
      teardown?.();
    };
  }, [client, terminalId, autoFocus]);

  useEffect(() => {
    const el = ref.current;
    const term = termRef.current;
    if (!el || !term) return;
    const frame = requestAnimationFrame(() => {
      term.options.theme = terminalTheme(el);
    });
    return () => cancelAnimationFrame(frame);
  }, [look]);

  function mount(el: HTMLDivElement, family: string, fontSize: number): () => void {
    const term = new Terminal({
      fontFamily: family,
      fontSize,
      lineHeight: 1.15,
      letterSpacing: 0,
      fontWeight: "normal",
      fontWeightBold: "bold",
      cursorBlink: true,
      scrollback: 10_000,
      minimumContrastRatio: 4.5,
      theme: terminalTheme(el),
    });
    termRef.current = term;
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);

    let disposed = false;
    // Replay the snapshot first, then live chunks newer than it (pushes can race the attach).
    let lastSeq: number | null = null;
    const decoder = new TextDecoder();
    const early: TerminalOutput[] = [];
    const apply = (p: TerminalOutput) => {
      if (lastSeq !== null && p.seq <= lastSeq) return;
      lastSeq = p.seq;
      const bytes = base64ToBytes(p.data);
      term.write(bytes);
      onTextRef.current?.(decoder.decode(bytes, { stream: true }));
    };
    const offOutput = client.on("terminal.output", (p) => {
      if (p.terminalId !== terminalId) return;
      if (lastSeq === null) early.push(p);
      else apply(p);
    });
    client.request("terminals.attach", { terminalId }).then(
      (snap) => {
        if (disposed) return;
        const bytes = base64ToBytes(snap.scrollback);
        term.write(bytes);
        onTextRef.current?.(decoder.decode(bytes, { stream: true }));
        lastSeq = snap.seq;
        for (const p of early.splice(0)) apply(p);
        resize(true);
      },
      (err: unknown) => {
        if (!disposed) term.write(`\r\n[EMP: ${errorMessage(err)}]\r\n`);
      },
    );

    // Writes are serialized: the daemon handles requests concurrently, so two in flight could
    // reach the PTY out of order. Keys typed meanwhile are batched into the next write.
    const encoder = new TextEncoder();
    let pending: number[] = [];
    let inflight = false;
    const flush = () => {
      if (inflight || pending.length === 0 || disposed) return;
      const data = bytesToBase64(Uint8Array.from(pending));
      pending = [];
      inflight = true;
      client.request("terminals.write", { terminalId, data }).then(
        () => {
          inflight = false;
          flush();
        },
        () => {
          inflight = false;
          pending = [];
        },
      );
    };
    const onData = term.onData((d) => {
      pending.push(...encoder.encode(d));
      flush();
    });
    const onBinary = term.onBinary((d) => {
      for (let i = 0; i < d.length; i += 1) pending.push(d.charCodeAt(i) & 0xff);
      flush();
    });

    let size = { cols: 0, rows: 0 };
    let timer: ReturnType<typeof setTimeout> | null = null;
    const resize = (now = false) => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(
        () => {
          timer = null;
          if (disposed || el.clientWidth === 0) return;
          fit.fit();
          if (term.cols === size.cols && term.rows === size.rows) return;
          size = { cols: term.cols, rows: term.rows };
          client.request("terminals.resize", { terminalId, ...size }).catch(() => {});
        },
        now ? 0 : 60,
      );
    };
    const observer = new ResizeObserver(() => resize());
    observer.observe(el);
    resize(true);
    if (autoFocus) term.focus();

    return () => {
      disposed = true;
      termRef.current = null;
      offOutput();
      onData.dispose();
      onBinary.dispose();
      observer.disconnect();
      if (timer) clearTimeout(timer);
      term.dispose();
    };
  }

  return <div ref={ref} className="emp-terminal__screen" aria-label="Terminal" />;
}
