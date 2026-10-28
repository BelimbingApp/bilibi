# Inline editing

A record page edits its facts in place: there is no separate Edit button or
`/:id/edit` route. The shared pieces are in `apps/base/ui`:

- `src/inline.rs` — the typed models `InlineField`, `InlineEdit`,
  `InlineLongText`, `InlineChoice`, `CommitOutcome`, and the per-fact outcome
  bookkeeping `CommitStatus`/`CommitStates`.
- `templates/inline.html` — the `inline_edit`, `inline_long_text`,
  `inline_choice` and `commit_status` macros, mirroring Bilimbi
  `components.ex` (`inline_edit`, `inline_choice`, `inline_long_text`,
  `commit_status`).
- `assets/inline.js` — the `InlineEdit`, `InlineLongText` and `InlineChoice`
  hooks, adapted from Bilimbi `inline_edit.js`, `inline_long_text.js` and
  `inline_commit.js`. It is served with the form hooks at `/assets/hooks.js`.

## Lifecycle

The fact is read-first: a trigger button shows the stored value (an em dash, or
the owner's `placeholder`, when empty) with a pencil on hover/focus.
`InlineField::hint` adds one short plain sentence under the fact, like a
field's hint; the trigger and the editor are described by it.

| Fact | Opens on | Commits on | Cancels on |
| --- | --- | --- | --- |
| `InlineEdit` | click, Enter, Space, F2 (caret at end), a typed character (replaces text) | Enter, blur; native date inputs commit on change | Escape |
| `InlineLongText` | click, Enter, Space, F2, a typed character (replaces) | blur (Enter adds a line) | Escape |
| `InlineChoice` | click, Enter, Space, F2 | `change` of the editor control named by the field | Escape, focus leaving the editor |

Enter and Escape return focus to the trigger; a blur commit leaves focus where
the person put it. An unchanged value sends nothing, and an emptied text value
sends nothing unless the owner calls `allow_empty()`. Text is trimmed;
long text and choice values are sent as entered.

A commit raises the field's `save_event` (default `save_field`) through the
queued transport with `{id, field, value, version}`. While its own reply is
outstanding the fact is `aria-busy` and shows "Saving…"; an unrelated patch
never ends that state. The displayed value is always the server's
`data-value`: the hook never paints typed text, so a refused save leaves the
stored value on screen.

## Owner responsibilities

The owner authorizes, validates and persists every intent; the browser draft is
never treated as stored.

1. Render each fact with a stable `id` and pass the stored record `version`
   (`InlineField::version`). Check it atomically when saving: a mismatch is a
   refusal, not a silent overwrite.
2. Re-check the capability on every save. A forbidden write is the whole
   outcome (Bilimbi `write_forbidden/2`): call `CommitStates::clear_saved`,
   re-render the facts the actor can no longer edit with `readonly()` (which
   discards the browser draft) and say why in the `LiveBody` notice. Do not
   return `Error::Forbidden` or `Error::Internal` for a refused write: the live
   transport treats those as fatal and clears the whole page.
3. Record each outcome with `CommitStates::put`, so "Saved" belongs to the most
   recent commit only and a refusal stays on its fact until that fact is
   committed again. Word refusals with `CommitStatus::refusal(label, submitted,
   reasons)`, which repeats at most 60 characters of the rejected value. Never
   pass a secret as `submitted`. A write that failed for a reason with no noun
   of its own reports `CommitStatus::failure()` on its fact.
4. For a choice, render the editor with the shared `Field` select or
   `Combobox` (no `data-event` of its own) and pass its committed label as
   `display`. The choice vocabulary and its validation stay with the owner.
   Give the editor no width of its own: inside a fact it fills the fact's
   floating-field box (`[data-fact]` in `assets/css/app.css`).

## Draft recovery

A refused or unconfirmed commit keeps its draft and the version it started
from. Opening the fact again restores that draft, so the person can correct it;
Escape discards it, and the next edit starts from the latest stored value and
version. A draft whose version has since moved on is refused by the owner as a
stale write rather than overwriting the newer value. If the socket drops before
the reply, the fact reports that the change was not confirmed and keeps the
draft; the stored value decides whether to retry.

## Running specimen

`apps/web/examples/u5-inline.rs` serves `/inline-reference` with an in-memory
record per tenant (no unit-test runner). Run it against an isolated, migrated
database (it installs the walkthrough fixture itself):

```sh
DATABASE_URL=… APP_ORIGIN=http://127.0.0.1:4100 BIND_ADDR=127.0.0.1:4100 \
  cargo run -p bilibi-web --example u5-inline
```

Sign in, open `/inline-reference`, and exercise: commit by Enter and by blur,
Escape cancel, an unchanged value, a too-short name (validation refusal and
reopened draft), `Unavailable` as relationship, `server-error` in a text
fact (failed write with draft kept), "Make readonly" after a refused save (the kept
draft is discarded; a save still in flight is refused with a notice and the
page stays), "Refresh facts" while a draft is open (draft survives), and the
same fact saved from two tabs (stale-version refusal).
