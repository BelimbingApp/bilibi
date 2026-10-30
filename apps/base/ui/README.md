# Shared UI

`base/ui` supplies the shared components, form controls and browser behaviour used by every Bilibi page.

Use the typed models exported by the crate or their Askama macros; [`DESIGN.md`](../../../DESIGN.md) sets the design rules and [`AGENTS.md`](AGENTS.md) records the recurring mistakes. The DOM mirrors Bilimbi's `UI.Components` and `UI.Components.Forms`, with Bilibi's floating labels.

Single selects and multi-selects share the 16 px outline chevron and right alignment. Single selects retain native keyboard and popup behaviour; multi-row selects have no dropdown chevron.

Multi-selects show the inset label and selected count above removable chips inside one field. The default keeps chips on one line with “…” for hidden selections; `.wrap_chips()` lets the field grow onto multiple lines. The empty prompt defaults to “Select options…”. Removal uses the checkbox's normal change event; `.chips(event)` can retain an owner's explicit remove event. Every selection remains available in the dropdown, including chips hidden by overflow.

List toolbars put search first and filter boxes to its right, wrapping below search on narrow screens. `FilterToolbar::new` preserves the order of filters while placing search first.

Page headers include a pin for the current address in signed-in pages, including workspace tiles. The shell keeps its state with the account. Dialogs do not show it.

## Searchable multi-select

Use `SearchMultiSelect` for a server-searched set of records. Supply selected `SearchMultiOption`s separately from a bounded list of results; the control renders at most 50 hits. An option may carry detail or be disabled with a note and an optional local link shown beside the note. `.create("Create company", "create-company")` adds the trailing action. `.wrap_chips()` shows all selected chips; the default limits the row to one line and offers an overflow button that reveals the remaining chips.

The owner handles the configured search event with `query`, pick and remove events with `value` (pick toggles that value), and the create event with `query`. Search is debounced 300 ms; results for an older query cannot be selected while the person types; the control compares the echoed query with the input ignoring surrounding spaces, so the owner may trim it. Keep selection in the mounted owner across searches, check each event against current authorization and availability, and recheck on save. The control posts selected values under its array name; the empty value denotes no selection. Bound the owner's query and database result count before rendering. The control never downloads or filters the full record set.

The Design Library’s searchable multi-select card exercises a mounted owner and server-side search. Source: [`src/search_multi.rs`](src/search_multi.rs), [`templates/search_multi.html`](templates/search_multi.html), and `SearchMultiSelect` in [`assets/hooks.js`](assets/hooks.js).

## Charts

Use `Chart::new_with_display_values` when values have a stored decimal scale: pass owner-formatted strings indexed by series and category alongside the numeric values used for geometry. The Data table, mark titles and KPI use those strings; absent strings fall back to the numeric value. Missing numeric values remain gaps. `Chart::new` is suitable when the float itself is the intended display value. Pie and donut totals are calculated from numeric values and displayed to 15 significant digits.

## Icons

**Use one bare icon name in menus, templates and Rust callers**, such as `building-office-2`, `edit` or `bookmark-solid`.

```rust
let edit = bilibi_ui::Icon { name: "edit", class: "size-4" };
```

Templates call the shared `ui::icon` macro with the same name. Prefer a semantic action such as `edit` or `manage` for a familiar action. The Design Library’s Graphic page lists every accepted name and copies it to the clipboard.

| Name | SVG variant | Natural size |
| --- | --- | --- |
| `check-circle` | Outline | 24 px |
| `check-circle-solid` | Solid | 24 px |
| `check-circle-mini` | Solid mini | 20 px |
| `check-circle-micro` | Solid micro | 16 px |

The macro defaults to `size-4` (16 px); pass a size class to change it. If you supply classes without a size, the SVG keeps its natural size. Icons inherit `currentColor`; outline strokes stay at 1.5 units. Busy buttons use the same `arrow-path` SVG with their existing motion class.

`Icons` is the public catalogue facade; `Icons::is_known` asks whether a name resolves without the warning. Cargo embeds the vendored Heroicons and product SVGs at build time, preserving path fill and clip rules. Semantic action and shell mappings live in `assets/vendor/product-icons/catalog.json`. There are no runtime file reads, CSS masks, icon plugins or icon safelists.

**An unknown name renders `missing-icon`**, a neutral dashed box in the caller’s size and colour, and logs one warning per name. At boot the host checks every menu contribution and includes its module in the warning; later rendering does not repeat that warning. Multiple contributing modules each get their own diagnostic. Missing icons never stop boot or panic during rendering. Names are exact: a prefixed or misspelled name gets the placeholder.

## Shell feedback

Theme and time-display saves use the shared flash UI through `feedback::local_flash_group`. Success dismisses after eight seconds; saving information and errors remain until replaced or dismissed. Browser-owned flashes dismiss locally without clearing the page’s server-owned flash. Stat-strip value labels use `ink-muted` so they remain readable in both themes.

## Form size

**Every live form shares a 1 MiB limit on its complete JSON event.** The browser measures UTF-8 bytes after JSON encoding, including the request ID. A larger event stays in the browser, keeps its draft and shows a persistent error flash. The host enforces the same limit before calling a page or panel.

- Match each field's `maxlength` to its owner's validation. Character limits are not byte limits: CJK commonly needs three UTF-8 bytes per character, and JSON escaping can need six bytes per UTF-16 code unit.
- Budget the whole form: all field names, values, selected arrays, hidden values and transport metadata. Leave headroom below `bilibi_ui::LIVE_EVENT_LIMIT`; two 10,000-character fields fit even when every code unit needs escaping.
- Existing Platform declarations, including Address's 10,000-character raw input, fit this budget. New repeating forms must bound their total content as well as each field. Use the shared file uploader for larger documents; never put file bytes into JSON.
- **A field's text limit belongs to its owner; read posted text with the checked extractors.** `params::form_values_checked(payload, form, limit)` and `params::string_values_checked(payload, limit)` return `Result<_, params::TooLong>`: a string over `limit` characters is an error, not an empty field. Pass `params::FIELD_LIMIT` (1,024) unless the owner allows more (Address 10,000, a saved query 100,000). Refuse the event with `.map_err(Error::refused)?` (`bilibi_database::Error::Refused`, shown as its message, nothing cleared) and the browser keeps the draft; the owner's validation still reports each field's own, shorter limit on the field. `params::form_values` and `params::string_values` keep their old signatures and silently drop a string over 1 KiB; they are deprecated, so move to the checked ones. `PeriodFilter::event` is deprecated the same way; use `event_checked`. Code that reads `payload` keys directly keeps its own limits.
- Keep using `Form`, `Field` and the shared adapter. Forms, change events, hooks and mounted panels receive the same protection without a module-owned size check or script.

The constants and refusal copy live in [`src/live_transport.rs`](src/live_transport.rs). [Live pages](../../../docs/live-pages.md#transport) defines the server's additional socket cap and reconnect behaviour.

## Colour input

Use **`ColourInput` for a choice of named colours**. Each choice is a chip in that colour's banner surface, border and text colour, with the name inside.

Build it from the form's `Field` and the allowed choices:

```rust
use bilibi_ui::{ColourChoice, ColourInput, ColourRole, Field};

let colour = ColourInput::new(
    Field::named("banner_colour").label("Banner colour").value("info"),
    vec![
        ColourChoice::new("Use role colour", "", ColourRole::Info),
        ColourChoice::new("Blue", "info", ColourRole::Info),
        ColourChoice::new("Amber", "warning", ColourRole::Warning),
    ],
);
```

The label is what people read; the value is what the form submits. The owner supplies the default choice's actual colour. Render with `Html::render(&colour)` or `forms::colour_input(control=colour)`.

- `ColourRole` permits the status roles from `DESIGN.md`. The chip uses the same light and dark surface, border and text tokens as the banner, without inline styles or fixed colour values. `ColourInput` adds the default colour's name to the choice with an empty value, so "Use role colour" shows as "Use role colour (Blue)".
- The field label stays small inside the field. `Field` supplies the hint, errors, required and disabled state.
- Arrow keys, Home, End and typing a colour's name move through the list. Enter or Space selects; Escape cancels and Tab leaves the field. Each option keeps its plain name for screen readers.
- A native select owns the form value and reset behaviour. Without JavaScript, the named select remains available.

The source is [`src/colour.rs`](src/colour.rs), the macro is in [`templates/forms.html`](templates/forms.html), and the browser behaviour is `ColourInput` in [`assets/hooks.js`](assets/hooks.js). The running specimen at `/assets/ui-components.html#specimen-colours` includes inherited, invalid, disabled and required fields.

## Inline form cells

Use `InlineForm` when a read-first table cell needs a complete form, including secret inputs or 2FA, rather than a text fact's automatic save.

- Supply the displayed value, an owner-rendered `Form`, and the open and cancel events. Opening reports the cell's `key`.
- The owner holds `editing` and non-secret drafts, rechecks access on each save, and closes only after a successful write. Pass a refusal through `CommitStatus` so it remains beside the cell.
- Save and Enter submit the form. Cancel and Escape discard the draft. The shared hook focuses the editor on opening and returns focus to the value on closing.
- Keep secret plaintext out of the read value, hint and status. Use `SecretInput::stored` in the form.

Source: [`src/inline.rs`](src/inline.rs), [`templates/inline.html`](templates/inline.html), and the hooks in [`assets/inline.js`](assets/inline.js). `InlineField` supports native text and date inputs; use `.input_type("date")` with `.display_value(...)` to keep the canonical editor value separate from read text. Date facts commit when the date changes. The Design Library includes reading and refused-editor states; `/system/settings` exercises the live lifecycle.

## 2FA proof input

Use `ProofInput` inside a protected form's `StepUp` component. Build `ProofMethod` values from `StepUp::enrolled_methods`, including each method's code length and numeric flag. It keeps the plain `CodeInput` id for one method (the wrapper is `{id}-proof`), suffixes the ids with the method's position when there are several, takes `.optional()` for a form that asks for a code only on some changes, and adds a shared selector when there are several. The `FactorProof` hook posts the chosen method and code as `method_id:code` in the existing code field, so the owner's event schema stays unchanged. Neither selecting nor rendering a method sends a challenge. The owner still calls `require` or `require_code`.

The specimen at `/assets/ui-components.html#specimen-factor-proof` shows six-digit and eight-character methods. The [Step-up provider contract](../step_up/README.md#provider) owns registration, challenge starts and verification.

## Browser location

Use **`LocationAction` to ask the browser where the person is** before an event that needs it (clocking in, a site visit): one button, no page-owned script.

```rust
use bilibi_ui::{ButtonVariant, LocationAction, LocationReport};

let clock_in = LocationAction::new("clock-in", "clock", "Clock in")
    .variant(ButtonVariant::Primary)
    .value("clock_type", "in")
    .sending("Recording clock event…");
```

Render with `Html::render(&clock_in)` or `forms::location_action(control=clock_in)`. On click the button shows "Finding your location…" while the browser locates, then raises `clock` with `clock_type`, `latitude`, `longitude` and `accuracy`, or with `outcome` `permission_denied`, `unavailable` or `insecure_context`. Decode the event with `LocationReport::from_event` and show `LocationReport::refusal` for a report without a position, adding what the person can do instead on that page. The contract is in [`docs/live-pages.md`](../../../docs/live-pages.md#browser-location); the Design Library's Browser location card is the running specimen.

## Day tiles

A work calendar's resolved day is a `DayTile` (Belimbing's `x-ui.day-tile`; no Bilimbi counterpart): the date as shown, its `DayKind` (`Working`, `Rest`, `Off`, `Holiday`) as a tint and as a word, and plain `lines` such as opening hours or a holiday's name. `description` is the whole day in words for the tooltip and for assistive technology, so the tint is never the only signal. `DayMonth` lays whole weeks of tiles under `WEEKDAYS`, Monday first, as a table that scrolls sideways inside its card when narrower than seven tiles; on a phone (below `sm`) it becomes a list of the month's own dates, each after its weekday. A neighbouring month's date is `outside`: dimmed in the grid, left out of the list. A Domain's roster or leave calendar maps its own day type onto `DayKind` and reuses both. The tints are the `day-*` colour roles (`DESIGN.md`); the running specimen is `/assets/ui-components.html#specimen-day-tiles`.

## Record references

`RelatedRecord` (a named pill through `RecordLink`, or unavailable) and `RecordPicker` (a server-driven type-ahead inside the owner's live `Form`) show and choose a reference to another module's record. Base Records resolves names and hits with the reader's permissions; this crate only draws them: [`../records/README.md`](../records/README.md).

## Where to go next

See [`base/system`](../system/README.md) for Banner colour on Instance settings.
