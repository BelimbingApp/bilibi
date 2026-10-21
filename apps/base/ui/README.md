# Shared UI

`base/ui` supplies the shared components, form controls and browser behaviour used by every Bilibi page.

Use the typed models exported by the crate or their Askama macros; [`DESIGN.md`](../../../DESIGN.md) sets the design rules and [`AGENTS.md`](AGENTS.md) records the recurring mistakes. The DOM mirrors Bilimbi's `UI.Components` and `UI.Components.Forms`, with Bilibi's floating labels.

Single selects and multi-selects share the 16 px outline chevron and right alignment. Single selects retain native keyboard and popup behaviour; multi-row selects have no dropdown chevron.

Multi-selects show the inset label and selected count above removable chips inside one field. The default keeps chips on one line with “…” for hidden selections; `.wrap_chips()` lets the field grow onto multiple lines. The empty prompt defaults to “Select options…”. Removal uses the checkbox's normal change event; `.chips(event)` can retain an owner's explicit remove event. Every selection remains available in the dropdown, including chips hidden by overflow.

List toolbars put search first and filter boxes to its right, wrapping below search on narrow screens. `FilterToolbar::new` preserves the order of filters while placing search first.

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

Source: [`src/inline.rs`](src/inline.rs), [`templates/inline.html`](templates/inline.html), and the `InlineForm` hook in [`assets/hooks.js`](assets/hooks.js). The Design Library includes reading and refused-editor states; `/system/settings` exercises the live lifecycle.

## 2FA proof input

Use `ProofInput` inside a protected form's `StepUp` component. Build `ProofMethod` values from the holder's live enrolled methods, including each method's code length and numeric flag. It retains `CodeInput` for one method and adds a shared selector when there are several. The `FactorProof` hook posts the chosen method and code as `method_id:code` in the existing code field, so the owner's event schema stays unchanged. Neither selecting nor rendering a method sends a challenge. The owner still calls `require` or `require_code`.

The specimen at `/assets/ui-components.html#specimen-factor-proof` shows six-digit and eight-character methods. The [Step-up provider contract](../step_up/README.md#provider) owns registration, challenge starts and verification.

## Where to go next

See [`base/system`](../system/README.md) for Banner colour on Instance settings.
