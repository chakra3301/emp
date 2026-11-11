import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import type { Thread, TranscriptItem } from "@emp/protocol";
import { ClampedText, EmptyState, ErrorState, Field, HarnessBadge, LoadingState } from "../components/common";
import {
  AttachButton,
  DraftFiles,
  DropHint,
  commandRun,
  slashName,
  useDraft,
  useFileDrop,
  usePasteFiles,
  MessageFiles,
  ReplyText,
  ScheduledTag,
  useSlashMenu,
} from "../components/ComposerParts";
import { IconChevron, IconClock, IconCopy, IconSend, IconStop } from "../components/icons";
import { folderLines, splitAttachments } from "../lib/attach";
import { CommandCard, QuestionCards } from "../components/ChatCards";
import { ElsewhereBar, GoalBar, ScheduleBar } from "../components/SessionBars";
import { newScheduleDraft } from "../lib/schedule";
import { Markdown } from "../components/Markdown";
import { Picker } from "../components/Picker";
import { TerminalView } from "../components/TerminalView";
import { scheduleDone, shortPath, stripContext, stripMarkdown } from "../lib/format";
import { earlierRange, laterRange } from "../lib/paging";
import { formatDateTime } from "../lib/time";
import { isCopilotModel } from "../lib/models";
import { useHost, useWorkspace, useWorkspaceStore } from "../store/context";
import { CHAT_HARNESSES, liveFor, projectName } from "../store/workspace";
import { canTerminal } from "./TabBar";

/** Characters shown before "Show all" in expanded tool output and long messages. */
const LONG_TEXT_LIMIT = 20_000;

/** Orion-only chats (stored in orion.db) have no harness session to continue. */
function orionOnly(t: Thread): boolean {
  return t.session.profile?.startsWith("orion:") ?? false;
}

function canChat(t: Thread): boolean {
  return !orionOnly(t) && CHAT_HARNESSES.includes(t.harness);
}

export function ThreadView() {
  const thread = useWorkspace((s) => s.thread);
  const paneTerminalId = useWorkspace((s) => s.paneTerminalId);
  const details = useWorkspace((s) => s.detailsOpen);
  const chatOwner = useWorkspace((s) => (s.selectedThreadId ? liveFor(s, s.selectedThreadId)?.owner : undefined));
  const drop = useFileDrop(thread.data?.id ?? "");
  const droppable = !!thread.data && canChat(thread.data) && chatOwner !== "terminal";

  return (
    <section className="emp-detail" data-orion-ui="panel" aria-label="Thread" {...(droppable ? drop.props : {})}>
      <DropHint dropping={droppable && drop.dropping} />
      {thread.status === "loading" && !thread.data && <LoadingState label="Loading thread…" />}
      {thread.status === "error" && !thread.data && <ErrorState message={thread.error ?? "Could not load thread"} />}
      {thread.data && details && <ThreadDetails thread={thread.data} />}
      <Transcript />
      {thread.data && paneTerminalId && <TerminalPane terminalId={paneTerminalId} />}
      {thread.data && <Composer thread={thread.data} />}
    </section>
  );
}

function ThreadDetails({ thread: t }: { thread: Thread }) {
  const project = useWorkspace((s) => projectName(s, t.projectId));
  return (
    <div className="emp-details">
      <dl className="emp-fields">
        <Field label="Harness">
          <HarnessBadge harness={t.harness} />
        </Field>
        <Field label="Model">{t.model ?? "—"}</Field>
        {project && <Field label="Project">{project}</Field>}
        <Field label="Directory">
          <span className="emp-mono emp-break">{t.cwd ?? "—"}</span>
        </Field>
        <Field label="Created">{formatDateTime(t.createdAt)}</Field>
        <Field label="Updated">{formatDateTime(t.updatedAt)}</Field>
        {t.messageCount !== null && <Field label="Messages">{t.messageCount.toLocaleString()}</Field>}
        <Field label="Session">
          <span className="emp-mono emp-break">{t.session.sessionId}</span>
        </Field>
        {t.importedFrom && (
          <Field label="Source">
            <span className="emp-mono emp-break" title={t.importedFrom.path}>
              {shortPath(t.importedFrom.path, 90)}
            </span>
          </Field>
        )}
      </dl>
      {canTerminal(t) && <ResumeBox />}
    </div>
  );
}

function ResumeBox() {
  const resume = useWorkspace((s) => s.resume);
  const host = useHost();
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);

  if (resume.status === "idle" || resume.status === "loading") return null;
  if (resume.status === "error") return <ErrorState message={resume.error ?? "Could not build the resume command"} />;
  if (!resume.spec) return null;

  const display = resume.spec.display;
  const clipboard = host.clipboard;
  return (
    <div className="emp-resume">
      <div className="emp-form__label">Resume in any terminal</div>
      <div className="emp-codebox">
        <code className="emp-mono emp-break">{display}</code>
        {clipboard && (
          <button
            type="button"
            className="emp-button emp-button--icon"
            data-orion-ui="button"
            aria-label={copied ? "Copied" : "Copy command"}
            title={copied ? "Copied" : "Copy"}
            onClick={() => {
              clipboard.write(display).then(
                () => setCopied(true),
                () => host.notify("Copy failed", "The clipboard is not available."),
              );
            }}
          >
            {copied ? "✓" : <IconCopy />}
          </button>
        )}
      </div>
      <div className="emp-note">One process per session: return EMP's chat or terminal first.</div>
    </div>
  );
}

function Transcript() {
  const tr = useWorkspace((s) => s.transcript);
  const loadEarlier = useWorkspace((s) => s.loadEarlier);
  const loadLater = useWorkspace((s) => s.loadLater);
  const scrollRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const anchor = useRef<{ height: number; top: number } | null>(null);
  const running = useWorkspace((s) => liveFor(s, s.selectedThreadId)?.status === "running");
  const { stuck, toBottom } = useStickToBottom(scrollRef, innerRef, `${tr.threadId}:${tr.status === "loading" ? "" : "ready"}`);

  // Open at the newest message.
  useLayoutEffect(() => {
    if (tr.status === "ready") toBottom();
  }, [tr.threadId, tr.status, toBottom]);

  // Sending (a turn starts) always brings the newest reply into view.
  useEffect(() => {
    if (running) toBottom();
  }, [running, toBottom]);

  // Keep the viewport steady when earlier items are prepended.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    const a = anchor.current;
    if (!el || !a) return;
    anchor.current = null;
    el.scrollTop = a.top + (el.scrollHeight - a.height);
  }, [tr.start]);

  if (tr.status === "loading") return <LoadingState label="Loading transcript…" />;
  if (tr.status === "error" && tr.items.length === 0) return <ErrorState message={tr.error ?? "Could not load transcript"} />;
  if (tr.status === "ready" && tr.total === 0) return <EmptyState title="Empty transcript" hint="This session has no messages EMP can show." />;

  const earlier = earlierRange(tr);
  const later = laterRange(tr);

  return (
    <div ref={scrollRef} className="emp-transcript" aria-label="Transcript">
      <div ref={innerRef} className="emp-transcript__inner">
        {earlier && (
          <div className="emp-transcript__pager">
            <button
              type="button"
              className="emp-button"
              data-orion-ui="button"
              disabled={tr.loadingEarlier}
              onClick={() => {
                const el = scrollRef.current;
                if (el) anchor.current = { height: el.scrollHeight, top: el.scrollTop };
                void loadEarlier();
              }}
            >
              {tr.loadingEarlier ? "Loading…" : `Load earlier (${tr.start} more)`}
            </button>
          </div>
        )}
        {tr.items.map((item, i) => (
          <TranscriptEntry key={tr.start + i} item={item} />
        ))}
        {later && (
          <div className="emp-transcript__pager">
            <button type="button" className="emp-button" data-orion-ui="button" disabled={tr.loadingLater} onClick={() => void loadLater()}>
              {tr.loadingLater ? "Loading…" : `Load more (${tr.total - tr.start - tr.items.length} more)`}
            </button>
          </div>
        )}
        {!later && <LiveTail />}
        {tr.error && tr.items.length > 0 && <ErrorState message={tr.error} />}
      </div>
      {!stuck && (
        <button type="button" className="emp-jump" onClick={toBottom}>
          <IconChevron open />
          Jump to latest
        </button>
      )}
    </div>
  );
}

/** Distance from the bottom (px) that still counts as "at the bottom". */
const STICK_SLACK = 48;

/**
 * Follow new content while the reader is at the bottom. "At the bottom" is decided by the
 * reader's own scrolling (before content grows), and any growth of the transcript (streamed
 * text, a long tool output, rows that render late) re-pins it while stuck.
 */
function useStickToBottom(scrollRef: RefObject<HTMLDivElement | null>, innerRef: RefObject<HTMLDivElement | null>, mountKey: string) {
  const [stuck, setStuck] = useState(true);
  const stuckRef = useRef(true);
  const programmatic = useRef(false);

  /** Scroll to the end; flags it so the resulting scroll event isn't read as the reader's. */
  const pin = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const target = el.scrollHeight - el.clientHeight;
    if (Math.abs(el.scrollTop - target) <= 1) return;
    programmatic.current = true;
    el.scrollTop = target;
  }, [scrollRef]);

  const toBottom = useCallback(() => {
    stuckRef.current = true;
    setStuck(true);
    pin();
  }, [pin]);

  useEffect(() => {
    const el = scrollRef.current;
    const inner = innerRef.current;
    if (!el || !inner) return;
    const onScroll = () => {
      if (programmatic.current) {
        programmatic.current = false;
        return;
      }
      const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= STICK_SLACK;
      if (atBottom !== stuckRef.current) {
        stuckRef.current = atBottom;
        setStuck(atBottom);
      }
    };
    const grow = new ResizeObserver(() => {
      if (stuckRef.current) pin();
    });
    el.addEventListener("scroll", onScroll, { passive: true });
    grow.observe(inner);
    grow.observe(el);
    return () => {
      el.removeEventListener("scroll", onScroll);
      grow.disconnect();
    };
    // Re-attach when the transcript element itself is replaced (another thread, loading → ready).
  }, [mountKey, scrollRef, innerRef, pin]);

  return { stuck, toBottom };
}

/** Items streamed since the transcript was fetched, plus the block being written right now. */
function LiveTail() {
  const view = useWorkspace((s) => s.liveView);
  const selected = useWorkspace((s) => s.selectedThreadId);
  const running = useWorkspace((s) => liveFor(s, s.selectedThreadId)?.status === "running");

  if (view.threadId !== selected) return null;
  return (
    <>
      {view.tail.map((item, i) => (
        <TranscriptEntry key={`live-${i}`} item={item} />
      ))}
      {view.partial?.kind === "assistant" && (
        <div className="emp-msg" data-kind="assistant" data-streaming="true">
          <Markdown className="emp-prose" text={scheduleDone(view.partial.text).text} />
        </div>
      )}
      {view.partial?.kind === "thinking" && (
        <div className="emp-msg emp-collapsible" data-kind="thinking" data-streaming="true">
          <div className="emp-collapsible__toggle">
            <span className="emp-collapsible__label">Thinking</span>
            <span className="emp-collapsible__preview emp-truncate">{previewText(stripMarkdown(view.partial.text.slice(-400)))}</span>
          </div>
        </div>
      )}
      {view.partial?.kind === "tool" && (
        <div className="emp-msg emp-collapsible" data-kind="toolResult" data-streaming="true">
          <pre className="emp-pre emp-mono">{view.partial.text.slice(-4000)}</pre>
        </div>
      )}
      {running && !view.partial && (
        <div className="emp-working" role="status">
          <span className="emp-working__dots" aria-hidden="true">
            <span />
            <span />
            <span />
          </span>
          Working
        </div>
      )}
    </>
  );
}

function TerminalPane({ terminalId }: { terminalId: string }) {
  const info = useWorkspace((s) => s.terminals[terminalId]);
  const live = useWorkspace((s) => liveFor(s, s.selectedThreadId));
  const setPane = useWorkspace((s) => s.setPaneTerminal);
  const kill = useWorkspace((s) => s.killTerminal);
  const release = useWorkspace((s) => s.release);
  const isTakeover = live?.owner === "terminal" && live.terminalId === terminalId;

  return (
    <div className="emp-pane" data-orion-ui="panel">
      <div className="emp-pane__bar" data-orion-ui="toolbar">
        <span className="emp-truncate">{info?.title ?? "Terminal"}</span>
        {info?.state === "exited" && (
          <span className="emp-note" data-orion-ui="muted">
            exited{info.exitCode !== null ? ` (${info.exitCode})` : ""}
          </span>
        )}
        <span className="emp-row__spacer" />
        {isTakeover ? (
          <button type="button" className="emp-button emp-button--small" data-orion-ui="button" onClick={() => void release()}>
            Return to chat
          </button>
        ) : (
          <button
            type="button"
            className="emp-button emp-button--small"
            data-orion-ui="button"
            onClick={() => {
              void kill(terminalId);
              setPane(null);
            }}
          >
            {info?.state === "exited" ? "Close" : "Kill"}
          </button>
        )}
        <button type="button" className="emp-button emp-button--small" data-orion-ui="button" title="Hide (keeps running)" onClick={() => setPane(null)}>
          Hide
        </button>
      </div>
      <TerminalView key={terminalId} terminalId={terminalId} />
    </div>
  );
}

/** The chat's model. A pick applies from the next message on and stays: the daemon keeps it. */
function ThreadModelPicker({ thread }: { thread: Thread }) {
  const profile = thread.harness === "hermes" ? (thread.session.profile ?? "") : "";
  const catalogue = useWorkspace((s) => s.models[`${thread.harness}:${profile}`]?.data);
  const loadModels = useWorkspace((s) => s.loadModels);
  const picked = useWorkspace((s) => s.threadModels[thread.id]);
  const setPick = useWorkspace((s) => s.setThreadModel);
  // A chat that hasn't been imported yet runs on its agent's model.
  const agentModel = useWorkspace((s) => s.agents.data?.find((a) => a.id === thread.agentId)?.model ?? null);
  useEffect(() => {
    void loadModels(thread.harness, profile || null);
  }, [thread.harness, profile, loadModels]);

  const saved = picked ?? thread.model ?? agentModel;
  const current = saved && !isCopilotModel(saved) ? saved : null;
  const short = current?.replace(/^[\w-]+\//, "") ?? null;
  if (!catalogue?.length) {
    return short ? (
      <span className="emp-composer__model emp-truncate" title={`${thread.harness} · ${current}`}>
        {short}
      </span>
    ) : null;
  }
  const known = catalogue.find((m) => m.id === current);
  return (
    <Picker
      label={known?.label ?? short ?? "Model"}
      title="Model for this chat, from your next message on"
      value={current}
      onChange={(id) => setPick(thread.id, id)}
      onOpen={() => void loadModels(thread.harness, profile || null, true)}
      options={[
        ...(current && !known ? [{ id: current, label: short!, detail: <span className="emp-mono">current</span> }] : []),
        ...catalogue.map((m) => ({ id: m.id, label: m.label, group: m.group ?? undefined })),
      ]}
    />
  );
}

/** Enter sends (steers while a turn runs) · ⌘/Ctrl+Enter queues a follow-up · Esc Esc stops. */
function Composer({ thread }: { thread: Thread }) {
  const live = useWorkspace((s) => liveFor(s, thread.id));
  const composer = useWorkspace((s) => s.composer);
  const send = useWorkspace((s) => s.send);
  const interrupt = useWorkspace((s) => s.interrupt);
  const takeover = useWorkspace((s) => s.takeover);
  const release = useWorkspace((s) => s.release);
  const runCommand = useWorkspace((s) => s.runCommand);
  const runNative = useWorkspace((s) => s.runNative);
  const setComposerError = useWorkspace((s) => s.setComposerError);
  const setNewAgent = useWorkspace((s) => s.setNewThreadAgent);
  const openNew = useWorkspace((s) => s.setNewThreadOpen);
  const clearDraft = useWorkspace((s) => s.clearDraft);
  const editSchedule = useWorkspace((s) => s.editSchedule);
  const store = useWorkspaceStore();
  const host = useHost();
  const draft = useDraft(thread.id);
  const onPaste = usePasteFiles(thread.id);
  const [text, setText] = useState("");
  const [lastEsc, setLastEsc] = useState(0);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const running = live?.status === "running";

  const submitText = async (raw: string, mode: "prompt" | "steer" | "follow-up", force = false) => {
    const msg = raw.trim();
    const attached = draft.ids.length + draft.folders.length;
    if ((!msg && attached === 0) || draft.uploading) return;
    const name = raw.startsWith("/") && attached === 0 ? slashName(msg) : null;
    const commands = name ? await slash.ensure() : [];
    const run = name ? commandRun(msg, commands) : "chat";
    // Codex runs no slash commands of its own in chat, so an unknown one would arrive as text.
    if (name && thread.harness === "codex" && !commands.some((c) => c.name === name)) {
      setComposerError(`Codex has no /${name}. Type / to see its commands, or start with a space to send the text as it is.`);
      return;
    }
    // Commands that need the harness's own UI (`/login`, `/settings`, …) open in a card here.
    if (run === "terminal") {
      if (await runCommand(msg, { threadId: thread.id })) setText("");
      return;
    }
    if (run === "native") {
      const name = slashName(msg);
      if (name === "copy") {
        const st = store.getState();
        const items = [...st.transcript.items, ...(st.liveView.threadId === thread.id ? st.liveView.tail : [])];
        const last = items.reverse().find((i) => i.kind === "assistant");
        if (last && last.kind === "assistant") void host.clipboard?.write(last.text);
        setText("");
      } else if (name === "new") {
        setNewAgent(thread.agentId);
        openNew(true);
        setText("");
      } else if (await runNative(thread.id, msg)) setText("");
      return;
    }
    // Claude only reads inside the folders it was given, so a dropped folder is given first.
    if (thread.harness === "claude") {
      for (const dir of draft.folders) if (!(await runNative(thread.id, `/add-dir ${dir}`))) return;
    }
    const full = draft.folders.length ? [msg, folderLines(draft.folders)].filter(Boolean).join("\n\n") : msg;
    if (await send(full, mode, force, draft.ids)) {
      setText("");
      clearDraft(thread.id);
    }
  };
  const slash = useSlashMenu(thread.harness, thread.agentId, text, setText, (t) => void submitText(t, running ? "steer" : "prompt"));

  useEffect(() => setText(""), [thread.id]);

  // Grow with the text, up to the CSS max-height.
  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [text]);

  if (!canChat(thread)) {
    return (
      <div className="emp-composer">
        <div className="emp-composer__notice">
          {orionOnly(thread)
            ? "This chat lives only in Orion Terminal, so there's no session to continue."
            : `Chat with ${thread.harness} threads arrives in a later phase. Open it in the terminal instead.`}
        </div>
      </div>
    );
  }
  if (live?.owner === "terminal") {
    return (
      <div className="emp-composer">
        <CommandCard scope={thread.id} />
        <div className="emp-composer__notice">
          <span>This session is open in the terminal.</span>
          <button type="button" className="emp-link" onClick={() => void release()}>
            Return to chat
          </button>
        </div>
      </div>
    );
  }

  const submit = (mode: "prompt" | "steer" | "follow-up", force = false) => submitText(text, mode, force);

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (slash.onKeyDown(e)) return;
    if (e.key === "Escape") {
      const now = Date.now();
      if (running && now - lastEsc < 600) {
        e.preventDefault();
        void interrupt();
        setLastEsc(0);
      } else setLastEsc(now);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      if (e.metaKey || e.ctrlKey) void submit("follow-up");
      else void submit(running ? "steer" : "prompt");
    }
  };

  const busyElsewhere = composer.error?.includes("retry with force");
  return (
    <div className="emp-composer">
      <ElsewhereBar thread={thread} />
      <GoalBar thread={thread} />
      <ScheduleBar thread={thread} />
      <CommandCard scope={thread.id} />
      <QuestionCards threadId={thread.id} />
      {live?.status === "failed" && live.error && <div className="emp-alert">{live.error}</div>}
      {composer.error && (
        <div className="emp-alert" role="alert">
          <span>{composer.error}</span>
          {busyElsewhere && (
            <span className="emp-alert__actions">
              <button type="button" className="emp-link" onClick={() => void submit("prompt", true)}>
                Send anyway
              </button>
              <button type="button" className="emp-link" onClick={() => void takeover(true)}>
                Open in terminal anyway
              </button>
            </span>
          )}
        </div>
      )}
      <div className="emp-composer__card" data-running={running || undefined}>
        {live && live.queued.length > 0 && (
          <div className="emp-queue">
            <span className="emp-queue__label">Queued</span>
            {live.queued.map((q, i) => (
              <span key={i} className="emp-queue__item emp-truncate" title={q}>
                {q}
              </span>
            ))}
          </div>
        )}
        {slash.menu}
        <DraftFiles draft={thread.id} agentId={thread.agentId} grants={thread.harness === "claude"} />
        <div className="emp-composer__row">
          <AttachButton draft={thread.id} disabled={composer.sending} />
          <textarea
            ref={inputRef}
            className="emp-composer__input"
            data-orion-ui="input"
            rows={1}
            onPaste={onPaste}
            aria-label={`Message ${thread.harness}`}
            title={running ? "↵ steer · ⌘↵ queue · esc esc stop" : "↵ send · shift ↵ new line"}
            placeholder={running ? "Steer the running turn…" : "Ask anything, / for commands"}
            value={text}
            disabled={composer.sending}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={onKeyDown}
          />
          <div className="emp-composer__controls">
            <ThreadModelPicker thread={thread} />
            <button
              type="button"
              className="emp-icon-button"
              aria-label="Schedule a message in this thread"
              title="Schedule this message: send it here later, or on a timetable"
              onClick={() => editSchedule(newScheduleDraft(null, { threadId: thread.id, prompt: text.trim(), title: "" }))}
            >
              <IconClock />
            </button>
            {running ? (
              <button type="button" className="emp-send" data-stop aria-label="Stop" title="Stop (esc esc)" onClick={() => void interrupt()}>
                <IconStop />
              </button>
            ) : (
              <button
                type="button"
                className="emp-send"
                aria-label="Send"
                title="Send (↵)"
                disabled={(!text.trim() && draft.ids.length + draft.folders.length === 0) || draft.uploading || composer.sending}
                onClick={() => void submit("prompt")}
              >
                <IconSend />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/** One-line preview: whitespace collapsed, so pretty-printed JSON still says something. */
function previewText(text: string, max = 140): string {
  const flat = text.slice(0, max * 4).replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}

const TranscriptEntry = memo(function TranscriptEntry({ item }: { item: TranscriptItem }) {
  switch (item.kind) {
    case "user": {
      const { text, files } = splitAttachments(stripContext(item.text));
      return (
        <div className="emp-msg" data-kind="user">
          <div className="emp-bubble">
            <ScheduledTag text={item.text} />
            {text && <ClampedText className="emp-msg__text" text={text} limit={LONG_TEXT_LIMIT} />}
            <MessageFiles files={files} />
          </div>
        </div>
      );
    }
    case "assistant":
      return (
        <div className="emp-msg" data-kind="assistant" title={item.model ?? undefined}>
          <ReplyText text={item.text} limit={LONG_TEXT_LIMIT} />
        </div>
      );
    case "thinking":
      return <Collapsible kind="thinking" label="Thought" preview={previewText(stripMarkdown(item.text))} body={item.text} markdown />;
    case "toolCall":
      return <Collapsible kind="toolCall" label={item.name} preview={previewText(item.input)} body={item.input} mono />;
    case "toolResult":
      return (
        <Collapsible
          kind="toolResult"
          label={item.isError ? "Error" : "Output"}
          preview={previewText(item.output)}
          body={item.output}
          mono
          error={item.isError}
        />
      );
    case "system":
      return (
        <div className="emp-msg" data-kind="system">
          <span>{item.text}</span>
        </div>
      );
  }
});

interface CollapsibleProps {
  kind: string;
  label: string;
  preview: string;
  body: string;
  mono?: boolean;
  markdown?: boolean;
  error?: boolean;
}

/** Collapsed by default; the body is only rendered once expanded. */
function Collapsible({ kind, label, preview, body, mono, markdown, error }: CollapsibleProps) {
  const [open, setOpen] = useState(false);
  return (
    <div className="emp-msg emp-collapsible" data-kind={kind} data-error={error || undefined}>
      <button type="button" className="emp-collapsible__toggle" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <span className="emp-collapsible__chevron">
          <IconChevron open={open} />
        </span>
        <span className="emp-collapsible__label">{label}</span>
        {!open && preview && (
          <span className="emp-collapsible__preview emp-truncate" data-orion-ui="muted">
            {preview}
          </span>
        )}
      </button>
      {open &&
        (markdown ? (
          <Markdown className="emp-collapsible__body" text={body} limit={LONG_TEXT_LIMIT} />
        ) : (
          <ClampedText className={mono ? "emp-pre emp-mono" : "emp-msg__text"} text={body} limit={LONG_TEXT_LIMIT} />
        ))}
    </div>
  );
}
