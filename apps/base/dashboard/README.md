# bilibi-dashboard

The workspace landing page at `/dashboard`. Installed modules contribute its
widgets and sections; this module owns the catalogue, the page and each
account's arrangement, and reads no contributor's tables (Bilimbi ADR 0009).

## Contributing an entry

Two declarations in the module that owns the data (list `base/dashboard` in its
dependencies):

```rust
bilibi_dashboard::contribute!(
    "core/orders",
    Widget::new("orders-open", "Open orders", "orders.dashboard-open") // id is stored in each account's layout: never rename
        .order(60).requires("orders.list").refresh(60_000)            // .section() / .size() are optional too
);
inventory::submit! { Contribution::new(EMBEDS, "orders.dashboard-open", /* provenance */, &Embed {
    slot: "dashboard.orders-open", context: EmbedContext::Dashboard,
    service: "core/orders.dashboard", operation: "open", access: Access::SignedIn,
}) }
```

Boot fails when an entry names an embed no module contributes with the
`Dashboard` context, a capability the vocabulary lacks, or a duplicate id. The
entry's capability *offers* the entry; the embed's `access` still guards every
panel mount and refresh.

## Run-time entries (providers)

Entries that exist only at run time (an operator's saved reports) come from a provider instead of `contribute!`:

```rust
bilibi_dashboard::provide!("base/reports", Provider {
    prefix: "report",                 // entry ids are report-<key>: never rename
    embed: "base/reports.dashboard",  // an Embed with the Dashboard context
    placement: Placement::Grid, capability: Some("reports.view"), refresh_interval: 60_000,
    entries: panel::entries,          // fn(&Scope, placed keys) -> the reader's entries (key, label)
});
```

- The page calls `entries` with the reader's sealed scope and the keys of that provider's entries the reader has placed, on every update; the owner answers only what that reader may see. Every placed key it still answers stays, however many entries sort before it; at most `MAX_PROVIDED` others per provider are offered.
- Provided entries are offered in Customize and placed only by the reader, never by default, so a new report does not appear on everyone's dashboard. A placed one that is no longer offered drops out like a contributed one.
- A page holds at most 32 panels; adding past that is refused with a message.
- Boot fails for a prefix that begins a contributed entry's id, a duplicate prefix, an embed without the `Dashboard` context or an unknown capability.

## What a panel is

The embed's service is a `PageService<Scope, Error>` whose `mount` returns a
`LiveState` (one independent instance per panel). `PanelArgs::from_request`
reads what the dashboard passed (`editing`, `refresh`); `update` renders **one
root element whose id is `PanelArgs::id()`**. Withhold links while `editing` is
true (the page lays move/remove controls over the corner). `PanelCache` keeps
the last rendering so the dashboard's frequent refreshes do not become reads:
it answers the stored HTML until `refresh_interval` passes (for good when it is
zero). Web refreshes every panel on every parent update and on its 15 s tick,
so that tick is the floor of `refresh_interval`.

## Behaviour

- **Arrangement** is the per-user `ui.dashboard.layout` (grid) and
  `ui.dashboard.sections` settings, written in an audited transaction. No stored
  row shows the whole authorized catalogue; a stored row keeps its order and
  drops ids the account can no longer see.
- **Editing** (Customize/Close) is mounted state. Add, remove, move and drag
  reorder persist immediately; a drag is validated as an exact permutation of
  the current ids, so a stale or forged report changes nothing.
- **Permission loss** is per panel and rechecked on every update: an entry whose
  capability is gone leaves the page (its panel state is dropped) and returns in
  its stored place when the grant does; an embed whose own access is refused
  empties only its anchor while siblings keep running.

## Try it

`apps/web/examples/dashboard-reference.rs` contributes five entries (read once,
every second, capability-gated, guarded embed, a section). Run it as described
in `docs/runtime-parity.md` and open `/dashboard`:

```sh
cargo run -p bilibi-web --example dashboard-reference
```
