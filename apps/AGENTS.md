# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Correct entries that work proves wrong; add new ones only by deliberate maintainer choice, never as routine task output.

## Any module under `apps/`

Read this before editing a page, an event handler or a facade. The component that can make a mistake impossible owns the rule: follow its doc comment and its README. This note holds what no component owns. It mirrors the parts of Bilimbi `apps/AGENTS.md` that apply to Rust.

- Take the person who performed an operation (approver, overrider, requester) from the sealed `Scope` (`Scope::actor`, `Scope::mutation_context`), never from an argument, a form field or an ID you look up yourself. A facade that accepts an actor or tenant ID checks whoever the caller names.
- Recheck the capability in the owner's mutation transaction. The host checks route access before you render, but a mount-time flag that hides a control is presentation, not authority; a stale page can still send the event (`apps/web/README.md`). Do not copy the host's session or route checks into a handler.
- A control or field that is absent says why and what to do next: `EmptyState::missing`/`forbidden` or the table's empty slot, never a silent `if` that removes it. A field some readers must not see is restricted by an operator at run time through Authz masking (`apps/base/authz/README.md`), not by a capability in code; build the read model through the masking API and refuse writes to a restricted field.
- Render an instant through `bilibi_datetime` (`Rendered`) so the reader's saved clock and timezone apply. Pin a display only for a context of your own.
- Reach agents through an `agent_api` contribution (`apps/base/agent_api/README.md`), a route through a `Route` contribution, a download through `DOWNLOADS`, a file through `Media`. Add no per-module endpoint and no host-router entry (`apps/web/README.md`).
- A list keeps its page, search, filters, sort and page size in the URL through `ListState`; keep a module's existing query names and page sizes. A private `to_int` or `parse_page` is the copy `Params` and `ListState` replaced.
- To ask the account holder for a fresh second-factor check before an operation, declare it with `bilibi_step_up::declare!` in your own module and call `StepUp::require` from your facade (`apps/base/step_up/README.md`). Never decide in code whether it applies, never import the authenticator, and never let the step-up or two-factor modules name your module.
- Message kind follows the outcome: `Success` only for a completed write, `Info` informs, a refusal names its real cause. Kinds and timing: `DESIGN.md` "Honest feedback".

## Maintaining this file

Keep this file short. Point at the component, its doc comment or `DESIGN.md`; do not copy them.
