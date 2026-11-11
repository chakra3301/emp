// Layout edit mode, copied from Hermes desktop: every zone gets a dashed accent veil with a
// grip pill (drag the sidebar or the thread zone across to swap sides), and a floating,
// draggable "Layouts" card offers the interface mode, templates and saved arrangements.

import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { IconClose, IconGrip, IconSave } from "../components/icons";
import { useWorkspace } from "../store/context";
import { INTERFACE_MODES, LAYOUT_TEMPLATES, sameArrangement, visible, type Arrangement } from "../store/settings";

type Zone = "sidebar" | "main" | "status";

const ZONE_LABELS: Record<Zone, string> = { sidebar: "Sidebar", main: "Threads", status: "Status bar" };

export function LayoutEditor() {
  const settings = useWorkspace((s) => s.settings);
  const update = useWorkspace((s) => s.updateSettings);
  const [drop, setDrop] = useState<"left" | "right" | null>(null);
  const [dragging, setDragging] = useState<Zone | null>(null);

  /** Drag a zone across the window; on release the sidebar lands on the side it was dropped. */
  const startDrag = (zone: "sidebar" | "main", e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const root = e.currentTarget.closest<HTMLElement>(".emp-root");
    if (!root) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    setDragging(zone);
    const target = e.currentTarget;
    const sideFor = (x: number): "left" | "right" => {
      const r = root.getBoundingClientRect();
      const left = x < r.left + r.width / 2;
      // Dropping the sidebar on a half puts it there; dropping the threads zone puts the sidebar opposite.
      return zone === "sidebar" ? (left ? "left" : "right") : left ? "right" : "left";
    };
    const move = (ev: PointerEvent) => setDrop(sideFor(ev.clientX));
    const up = (ev: PointerEvent) => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
      target.removeEventListener("pointercancel", up);
      setDragging(null);
      setDrop(null);
      // Keep keyboard focus inside the workspace so Esc and ⌘⇧\ still reach it.
      root.querySelector<HTMLElement>(".emp-layouts-card")?.focus();
      if (ev.type === "pointerup") {
        const side = sideFor(ev.clientX);
        if (side !== settings.sidebarSide) update({ sidebarSide: side });
      }
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
    target.addEventListener("pointercancel", up);
  };

  return (
    <>
      <div className="emp-edit-veil emp-edit-veil--sidebar" data-dragging={dragging === "sidebar" || undefined} onPointerDown={(e) => startDrag("sidebar", e)}>
        <VeilPill label={ZONE_LABELS.sidebar} />
      </div>
      <div className="emp-edit-veil emp-edit-veil--main" data-dragging={dragging === "main" || undefined} onPointerDown={(e) => startDrag("main", e)}>
        <VeilPill label={ZONE_LABELS.main} />
      </div>
      {visible(settings, "statusBar") && (
        <div className="emp-edit-veil emp-edit-veil--status" data-static="">
          <VeilPill label={ZONE_LABELS.status} grip={false} />
        </div>
      )}
      {drop && <div className="emp-drop-sheet" data-side={drop} aria-hidden="true" />}
      <LayoutsCard />
    </>
  );
}

function VeilPill({ label, grip = true }: { label: string; grip?: boolean }) {
  return (
    <span className="emp-veil-pill">
      {grip && <IconGrip />}
      <span className="emp-truncate">{label}</span>
    </span>
  );
}

/** The floating palette. Its header is the drag handle; position survives toggles in this window. */
function LayoutsCard() {
  const setEditing = useWorkspace((s) => s.setLayoutEditing);
  const reset = useWorkspace((s) => s.resetLayout);
  const pos = useWorkspace((s) => s.layoutCardPos);
  const setPos = useWorkspace((s) => s.setLayoutCardPos);
  const card = useRef<HTMLDivElement>(null);

  // Focus lands in the card, so Esc (handled on the workspace root) exits edit mode.
  useEffect(() => {
    const el = card.current;
    // Looked up now: on unmount the card is already out of the DOM.
    const root = el?.closest<HTMLElement>(".emp-root");
    el?.focus();
    // Hand focus back to the workspace when edit mode ends.
    return () => root?.focus({ preventScroll: true });
  }, []);

  const startMove = (e: ReactPointerEvent<HTMLElement>) => {
    const el = card.current;
    const parent = el?.parentElement;
    if (e.button !== 0 || !el || !parent) return;
    e.preventDefault();
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    const rect = el.getBoundingClientRect();
    const dx = e.clientX - rect.left;
    const dy = e.clientY - rect.top;
    const move = (ev: PointerEvent) => {
      const p = parent.getBoundingClientRect();
      setPos({
        x: Math.max(4, Math.min(p.width - rect.width - 4, ev.clientX - p.left - dx)),
        y: Math.max(4, Math.min(p.height - 40, ev.clientY - p.top - dy)),
      });
    };
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  };

  return (
    <div
      ref={card}
      className="emp-layouts-card"
      role="dialog"
      aria-label="Layouts"
      tabIndex={-1}
      data-placed={pos ? "" : undefined}
      style={pos ? { left: pos.x, top: pos.y } : undefined}
    >
      <header className="emp-layouts-card__head" onPointerDown={startMove}>
        <div className="emp-layouts-card__titles">
          <h2 className="emp-layouts-card__title">Layouts</h2>
          <p className="emp-layouts-card__hint">
            Pick a layout, or drag zones across. <kbd className="emp-layouts-card__kbd">⌘⇧\</kbd>
          </p>
        </div>
        <div className="emp-layouts-card__actions" onPointerDown={(e) => e.stopPropagation()}>
          <button type="button" className="emp-button emp-button--ghost emp-button--small" data-orion-ui="button" onClick={reset}>
            Reset
          </button>
          <button type="button" className="emp-button emp-button--small" data-orion-ui="button" onClick={() => setEditing(false)}>
            Done
          </button>
        </div>
      </header>
      <div className="emp-layouts-card__body">
        <LayoutPicker />
      </div>
    </div>
  );
}

function PickerLabel({ children }: { children: string }) {
  return <span className="emp-picker-label">{children}</span>;
}

function LayoutPicker() {
  const s = useWorkspace((st) => st.settings);
  const update = useWorkspace((st) => st.updateSettings);
  const apply = useWorkspace((st) => st.applyArrangement);
  const save = useWorkspace((st) => st.saveLayout);
  const remove = useWorkspace((st) => st.deleteLayout);
  const [saving, setSaving] = useState(false);
  const [name, setName] = useState("");

  const commit = () => {
    if (!name.trim()) return;
    save(name);
    setName("");
    setSaving(false);
  };

  return (
    <div className="emp-layout-picker">
      <section className="emp-layout-picker__section">
        <PickerLabel>Interface mode</PickerLabel>
        <div className="emp-layout-picker__modes">
          {INTERFACE_MODES.map((m) => (
            <button
              key={m.id}
              type="button"
              className="emp-select-card emp-mode-card"
              aria-pressed={s.interfaceMode === m.id}
              data-active={s.interfaceMode === m.id || undefined}
              onClick={() => update({ interfaceMode: m.id })}
            >
              <span className="emp-mode-card__label">{m.label}</span>
              <span className="emp-mode-card__desc">{m.description}</span>
            </button>
          ))}
        </div>
        <p className="emp-layout-picker__hint">Changes what is shown, not what EMP can do.</p>
      </section>

      <section className="emp-layout-picker__section">
        <PickerLabel>Templates</PickerLabel>
        <div className="emp-layout-picker__grid">
          {LAYOUT_TEMPLATES.map((t) => (
            <PresetCard key={t.id} name={t.name} arrangement={t.arrangement} active={sameArrangement(s, t.arrangement)} onApply={() => apply(t.arrangement)} />
          ))}
        </div>
      </section>

      <section className="emp-layout-picker__section">
        <PickerLabel>Custom</PickerLabel>
        {s.savedLayouts.length > 0 && (
          <div className="emp-layout-picker__grid">
            {s.savedLayouts.map((l) => (
              <PresetCard
                key={l.id}
                name={l.name}
                arrangement={l.arrangement}
                active={sameArrangement(s, l.arrangement)}
                onApply={() => apply(l.arrangement)}
                onDelete={() => remove(l.id)}
              />
            ))}
          </div>
        )}
        {saving ? (
          <form
            className="emp-layout-picker__save"
            onSubmit={(e) => {
              e.preventDefault();
              commit();
            }}
          >
            <input
              className="emp-input"
              data-orion-ui="input"
              autoFocus
              placeholder="Name this layout…"
              value={name}
              maxLength={40}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.stopPropagation();
                  setSaving(false);
                  setName("");
                }
              }}
            />
            <button type="submit" className="emp-button emp-button--small" data-orion-ui="button" disabled={!name.trim()}>
              Save
            </button>
            <button type="button" className="emp-button emp-button--ghost emp-button--small" data-orion-ui="button" onClick={() => setSaving(false)}>
              Cancel
            </button>
          </form>
        ) : (
          <button type="button" className="emp-text-button" onClick={() => setSaving(true)}>
            <IconSave />
            Save current arrangement as a template
          </button>
        )}
      </section>
    </div>
  );
}

function PresetCard(props: { name: string; arrangement: Arrangement; active: boolean; onApply(): void; onDelete?(): void }) {
  return (
    <div className="emp-preset">
      <button type="button" className="emp-select-card emp-preset__card" data-active={props.active || undefined} aria-pressed={props.active} onClick={props.onApply}>
        <Thumbnail arrangement={props.arrangement} />
        <span className="emp-preset__name emp-truncate">{props.name}</span>
      </button>
      {props.onDelete && (
        <button type="button" className="emp-preset__delete" aria-label={`Delete ${props.name}`} onClick={props.onDelete}>
          <IconClose />
        </button>
      )}
    </div>
  );
}

/** Miniature of an arrangement: sidebar block on its side, the thread zone (a centred column when readable). */
function Thumbnail({ arrangement: a }: { arrangement: Arrangement }) {
  const side = <span className="emp-thumb__zone" style={{ flex: `${0.3 * a.sidebarScale} 0 0` }} />;
  const main = (
    <span className="emp-thumb__main" data-readable={a.threadWidth === "readable" || undefined}>
      <span className="emp-thumb__zone" />
    </span>
  );
  return (
    <span className="emp-thumb" aria-hidden="true">
      {a.sidebarSide === "left" ? side : main}
      {a.sidebarSide === "left" ? main : side}
    </span>
  );
}
