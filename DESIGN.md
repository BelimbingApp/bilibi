---
version: stable
name: Bilibi
description: Architecture and UI design reference for Bilibi, the Rust port of Bilimbi.
---

# DESIGN.md

Bilibi is warm, compact workflow software for long operational sessions. This file is the design reference. Agents also read `AGENTS.md`.

- **Parents:** Belimbing (Laravel) gives the business meaning. Bilimbi (Phoenix) gives the behaviour, the DOM and the module topology.
- **Status marks:** *built* means the code exists and was checked. *planned* means it is not built yet.
- **Authority:** where this file and the code differ, the code wins. Fix this file.

| Section | Covers |
| --- | --- |
| [Architecture](#architecture) | layers, modules, composition, data, audit, security |
| [UI design](#ui-design) | character, tokens, components, pages, shell, feedback |

---

# Architecture

## Layers

Four roots in one Cargo workspace, plus the web host. Dependencies point downward only.

| Layer | Path | Role | Mounted from |
| --- | --- | --- | --- |
| Base | `apps/base/<module>` | Infrastructure and cross-cutting mechanisms | this repository |
| Core | `apps/core/<module>` | The required enterprise domain | this repository |
| Domains | `apps/domains/<container>/<module>` | Optional business capabilities | one repository per container |
| Extensions | `apps/extensions/<container>/<module>` | Operator-chosen adaptations | one repository per container |
| Web | `apps/web` | Executable host: routes, sockets, seed command | this repository |

- Order is fixed: Base, Core, Domains, Extensions.
- Base never depends on Core.
- Domains and Extensions are empty in this repository. *built*: mounting, pinning and CI (`apps/domains/AGENTS.md`).
- Base and Core own no business code in their composition crates. `apps/base`, `apps/core`, `apps/domains` and `apps/extensions` are generated linkage only.

## Deep modules

A module is one Cargo crate with one facade. Callers use the facade and never see tables, queries or files.

- **Facade:** the crate root exports one service and its public types. Everything else is private.
  - *Status:* the rule is in force. Several older crates still export extra `pub mod`s. They are narrowed as they are touched.
- **Ownership:** a module owns its migrations, SQL, templates, routes, README and registered contributions.
- **Identity:** stable IDs `base/<m>`, `core/<m>`, `<container>/<m>`. Other modules and stored data name them.
- **Contract:** each module README is the contract. Do not duplicate it elsewhere.

## Registry and discovery

Modules register themselves. The host keeps no module, route, menu or migration list. *built*

| Piece | Where | What it does |
| --- | --- | --- |
| `Module` descriptor | `apps/base/module_registry` | ID, layer, dependencies, migrations, service factories, submitted with `inventory` |
| `Contribution` | same | Typed, validated entries for a consumer: routes, embeds, downloads, seeds, menu, settings, vocabulary, grid tables, ports, agent operations |
| Boot validation | `Services::install` | Rejects missing, duplicate, upward and cyclic dependencies, duplicate migrations, unknown consumers and wrong payload types |
| Discovery script | `scripts/discover-modules.py` | Generates the four composition manifests before Cargo resolves. `--check` runs in CI |
| Container check | `bilibi.container.toml` | A mounted container declares `id` and `layer`. `bilibi-web` refuses a descriptor that differs from the mounted crates |

- A service factory resolves only its own module and its declared direct dependencies.
- Contributions carry module, file and line provenance, so errors point at the owner.
- Details: `apps/base/module_registry/README.md`.

## Dependency rules

| Edge | Rule |
| --- | --- |
| Base to Core | never |
| Domain to Base or Core | allowed, declared |
| Domain to Domain | never. Cross-domain flows use Base domain events |
| Domain to Extension | never |
| Within one Domain | minimal, declared, acyclic, and only where Bilimbi has the same edge |
| Optional call into an unmounted module | `base/ports`, only where Bilimbi has the same seam |
| Extension | adapts through public contracts. It must not make a module fail when absent |

- *planned:* Base domain events at `apps/base/events`, in progress in a parallel change. Design and contract live there once it lands. Until then, no cross-domain mechanism exists.
- *built:* `base/ports` (`apps/base/ports/README.md`) lets a consumer call a provider by ID with no Cargo edge. Boot refuses a port named outside its module.

## Containers

- A Domain is its own repository, `BelimbingApp/bilibi-dom-<id>`, cloned to `apps/domains/<container>`. Extensions use `bilibi-ext-<id>` under `apps/extensions`.
- It is a nested checkout. It is not a submodule and not a symlink.
- The container root has `bilibi.container.toml` and one folder per module. Each module is a crate named `bilibi-<container>-<module>`.
- A company build pins its composition with `scripts/discover-modules.py --pin` and rebuilds it with `--restore`.
- Domain CI is `docs/domain-ci/ci.yml` and `scripts/domain-ci.sh`.
- Domain code uses meta-terms (material, resource, operation, role). Industry names are configuration data.

## Installed modules

*built.* Each has a README.

| Layer | Modules |
| --- | --- |
| Base | `agent_api`, `artifacts`, `audit`, `audit_views`, `authz`, `dashboard`, `database`, `datetime`, `design_library`, `grid`, `integration`, `locale`, `mail`, `media`, `menu`, `module_registry`, `notifications`, `party_ids`, `perf`, `ports`, `principal_directory`, `queue`, `schedule`, `security`, `session`, `settings`, `settings_editor`, `system`, `tenancy`, `tiling`, `ui`, `workflow` |
| Core | `address`, `affiliation`, `agent_access`, `company`, `geonames`, `party`, `person`, `user`, `user_administration` |

- `base/security` is a compatibility facade over Session, Tenancy and Authz.
- Not ported: a plugin system, a Laravel compatibility layer, adoption of a Belimbing database.

## Web host

The host is generic. Modules plug into it.

- Stack: Axum on Tokio, Askama templates, a native WebSocket adapter. No SPA framework and no client model.
- Pages are contributions. A module registers a `Route` that names a `PageService`. The host checks access, then the owner renders. *built*
- Access kinds: Public, SignedIn, Capability, Operator, OperatorCapability.
- Downloads are `DOWNLOADS` contributions, GET only, never public.
- Live pages: the server renders HTML, the browser patches it by element id, and every intent is queued and acknowledged by request ID. Contract: `docs/live-pages.md`.
- Agent edge: `/api/v1`, `/mcp`, device-code OAuth. Both resolve to the `base/agent_api` registry.
- Details: `apps/web/README.md`.

## Data principles

- **Fresh schema.** Bilibi creates its own PostgreSQL 18 schema. It does not adopt a Belimbing database.
- **Migrations** are embedded SQL in the owning module, ID `<layer>-<module>-NNN-<slug>`. They run only on an explicit command.
- **Tenant first.** Every statement binds its data and carries its tenant predicate.
- **Soft delete** is an explicit query policy per owner.
- **UTC storage.** `base/datetime` owns display policy: timezone, clock mode, periods.
- **No unbounded reads.** Page, or ask PostgreSQL for the aggregate.
- **Private files** go through `base/media`. Jobs go through `base/queue`. Outbound HTTP goes through `base/integration`.
- **Settings over constants.** Operator-settable values are settings with an editor. Secrets are encrypted and never shown back.

## Tenancy and identity

- **Tenant** is the outer isolation boundary. **Company** is the inner organisational boundary.
- Exactly one tenant is the platform operator, recorded in a table. IDs have no meaning, and `grant_all` is not operator proof.
- `Scope` is the sealed proof of one unit of work: tenant, actor, optional impersonator. It has no public constructor and no `Deserialize`.
- Modules take a `Scope`, never a raw tenant or actor ID.
- The party model: an account links to a Person, and through an affiliation to a Company. Every session check re-proves those links through their owners.
- Archiving a company freezes its accounts for good. There is no restore.

## Writes and audit

*built.* One path for every business write.

1. A module opens a transaction on `MutationBoundary`.
2. It writes and returns each affected record as a `Change`.
3. Audit captures the change inside the same PostgreSQL transaction.
4. After commit, registered callbacks run (notifications, caches).

- A refused audit row fails the business commit. There is no best-effort mode.
- Evidence tables (`base_audit_mutations`, `base_audit_actions`) are append-only. PostgreSQL refuses UPDATE, DELETE and TRUNCATE.
- Secrets are redacted by field name. Rows outlive actors and records, with no foreign keys.
- Impersonation records both the actor and the operator.
- Contracts: `apps/base/database/README.md`, `apps/base/audit/README.md`.

## Security model

| Concern | Rule |
| --- | --- |
| Authentication | Server-held random sessions, Argon2id passwords, 24 h absolute expiry, idle expiry from settings |
| Session cookie | `HttpOnly`, `SameSite=Lax`, `Secure` on HTTPS |
| CSRF | Non-GET requests need the configured Origin. An authenticated scope also needs `X-CSRF-Token`. The token rotates on impersonation |
| Authorization | Declared capability vocabulary. Unknown key denies. Direct deny beats every grant. Platform capabilities need operator proof |
| Decisions | Each capability check writes a tenant-scoped decision-log row |
| Field masking | Operator-set restrictions mask fields before rendering, in audit values and JSON projections |
| Re-check | Access is read afresh. The owner re-authorizes inside its transaction |
| Headers | Strict CSP: same-origin scripts and styles only, no inline script |
| Files | Private disks, content-type checks, signed streams, `nosniff` |
| Agents | Machine access goes through `agent_api` with the same `Scope` rules. System scopes cannot enter |
| Known gaps | Sign-in has no attempt throttle yet. PostgreSQL connections use `NoTls`. Broadcasts are process-local. *planned* |

## Quality gates

- No unit tests. The gates are the compiler, Clippy, discovery and running the app.
- Commands are in `AGENTS.md`. Running-app evidence: `docs/runtime-parity.md`.

---

# UI design

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
