// Composer add-ons shared by the thread and New thread composers: attached files (drop, paste,
// paperclip) and the slash command menu.

import { useCallback, useEffect, useRef, useState, type ClipboardEvent, type DragEvent, type KeyboardEvent } from "react";
import type { Harness, SlashCommand } from "@emp/protocol";
import { carriesFiles, formatSize, prepareFile, splitTransfer, transferFiles, type DroppedFolder } from "../lib/attach";
import { scheduleDone, scheduledBy, shortPath } from "../lib/format";
import { useWorkspace, useWorkspaceStore } from "../store/context";
import type { DraftFile, DraftFolder } from "../store/workspace";
import { IconClock, IconClose, IconFile, IconFolder, IconPaperclip } from "./icons";
import { Markdown } from "./Markdown";

const NO_FILES: DraftFile[] = [];

/** Adds files to a composer's draft (read, shrunk if huge, uploaded). */
export function useAddFiles(draft: string) {
  const attach = useWorkspace((s) => s.attachFiles);
  return useCallback(
    (files: File[]) => {
      if (files.length) void Promise.all(files.map(prepareFile)).then((prepared) => attach(draft, prepared));
    },
    [attach, draft],
  );
}

/** Adds dropped folders: on New thread the first becomes the working folder, elsewhere context. */
export function useAddFolders(draft: string, use: DraftFolder["use"]) {
  const attach = useWorkspace((s) => s.attachFolders);
  return useCallback(
    (folders: Promise<DroppedFolder[]>) => {
      void folders.then((list) => {
        if (list.length) void attach(draft, list, use);
      });
    },
    [attach, draft, use],
  );
}

/** Above a message a schedule sent: which one. */
export function ScheduledTag({ text }: { text: string }) {
  const by = scheduledBy(text);
  if (!by) return null;
  return (
    <span className="emp-msg__sched">
      <IconClock />
      Scheduled · {by}
    </span>
  );
}

/** An agent's reply, without the line that tells EMP a schedule's stop condition holds; a tag says so instead. */
export function ReplyText({ text, limit }: { text: string; limit?: number }) {
  const { text: shown, done } = scheduleDone(text);
  return (
    <>
      <Markdown className="emp-prose" text={shown} limit={limit} />
      {done && (
        <span className="emp-msg__sched">
          <IconClock />
          Condition met · the schedule finished
        </span>
      )}
    </>
  );
}

/** The files and folders a sent message carries, under its text. */
export function MessageFiles({ files }: { files: string[] }) {
  if (!files.length) return null;
  return (
    <div className="emp-msg__files">
      {files.map((path) => (
        <span key={path} className="emp-msg__file" title={path}>
          {path.endsWith("/") ? <IconFolder /> : <IconFile />}
          <span className="emp-truncate">{path.replace(/\/$/, "").split("/").pop()}</span>
        </span>
      ))}
    </div>
  );
}

/** Drop target props for a whole view; `dropping` while files hover over it. */
export function useFileDrop(draft: string, folderUse: DraftFolder["use"] = "context") {
  const add = useAddFiles(draft);
  const addFolders = useAddFolders(draft, folderUse);
  const [dropping, setDropping] = useState(false);
  const depth = useRef(0);
  const props = {
    onDragEnter(e: DragEvent) {
      if (!carriesFiles(e.dataTransfer)) return;
      e.preventDefault();
      depth.current += 1;
      setDropping(true);
    },
    onDragOver(e: DragEvent) {
      if (!carriesFiles(e.dataTransfer)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
    },
    onDragLeave(e: DragEvent) {
      if (!carriesFiles(e.dataTransfer)) return;
      depth.current = Math.max(0, depth.current - 1);
      if (depth.current === 0) setDropping(false);
    },
    onDrop(e: DragEvent) {
      if (!carriesFiles(e.dataTransfer)) return;
      e.preventDefault();
      depth.current = 0;
      setDropping(false);
      const { files, folders } = splitTransfer(e.dataTransfer);
      add(files);
      addFolders(folders);
    },
  };
  return { dropping, props };
}

/** Paste handler for the textarea: pasted files (screenshots) attach; text pastes as usual. */
export function usePasteFiles(draft: string) {
  const add = useAddFiles(draft);
  return (e: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = transferFiles(e.clipboardData);
    if (files.length) {
      e.preventDefault();
      add(files);
    }
  };
}

export function useDraft(draft: string) {
  const files = useWorkspace((s) => s.drafts[draft] ?? NO_FILES);
  const ready = files.filter((f) => f.status === "ready");
  return {
    files,
    ids: ready.filter((f) => f.id).map((f) => f.id!),
    /** Folders handed over as context (their `[Attached: …/]` lines go in the text). */
    folders: ready.filter((f) => f.folder?.use === "context" && f.folder.path).map((f) => f.folder!.path!),
    /** The folder to work in (New thread). */
    workdir: ready.find((f) => f.folder?.use === "workdir" && f.folder.path)?.folder?.path ?? null,
    uploading: files.some((f) => f.status === "uploading"),
  };
}

export function DropHint({ dropping }: { dropping: boolean }) {
  if (!dropping) return null;
  return (
    <div className="emp-drop" aria-hidden="true">
      <span>Drop files or a folder</span>
    </div>
  );
}

export function AttachButton({ draft, disabled }: { draft: string; disabled?: boolean }) {
  const add = useAddFiles(draft);
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <button
        type="button"
        className="emp-icon-button emp-composer__attach"
        aria-label="Attach files"
        title="Attach files (or drop / paste them)"
        disabled={disabled}
        onClick={() => input.current?.click()}
      >
        <IconPaperclip />
      </button>
      <input
        ref={input}
        type="file"
        multiple
        hidden
        tabIndex={-1}
        onChange={(e) => {
          add(Array.from(e.target.files ?? []));
          e.target.value = "";
        }}
      />
    </>
  );
}

/**
 * A folder chip. On New thread it's the working folder or context (switchable); in a thread
 * it's context, with a way to start a new thread in it instead.
 */
function FolderChip({ draft, f, agentId, grants }: { draft: string; f: DraftFile & { folder: DraftFolder }; agentId?: string | null; grants?: boolean }) {
  const remove = useWorkspace((s) => s.removeDraftFile);
  const setFolder = useWorkspace((s) => s.setDraftFolder);
  const newThreadIn = useWorkspace((s) => s.newThreadInFolder);
  const isNew = agentId === undefined;
  const work = f.folder.use === "workdir";
  return (
    <div className="emp-attachment" data-status={f.status} data-folder={work ? "workdir" : "context"} title={f.error ?? f.folder.path ?? f.name}>
      <span className="emp-attachment__icon">
        <IconFolder />
      </span>
      <span className="emp-attachment__text">
        <span className="emp-attachment__head">
          <span className="emp-attachment__name emp-truncate">{f.name}</span>
          {f.status === "ready" && <span className="emp-attachment__role">{work ? "Working folder" : grants ? "Context · Claude can use it" : "Context"}</span>}
        </span>
        {f.status === "uploading" ? (
          <span className="emp-attachment__meta">Finding it…</span>
        ) : f.status === "error" ? (
          <span className="emp-attachment__meta">{f.error}</span>
        ) : f.folder.options.length > 1 ? (
          <select
            className="emp-attachment__pick emp-mono"
            aria-label={`Which ${f.name}`}
            value={f.folder.path ?? ""}
            onChange={(e) => setFolder(draft, f.key, { path: e.target.value })}
          >
            {f.folder.options.map((p) => (
              <option key={p} value={p}>
                {shortPath(p, 56)}
              </option>
            ))}
          </select>
        ) : (
          <span className="emp-attachment__meta emp-mono emp-truncate">{shortPath(f.folder.path, 56)}</span>
        )}
        {f.status === "ready" && (
          <button
            type="button"
            className="emp-attachment__action"
            onClick={() => (isNew ? setFolder(draft, f.key, { use: work ? "context" : "workdir" }) : newThreadIn(draft, f.key, agentId))}
          >
            {isNew ? (work ? "Use as context instead" : "Work in this folder") : "New thread in this folder"}
          </button>
        )}
      </span>
      <button type="button" className="emp-attachment__remove" aria-label={`Remove ${f.name}`} onClick={() => remove(draft, f.key)}>
        <IconClose />
      </button>
    </div>
  );
}

/**
 * Chips for the draft's files and folders: thumbnail or icon, name, size, remove. `agentId` is
 * the thread's agent (in a thread); leave it out on New thread.
 */
/** `grants`: a context folder also becomes one the agent may use (Claude chats). */
export function DraftFiles({ draft, agentId, grants }: { draft: string; agentId?: string | null; grants?: boolean }) {
  const { files } = useDraft(draft);
  const remove = useWorkspace((s) => s.removeDraftFile);
  if (!files.length) return null;
  return (
    <div className="emp-attachments" aria-label="Attached files">
      {files.map((f) =>
        f.folder ? (
          <FolderChip key={f.key} draft={draft} f={f as DraftFile & { folder: DraftFolder }} agentId={agentId} grants={grants} />
        ) : (
          <div key={f.key} className="emp-attachment" data-status={f.status} title={f.error ?? f.name}>
            {f.preview ? <img className="emp-attachment__thumb" src={f.preview} alt="" /> : <span className="emp-attachment__icon"><IconFile /></span>}
            <span className="emp-attachment__text">
              <span className="emp-attachment__name emp-truncate">{f.name}</span>
              <span className="emp-attachment__meta emp-truncate">
                {f.status === "uploading" ? "Attaching…" : f.status === "error" ? f.error : formatSize(f.size)}
              </span>
            </span>
            <button type="button" className="emp-attachment__remove" aria-label={`Remove ${f.name}`} onClick={() => remove(draft, f.key)}>
              <IconClose />
            </button>
          </div>
        ),
      )}
    </div>
  );
}

// ---- slash commands ----------------------------------------------------------------------

/** `/name args…` → `name`; `null` for anything else (including paths like `/Users/x`). */
export function slashName(text: string): string | null {
  const m = /^\/([A-Za-z0-9][\w:.-]*)(?:\s|$)/.exec(text.trim());
  return m ? m[1]! : null;
}

/**
 * Where a message runs: `terminal` for commands that need the harness's own UI (an in-chat
 * terminal card); `native` for ones EMP runs itself; `chat` for everything else (the harness
 * expands its templates, skills and custom commands itself).
 */
export function commandRun(text: string, commands: readonly SlashCommand[]): SlashCommand["run"] {
  const name = slashName(text);
  return (name && commands.find((c) => c.name === name)?.run) || "chat";
}

const MENU_MAX = 50;

/**
 * The `/` menu: open while the message is a lone `/word`. ↑↓ move, Tab completes, ↵ runs the
 * highlighted command, Esc closes.
 */
export function useSlashMenu(harness: Harness | null, agentId: string | null, text: string, setText: (t: string) => void, run: (text: string) => void) {
  const loadCommands = useWorkspace((s) => s.loadCommands);
  const api = useWorkspaceStore();
  const key = harness ? `${harness}:${agentId ?? ""}` : "";
  const commands = useWorkspace((s) => (key ? s.commands[key]?.data : null)) ?? null;
  const [index, setIndex] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);

  // Load on mount, so a command typed fast still routes right.
  useEffect(() => {
    if (harness) void loadCommands(harness, agentId);
  }, [harness, agentId, loadCommands]);

  const query = /^\/([\w:.-]*)$/.exec(text)?.[1];
  const open = query !== undefined && dismissed !== text;
  const q = (query ?? "").toLowerCase();
  const matches = open
    ? (commands ?? [])
        .filter((c) => c.name.toLowerCase().split(/[-:_.]/).some((part) => part.startsWith(q)) || c.name.toLowerCase().startsWith(q))
        .sort((a, b) => Number(!a.name.toLowerCase().startsWith(q)) - Number(!b.name.toLowerCase().startsWith(q)))
        .slice(0, MENU_MAX)
    : [];
  useEffect(() => setIndex(0), [q]);

  /** The command list, waiting for it if it's still loading. */
  const ensure = async (): Promise<readonly SlashCommand[]> => {
    if (!harness) return [];
    await loadCommands(harness, agentId);
    return api.getState().commands[key]?.data ?? [];
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>): boolean => {
    if (!open || matches.length === 0) return false;
    const pick = matches[Math.min(index, matches.length - 1)]!;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setIndex((i) => (e.key === "ArrowDown" ? (i + 1) % matches.length : (i - 1 + matches.length) % matches.length));
      return true;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      setText(`/${pick.name} `);
      return true;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      run(`/${pick.name}`);
      return true;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      setDismissed(text);
      return true;
    }
    return false;
  };

  const menu =
    open && (matches.length > 0 || commands === null) ? (
      <div className="emp-slash" role="listbox" aria-label="Commands">
        {commands === null && <div className="emp-slash__note">Loading commands…</div>}
        {matches.map((c, i) => (
          <button
            key={c.name}
            type="button"
            role="option"
            aria-selected={i === index}
            className="emp-slash__item"
            onMouseEnter={() => setIndex(i)}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => run(`/${c.name}`)}
          >
            <span className="emp-slash__name emp-mono">/{c.name}</span>
            {c.description && <span className="emp-slash__desc emp-truncate">{c.description}</span>}
            {c.run === "terminal" && <span className="emp-slash__tag">terminal</span>}
          </button>
        ))}
      </div>
    ) : null;

  return { menu, onKeyDown, ensure };
}
