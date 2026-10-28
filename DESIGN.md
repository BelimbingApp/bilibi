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
- Tailwind CSS 4.3.0 builds one generated stylesheet, embedded by Cargo. Commit its sources only; Node is needed on build machines.

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
| `scale-1..5`, `cat-1..8` | Data colour for ordered and categorical values. Never read as status. Charts take `cat-1..8` in series order |

- Light and dark themes share the same roles.
- Success and info announce politely. Warning and error announce as an alert.

## Components

Reuse the shared macros and typed models. Do not hand-write markup or copy classes.

| Need | Use | Source |
| --- | --- | --- |
| Page, card, section, button, badge, icon | `templates/components.html` and `src/components.rs` | `apps/base/ui` |
| Form control | `templates/forms.html` and `src/forms.rs` | `apps/base/ui` |
| Flash, alert, panel notice | `templates/feedback.html` and `src/feedback.rs` | `apps/base/ui` |
| Which Bilibi this is (badge, banner) | `templates/feedback.html` and `src/instance.rs`, through `bilibi_system::Instance` | `apps/base/ui`, `apps/base/system` |
| Modal, confirmation | `templates/dialog.html` | `apps/base/ui` |
| Tabs | `templates/tabs.html` | `apps/base/ui` |
| Inline edit | `templates/inline.html` and `src/inline.rs` | `apps/base/ui` |
| List and table | `templates/lists.html`, `flex_table.html` | `apps/base/ui` |
| Chart (KPI, bar, stacked bar, line, pie, donut) | `templates/chart.html` and `src/chart.rs` (`Chart`, `ChartKind`, `Series`) | `apps/base/ui` |

- Name variants through the enums (`ButtonVariant`, `BadgeKind`, `PageVariant`), not class strings.
- Icons use bare names from `base/ui` (`edit`, `pencil`, `bookmark-solid`), rendered as inline SVG. Use the Belimbing equivalent for a familiar action; see `apps/base/ui/README.md` for names and missing-icon behaviour.
- Icon-only actions need an accessible label and a title. Inline controls are 24 px; table and toolbar controls are 28 px. Touch screens expand icon, tab, pager and choice targets to 40 px; the labelled button stays 24 px.
- Links navigate; buttons act.
- Destructive actions use calm danger text, never a solid red button.
- Secrets use `SecretInput`. It starts masked. A stored value shows a keep-current mask, never plaintext.
- Specimens: the Design Library (`/assets/ui-components.html`, `/assets/ui-feedback.html`) is the screenshot baseline. Extend it with each new primitive.

## Compact scale

Shared UI sizes follow the sidebar's 12 px text and tight row rhythm; pages do not choose their own density.

| Part | Size |
| --- | --- |
| Body and button text | 12 / 16 px (`text-sm`); sidebar remains 12 / 14 px |
| Hints, badges and table headings | 11 / 14 px (`text-xs`) |
| Field values and record facts | 14 / 20 px; input values stay 16 px on phones to prevent zoom |
| Page title | 16 / 24 px; card and empty-state headings 12 / 16 px; dialog title 14 / 20 px |
| Floating fields | 40 px minimum; toolbar and table fields 28 px (`.ui-compact-controls`) |
| Buttons | 24 px with 8 px side padding, one size everywhere (`ui-button`); a caller's padding, height and text-size classes are discarded (`button_class`) |
| Touch targets | Utility `IconButton`, tabs, pagination and choice labels on coarse pointers: 40 px minimum. `Button` keeps 24 px on phones and touch screens |
| Card | 12 px body padding; header 8 px vertically and 12 px horizontally; table frames stay at zero |
| Feedback | 8 px vertically and 12 px horizontally; panel notices use 6 px vertically |
| Spacing | 4 px steps: 4 within a group, 8 between controls, 12 within surfaces, 16 between groups |

The scale lives in `apps/base/ui/assets/css/app.css`. Keep named sizes and shared macros; a caller's padding utilities do not change card density. Standard and compact table row zooms remain available.

## Page layout

Width is a shared decision. Every page uses `PageVariant`. All three share one column, so the title, subtitle and Back link sit in the same place on every page: next to the menu, and centred only once the screen is wider than a list.

| Variant | Use |
| --- | --- |
| `List` | Operational index pages |
| `Form` | Create or edit forms. The header is as wide as a list; the form below it stops at about two fields side by side |
| `Detail` | Record pages and the dashboard. Same width as a list |

- Never set a page's own max width.
- Compact does not mean cramped. Keep related controls close and the primary action clear.
- Layouts are responsive. There is no separate mobile product.

## Forms

*One field, one box.* The label sits inside the field, so each field is one element and the value is what the eye lands on. *built* (`apps/base/ui/src/forms.rs`, `LabelPlacement`)

- **Text fields float the label.** Empty, it rests where the value goes. On focus, once filled, or when the browser autofills, it moves up small in muted ink. It never leaves the box.
- **Selects, textareas, date-like inputs and the combobox pin it small at the top.** They always show something (a prompt, a mask, a selection) that a resting label would cover.
- **Multi-selects:** one box holds the inset label and `(N selected)` count, with removable selected chips beneath. The default keeps complete chips on one line and shows “…” for overflow; `wrap_chips` expands the box onto multiple lines. The chevron stays beside the label. Empty fields show the label, `(0 selected)` and a selection prompt. The dropdown always exposes every option.
- **Choice chevrons:** selects and multi-selects use the same 16 px outline chevron, aligned 11 px inside the field's right edge.
- **Colour choices use `ColourInput`.** Each named choice and the selected value are one chip: that status role's banner surface, border and text colour, with the name inside, including the default choice. Chips follow the light and dark themes. The pinned label, keyboard controls and native select keep it usable with assistive technology and without JavaScript.
- **Compact.** A single-value inset box is 40 px with a 14 px value (`text-base`); the floated or pinned label is 11 px. A multi-select starts at 48 px to hold its label and chip row, and grows when wrapping is enabled. Inset fields sit 12 px apart. Phones get a 16 px input value so iOS does not zoom.
- **2FA method choice:** Settings and Confirm access show each installed method’s own setup or prompt destination. A single authenticator retains its existing flow. Reading a choice never starts or sends a challenge.
- **2FA before saving:** use `StepUp` (`base/ui`) for the protected submit actions, with `ProofInput` for a live method choice and code. Without 2FA, show a plain setup notice and link first. Keep unrelated changes available.
- **Sections:** draw a bordered part of a form with `forms::section` (one shared 12 px box and heading), not page-owned box classes. Create Address uses it.
- **Classic label above** stays for a field with no visible label (toolbars, `sr-only`), a field with its own `class` or `label_class`, checkboxes and radio groups. Multi-selects keep their canonical inset label.
- Placeholder text shows only once the person is in the field, as an example, never as the label.
- Help and error lines sit under the box. An error is a sentence that names its field (`error_sentence`): "Parse confidence must be a number from 0 to 1…".
- After a refused save, `ErrorSummary` lists the problems above the form, each linking to its field. Inline errors stay.
- Keep rarely used parts out of the way: `Fold` folds a part under a one-line summary of what it holds and opens itself on an error; `AddMore` ("+ Add address line 2") reveals one more optional field.
- A greyed field says why, in one line near the field that unlocks it.
- This departs from Bilimbi's label-above DOM on purpose; the shared macros own it, so no page styles its own label.

## Lists

*Every list is a `FlexTable`.* Never the plain `Table`.

- **State:** `ListState` keeps page, search, filters, sort and page size in the URL. Reload and shared links reopen the same view.
- **Toolbar:** search sits first, with filter boxes to its right, above the table in the open. On narrow screens filters wrap below search in the same order. No second card.
- **Rows:** `py-0.5`, `px-2`. Headers `py-1`, sunken background, `text-xs font-semibold`.
- **Numbers:** tabular numerals for IDs, counts, dates and amounts.
- **Corners:** table frames are square. A card is flat only where it frames the table.
- **Pagination:** shown only when another page exists. The rows-per-page selector lives in the component.
- **Settings:** the operator and company settings table opens each Value cell as a form with Save and Cancel (`InlineForm`). Enter saves; Escape cancels. Protected values ask for 2FA in the row.
- **Empty:** `EmptyState` says why and what to do next.

### Flexible columns

- A small "Customize table" notch on the table edge opens the column bar.
- People add, remove and reorder columns, and pick a lens: value, bar, colour band, trend, change since a date.
- Many-links become rollup cells that expand in place.
- Row height has two named zooms: Compact and Normal. A compact row is one line.
- The arrangement is remembered per account and page (`PageColumns`). An address that carries it wins. "Reset to default columns" forgets it.
- Every scalar detail field is available in the column picker under its detail label; shared `RecordField` definitions keep them together. Existing defaults stay unchanged. Long text, files and nested collections stay on the detail page. Restrictions apply to the picker and cells as they do to the record.
- Catalog and field contract: `apps/base/grid/README.md`.

## Record pages

*Read first, edit in place.* No Edit button and no separate edit route for the same facts.

- A detail page is the header plus a stack of sections.
- A section is a card with a heading, optional count, description and actions, then its facts or table.
- Facts wrap long identifiers without changing the copied text. They read like the filled-in form without its boxes: a small muted label (the floated label's size and ink) over the value, no dividers. They flow on a grid that follows the card's width: one column on a phone, two on a normal screen, three on a wide one. A long text or `pre` value takes the whole row. Split a card's facts into several `Facts` to start a new row, as Address Details does for its street lines.
- An inline editor opens as the floating field in the fact's place and size, so nothing moves between reading and editing (`[data-fact]` in `assets/css/app.css`).
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
| Confirmation | `ConfirmDialog` for irreversible actions: consequence first, Cancel beside the destructive verb. Its shared macro accepts a body for a reason field. Never `window.confirm` |
| Flash | Stacks at the right above the page, most severe first, reserving space so messages never cover page actions. A message that arrives with a redirect floats top right instead. Info, warning and error stay until dismissed. Success clears after 8 s |
| Shell preferences | Theme and time-display saves use the shared floating flash, with the same success timeout and persistent errors |
| Success | Only for a completed write |
| Info | Informs without confirming a write |
| Panel notice | Where a panel cannot reach the page flash |
| Connection | Banners announce a dropped socket. The page reconnects with backoff and remounts in place |
| Error pages | One shell from `apps/web/src/errors.rs`. Debug detail only in debug builds |
| Which Bilibi | The status bar always shows the stored server role, instance label and colour. A Core Administrator can click it to open Instance settings. Staging also shows the top-bar badge; Dev and Staging show a badge on sign-in. A review or confirmation opens with the instance banner on every instance, production included. The colour is the operator's `instance.colour` status role, and the label is the operator's `instance.label`, so the name is always in words |

- A failure says what happened, what it cost, and what to do next.
- Recover before reporting. Users are not an error channel.

## Shell

*Two shells, both minimal.*

- **Credential layout:** a centered card for sign-in and password recovery. No navigation. When the sign-in policy requires 2FA, a second card shows enrolled methods, explicit start/resend, code entry and Cancel before opening the workspace. Reading it sends no challenge.
- **Workspace shell:** `h-7` top bar, sidebar menu, status bar.
  - Top bar: sidebar toggle, brand mark, search, notification bell, timezone and clock selector, theme selector.
  - Sidebar: the menu, pinned items, account entry at the bottom.
  - Status bar: the Platform version, then each mounted Domain's version (`finance 0.1.0`), with commits in the tooltip (`docs/versions.md`). It also shows the server role and instance label in the configured colour, plus the listen address when the role is Dev. The role comes from `instance.role`, never the build mode.
  - In the status bar's centre, the AI tag: the text AI in a bordered box, drawn like the role tag. A faint border means no AI app is connected; the normal border means one is. The tooltip says which ("Claude, ChatGPT · last used 5 min ago" or "No AI app connected"), so colour is never the only cue. It stays visible at phone width. Clicking it opens a panel upward: when nothing is connected, the prompt to paste into an AI app and the Bilibi address, each with a Copy button, plus a note to use an AI program on this computer when the server is a Dev copy or not reachable from the internet; when connected, the connected apps with last use and end date, a Manage link, and what the AI read and did, newest first. The panel follows new requests and connection changes live. Approvals, bot notifications and an in-app assistant are later stages.
  - Below `lg` the menu is an off-canvas drawer.
- Pages never render chrome. The host wraps them (`apps/web/src/shell.rs`).
- The bell shows the unread count and follows new notifications live. It opens recent items.
- Search is the button before the bell, or `/` when not typing in a field. It opens a dialog that finds every page the person may open that has a name (its page title or menu row): menu pages, account pages and pages with no menu row. A query matches a page's name or any place it lives under, ignoring case, and each result shows where it lives (`Administration › Authorization › Roles`, `Account › My Settings › Password`). ↑/↓ and Enter open one; Esc or a click outside closes it. On a phone it fills the screen.
- For a person who may search records, the same dialog lists records between Pages and Account once the query is two characters long: a group per kind (Contacts, Companies, a Domain's kinds), each row the record's name with its facts underneath, a record that is no longer current marked with its state, and "All results" for the `/search` page. Records come from the server (`apps/base/search/README.md`); "Nothing found" waits for them.
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
- **Help text** (subtitles, section descriptions, field hints, empty states) is one short sentence a business user would say, telling them what they can do here. Leave it out when the title already says it or no short plain sentence helps; no text beats text that leaves the reader wondering. No internal terms (actor, tenant, scope, principal, capability, operation, contributed, registered). Keep a rule the reader needs, such as "This cannot be undone." Say "2FA", not "two-factor" or "MFA".

| Before | After |
| --- | --- |
| Which operations ask for the account holder's 2FA code before they go ahead | Set the actions that require 2FA. |
| Every live company in this tenant | *(removed: the title says it)* |
| Lowercase letters, digits and underscores. Unique within this tenant. | Lowercase letters, digits and underscores; must be unique. |

## Accessibility and resilience

- Keyboard use, visible focus, readable contrast, semantic structure.
- Loading, empty, error, disabled and permission-denied states are part of the design.
- `prefers-reduced-motion: reduce` collapses CSS motion at the platform level. Components add no motion variants.
- Pages stay understandable when a connection drops. Server-rendered content stays, and reconnection is announced.

## Planned

| Item | Status |
| --- | --- |
| Base events (`apps/base/events`) | In progress in a parallel change |
| Sign-in attempt throttle | Not built |
| Distributed notification fan-out | Not built. Broadcasts are process-local |
| TLS for PostgreSQL connections | Not built |
