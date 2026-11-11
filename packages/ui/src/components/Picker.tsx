import { useRef, useState, type FocusEvent, type ReactNode } from "react";
import { IconChevron } from "./icons";

export interface PickerOption {
  id: string;
  label: string;
  detail?: ReactNode;
  /** Group heading shown above the first option of each group. */
  group?: string;
  disabled?: boolean;
}

/** Lists longer than this get a filter box. */
const FILTER_AT = 8;

/**
 * Quiet text button that opens a menu above it (composer pickers, like Hermes's model picker).
 * Closes on selection, Escape or focus leaving; no window-level listeners.
 */
export function Picker(props: { label: ReactNode; title?: string; options: PickerOption[]; value: string | null; onChange(id: string): void; onOpen?(): void }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const root = useRef<HTMLDivElement>(null);
  const q = query.trim().toLowerCase();
  const shown = q
    ? props.options.filter((o) => o.label.toLowerCase().includes(q) || o.id.toLowerCase().includes(q) || o.group?.toLowerCase().includes(q))
    : props.options;
  const onBlur = (e: FocusEvent<HTMLDivElement>) => {
    if (!root.current?.contains(e.relatedTarget as Node | null)) setOpen(false);
  };
  return (
    <div ref={root} className="emp-picker" onBlur={onBlur} onKeyDown={(e) => e.key === "Escape" && setOpen(false)}>
      <button
        type="button"
        className="emp-picker__button"
        aria-haspopup="listbox"
        aria-expanded={open}
        title={props.title}
        onClick={(e) => {
          // WebKit on macOS doesn't focus clicked buttons. Keep focus inside the picker
          // so its blur handler can distinguish an outside click from a menu selection.
          e.currentTarget.focus();
          if (!open) props.onOpen?.();
          setQuery("");
          setOpen((v) => !v);
        }}
      >
        <span className="emp-truncate">{props.label}</span>
        <IconChevron open />
      </button>
      {open && (
        <div className="emp-menu" role="listbox">
          {props.options.length > FILTER_AT && (
            <input
              className="emp-menu__filter"
              aria-label="Filter"
              placeholder="Filter…"
              value={query}
              autoFocus
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                const first = shown.find((o) => !o.disabled);
                if (e.key === "Enter" && first) {
                  e.preventDefault();
                  props.onChange(first.id);
                  setOpen(false);
                  setQuery("");
                }
              }}
            />
          )}
          {shown.length === 0 && <div className="emp-menu__empty">No matches</div>}
          {shown.map((o, i) => (
            <MenuOption
              key={o.id}
              option={o}
              heading={o.group && o.group !== shown[i - 1]?.group ? o.group : undefined}
              selected={o.id === props.value}
              onPick={() => {
                props.onChange(o.id);
                setOpen(false);
                setQuery("");
              }}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function MenuOption({ option: o, heading, selected, onPick }: { option: PickerOption; heading?: string; selected: boolean; onPick(): void }) {
  return (
    <>
      {heading && <div className="emp-menu__group">{heading}</div>}
      <button
        type="button"
        role="option"
        aria-selected={selected}
        disabled={o.disabled}
        className="emp-menu__item"
        // In WebKit a mouse press can blur the filter without focusing this button.
        // That unmounts the menu before click fires; retain focus until selection.
        onMouseDown={(e) => e.preventDefault()}
        onClick={onPick}
      >
        <span className="emp-menu__label emp-truncate">{o.label}</span>
        {o.detail && <span className="emp-menu__detail emp-truncate">{o.detail}</span>}
      </button>
    </>
  );
}
