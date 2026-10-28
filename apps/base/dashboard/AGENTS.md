# Dashboard agent notes

- Contribute an entry with `bilibi_dashboard::contribute!` plus an `Embed` whose context is `EmbedContext::Dashboard`, not a card or page of your own on `/dashboard`: the page mounts each entry as an independent server-declared panel, so permission loss, refresh and drag order work per entry; `README.md` is the contract and `apps/web/examples/dashboard-reference.rs` a runnable specimen.
- A panel renders one root element whose id is `PanelArgs::id()` and reads through `PanelCache`, not on every `update`: the host refreshes every panel on every parent update and on its 15 s tick, so an uncached panel turns each Customize click into a read.
- Never rename an entry id or rewrite the `ui.dashboard.*` keys: ids are stored in each account's arrangement.
- The page keeps each panel in a keyed `data-live-ignore` slot (`slot-<id>`) inside a keyed wrapper; drop the keys or the ignore and a page patch blanks the panels until the next fragment.
- Entries that exist only at run time (saved reports) go in a `bilibi_dashboard::provide!` `Provider`, not a `contribute!` per record: the page asks it on every update and the `entries` function takes its own pooled client, so never hold a pooled client across that call (concurrent page updates would starve the 16-connection pool); `README.md` has the contract.
