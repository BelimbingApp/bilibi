---
version: stable
name: Bilibi
description: UI design reference for Bilibi, the Rust port of Bilimbi.
---

# DESIGN.md

Bilibi is warm, compact workflow software for long operational sessions. This file is the UI design reference. Architecture lives in [`docs/architecture/`](docs/architecture/README.md). Agents also read `AGENTS.md`.

- **Parents:** Belimbing (Laravel) gives the business meaning. Bilimbi (Phoenix) gives the behaviour, the DOM and the module topology.
- **Status marks:** *built* means the code exists and was checked. *planned* means it is not built yet.
- **Authority:** where this file and the code differ, the code wins. Fix this file.

## Overview

Professional, compact, warm, trustworthy. Dense enough for operations, calm enough for judgment.

- Success means users finish real work faster and leave.
- The interface is the brand. It is not a marketing site and not generic enterprise gray.
- Less, but better. Every surface, label and control must earn its place.

## Character

*Ledger geometry:* compact scale, hairline rules, tabular numerals, warm stone base.

- Controls use `rounded-md`. Surfaces use `rounded-xl`.
- Lime marks orientation only: card top edge, active navigation, selection, unseen markers. Lime never reports status.
- Font: Instrument Sans, vendored locally (SIL OFL). No remote font requests.
- Tailwind CSS 4.3.0 builds one committed `app.css`. Node is needed only to rebuild it.

## Tokens

Use semantic roles. Never use arbitrary colors. Values live in `apps/base/ui/assets/css/app.css`.

| Role | Meaning |
| --- | --- |
| `canvas`, `surface`, `surface-sidebar`, `surface-sunken` | Warm stone base. Sunken is for table headers and code |
| `ink`, `ink-muted`, `ink-subtle`, `ink-faint`, `link`, `muted` | Text hierarchy. Links stay stronger than muted text |
| `line`, `high-contrast-line`, `low-contrast-line` | Structural lines, ordered by contrast |
| `action`, `action-hover`, `action-ink` | Primary buttons and page headings |
| `brand`, `brand-strong`, `brand-surface`, `brand-ink` | Orientation accent |
| `success`, `info`, `warning`, `danger` | Honest status, each with `-surface`, `-line`, `-ink` |
| `scale-1..5`, `cat-1..8` | Data colour for ordered and categorical values. Never read as status |

- Light and dark themes share the same roles.
- Success and info announce politely. Warning and error announce as an alert.

## Components

Reuse the shared macros and typed models. Do not hand-write markup or copy classes.

| Need | Use | Source |
| --- | --- | --- |
| Page, card, section, button, badge, icon | `templates/components.html` and `src/components.rs` | `apps/base/ui` |
| Form control | `templates/forms.html` and `src/forms.rs` | `apps/base/ui` |
| Flash, alert, panel notice | `templates/feedback.html` and `src/feedback.rs` | `apps/base/ui` |
| Modal, confirmation | `templates/dialog.html` | `apps/base/ui` |
| Tabs | `templates/tabs.html` | `apps/base/ui` |
| Inline edit | `templates/inline.html` and `src/inline.rs` | `apps/base/ui` |
| List and table | `templates/lists.html`, `flex_table.html` | `apps/base/ui` |

- Name variants through the enums (`ButtonVariant`, `BadgeKind`, `PageVariant`), not class strings.
- Icons come from `src/icons.rs` action names or `hero-*`. Use the Belimbing equivalent for a familiar action.
- Icon-only actions need an accessible label and a title. Inline controls are `size-6`. Table and toolbar controls are `size-7`.
- Destructive actions use calm danger text, never a solid red button.
- Secrets use `SecretInput`. It starts masked. A stored value shows a keep-current mask, never plaintext.
- Specimens: the Design Library (`/assets/ui-components.html`, `/assets/ui-feedback.html`) is the screenshot baseline. Extend it with each new primitive.

## Page layout

Width is a shared decision. Every page uses `PageVariant`.

| Variant | Use |
| --- | --- |
| `List` | Operational index pages |
| `Form` | Single-column create or edit forms |
| `Detail` | Record pages and the dashboard. Same width as a list |

- Never set a page's own max width.
- Compact does not mean cramped. Keep related controls close and the primary action clear.
- Layouts are responsive. There is no separate mobile product.

## Lists

*Every list is a `FlexTable`.* Never the plain `Table`.

- **State:** `ListState` keeps page, search, filters, sort and page size in the URL. Reload and shared links reopen the same view.
- **Toolbar:** search and filters sit above the table in the open. No second card.
- **Rows:** `py-0.5`, `px-2`. Headers `py-1.5`, sunken background, `text-xs font-semibold`.
- **Numbers:** tabular numerals for IDs, counts, dates and amounts.
- **Corners:** table frames are square. A card is flat only where it frames the table.
- **Pagination:** shown only when another page exists. The rows-per-page selector lives in the component.
- **Empty:** `EmptyState` says why and what to do next.

### Flexible columns

- A small "Customize table" notch on the table edge opens the column bar.
- People add, remove and reorder columns, and pick a lens: value, bar, colour band, trend, change since a date.
- Many-links become rollup cells that expand in place.
- Row height has two named zooms: Compact and Normal. A compact row is one line.
- The arrangement is remembered per account and page (`PageColumns`). An address that carries it wins. "Reset to default columns" forgets it.
- Catalog: `apps/base/grid/README.md`.

## Record pages

*Read first, edit in place.* No Edit button and no separate edit route for the same facts.

- A detail page is the header plus a stack of sections.
- A section is a card with a heading, optional count, description and actions, then its facts or table.
- Facts are one list: label column, left-aligned value.
- **Text facts:** `InlineEdit`. Enter or leaving commits. Escape cancels.
- **Long text:** `InlineLongText`. Saves on blur.
- **Choices:** `InlineChoice`. The read state is the trigger.
- The field shows "Saving…", then the outcome where the person typed. A refusal is never a silent revert.
- Outcomes use `CommitStates`. A refused write must not return a fatal error. Contract: `docs/inline-editing.md`.
- Back is a plain link (`BackLink`). History is a quiet disclosure, not a button.
- A quiet action that submits uses `ActionLink` with `href` and `method`.

## Dialogs and feedback

| Pattern | Rule |
| --- | --- |
| Modal | Native `<dialog>`. Focus enters, stays, and returns. Escape and Cancel are one action. A click on the backdrop does nothing |
| Confirmation | `ConfirmDialog` for irreversible actions: consequence first, Cancel beside the destructive verb. Never `window.confirm` |
| Flash | Stacks top right, most severe first. Info, warning and error stay until dismissed. Success clears after 8 s |
| Success | Only for a completed write |
| Info | Informs without confirming a write |
| Panel notice | Where a panel cannot reach the page flash |
| Connection | Banners announce a dropped socket. The page reconnects with backoff and remounts in place |
| Error pages | One shell from `apps/web/src/errors.rs`. Debug detail only in debug builds |

- A failure says what happened, what it cost, and what to do next.
- Recover before reporting. Users are not an error channel.

## Shell

*Two shells, both minimal.*

- **Credential layout:** a centered card for sign-in and password recovery. No navigation.
- **Workspace shell:** `h-7` top bar, sidebar menu, status bar.
  - Top bar: sidebar toggle, brand mark, notification bell, timezone and clock selector, theme selector.
  - Sidebar: the menu, pinned items, account entry at the bottom.
  - Status bar: version. In development it also shows `dev` and the listen address.
  - Below `lg` the menu is an off-canvas drawer.
- Pages never render chrome. The host wraps them (`apps/web/src/shell.rs`).
- The bell shows the unread count and follows new notifications live. It opens recent items.
- Theme, clock mode and pins belong to the account. They are not `localStorage`.
- "Viewing as … · Stop" appears only while impersonating.
- A platform-operator row appears in the account menu. Operator-only screens carry their own caution where reach is unfiltered.
- Navigation: sorted alphabetically, chevron toggles, the active route in lime text, ancestors accented. A page joins the menu when it ships.
- An empty menu says no destinations are available and names the recovery.

## Tiled workspace

`/workspace` shows several pages side by side. *built* (`apps/base/tiling/README.md`)

- Each tile is a same-origin frame of a normal page. Framed pages render without chrome.
- The tree is in the address. Back, Forward and copied links work.
- Pages open by splitting the tile with the most room.
- Layouts save per account by name, with a master mode.
- Keyboard mode: `Ctrl+.` enters, single keys move, swap, resize, flip and close. Escape leaves.
- Tiles can follow a record selected in another tile.
- Below `lg` one tile shows at a time.
- Inside a tile, pages never write `history` themselves.

## Writing and tone

- **Scan before reading.** Headings, labels, badges, counts and table structure carry the workflow. Do not explain what a visible label already says.
- **Put information where it acts.** A page title stays true across tabs. Tab guidance lives in the tab.
- **Consistency.** Same thing, same look, same place.
- **Calm.** No urgency, no badge spam, no dark patterns.
- **Plain language.** Write for the person doing the work. Show the idea, not its internal code.

## Accessibility and resilience

- Keyboard use, visible focus, readable contrast, semantic structure.
- Loading, empty, error, disabled and permission-denied states are part of the design.
- `prefers-reduced-motion: reduce` collapses CSS motion at the platform level. Components add no motion variants.
- Pages stay understandable when a connection drops. Server-rendered content stays, and reconnection is announced.

## Planned

| Item | Status |
| --- | --- |
| Base domain events (`apps/base/events`) | In progress in a parallel change |
| Sign-in attempt throttle | Not built |
| Distributed notification fan-out | Not built. Broadcasts are process-local |
| TLS for PostgreSQL connections | Not built |
