// Small stroke icons (16px grid, 1.5 stroke, currentColor). Decorative: pair with visible text
// or an aria-label on the button.

import type { ReactNode } from "react";

function Icon({ children, size = 16 }: { children: ReactNode; size?: number }) {
  return (
    <svg
      className="emp-icon"
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

export const IconPlus = () => (
  <Icon>
    <path d="M8 3.5v9M3.5 8h9" />
  </Icon>
);

export const IconThreads = () => (
  <Icon>
    <path d="M2.5 4.5a2 2 0 0 1 2-2h7a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H7l-3 2.5v-2.5h0a1.5 1.5 0 0 1-1.5-1.5z" />
  </Icon>
);

export const IconTerminal = () => (
  <Icon>
    <rect x="2" y="2.75" width="12" height="10.5" rx="2" />
    <path d="M5 6.5 6.75 8 5 9.5M8.5 10h2.5" />
  </Icon>
);

/** Suki, the coordinator: a small face. */
export const IconSuki = () => (
  <Icon>
    <rect x="2.25" y="2.75" width="11.5" height="10.5" rx="4" />
    <path d="M6 7v.5M10 7v.5M6.25 10c.9.75 2.6.75 3.5 0" />
  </Icon>
);

export const IconServer = () => (
  <Icon>
    <rect x="2.25" y="2.75" width="11.5" height="4.5" rx="1.5" />
    <rect x="2.25" y="8.75" width="11.5" height="4.5" rx="1.5" />
    <path d="M5 5h.01M5 11h.01M8 5h3M8 11h3" />
  </Icon>
);

export const IconAgents = () => (
  <Icon>
    <circle cx="6" cy="5.5" r="2.25" />
    <path d="M2 13c.4-2.2 2-3.5 4-3.5s3.6 1.3 4 3.5" />
    <path d="M10.5 3.6a2.1 2.1 0 0 1 0 3.8M12 9.9c1 .5 1.7 1.6 2 3.1" />
  </Icon>
);

export const IconSources = () => (
  <Icon>
    <ellipse cx="8" cy="4" rx="5" ry="1.75" />
    <path d="M3 4v4c0 1 2.2 1.75 5 1.75S13 9 13 8V4" />
    <path d="M3 8v4c0 1 2.2 1.75 5 1.75S13 13 13 12V8" />
  </Icon>
);

export const IconSearch = () => (
  <Icon>
    <circle cx="7" cy="7" r="4.25" />
    <path d="m10.25 10.25 3 3" />
  </Icon>
);

export const IconChevron = ({ open }: { open?: boolean }) => (
  <Icon size={12}>
    <path d={open ? "m4 6 4 4 4-4" : "m6 4 4 4-4 4"} />
  </Icon>
);

export const IconSend = () => (
  <Icon>
    <path d="M8 13V3.5M4 7.5l4-4 4 4" />
  </Icon>
);

export const IconStop = () => (
  <Icon>
    <rect x="4.25" y="4.25" width="7.5" height="7.5" rx="1.25" />
  </Icon>
);

export const IconInfo = () => (
  <Icon>
    <circle cx="8" cy="8" r="5.75" />
    <path d="M8 7.25V11M8 5h.01" />
  </Icon>
);

export const IconExpand = () => (
  <Icon>
    <path d="M9.5 2.75h3.75V6.5M13.25 2.75 8.5 7.5M6.5 13.25H2.75V9.5M2.75 13.25 7.5 8.5" />
  </Icon>
);

export const IconReturn = () => (
  <Icon>
    <path d="M6 4.5 3 7.5l3 3" />
    <path d="M3 7.5h6.5a3.5 3.5 0 0 1 0 7H8" />
  </Icon>
);

export const IconCopy = () => (
  <Icon>
    <rect x="5.25" y="5.25" width="8" height="8" rx="1.5" />
    <path d="M10.75 5.25V4.25a1.5 1.5 0 0 0-1.5-1.5h-5a1.5 1.5 0 0 0-1.5 1.5v5a1.5 1.5 0 0 0 1.5 1.5h1" />
  </Icon>
);

export const IconClose = () => (
  <Icon size={14}>
    <path d="m4.5 4.5 7 7M11.5 4.5l-7 7" />
  </Icon>
);

export const IconFilter = () => (
  <Icon size={14}>
    <path d="M2.5 4h11M4.5 8h7M6.5 12h3" />
  </Icon>
);

export const IconSpark = () => (
  <Icon size={14}>
    <path d="M8 2.5v3M8 10.5v3M2.5 8h3M10.5 8h3M4.4 4.4l1.8 1.8M9.8 9.8l1.8 1.8M11.6 4.4 9.8 6.2M6.2 9.8l-1.8 1.8" />
  </Icon>
);

export const IconFolder = () => (
  <Icon>
    <path d="M2.5 4.5a1.5 1.5 0 0 1 1.5-1.5h2.6l1.5 1.75H12a1.5 1.5 0 0 1 1.5 1.5v5.25A1.5 1.5 0 0 1 12 13H4a1.5 1.5 0 0 1-1.5-1.5z" />
  </Icon>
);

export const IconSettings = () => (
  <Icon>
    <path d="M2.5 4.5h6M11.5 4.5h2M2.5 11.5h2M7.5 11.5h6" />
    <circle cx="10" cy="4.5" r="1.5" />
    <circle cx="6" cy="11.5" r="1.5" />
  </Icon>
);

export const IconChevronUp = () => (
  <Icon size={12}>
    <path d="m4 10 4-4 4 4" />
  </Icon>
);

export const IconGrip = () => (
  <Icon size={12}>
    <path d="M6 4h.01M10 4h.01M6 8h.01M10 8h.01M6 12h.01M10 12h.01" strokeWidth={2} />
  </Icon>
);

export const IconPalette = () => (
  <Icon>
    <path d="M8 2.5a5.5 5.5 0 1 0 0 11c.8 0 1.2-.5 1.2-1.1 0-.9-.9-1.2-.9-2 0-.6.5-1.1 1.1-1.1h1.4a2.7 2.7 0 0 0 2.7-2.7C13.5 4.4 11.1 2.5 8 2.5z" />
    <circle cx="5.3" cy="7" r=".6" />
    <circle cx="7.5" cy="5" r=".6" />
    <circle cx="10.2" cy="5.6" r=".6" />
  </Icon>
);

export const IconMonitor = () => (
  <Icon>
    <rect x="2" y="3" width="12" height="8" rx="1" />
    <path d="M6 13.5h4M8 11v2.5" />
  </Icon>
);

export const IconKeyboard = () => (
  <Icon>
    <rect x="1.75" y="4" width="12.5" height="8" rx="1" />
    <path d="M4.5 6.5h.01M7 6.5h.01M9.5 6.5h.01M12 6.5h.01M5 9.5h6" />
  </Icon>
);

export const IconChat = () => (
  <Icon>
    <path d="M2.5 4a1 1 0 0 1 1-1h9a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H6.5L3.5 13.5V11h0a1 1 0 0 1-1-1z" />
  </Icon>
);

export const IconSwap = () => (
  <Icon>
    <path d="M3 5.5h10m0 0L10.5 3M13 5.5 10.5 8M13 10.5H3m0 0L5.5 8M3 10.5 5.5 13" />
  </Icon>
);

/** Hermes's layout-editor glyph: a split pane with a small zone. */
export const IconLayout = () => (
  <Icon>
    <rect x="2.5" y="2.5" width="11" height="11" rx="1" />
    <path d="M7 2.5v11M7 8h6.5" />
  </Icon>
);

export const IconSave = () => (
  <Icon size={13}>
    <path d="M3 3.5a.5.5 0 0 1 .5-.5h7.3L13 5.2v7.3a.5.5 0 0 1-.5.5h-9a.5.5 0 0 1-.5-.5z" />
    <path d="M5.5 3v3h4.5V3M5 13V9.5h6V13" />
  </Icon>
);

export const IconRefresh = () => (
  <Icon>
    <path d="M13 8a5 5 0 0 1-8.9 3.1M3 8a5 5 0 0 1 8.9-3.1M12 2.5v2.6H9.4M4 13.5v-2.6h2.6" />
  </Icon>
);

export const IconChevronRight = () => (
  <Icon size={12}>
    <path d="m6 4 4 4-4 4" />
  </Icon>
);

export const IconPhone = () => (
  <Icon>
    <rect x="4.5" y="1.75" width="7" height="12.5" rx="1.5" />
    <path d="M7.25 12h1.5" />
  </Icon>
);

export const IconMenu = () => (
  <Icon>
    <path d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11" />
  </Icon>
);

export const IconPin = () => (
  <Icon size={13}>
    <path d="M9.5 2.5 13.5 6.5M10.5 3.5 7 7l-2.5.5L3.5 8.5l4 4 1-1L9 9l3.5-3.5M5.5 10.5 2.5 13.5" />
  </Icon>
);

export const IconFolderPlus = () => (
  <Icon size={14}>
    <path d="M2.5 4.5a1.5 1.5 0 0 1 1.5-1.5h2.6l1.5 1.75H12a1.5 1.5 0 0 1 1.5 1.5v5.25A1.5 1.5 0 0 1 12 13H4a1.5 1.5 0 0 1-1.5-1.5z" />
    <path d="M8 6.75v3.5M6.25 8.5h3.5" />
  </Icon>
);

export const IconPaperclip = () => (
  <Icon>
    <path d="M13 7.5 8.2 12.3a3 3 0 0 1-4.25-4.25l5-5a2 2 0 0 1 2.83 2.83L6.8 10.86a1 1 0 0 1-1.42-1.42L9.8 5" />
  </Icon>
);

export const IconFile = () => (
  <Icon size={14}>
    <path d="M9 2.5H4.5a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1h7a1 1 0 0 0 1-1V6M9 2.5 12.5 6M9 2.5V6h3.5" />
  </Icon>
);

export const IconClock = () => (
  <Icon>
    <circle cx="8" cy="8" r="5.5" />
    <path d="M8 5v3l2 1.5" />
  </Icon>
);

export const IconGoal = () => (
  <Icon>
    <circle cx="8" cy="8" r="5.5" />
    <circle cx="8" cy="8" r="2.5" />
    <path d="M8 8l5-5M11 3h2v2" />
  </Icon>
);
