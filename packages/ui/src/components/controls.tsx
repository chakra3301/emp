// Hermes's settings controls: the Switch (every on/off) and the SegmentedControl
// (small mutually-exclusive choices: a tinted track with a raised active pill).

import { useState } from "react";

export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange(on: boolean): void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      className="emp-switch"
      onClick={() => onChange(!checked)}
    >
      <span className="emp-switch__thumb" aria-hidden="true" />
    </button>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
  disabled,
}: {
  value: T;
  options: readonly (readonly [T, string])[];
  onChange(v: T): void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <div className="emp-segmented" role="group" aria-label={label} data-disabled={disabled || undefined}>
      {options.map(([id, text]) => (
        <button key={id} type="button" aria-pressed={value === id} disabled={disabled} className="emp-segmented__option" onClick={() => onChange(id)}>
          <span className="emp-truncate">{text}</span>
        </button>
      ))}
    </div>
  );
}

/** A button that asks once before acting (unless `skipConfirm`); it resets when focus leaves. */
export function ConfirmButton({
  label,
  confirm,
  onConfirm,
  className,
  disabled,
  skipConfirm,
}: {
  label: string;
  confirm: string;
  onConfirm(): void;
  className: string;
  disabled?: boolean;
  skipConfirm?: boolean;
}) {
  const [asking, setAsking] = useState(false);
  return (
    <button
      type="button"
      className={asking ? `${className} emp-button--danger` : className}
      data-orion-ui="button"
      disabled={disabled}
      onBlur={() => setAsking(false)}
      onClick={() => {
        if (skipConfirm || asking) {
          setAsking(false);
          onConfirm();
        } else {
          setAsking(true);
        }
      }}
    >
      {asking ? confirm : label}
    </button>
  );
}
