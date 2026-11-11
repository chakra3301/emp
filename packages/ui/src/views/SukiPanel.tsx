// Suki, EMP's coordinator (Amp's Puck): a chat that opens over whatever page is showing and
// knows what that page is. It starts, finds and steers the owner's agents through the daemon's
// `emp` tools; their calls show here as short activity lines instead of raw JSON.

import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import type { TranscriptItem } from "@emp/protocol";
import { CommandCard, QuestionCards } from "../components/ChatCards";
import {
  AttachButton,
  DraftFiles,
  DropHint,
  MessageFiles,
  ReplyText,
  ScheduledTag,
  useDraft,
  useFileDrop,
  usePasteFiles,
} from "../components/ComposerParts";
import { Markdown } from "../components/Markdown";
import { Picker, type PickerOption } from "../components/Picker";
import { isCopilotModel } from "../lib/models";
import {
  IconClose,
  IconExpand,
  IconPlus,
  IconSend,
  IconStop,
  IconSuki,
} from "../components/icons";
import { folderLines, splitAttachments } from "../lib/attach";
import { cleanTitle, scheduleDone, stripContext } from "../lib/format";
import { formatShort } from "../lib/time";
import { useWorkspace } from "../store/context";
import { coordinatorAgent, liveFor, SUKI_DRAFT } from "../store/workspace";

const INACTIVE_MS = 72 * 3_600_000;
const NEW_CHAT = "__new";

const SUGGESTIONS = [
  "What's running right now?",
  "Find the thread where I last worked on the login page",
  "Which dev servers can I stop?",
];

export function SukiPanel() {
  const suki = useWorkspace((s) => s.suki);
  const chats = useWorkspace((s) => s.sukiChats);
  const setOpen = useWorkspace((s) => s.setSukiOpen);
  const select = useWorkspace((s) => s.selectSukiChat);
  const openThread = useWorkspace((s) => s.openThread);
  const now = useWorkspace((s) => s.now);
  const drop = useFileDrop(SUKI_DRAFT);
  const current = chats.data?.find((t) => t.id === suki.threadId) ?? null;

  const options = useMemo<PickerOption[]>(() => {
    const list = chats.data ?? [];
    return [
      { id: NEW_CHAT, label: "New conversation" },
      ...list.map((t) => ({
        id: t.id,
        label: cleanTitle(t.title),
        detail: formatShort(t.updatedAt, now),
        group:
          now - t.updatedAt > INACTIVE_MS
            ? "Inactive last 72h"
            : "Conversations",
      })),
    ];
  }, [chats.data, now]);

  return (
    <aside
      className="emp-suki"
      aria-label="Suki"
      {...drop.props}
      onKeyDown={(e) =>
        e.key === "Escape" && !e.defaultPrevented && setOpen(false)
      }
    >
      <DropHint dropping={drop.dropping} />
      <header className="emp-suki__head">
        <span className="emp-suki__mark" aria-hidden="true">
          <IconSuki />
        </span>
        <div className="emp-suki__title">
          <Picker
            label={
              suki.threadId
                ? cleanTitle(current?.title ?? "Conversation")
                : "Suki"
            }
            title="Conversations"
            value={suki.threadId ?? NEW_CHAT}
            options={options}
            onChange={(id) => select(id === NEW_CHAT ? null : id)}
          />
        </div>
        <button
          type="button"
          className="emp-icon-button"
          title="New conversation"
          aria-label="New conversation"
          onClick={() => select(null)}
        >
          <IconPlus />
        </button>
        {suki.threadId && (
          <button
            type="button"
            className="emp-icon-button"
            title="Open as a tab"
            aria-label="Open as a tab"
            onClick={() => void openThread(suki.threadId!)}
          >
            <IconExpand />
          </button>
        )}
        <button
          type="button"
          className="emp-icon-button"
          title="Close (⌘/)"
          aria-label="Close Suki"
          onClick={() => setOpen(false)}
        >
          <IconClose />
        </button>
      </header>
      <SukiTranscript />
      <SukiComposer />
    </aside>
  );
}

function SukiTranscript() {
  const suki = useWorkspace((s) => s.suki);
  const running = useWorkspace(
    (s) => liveFor(s, s.suki.threadId)?.status === "running",
  );
  const send = useWorkspace((s) => s.sendToSuki);
  const scrollRef = useRef<HTMLDivElement>(null);
  const rows = useMemo(
    () => pairTools([...suki.items, ...suki.tail]),
    [suki.items, suki.tail],
  );
  const working = running || (suki.sending && !!suki.pending);

  // Follow the conversation (it's short-lived and read from the bottom).
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [rows.length, suki.partial?.text, suki.pending, working]);

  const empty = !suki.threadId && !suki.pending;
  return (
    <div ref={scrollRef} className="emp-suki__body" aria-live="polite">
      {empty && (
        <div className="emp-suki__intro">
          <span className="emp-suki__face" aria-hidden="true">
            <IconSuki />
          </span>
          <p>
            Ask me to start an agent, find a thread, check on what's running, or
            tidy up.
          </p>
          <div className="emp-suki__suggestions">
            {SUGGESTIONS.map((q) => (
              <button
                key={q}
                type="button"
                className="emp-suki__suggestion"
                onClick={() => void send(q)}
              >
                {q}
              </button>
            ))}
          </div>
        </div>
      )}
      {suki.status === "loading" && (
        <div className="emp-suki__note">Loading…</div>
      )}
      {rows.map((r, i) => (
        <SukiRow key={i} row={r} />
      ))}
      {suki.pending && <UserBubble text={suki.pending} raw={suki.pending} />}
      {suki.partial?.kind === "assistant" && (
        <div
          className="emp-suki__msg emp-msg"
          data-kind="assistant"
          data-streaming="true"
        >
          <Markdown className="emp-prose" text={scheduleDone(suki.partial.text).text} />
        </div>
      )}
      {working && !(suki.partial?.kind === "assistant") && (
        <div className="emp-working" role="status">
          <span className="emp-working__dots" aria-hidden="true">
            <span />
            <span />
            <span />
          </span>
          {suki.pending ? "Starting Suki" : "Working"}
        </div>
      )}
    </div>
  );
}

type Row =
  | { kind: "item"; item: TranscriptItem }
  | {
      kind: "tool";
      name: string;
      input: string;
      output: string | null;
      isError: boolean;
    };

/** Tool calls joined with their results; thinking and setup noise left out. */
function pairTools(items: TranscriptItem[]): Row[] {
  const rows: Row[] = [];
  const open = new Map<string, Extract<Row, { kind: "tool" }>>();
  let last: Extract<Row, { kind: "tool" }> | null = null;
  for (const item of items) {
    if (item.kind === "toolCall") {
      const row = {
        kind: "tool" as const,
        name: item.name,
        input: item.input,
        output: null,
        isError: false,
      };
      rows.push(row);
      if (item.callId) open.set(item.callId, row);
      last = row;
    } else if (item.kind === "toolResult") {
      const row = (item.callId && open.get(item.callId)) || last;
      if (row && row.output === null) {
        row.output = item.output;
        row.isError = item.isError;
      }
    } else if (item.kind === "thinking") {
      continue;
    } else if (
      item.kind === "system" &&
      /^(Model changed|Thinking level|\[)/.test(item.text)
    ) {
      continue;
    } else {
      rows.push({ kind: "item", item });
    }
  }
  return rows;
}

const SukiRow = memo(function SukiRow({ row }: { row: Row }) {
  if (row.kind === "tool")
    return (
      <ToolRow
        name={row.name}
        input={row.input}
        output={row.output}
        isError={row.isError}
      />
    );
  const item = row.item;
  switch (item.kind) {
    case "user":
      return <UserBubble text={stripContext(item.text)} raw={item.text} />;
    case "assistant":
      return (
        <div className="emp-suki__msg emp-msg" data-kind="assistant">
          <ReplyText text={item.text} />
        </div>
      );
    case "system":
      return <div className="emp-suki__note">{item.text}</div>;
    default:
      return null;
  }
});

function parse(
  json: string | null,
): Record<string, unknown> | unknown[] | null {
  if (!json) return null;
  try {
    const v: unknown = JSON.parse(json);
    return v && typeof v === "object" ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

const str = (v: unknown) => (typeof v === "string" ? v : "");

/** "Started Reviewer", "Waited for 2 threads", … with the threads involved as links. */
function ToolRow({
  name,
  input,
  output,
  isError,
}: {
  name: string;
  input: string;
  output: string | null;
  isError: boolean;
}) {
  const openThread = useWorkspace((s) => s.openThread);
  const tool = name.replace(/^mcp__emp__/, "");
  const args = (parse(input) ?? {}) as Record<string, unknown>;
  const result = parse(output);
  const done = output !== null;
  const links: { id: string; title: string; status?: string }[] = [];
  let label: string;
  switch (tool) {
    case "list_agents":
      label = "Looked up agents";
      break;
    case "list_projects":
      label = "Looked up projects";
      break;
    case "list_threads":
      label = args.query
        ? `Searched threads for "${str(args.query)}"`
        : args.active_only
          ? "Checked what's running"
          : "Looked through threads";
      break;
    case "read_thread":
      label = "Read a thread";
      break;
    case "start_thread":
      label = done
        ? `Started ${str(args.agent)}`
        : `Starting ${str(args.agent)}…`;
      if (result && !Array.isArray(result) && str(result.thread_id))
        links.push({ id: str(result.thread_id), title: str(result.title) });
      break;
    case "send_message":
      label = args.mode === "steer" ? "Steered a thread" : "Sent instructions";
      break;
    case "interrupt_thread":
      label = "Stopped a turn";
      break;
    case "wait_for_threads": {
      const n = Array.isArray(args.thread_ids) ? args.thread_ids.length : 0;
      label = done
        ? `Waited for ${n} thread${n === 1 ? "" : "s"}`
        : `Waiting for ${n} thread${n === 1 ? "" : "s"}…`;
      if (Array.isArray(result))
        for (const r of result as Record<string, unknown>[])
          links.push({
            id: str(r.thread_id),
            title: str(r.title),
            status: str(r.status),
          });
      break;
    }
    case "open_thread":
      label = "Opened a thread";
      break;
    case "list_dev_servers":
      label = "Checked dev servers";
      break;
    case "stop_dev_server":
      label = "Stopped a dev server";
      break;
    case "schedule_task": {
      const made = result && !Array.isArray(result) ? result : null;
      label = made
        ? `Scheduled "${str(made.title)}" · ${str(made.when)}`
        : done
          ? "Tried to schedule a task"
          : "Scheduling…";
      break;
    }
    case "update_schedule":
      label = args.paused === true ? "Paused a schedule" : args.paused === false ? "Resumed a schedule" : "Changed a schedule";
      break;
    case "list_schedules":
      label = "Checked the schedules";
      break;
    case "delete_schedule":
      label = "Deleted a schedule";
      break;
    case "run_schedule_now":
      label = "Ran a schedule now";
      break;
    default:
      label = tool;
  }
  return (
    <div
      className="emp-suki__tool"
      data-error={isError || undefined}
      data-done={done || undefined}
    >
      <span className="emp-suki__tool-dot" aria-hidden="true" />
      <span className="emp-suki__tool-label">
        {label}
        {isError && output && (
          <span className="emp-suki__tool-error">
            {" "}
            · {output.slice(0, 160)}
          </span>
        )}
      </span>
      {links.map(
        (l) =>
          l.id && (
            <button
              key={l.id}
              type="button"
              className="emp-suki__thread"
              title="Open this thread"
              onClick={() => void openThread(l.id)}
            >
              <span className="emp-truncate">
                {cleanTitle(l.title) || "Thread"}
              </span>
              {l.status && (
                <span className="emp-suki__thread-status">{l.status}</span>
              )}
            </button>
          ),
      )}
    </div>
  );
}

/** The owner's message, with what it carried (`[Attached: …]` lines) as chips. */
function UserBubble({ text: shown, raw }: { text: string; raw: string }) {
  const { text, files } = splitAttachments(shown);
  return (
    <div className="emp-suki__msg" data-kind="user">
      <div className="emp-bubble">
        <ScheduledTag text={raw} />
        {text}
        <MessageFiles files={files} />
      </div>
    </div>
  );
}

function SukiComposer() {
  const suki = useWorkspace((s) => s.suki);
  const agent = useWorkspace(coordinatorAgent);
  const running = useWorkspace(
    (s) => liveFor(s, s.suki.threadId)?.status === "running",
  );
  const send = useWorkspace((s) => s.sendToSuki);
  const interrupt = useWorkspace((s) => s.interruptSuki);
  const setModel = useWorkspace((s) => s.setSukiModel);
  const loadModels = useWorkspace((s) => s.loadModels);
  const models = useWorkspace((s) =>
    agent ? s.models[`${agent.harness}:`] : undefined,
  );
  // The open chat's model, as the daemon has it (it keeps a switch).
  const chatModel = useWorkspace(
    (s) => s.sukiChats.data?.find((t) => t.id === s.suki.threadId)?.model ?? null,
  );
  const runCommand = useWorkspace((s) => s.runCommand);
  const signingIn = useWorkspace((s) => !!s.commandCards.suki);
  const [text, setText] = useState("");
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const draft = useDraft(SUKI_DRAFT);
  const clearDraft = useWorkspace((s) => s.clearDraft);
  const onPaste = usePasteFiles(SUKI_DRAFT);
  const attached = draft.ids.length + draft.folders.length;

  useEffect(() => inputRef.current?.focus(), [suki.threadId]);
  useEffect(() => {
    if (agent && agent.harness !== "hermes")
      void loadModels(agent.harness, null);
  }, [agent, loadModels]);
  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [text]);

  const catalogue = models?.data ?? [];
  const shownModel = suki.model ?? chatModel;
  const defaultModel = chatModel ?? agent?.model;
  const defaultAllowed = !defaultModel || !isCopilotModel(defaultModel);
  const provider =
    agent?.harness === "pi" ? agent.model?.split("/")[0] : undefined;
  const signInNeeded =
    !suki.threadId &&
    !suki.model &&
    !!provider &&
    models?.status === "ready" &&
    catalogue.length > 0 &&
    !catalogue.some((m) => m.group === provider);
  const submit = async () => {
    const body = text;
    if ((!body.trim() && attached === 0) || draft.uploading) return;
    const full = draft.folders.length
      ? [body.trim(), folderLines(draft.folders)].filter(Boolean).join("\n\n")
      : body;
    setText("");
    if (await send(full, draft.ids)) clearDraft(SUKI_DRAFT);
    else setText(body);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void submit();
    }
  };

  return (
    <div className="emp-suki__foot">
      <CommandCard scope="suki" />
      {suki.threadId && <QuestionCards threadId={suki.threadId} />}
      {suki.error && (
        <div className="emp-suki__error" role="alert">
          {suki.error}
        </div>
      )}
      {signInNeeded && agent && !signingIn && (
        <div className="emp-composer__note" role="note">
          pi isn't signed in to <span className="emp-mono">{provider}</span>, so
          Suki can't use <span className="emp-mono">{agent.model}</span> yet.{" "}
          <button
            type="button"
            className="emp-link"
            onClick={() => void runCommand("/login", { agentId: agent.id }, "suki")}
          >
            Sign in
          </button>{" "}
          or pick another model below.
        </div>
      )}
      <div className="emp-composer emp-suki__composer">
        <div className="emp-composer__card" data-running={running || undefined}>
          <DraftFiles draft={SUKI_DRAFT} agentId={null} />
          <div className="emp-composer__row">
            <AttachButton draft={SUKI_DRAFT} disabled={suki.sending} />
            <textarea
              ref={inputRef}
              className="emp-composer__input"
              data-orion-ui="input"
              rows={1}
              aria-label="Message Suki"
              placeholder={
                running
                  ? "Add to what Suki is doing…"
                  : suki.threadId
                    ? "Reply to Suki"
                    : "Ask Suki…"
              }
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={onKeyDown}
              onPaste={onPaste}
            />
            <div className="emp-composer__controls">
              {agent && catalogue.length > 0 && (
                <Picker
                  label={
                    shownModel && !isCopilotModel(shownModel)
                      ? (catalogue.find((m) => m.id === shownModel)?.label ?? shownModel)
                      : defaultAllowed ? "Suki's model" : "Pick a model"
                  }
                  title={suki.threadId ? "Model for this conversation, from your next message on" : "Model for this conversation"}
                  value={suki.model ?? ""}
                  onChange={(id) => setModel(id || null)}
                  options={[
                    ...(defaultAllowed ? [{
                      id: "",
                      label: suki.threadId ? "Keep this chat's model" : "Suki's model",
                      detail: <span className="emp-mono">{defaultModel}</span>,
                    }] : []),
                    ...catalogue.map((m) => ({
                      id: m.id,
                      label: m.label,
                      group: m.group ?? undefined,
                    })),
                  ]}
                />
              )}
              {running && !text.trim() && attached === 0 ? (
                <button
                  type="button"
                  className="emp-send"
                  data-stop
                  aria-label="Stop"
                  title="Stop"
                  onClick={() => void interrupt()}
                >
                  <IconStop />
                </button>
              ) : (
                <button
                  type="button"
                  className="emp-send"
                  aria-label="Send"
                  title="Send"
                  disabled={
                    (!text.trim() && attached === 0) ||
                    draft.uploading ||
                    suki.sending
                  }
                  onClick={() => void submit()}
                >
                  <IconSend />
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
