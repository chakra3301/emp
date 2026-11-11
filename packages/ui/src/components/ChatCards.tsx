// Cards that sit in a chat above its composer: a terminal for a command that needs the
// harness's own UI (`/login`, `/settings`), and questions the harness is waiting on.

import { useRef, useState } from "react";
import type { Question } from "@emp/protocol";
import { lastLink } from "../lib/format";
import { useHost, useWorkspace } from "../store/context";
import { liveFor } from "../store/workspace";
import { IconClose, IconTerminal } from "./icons";
import { TerminalView } from "./TerminalView";

/** The terminal a slash command opened in this chat (`scope`: a thread id, `new` or `suki`). */
export function CommandCard({ scope }: { scope: string }) {
  const host = useHost();
  const terminalId = useWorkspace((s) => s.commandCards[scope]);
  const info = useWorkspace((s) => (terminalId ? s.terminals[terminalId] : undefined));
  const close = useWorkspace((s) => s.closeCommandCard);
  const showSection = useWorkspace((s) => s.showSection);
  const focusTerminal = useWorkspace((s) => s.focusTerminal);
  const [link, setLink] = useState<string | null>(null);
  const seen = useRef("");
  if (!terminalId) return null;

  const exited = info?.state === "exited";
  const onText = (chunk: string) => {
    seen.current = (seen.current + chunk).slice(-16_384);
    const found = lastLink(seen.current);
    if (found) setLink((prev) => (prev === found ? prev : found));
  };

  return (
    <section className="emp-cmdcard" aria-label={info?.title ?? "Command"} data-exited={exited || undefined}>
      <header className="emp-cmdcard__bar">
        <span className="emp-cmdcard__icon" aria-hidden="true">
          <IconTerminal />
        </span>
        <span className="emp-cmdcard__title emp-truncate">{info?.title ?? "Starting…"}</span>
        <span className="emp-cmdcard__state">{exited ? `Done${info?.exitCode ? ` (exit ${info.exitCode})` : ""}` : "Type here to answer it"}</span>
        <span className="emp-row__spacer" />
        {link && host.openExternal && (
          <button type="button" className="emp-button emp-button--small emp-button--primary" data-orion-ui="button" title={link} onClick={() => host.openExternal?.(link)}>
            Open sign-in page
          </button>
        )}
        <button
          type="button"
          className="emp-button emp-button--small"
          data-orion-ui="button"
          onClick={() => {
            showSection("terminals");
            focusTerminal(terminalId);
          }}
        >
          Open in Terminals
        </button>
        <button type="button" className="emp-icon-button" title={exited ? "Close" : "Close and stop it"} aria-label="Close" onClick={() => void close(scope)}>
          <IconClose />
        </button>
      </header>
      <div className="emp-cmdcard__screen">
        <TerminalView key={terminalId} terminalId={terminalId} onText={onText} />
      </div>
    </section>
  );
}

/** Every question the thread is waiting on. */
export function QuestionCards({ threadId }: { threadId: string }) {
  const questions = useWorkspace((s) => liveFor(s, threadId)?.questions);
  if (!questions?.length) return null;
  return (
    <>
      {questions.map((q) => (
        <QuestionCard key={q.id} threadId={threadId} question={q} />
      ))}
    </>
  );
}

const FILTER_AT = 8;

function QuestionCard({ threadId, question: q }: { threadId: string; question: Question }) {
  const answer = useWorkspace((s) => s.answerQuestion);
  const [text, setText] = useState(q.prefill ?? "");
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const send = async (a: { value?: string; confirmed?: boolean; cancelled?: boolean }) => {
    setBusy(true);
    await answer(threadId, q.id, a);
    setBusy(false);
  };
  const f = filter.trim().toLowerCase();
  const options = f ? q.options.filter((o) => o.toLowerCase().includes(f)) : q.options;
  const fromEmp = q.id.startsWith("emp:");

  return (
    <section className="emp-question" aria-label={q.title} data-kind={q.kind}>
      <div className="emp-question__head">
        <span className="emp-question__from">{fromEmp ? "Pick one" : "The agent is asking"}</span>
        <span className="emp-question__title">{q.title}</span>
      </div>
      {q.message && <p className="emp-question__message">{q.message}</p>}
      {q.kind === "select" && (
        <>
          {q.options.length > FILTER_AT && (
            <input className="emp-question__input" aria-label="Filter" placeholder="Filter…" value={filter} autoFocus onChange={(e) => setFilter(e.target.value)} />
          )}
          <div className="emp-question__options" role="listbox" aria-label={q.title}>
            {options.map((o) => (
              <button key={o} type="button" role="option" aria-selected={false} className="emp-question__option" disabled={busy} onClick={() => void send({ value: o })}>
                {o}
              </button>
            ))}
            {options.length === 0 && <span className="emp-question__empty">No matches</span>}
          </div>
        </>
      )}
      {(q.kind === "input" || q.kind === "editor") && (
        <form
          className="emp-question__form"
          onSubmit={(e) => {
            e.preventDefault();
            void send({ value: text });
          }}
        >
          {q.kind === "input" ? (
            <input className="emp-question__input" aria-label={q.title} placeholder={q.placeholder ?? ""} value={text} autoFocus onChange={(e) => setText(e.target.value)} />
          ) : (
            <textarea className="emp-question__input emp-question__editor emp-mono" aria-label={q.title} value={text} autoFocus onChange={(e) => setText(e.target.value)} />
          )}
        </form>
      )}
      <div className="emp-question__actions">
        {q.kind === "confirm" && (
          <>
            <button type="button" className="emp-button emp-button--small emp-button--primary" data-orion-ui="button" disabled={busy} onClick={() => void send({ confirmed: true })}>
              Yes
            </button>
            <button type="button" className="emp-button emp-button--small" data-orion-ui="button" disabled={busy} onClick={() => void send({ confirmed: false })}>
              No
            </button>
          </>
        )}
        {(q.kind === "input" || q.kind === "editor") && (
          <button type="button" className="emp-button emp-button--small emp-button--primary" data-orion-ui="button" disabled={busy} onClick={() => void send({ value: text })}>
            Send
          </button>
        )}
        <button type="button" className="emp-button emp-button--small emp-button--ghost" data-orion-ui="button" disabled={busy} onClick={() => void send({ cancelled: true })}>
          {fromEmp ? "Cancel" : "Dismiss"}
        </button>
      </div>
    </section>
  );
}
