# Design

> **v3 colours (2026-10-20):** EMP is white and red, the owner's own palette (see `tokens.json` `color.$comment`). Where this file says "blue" or "Nous blue" for the accent, read EMP red `#e8173f`. Layout and components still follow Hermes.

<!-- Visual reference: Hermes Agent desktop, MIT, © Nous Research. -->
## Direction

EMP looks like the Hermes Agent desktop app: a flat, light, quiet work surface. The values were read from the running app's computed styles, not eyeballed.

- **Surfaces:**
  - work area: white (`#fefefe`)
  - sidebar: cool grey `#f6f8fa`
  - ink: `#1f2328` at 94 / 74 / 54 / 36 %
  - hairlines: `#d0d7de`
- **Accent:** one Nous blue `#0053fd`. Use it for the section labels, the active tab underline, links and focus, and for the primary button.
- **Type:** the system font (SF Pro on macOS); JetBrains Mono for code; the system font for the `EMP` wordmark: the sidebar brand and the big empty-screen mark, set at regular weight (no faux bold).

## Layout

- **Two columns + status bar.**
  - **Sidebar** (`layout.sidebar`):
    - `Threads / Agents` underline tabs
    - nav (New thread ⌘N, Terminals, Projects, Agents, Sources, Settings ⌘,)
    - borderless search
    - `LIVE` and `THREADS` sections, with date groups (hairline after the label)
  - **Main:** an uppercase tab strip (New thread, open threads, pages), with the open thread's actions as quiet icons on the right.
  - **Status bar:** full width; "empd ready" on the left, counts and version (blue) on the right.
- **New thread:** a centred blue wordmark plus the agent's role. The composer at the bottom picks the agent (a quiet text picker, like Hermes's model picker) and the folder (`+`).
- **Thread:**
  - The transcript is full width with no centred column.
  - Your turns sit in a hairline box on `raised`; agent prose is 14.3px at line-height 1.5.
  - Tool activity is a grey 12.1px one-liner that expands; thinking is a muted row.
  - The composer is one hairline bar (`composerLine`, 5px radius) with the model name and a round dark send button (stop while running).

## Rules

- **Corners:** nearly square (2–3px; 5px on the composer). No glows, no gradients, no card-in-card.
- **Labels:** sections use 10.24px semibold uppercase, tracked 0.16em, in blue, with a dither square. Date groups and table headers use the same size, tracked 0.12em, at 36% ink.
- **Tabs:** 11px medium uppercase, tracked 0.025em, with a 2px blue underline when active.
- **Phone layout:** below 720px of workspace width (`data-compact`, measured with a ResizeObserver, never the viewport), the sidebar becomes a drawer behind a menu button in the tab bar, the status bar, shortcut hints and desktop-only controls hide, Settings becomes a full sheet with the rail as a scrolling row, and the composer puts its pickers under the message. Safe-area insets pad the edges.
- **Current chat (Hermes):** the open thread's row gets the active band (accent 8% over 5% ink) and medium-weight full ink. Truncated titles in rows and tabs scroll their hidden tail into view on hover (`lib/marquee.ts`).
- **Rows:** 13px at 74% ink with a grey bullet. The active row gets the 6.9% blue tint; hover gets the 4% tint. Live threads tint the bullet: blue = running, amber = in terminal, red = failed.
- **Ink tiers** (`text` / `secondary` / `muted` / `faint`): use these for hierarchy rather than extra weights.
- **Owner overrides (Settings):**
  - **Highlight colour** (a colour wheel plus a brightness slider and hex field): replaces `color.accent`. It is still the one accent. With a non-default accent, `accentHover`, `accentSoft`, `hover`, `selected`, `composerLine` and `running` are mixed from it (`color-mix`); the default keeps the exact Nous values.
  - **Background** (a colour wheel): replaces `color.background`.
    - `panel`, `raised` and `overlay` are mixed from it toward the ink (96%, 98.5% and 93%).
    - A dark background (luminance < 0.18) sets `data-tone="dark"`. The ink becomes `onAccent` white at the same 92/72/54/38% tiers, lines and hovers become white tints, and selection tints get stronger. This is an owner-picked dark mode, not a designed one.
  - **Layout:** the owner can move the sidebar to the right, scale its width (0.8–1.8 × `layout.sidebar`), reorder or hide nav items, hide search, Live and the status bar, and pick a readable thread width (`layout.content`). Simple interface mode hides search, Live and the status bar together.
- **Settings and the layout editor copy Hermes desktop** (`apps/desktop/src/app/settings`, `overlays/`, `components/pane-shell/tree/renderer/edit-bar.tsx` + `layout-picker.tsx`):
  - **Settings overlay:**
    - a 22%-ink scrim with a 2px blur;
    - a raised card with a `field`-wide rail of sections (boxed active item, indented sub-pages), a breadcrumb and ListRows on a fixed label/control split;
    - a Switch for every on/off and a SegmentedControl for small choices;
    - selectable cards (accent border, 6% fill, 20% ring);
    - a "Search settings" pill riding the card's top edge.
  - **Layout edit mode (⌘⇧\):**
    - a dashed accent veil (55%) with a 6% tint, a light blur and an uppercase grip pill over each zone;
    - a drop sheet while dragging;
    - a floating, draggable "Layouts" card with Reset/Done, Interface mode cards, template thumbnails and Custom saved arrangements.
- **Tokens only** in `packages/ui` CSS; `scripts/check-sandbox.mjs` enforces this. Fonts are embedded as data URLs (`npm run fonts`).

## Do

- Sentence case in body text; uppercase only for labels and tabs.
- Show paths as `~/…`, with the middle trimmed when long.
- Keep metadata behind the Details icon.

## Don't

- Dark surfaces or neon (v1's graphite and ember are gone).
- A second accent colour.
- Boxed sections inside panels.
- Raw markdown or absolute paths in titles.

## References

- Hermes Agent desktop: its design guidance, styles, and theme presets ("nous" = GitHub Light Default).
- `design/tokens.json` v2: the EMP mapping.
- `packages/ui/FONTS-LICENSE.txt`: JetBrains Mono is OFL; the public wordmark uses system fonts.
