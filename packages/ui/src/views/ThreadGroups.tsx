// The owner's thread groups in the sidebar: collapsible group rows (pinned ones in their own
// section at the top), a ⋯ menu on groups and threads, an inline "new group" field, and
// drag-and-drop of threads onto groups. Groups live in the daemon, so every window and phone
// shows the same ones; which groups are open is per device.

import { useEffect, useRef, useState, type DragEvent, type FocusEvent, type ReactNode } from "react";
import type { Thread, ThreadGroup } from "@emp/protocol";
import { IconChevron, IconFolder, IconPin } from "../components/icons";
import { cleanTitle } from "../lib/format";
import { useWorkspace } from "../store/context";

/** Drag payload type for a thread row. */
export const THREAD_DRAG = "application/x-emp-thread";

export function dragThread(e: DragEvent<HTMLElement>, threadId: string) {
  e.dataTransfer.effectAllowed = "move";
  e.dataTransfer.setData(THREAD_DRAG, threadId);
  e.dataTransfer.setData("text/plain", threadId);
}

/** Props for a drop target that accepts dragged threads. */
export function useThreadDrop(onDrop: (threadId: string) => void) {
  const [over, setOver] = useState(false);
  return {
    over,
    props: {
      onDragOver: (e: DragEvent<HTMLElement>) => {
        if (!e.dataTransfer.types.includes(THREAD_DRAG)) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        setOver(true);
      },
      onDragLeave: (e: DragEvent<HTMLElement>) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOver(false);
      },
      onDrop: (e: DragEvent<HTMLElement>) => {
        const id = e.dataTransfer.getData(THREAD_DRAG);
        setOver(false);
        if (!id) return;
        e.preventDefault();
        onDrop(id);
      },
    },
  };
}

/** A small popover menu anchored to a ⋯ button; closes on blur, Escape or a pick. */
export function RowMenu({ label, children, className }: { label: string; children: (close: () => void) => ReactNode; className?: string }) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const onBlur = (e: FocusEvent<HTMLDivElement>) => {
    if (!root.current?.contains(e.relatedTarget as Node | null)) setOpen(false);
  };
  return (
    <div
      ref={root}
      className={`emp-rowmenu${className ? ` ${className}` : ""}`}
      data-open={open || undefined}
      onBlur={onBlur}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          setOpen(false);
        }
      }}
    >
      <button type="button" className="emp-icon-button emp-rowmenu__button" aria-label={label} aria-expanded={open} title={label} onClick={() => setOpen((v) => !v)}>
        <span aria-hidden="true">⋯</span>
      </button>
      {open && (
        <div className="emp-rowmenu__panel" role="menu">
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}

function MenuItem({ children, onPick, danger }: { children: ReactNode; onPick(): void; danger?: boolean }) {
  return (
    <button type="button" role="menuitem" className="emp-rowmenu__item" data-danger={danger || undefined} onClick={onPick}>
      {children}
    </button>
  );
}

/** Text field that commits on Enter (or blur with text) and cancels on Escape / empty blur. */
function NameField({ initial = "", placeholder, onCommit, onCancel }: { initial?: string; placeholder: string; onCommit(name: string): void; onCancel(): void }) {
  const [name, setName] = useState(initial);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);
  const done = () => (name.trim() ? onCommit(name.trim()) : onCancel());
  return (
    <input
      ref={input}
      className="emp-input emp-sb-namefield"
      data-orion-ui="input"
      aria-label={placeholder}
      placeholder={placeholder}
      maxLength={60}
      value={name}
      onChange={(e) => setName(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") done();
        if (e.key === "Escape") onCancel();
      }}
      onBlur={done}
    />
  );
}

/** Inline "new group" row shown at the top of the Threads section. */
export function NewGroupField({ onDone }: { onDone(): void }) {
  const create = useWorkspace((s) => s.createGroup);
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="emp-sb-newgroup">
      <IconFolder />
      <NameField
        placeholder="Group name"
        onCancel={onDone}
        onCommit={async (name) => {
          const err = await create(name);
          if (err) setError(err);
          else onDone();
        }}
      />
      {error && <div className="emp-sb-note emp-sb-note--error">{error}</div>}
    </div>
  );
}

/** The ⋯ menu on a thread row: move to a group, new group with it, or take it out. */
export function ThreadMenu({ threadId, groupId }: { threadId: string; groupId: string | null }) {
  const groups = useWorkspace((s) => s.groups.data) ?? [];
  const add = useWorkspace((s) => s.addToGroup);
  const remove = useWorkspace((s) => s.removeFromGroup);
  const create = useWorkspace((s) => s.createGroup);
  const [naming, setNaming] = useState(false);
  const others = groups.filter((g) => g.id !== groupId);

  return (
    <RowMenu label="Thread options" className="emp-sb-row__menu">
      {(close) => (
        <>
          <div className="emp-rowmenu__heading">Move to group</div>
          {others.map((g) => (
            <MenuItem
              key={g.id}
              onPick={() => {
                close();
                void add(g.id, [threadId]);
              }}
            >
              {g.pinned ? <IconPin /> : <IconFolder />}
              <span className="emp-truncate">{g.name}</span>
            </MenuItem>
          ))}
          {naming ? (
            <div className="emp-rowmenu__field">
              <NameField
                placeholder="New group name"
                onCancel={() => setNaming(false)}
                onCommit={(name) => {
                  setNaming(false);
                  close();
                  void create(name, { threadIds: [threadId] });
                }}
              />
            </div>
          ) : (
            <MenuItem onPick={() => setNaming(true)}>
              <span className="emp-rowmenu__plus">+</span>
              New group…
            </MenuItem>
          )}
          {groupId && (
            <>
              <div className="emp-rowmenu__sep" role="separator" />
              <MenuItem
                onPick={() => {
                  close();
                  void remove(threadId);
                }}
              >
                Remove from group
              </MenuItem>
            </>
          )}
        </>
      )}
    </RowMenu>
  );
}

/** One collapsible group: header row (click to open), then its threads when open. */
export function GroupRow({ group, renderThread }: { group: ThreadGroup; renderThread(thread: Thread, groupId: string): ReactNode }) {
  const open = useWorkspace((s) => s.settings.expandedGroups.includes(group.id));
  const toggle = useWorkspace((s) => s.toggleGroup);
  const add = useWorkspace((s) => s.addToGroup);
  const rename = useWorkspace((s) => s.renameGroup);
  const pin = useWorkspace((s) => s.setGroupPinned);
  const remove = useWorkspace((s) => s.deleteGroup);
  const live = useWorkspace((s) => s.live);
  const [renaming, setRenaming] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const drop = useThreadDrop((threadId) => void add(group.id, [threadId]));
  const running = group.threads.filter((t) => live[t.id]?.status === "running").length;

  return (
    <div className="emp-sb-groupwrap" role="presentation">
      <div
        className="emp-sb-row emp-sb-grouprow"
        role="button"
        tabIndex={0}
        aria-expanded={open}
        data-drop={drop.over || undefined}
        title={`${group.name} · ${group.threads.length} thread${group.threads.length === 1 ? "" : "s"}`}
        onClick={() => !renaming && toggle(group.id)}
        onKeyDown={(e) => {
          if (e.target !== e.currentTarget) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            toggle(group.id);
          }
        }}
        {...drop.props}
      >
        <span className="emp-sb-grouprow__chevron">
          <IconChevron open={open} />
        </span>
        {renaming ? (
          <NameField
            initial={group.name}
            placeholder="Group name"
            onCancel={() => setRenaming(false)}
            onCommit={(name) => {
              setRenaming(false);
              if (name !== group.name) void rename(group.id, name);
            }}
          />
        ) : (
          <span className="emp-sb-row__title emp-truncate">{group.name}</span>
        )}
        {running > 0 && <span className="emp-sb-bullet" data-status="running" title={`${running} running`} />}
        <span className="emp-sb-grouprow__count">{group.threads.length}</span>
        <RowMenu label="Group options" className="emp-sb-row__menu">
          {(close) => (
            <>
              <MenuItem
                onPick={() => {
                  close();
                  void pin(group.id, !group.pinned);
                }}
              >
                <IconPin />
                {group.pinned ? "Unpin" : "Pin to top"}
              </MenuItem>
              <MenuItem
                onPick={() => {
                  close();
                  setRenaming(true);
                }}
              >
                Rename
              </MenuItem>
              <div className="emp-rowmenu__sep" role="separator" />
              <MenuItem
                danger
                onPick={() => {
                  if (!confirmDelete) return setConfirmDelete(true);
                  close();
                  setConfirmDelete(false);
                  void remove(group.id);
                }}
              >
                {confirmDelete ? "Click again to delete" : "Delete group"}
              </MenuItem>
              {confirmDelete && <div className="emp-rowmenu__hint">Its threads go back to the list.</div>}
            </>
          )}
        </RowMenu>
      </div>
      {open && (
        <div className="emp-sb-groupkids" role="group" aria-label={group.name}>
          {group.threads.length === 0 ? (
            <div className="emp-sb-note">Empty. Drag threads here, or use a thread's ⋯ → Move to group.</div>
          ) : (
            group.threads.map((t) => renderThread(t, group.id))
          )}
        </div>
      )}
    </div>
  );
}

/** Title for a thread that may only be known through a group (not in the main list). */
export function groupThreadTitle(groups: ThreadGroup[] | null, threadId: string): string | null {
  for (const g of groups ?? []) {
    const t = g.threads.find((x) => x.id === threadId);
    if (t) return cleanTitle(t.title);
  }
  return null;
}
