# Module live pages and panels

The shared contracts are in `apps/base/ui/src/page.rs`; socket dispatch and guards
are in `apps/web/src/live.rs`. Company implements the contracts in
`apps/core/company/src/web.rs`. Base UI keeps the context/error types generic so
it does not import Security or Core.

Contribute the same `PageService<Scope, Error>` used by HTTP. Implement
`Page::mount` to return a fresh `LiveState` for each socket mount; enable
`PageBody.live` only for a page with a mount adapter. Store typed business state
in that owner, decode a bounded module event enum there, and render escaped HTML
through its templates. The default mount adapter declines live operation.

Every update receives a freshly guarded scope and the server-owned credential.
The owner still authorizes record reads and mutations through its service APIs;
a route capability or a parent page is not a record/subject proof. State should
change only after validation and successful reads/writes. A transient invalid
intent or edit conflict preserves the mounted state. A revoked session, denied
parent route, missing parent record or unavailable parent data clears the page
and closes the socket. A session ended on purpose (logout, an operator's
termination, a credential change) is announced by Base Session; every socket of
that session is sent back through the page it holds and closed, and the page's
guard forwards the browser to sign-in with the expired-session message
(`apps/base/session/README.md`).

## Transport

`/live` validates cookie and Origin and proves the signed-in session. It does not
require a Company capability. The page is selected by the discovered GET route,
using the same literal/parameter precedence as HTTP, and its framing/access
requirements are checked before mount. Paths are local absolute paths, without
query or fragment; query values travel separately. Percent-encoded path segments
are decoded once and encoded path delimiters, control bytes and invalid UTF-8
are refused. Public HTTP pages remain independent of this authenticated socket.

Client intents use these shapes (an optional unsigned `request_id` is echoed):

```json
{"type":"mount","path":"/companies","params":{"q":"Company"},"request_id":1}
{"type":"navigate","path":"/companies/1","params":{},"request_id":2}
{"type":"params","params":{"page":"2"},"path":"/companies","request_id":3}
{"type":"event","target":"page","payload":{"type":"sort","field":"name"},"request_id":4}
{"type":"event","target":"counter-a","payload":{"type":"increment"},"request_id":5}
```

Mount/navigation creates new state and drops old state/subscriptions. Params
updates the current owner; its optional `path` is the browser's pathname after
Back or Forward, which keeps the mounted state when the same service and
operation serve it (its route parameters join the params, after the query) and
mounts the named page otherwise. A page may also move itself among its own
routes by returning `LiveBody.path` (the workspace opening a saved layout at
`/workspace/{slug}`): Web resolves it, requires the mounted page's service and
operation, guards the new route's access and refuses any other page's path. `page` targets its state; any other module target must
be a mounted panel ID: a server-declared page panel, or a shell panel. Outside
framed pages Web mounts every `EmbedContext::Shell` contribution once per socket
under its element id (slot `shell.notifications` is `#app-notifications`, the
bell) and drops it when its access is refused. Client payloads cannot
choose an embed, operation, service, tenant, account or record binding.

Responses are `page` (complete `#app`, canonical URL, notice and row count), `fragment` (one panel's complete root element with its existing ID), or `error` (message, target and clear flag). Accepted user events echo the request ID; background updates use null. A rejected intent with a valid request ID also acknowledges that ID. Canonical query values are encoded by Web, never supplied as a client URL. A mount or Back keeps the browser's address while the page's canonical query only adds values it left out, so a list opened at its bare path stays there (Bilimbi pushes a patch only on change); a page that drops or corrects a value the address carries replaces it with the canonical one. After that the URL is written only when the canonical address changes: user navigation/query changes push history, background refresh and history corrections replace it. A popstate that changes only the fragment leaves the page as mounted, since a remount would drop what was typed, so an ordinary `href="#id"` link keeps its native behaviour. A link marked `data-focus-link` (an error summary's link to its field) takes focus to its target instead, opening any closed `details` around it, with no history entry. Browser Back remounts the historical URL, except within a page that owns panel query namespaces (same pathname only) or marks an element `data-history="params"` (the tiled workspace): there a Back or Forward is a `params` update of the mounted state, Bilimbi's `handle_params` on a live patch, so the DOM it keeps is reconciled rather than rebuilt. A page inside a workspace frame never adds a history entry: its address changes are replaced, a plain same-origin link click becomes `location.replace`, and `bilibi:location` is raised on its `window` after each address change so the workspace can record where the tile went.
`page` and `fragment` also carry the owner's `LiveBody.events`, which the
browser dispatches after patching as `window` events `bilibi:<name>` with the
pushed detail (Bilimbi's `push_event`); the stored-secret reveal uses
`secret:reveal`. Never push a value the owner has not just reauthorized.

A reply that sets `LiveBody.redirect` to a local path may also set `LiveBody.flash`
(kind plus the source's copy: Bilimbi's `put_flash` + `push_navigate`, e.g.
`Success`, "Company created successfully."). The host holds it for that session
(`apps/web/src/landing.rs`, 60 seconds, text never leaves the server) and the
session's next navigation to that path renders it once in the flash group. A
flash without a redirect is dropped; an OAuth callback never carries one.

The browser adapter supports `data-target` on event buttons/forms; a button or
form without one reports to the nearest `data-target` ancestor (a mounted
panel's root element), else to the page. It also supports these DOM events:

```js
document.dispatchEvent(new CustomEvent('bilibi:event', {
  detail: {target: 'counter-a', payload: {type: 'increment'}}
}));
document.dispatchEvent(new CustomEvent('bilibi:navigate', {
  detail: {path: '/companies/1', params: {}}
}));
```

The socket processes events sequentially. Messages are limited to one upload
chunk frame (`bilibi_web::live::FRAME_LIMIT`, 64 KiB plus a header); every
JSON intent is far smaller.

## Browser adapter

`apps/base/ui/assets/live.js` is the browser half. It owns four behaviours:

- **Queue and request IDs.** Every intent is appended to a queue and sent in
  order with the next `request_id`; nothing waits for or is dropped by an
  in-flight round trip, because the server processes one socket sequentially
  and echoes the ID on each reply. Intents raised before the socket is open or
  while it is down are sent after the next mount. A reply or error whose
  `request_id` matches acknowledges the intent before anything is patched.
- **Keyed patch.** A `page` reply is reconciled into the existing `#app`, a
  `fragment` into its panel root. Elements with an `id` keep their identity (a
  keyed element found elsewhere under the root is moved, not recreated);
  unkeyed siblings pair by position, and only attributes and text that differ
  are written. The browser's own dirty flags keep typed values, so an unfinished
  draft survives any patch, and the hidden inputs of a form holding a draft keep
  their values (a record version still conflicts on save). The form whose
  submission was accepted is reset to the stored values. On an existing element
  the patcher leaves an iframe's `src`, a `<details>`/`<dialog>` `open` and a
  `data-live-ignore` subtree (except its `data-*` attributes) alone, keeps the
  `aria-expanded` a person gave the trigger of a `data-live-expanded`
  disclosure (a multi-select whose checkboxes raise a `data-change` event stays
  open through the reply, as a LiveView JS command's attribute survives a
  patch), and never
  descends into an iframe; a frame whose id and order are unchanged keeps its
  document, including when keyed siblings before it leave (they are removed
  around it rather than it being moved, since a moved frame reloads). Focus and selection are restored by id when the focused element was
  replaced; scroll positions of `id`-bearing containers and the window are
  restored except after a `navigate`/`mount`, which starts at the top. Hooks are
  mounted and destroyed by the `ui.js` mutation observer; the patcher reports the
  hooks whose attributes it rewrote through `BilibiUI.patched`, so each receives
  one `updated` per patch.
- **Busy state.** The control that raised an intent carries `aria-busy="true"`
  and `data-live-busy` until its acknowledgement, and is not patched meanwhile;
  a server-rendered `aria-busy` is never touched. `data-working` labels (see
  `ui.js`) are restored at the same moment. Page-level round trips (mount,
  navigate, params, Back) show the vendored topbar after 300ms.
- **Reconnect.** A closed socket raises `bilibi:disconnected`, retries with
  backoff (500ms doubling to 5s; `online` and window focus retry at once), then
  remounts the current URL into the existing DOM and sends the intents raised
  meanwhile. Intents that were in flight when the socket dropped cannot be
  confirmed; the status region reports their count. Development reloads instead
  so a rebuilt server serves fresh assets. A `clear` error on the page removes
  `#app`, offers sign-in and stops reconnecting.

Forms raise intent with `data-event`: `params` sends the form values as a
params update, `save` the Company edit shape, and any other name an event of
that type carrying the form values; `data-target` picks a mounted panel. A
control named with a `[]` suffix (a multi-select's checkboxes, Bilimbi's list
parameter spelling) posts every checked value as one JSON array under that
name; any other name carries its last value. A form
also marked `data-change` (Bilimbi's `phx-change`) raises the same intent as
its values change: at once for a select, after a control's `data-debounce`
milliseconds of typing, and at once on commit or Enter; identical values raised
twice within 500ms are sent once, and the form is not reset afterwards. As
with LiveView, a reply sets every control of a `data-change` form to the
rendered value except the focused one, so Clear and Back reach the filters
while typing continues undisturbed. Such a form submits as one more change,
unless `data-submit-event` names the event its submission raises instead
(Bilimbi's `phx-submit` beside `phx-change`): the Geonames postcode dialog
validates as values change and saves on Enter or Save. Buttons
send their `data-*` values (`data-page="3"` becomes `page: "3"`), and so does an SVG shape that carries `data-event` (a chart's bar or slice, `bilibi_ui::Chart::drill`), which cannot be a button; only Company's
relative pager (`page` with `data-delta`) and `sort` with `data-field` keep
their fixed shapes. A `bilibi:event` may carry `{target, payload}` or the controller shape
`{event, ...values}`, which becomes `{type: event, ...values}` aimed at the
nearest `data-target` ancestor. A controller that must know when its own
intent is answered (the inline editors' "Saving…") adds `onReply`, a function
that stays in the browser: it is called once, after that reply is patched, with
the reply message, or with an `error` if the socket drops before the reply.
A controller whose intent only records where something already is (the
workspace's frame reports) adds `history: 'replace'`, so a changed canonical
URL replaces the address instead of pushing an entry.

## File uploads

Bilimbi's `allow_upload`/`live_file_input` is `bilibi_ui::upload`
(`apps/base/ui/src/upload.rs`): the owner keeps an `Uploads` in its mounted
state, declared at mount with one `UploadSpec` per file input (name, `accept`,
`max_entries`, `max_file_size`), and returns it from `LiveState::uploads`. It
renders each input with the `forms::file_input` macro through
`Uploads::input`, which lists the entries and errors the state holds, and in
the event that acts on the files takes the completed bytes with
`Uploads::consume` (Bilimbi's `consume_uploaded_entries`); an empty result
means nothing has finished arriving. Nothing else ever reads the bytes, and
they go with the state when it is dropped. The Design Library's File input
card is the running specimen.

The host owns the stream. A chosen file is announced as an `upload` intent
(`{"type":"upload","target":"page","upload":"statement","entries":[{"ref":"1","name":"x.csv","size":1200,"content_type":"text/csv"}]}`),
queued and acknowledged like any other; the host registers each entry under
the target's spec or refuses it (`too_large`, `not_accepted`, and
`too_many_files` for the surplus over `max_entries`, which stays an
input-level error), answers `{"type":"upload", "entries":[{"ref":"1","id":7,"chunk":65536,"done":false}, …]}`
with the id to stream each accepted entry under, and re-renders the target so
the rows appear. A single-entry input replaces its entry on a new choice, as
the browser control does. The browser then sends binary frames of
`u32 id, u32 offset, bytes` (at most `CHUNK_SIZE`), one chunk in flight per
socket; each is answered by `{"type":"chunk","id":7,"received":65536,"size":…,"done":false}`
(no `request_id`), which `live.js` relays as the window event
`bilibi:upload`. A chunk that does not continue where the last ended, or
overruns the announced size, marks the entry `interrupted`. A completed or
refused entry re-renders its owner under the owner's guard; a removal is the
`upload_cancel` intent (`{"type":"upload_cancel","target":"page","id":7}`).
Ids are unique per socket, so a frame never reaches another state's entry,
and a dropped socket ends every stream: the remounted state holds no bytes.
The `FileUpload` hook (`apps/base/ui/assets/upload.js`) does the browser's
part and holds a form's submit until every file of its input has arrived.

## Browser location

Bilimbi's colocated `.ClockLocation` hook (People Attendance `web/my_live.ex`)
is `bilibi_ui::LocationAction` (`apps/base/ui/src/forms.rs`, macro
`forms::location_action`), since a Domain owns no JavaScript. The owner
renders a button with its event, label and any `value(key, value)` pairs the
event should carry; the `LocationAction` hook (`apps/base/ui/assets/hooks.js`)
asks `navigator.geolocation` on click (high accuracy, no cached fix, 15 s
unless `timeout_ms` says otherwise) and raises one `bilibi:event` per click:
the values plus `latitude`, `longitude` and `accuracy` (metres), or the values
plus `outcome` `permission_denied`, `unavailable` or `insecure_context`. The
button is busy (disabled, `aria-busy`, `data-live-busy`) with the `working`
label while the browser locates, with `sending` once the event is raised, and
settles when the owner's reply arrives, then takes the disabled state the
reply rendered. The owner decodes the payload with
`LocationReport::from_event` (a position, or `None` for a payload this control
did not raise; coordinates off the globe are `Unavailable`) and answers a
refusal in words through `LocationReport::refusal`, adding the page's own way
out. The event's `type` is the event name, so a value key is never `type`,
`outcome`, `latitude`, `longitude` or `accuracy`. The Design Library's Browser
location card is the running specimen; `/assets/ui-components.html` shows the
variants.

## Server-declared panels and background updates

`LiveState::panels` returns up to 32 unique `PanelSpec`s: stable HTML element ID,
contributed embed ID and owner-supplied path/query arguments. Live embeds use a
`PageService<Scope, Error>` service and implement mount; their render body must
contain the root element named by the panel ID. The owner must validate the
embed's subject arguments on every update. Shell embeds use the same contract; Web
renders them into the shell with `Page::render` and mounts them with
`Page::mount` (`apps/web/src/shell.rs`, `live.rs`).

Each instance owns separate state, even for the same embed/service. An unchanged
spec preserves that state across parent refresh; a changed or removed spec drops
it. Parent-route access and panel access are rechecked before panel events and
background callbacks. A panel refusal removes its state and sensitive HTML while
retaining an empty DOM anchor, leaving the parent and siblings usable. A restored
permission can mount it again during a parent refresh.

An embedded list can override `LiveState::query_prefix` with its `ListSpec.param_prefix` (a stable namespace ending in `_`) and return `ListState::to_params()` as `LiveBody.query`. Web replaces only that namespace, preserving the page and sibling panels' keys. Prefixes must not overlap, and a page cannot return panel-owned keys. Web sends `Params` before a new panel's first render and on history restoration; replace only `ListState`, retaining mounted columns and the subject from `PanelSpec.path`. Ordinary parent refreshes send `Refresh`. Shell panels cannot own query keys. Empty prefixes keep local-only state. Prefer `omit_defaults` for embedded lists so an untouched sibling adds no defaults to the address. This mirrors Bilimbi Company ShowLive's `users_` list state without making the parent depend on discovered owners.

Return `Subscription`s from the mounted state's `subscriptions` method. Publish
`Change` only after a business transaction commits, with its tenant and optional
recipient account. Web forwards wakes through a bounded 64-entry channel,
rechecks scope/access, discards other tenants/recipients and stale mount
generations, then refreshes only the target. Broadcast lag causes a guarded
refresh rather than treating a lost message as current data. Dropping the mount
aborts subscription tasks and releases receivers. Modules that own additional
tasks/resources must release them when their state drops.

A notification is such a `Change` with its recipient: it wakes that account's
bell panel and `/notifications` state only, never the page underneath. The
15-second fallback revalidates the page and child panels (and re-renders the
shell, so a notification another process wrote shows there). Neither notification, subscription nor
timer refresh records session activity. Only accepted user navigation/events do.

## Running reference specimen

Use an isolated migrated database as described in
`docs/runtime-parity.md`, then run:

```sh
DATABASE_URL='<isolated database URL>' \
APP_ORIGIN='http://127.0.0.1:4315' BIND_ADDR='127.0.0.1:4315' \
cargo run -p bilibi-web --example live-reference
```

Log in with a walkthrough fixture account, visit `/live-reference`, increment either panel and
announce from the page. Each counter retains independent state and subscriptions;
Company pages are served by the same app. This is a manual specimen command,
not a test suite or a product route. Its two panels require the fixture capability `companies.view`;
the parent requires only sign-in, allowing independent revoke/restore and
Company-denied socket demonstrations. The normal app does not register this route.

The page also carries the browser-adapter checks: click Increment twenty times
as fast as possible and the count reaches twenty; type in the scratch note
without pressing Keep, announce or wait for the 15-second refresh, and the typed
text, caret and focus stay; scroll the box and the scroll position stays through
the same patches; increment inside the island iframe (`/live-reference/island`,
its own live page) and its count survives every parent patch because the frame
is keyed by id and never replaced.

F5 runtime evidence lives outside the repository in the task evidence directory.
Company list/filter/Back/save, targeted panel identity, subscription wakes,
independent permission loss and unchanged background `last_activity` were
observed through Chrome and the disposable database. Bilimbi's running reference
returned HTTP 500 (`ArgumentError at GET /`), so an equal-fixture visual comparison
could not be completed. No visual-parity claim is made by this transport slice;
its Company HTML/templates/styles are unchanged.
