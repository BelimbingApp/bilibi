# List and table parity audit

Every operational table in the Base/Core/Web composition uses the shared FlexTable; page-specific feature choices follow the pinned Bilimbi owners below.

## Reference and browser run

- Reference: [Bilimbi at f1a1e10](https://github.com/BelimbingApp/bilimbi/tree/f1a1e101a89e3b6fea857004ec4567556adbabbb), read-only LiveViews and their inline or adjacent HEEx templates. Paths below are relative to `apps/`; `.ex` identifies the LiveView and its adjacent `.html.heex` where present.
- Business-only fallback: [Belimbing at 80acb0b](https://github.com/BelimbingApp/belimbing/tree/80acb0b7f9f22e808fe6916ac92104f51b7301a4), for the database-table inspector absent from the Bilimbi pin. No Laravel implementation was ported.
- Bilibi before baseline: [c0db458](https://github.com/belimbingapp/bilibi/commit/c0db458); rebased onto main's subsequent documentation-only ADR update before delivery. Run: 17 October 2026, PostgreSQL 18, disposable `list_parity` database populated from `fixtures/walkthrough` plus temporary asymmetric records; no fixture or product seed changes committed.
- Browser: agent-browser Chromium, desktop fine pointer, America/New_York, 1280px viewport, device scale 2. The app was browsed at exactly its configured `APP_ORIGIN`; separate managed services ran the app and list-reference example. No Bilimbi runtime or pixel-equivalence claim is made: reference features were established from source, Bilibi behavior from the running system.
- Coverage: 46 app route/state variants containing 60 distinct FlexTable IDs, plus the standalone list-reference specimen and its forbidden/empty state. The matrix separates embedded surfaces and conditional/modal tables rather than counting only index routes.

## Shared controls and commit key

**LC** means ListColumns: column settings, hide/re-add, drag before/after, row-height steps, Compact/Normal and reset. Arrangement is mounted state, not an account preference. **PC** means PageColumns: LC controls plus catalog-walked fields, supported lenses/rollups/comparison, shareable `cols/lens/z/since` and account-remembered arrangement. Neither owner exposes named saved table views; saved SQL queries and saved workspace layouts are separate features. A dash under sorting/paging means the reference does not enable it, not that a feature was omitted.

**P4** = 25/50/100/300; **P3** = 25/50/100, initially 25. **OK** means the before implementation already supplied the reference features, and browser controls were exercised; it does not claim every business mutation was replayed. Every LC/PC row below includes the shared controls just defined.

| Key | Fix commits |
|---|---|
| C1 | [Company/contact parity](https://github.com/belimbingapp/bilibi/commit/e01ad15) |
| C2 | [Notifications/connections FlexTables](https://github.com/belimbingapp/bilibi/commit/57d03b7), [compact actions](https://github.com/belimbingapp/bilibi/commit/cc53618) |
| C3 | [Interactive specimens](https://github.com/belimbingapp/bilibi/commit/e43940b) |
| C4 | [Sort-key presentation](https://github.com/belimbingapp/bilibi/commit/54eb4bb), [alias and duplicate-name disambiguation](https://github.com/belimbingapp/bilibi/commit/1917d5f), [registry owner keys](https://github.com/belimbingapp/bilibi/commit/30040a6) |
| C5 | [Local calendar bounds and remount handshake](https://github.com/belimbingapp/bilibi/commit/5becec3), [history replacement](https://github.com/belimbingapp/bilibi/commit/9cc5598) |
| C6 | [Embedded URL namespaces](https://github.com/belimbingapp/bilibi/commit/1dd5263), [shared Users search toolbar](https://github.com/belimbingapp/bilibi/commit/4e3084c) |

## Core lists and record panels

| Bilibi page / surface | Bilimbi owner and enabled features | Bilibi before | Result / reason |
|---|---|---|---|
| `/companies` | `core/company/lib/company/web/index_live.ex`: PC; sort name/status/jurisdiction; search, status filter, P4; record/workspace links | PC and filters present | OK; account persistence and walked-column lens state also exercised |
| `/companies/{id}` — Subsidiaries | `company/web/show_live.ex`: LC; Name/Status/Legal Entity Type/Jurisdiction; no sort/search/pager; record links | Plain Table; columns incomplete; shared-list controls absent | C1: independent LC, four reference columns, masked jurisdiction and workspace-aware links; explicit notice if bounded read reaches 300 |
| Company detail — Departments | `company/web/show_live.ex`: LC; Type/Category/Head/Status; no sort/paging; Manage link | LC; a redraw erased add-column suggestions | C1; hide/re-add/drag/height exercised |
| Company detail — Relationships | `company/web/show_live.ex`: LC; Company/Role/Direction/Effective/Status; no sort/paging; Manage link | LC | OK; retained Bilibi's owned relationship embed |
| Company detail — Users | `core/user/lib/user/web/company_users_panel.ex`, parent `company/web/show_live.ex`: LC; sort name/email/email_verified; name/email search; P4; `users_` URL state | LC and controls present, but search/sort/page/size lost on reload; search icon overlapped input text | C6: reference query names/aliases, independent URL state, shared toolbar; reload, Back/Forward, paging and parent refresh exercised |
| Company detail — Affiliations | Closest reference: `core/employee/lib/employee/web/company_employees_panel.ex` plus Contact affiliations; LC on employee preview; see model decision below | LC, search, history, paging and sort; local-only list state | C4, C6: party sort indicator and `affiliations_` URL state; no HR fields invented |
| Company/contact detail — Addresses | `core/address/lib/address/web/addresses_panel.ex`: LC; sort label/address/kind/primary/priority/from/to; no search/paging; attach/create-and-attach/inline facts/confirmed unlink | LC; owner add-column suggestions erased on redraw | C1; both embeds exercised; existing owner actions retained |
| `/companies/{id}/departments` | `company/web/departments_live.ex`: LC; no sort/search/pager; add/update/retire department | LC | OK |
| `/companies/{id}/relationships` | `company/web/relationships_live.ex`: LC; no sort/search/pager; add/edit dates/end/confirmed unlink | LC, but no End action | C1: End uses held version and audited owner facade; browser End retained start date/history |
| `/companies/department-types` | `company/web/reference_types_live.ex`: LC; category filter; no sort/pager; create/edit/confirmed delete | LC | OK |
| `/companies/legal-entity-types` | Same reference owner: LC; no sort/pager; create/edit/confirmed delete | LC | OK |
| `/companies/relationship-types` | Same reference owner: LC; no sort/pager; platform-global activate/deactivate catalog | LC; tenant-owned retirement catalog | Existing model retained; see model decision below |
| `/contacts` (`/persons` redirects here) | `core/person/lib/person/web/index_live.ex`: PC; default Name/Email/Phone; no sort; search; P3; record/workspace links | PC but default Kind/Created columns differed | C1: reference defaults and fresh contact-field masking; personal Email/Phone stay separate from affiliation-owned Work email, Role and Company |
| `/contacts/{id}` — Affiliations | `person/web/show_live.ex`: plain table, company/title/email/phone/start/end; no table controls/sort/pager; edit/add/unlink | LC already richer: search/history/paging/sort plus in-place edits/end/unlink | C4, C6; retain richer LC per standing rule; URL-backed list state now survives reload/history |
| Contact detail — Company roles | `person/web/show_live.ex`: plain table; company/role/period/status; edit/add/end/unlink | LC already richer | OK; preserve existing permissions/model and shared controls |
| Contact detail — Accounts | Closest: `core/user/lib/user/web/show_live.ex` account/identity links, not an identical reference table | LC; account identity links/confirmed unlink | OK; explicit Person/account model adaptation, not an HR employee table |
| `/users` | `core/user_administration/lib/user_administration/web/index_live.ex` and `index_live/index.html.heex`: PC; sort name/email/company_name/created_at; search, roles filter, P4; row links/delete safeguards | PC | OK |
| `/users/{id}` — Affiliations | `core/user/lib/user/web/show_live.ex`: LC linked Employees; six sortable columns; no pager/search | LC Affiliation/account binding, not HR | C4 fixes aliased sort presentation where applicable; preserve Person/Affiliation binding workflow |
| `/dashboard` — Users | `user/web/dashboard_people_panel.ex`: LC; first five accounts, no sort/search/pager; All users link | LC | OK |
| `/addresses` | `address/web/index_live.ex`: LC; sort label/country/status; search; P3; record/create/confirmed delete with attachment refusal | LC | OK |
| `/addresses/{id}` — Linked entities | `address/web/show_live.ex`: LC; all seven columns sortable; no search/pager; owner links | LC | OK; populated address detail exercised |
| `/geonames/countries` | `core/geonames/lib/geonames/web/countries_live.ex`: LC; ISO/name/capital/phone/currency/population/updated sortable; search, P4; inline name/import action | LC | OK |
| `/geonames/admin1` | `geonames/web/admin1_live.ex`: LC; country/code/name/alternate/updated sortable; search, country filter, P4; inline name | LC | OK |
| `/geonames/postcodes` | `geonames/web/postcodes_live.ex`: LC; country/postcode/place/admin1/updated sortable; search, P4; create/edit/inline place | LC | OK |
| Postcodes — Country summary | Same owner: separate LC; Country/ISO/Records sortable; no rendered search/pager; conditional nonempty summary | LC | OK; populated MY/SG summary exercised |
| `/admin/system/database-queries` | `user/web/database_queries_live/index.ex` and `.html.heex`: LC; name/description/created/updated sortable; search, P4; saved query links/create/delete | LC | OK |
| `/admin/system/database-queries/{slug}` — Results | Same directory `show.ex` and `.html.heex`: LC dynamic projection; result columns sortable; P4; Run/save/duplicate/delete query | LC, but sort arrow did not follow aliased owner keys correctly | C4; alias/spec collision and duplicate SQL labels exercised; only first duplicate is sortable because C9 resolves names to first position |
| `/notifications` | `user/web/notifications_live.ex` and `.html.heex`: cards; All/Unread/Read tabs, P4, mark-one/all and visit links | Cards: no column/drag/height controls | C2 by standing FlexTable rule; tabs/paging/read/deep-link/empty behavior retained; compact actions fit one line |
| `/settings/ai-connections` | `core/agent_access/lib/agent_access/web/connections_live.ex`: cards; expiry/revoked/expired/identity-warning data; confirmed revoke; no search/sort/pager | Cards: no column/drag/height controls | C2 by standing rule; LC, typed expiry and warnings retained; confirmation/cancel/revoke exercised, including compact rows |

## Base and Web lists

| Bilibi page / surface | Bilimbi owner and enabled features | Bilibi before | Result / reason |
|---|---|---|---|
| `/authz/roles` | `base/authz/lib/authz/web/roles_index_live.ex`: LC; name/code/scope header sort; search, P4; create/detail links | LC | OK; accepted created_at key is not a displayed reference column |
| `/authz/roles/{id}` — Principals | `authz/web/role_show_live.ex`: LC; Type/Principal/Company; no sort/search/pager | LC | OK; capabilities remain permission controls, not a data table |
| `/authz/capabilities` | `authz/web/capabilities_live.ex`: LC; key/domain/resource/action/module sortable; search, domain, P4 | LC | OK |
| `/authz/decision-logs` | `authz/web/decision_logs_live.ex`: LC; occurred/actor_type/capability/result/reason/resource sortable; search, result, P4 | LC | OK; actor sort is actor type, not resolved name |
| `/authz/field-restrictions` | `authz/web/field_restrictions_live.ex`: LC; no sort/search/pager; add/edit roles/confirmed remove | LC | OK |
| `/tenancy/tenants` | `base/tenancy/lib/tenancy/web/live/tenants_live.ex`: LC; id/name/status sortable; full platform set, no search/pager; create form | LC | OK |
| `/system/sessions` | `base/session/lib/session/web/live/index_live.ex`: LC; user_id/IP/user-agent/activity sortable; search, P4; confirmed terminate, not current session | LC | OK |
| `/system/menu-inspector` | `base/system/lib/system/web/menu_inspector_live.ex`: LC; no sort; search, source, P4; diagnostic only | LC | OK |
| `/system/performance` — Regressions/Samples | `base/perf/lib/perf/web/index_live.ex`: two LCs; no header sort; bounded regressions; sample kind/outcome/identity filters, P4 | Two LCs | OK |
| `/system/schedule` — Tasks | `base/schedule/lib/schedule/web/index_live.ex`: LC; name/next_due/last_run sortable; search/status; no pager; Tasks/History/Settings tabs and guarded commands | LC | OK |
| `/system/schedule?tab=history` | Same owner: LC; started/name/source/status sortable; search/status/start/end calendar days, P3 | LC; Local display still used UTC date bounds; remount reporting could clamp too early | C5: browser-local inclusive days, exclusive next-day bound, one report per mount, URL correction replaces history |
| `/system/api-exchanges` — Outgoing/Incoming | `base/integration/lib/integration/web/index_live.ex`: PC; no header sort; direction tabs, search/outcome/period, P4; operation detail link | PC | OK; both directions exercised |
| `/audit/actions` | `base/audit/lib/audit/web/actions_live.ex`: LC; occurred/actor_type/event/trace header sort; search, actor/family/result/diagnostics, P4; retain toggle | LC | OK |
| `/audit/mutations` | `audit/web/mutations_live.ex`: LC; occurred/actor_type/event/subject-type/trace sort; search/event filter, P4; expanded detail | LC | OK |
| `/workspace` — Saved layouts modal | `base/tiling/lib/tiling/web/workspace_live.ex`: LC; no sort/search/pager; open/rename/default/layout/save/update/confirmed delete | LC | OK; modal opened before exercising controls |
| `/workspace` — Shared with you modal | Same owner: separate LC; no sort/search/pager; open/copy | LC | OK |
| `/workspace/shared-layouts` | `tiling/web/shared_layouts_live.ex`: LC; no sort/search/pager; publish/name/audience/confirmed delete | LC | OK |
| `/system/database-tables` | Absent from Bilimbi pin; Belimbing `app/Base/Database/Livewire/DatabaseTables/Index.php`: registry search/wildcards, table/module/migration sorting, paging | LC plus step-up gate, P3 | C4: canonical owner-key sort indicators; retained established Bilibi contract; browser checked after step-up |
| `/system/database-tables/{table}` — Data | Belimbing `resources/core/views/livewire/admin/system/database-tables/partials/show-data-tab.blade.php`: dynamic columns, search/sort, raw/formatted and foreign-key links | LC dynamic projection | C4: owner-name sort indicator; step-up/raw/formatted/search/paging retained |
| DB table — Schema | Belimbing `…/partials/show-schema-tab.blade.php`: columns/indexes; Bilibi also shows constraints | Three LCs | OK; columns/indexes/constraints all exercised |
| DB table — Relationships | Same business inspection contract; outgoing/incoming references | Two LCs | OK; both directions exercised |
| `/workflow/reference/{id}` — Work/history | Closest `base/workflow` reference owner/specimen: work facts and state history, no index pager | Two LCs | OK; deliberate platform reference route, no domain-specific table added |
| `/system/design-library/components` — confirmation rows, simple list, list pattern | Bilimbi `base/ui` DesignLibraryLive; primitive/pattern demonstrations | Plain Tables alongside existing FlexSample | C3: interactive lists use LC; search/sort/page and confirm flow remain; specimen search is mounted-only, not an operational bookmark |
| Same page — framed/plain FlexSample | Bilimbi UI DesignLibraryLive FlexSample: walked sample columns/rollups/lenses and controls | Existing FlexSample | OK; retained held-back suggestion vocabulary, not a general Grid catalog |
| `/system/design-library/design-spec`, `/assets/ui-components.html`, feedback/inline/workspace specimens | Bilimbi UI static visual baselines/primitive documentation | Deliberate static Table examples | Not operational lists: primitive Table anatomy remains intentionally visible; no live table host attached to a static screenshot specimen |
| Standalone `/list-reference`, `/list-reference/forbidden` example | Bilimbi list-control vocabulary; deterministic standalone Web specimen, not a product route | Plain Table with ListState | C3: LC/FlexTable with URL-backed list state; running example checked for controls, sort/filter/page and permission empty state |
| Standalone `/flex-reference`, `/g3-lenses`, `/g4-page-columns` examples | Bilimbi FlexTable/Grid teaching surfaces, not product pages | Already shared FlexTable/Grid hosts | Inventoried; unchanged, not new operational routes in the default binary |
| Standalone `/reference/exports` and print/download examples | Bilimbi SST download contract, not a mounted product list | Download links and standalone printable HTML document | Excluded from interactive table parity: these demonstrate file/CSP delivery, and printed tables must remain self-contained without live table controls |

## Model boundaries are intentional

- Person/Affiliation replaces the old contact/employee coupling. Do not reconstruct an HR owner, employee number/designation/type/status or legacy external-access grant model for a table migration. Company/contact/user pages expose the existing proven Person/Affiliation/account bindings instead. Company external-access and employee tables have no corresponding owner in this composition.
- Contacts keep personal email/phone separate. Bilimbi's work-contact fallback crosses the separation mandated by Bilibi's Person/Affiliation contract, so missing personal fields remain empty and hidden fields remain restricted. Work email, Role and Company are separate columns from current affiliations (see the Person and Affiliation READMEs).
- Relationship roles remain tenant-owned and retired through Bilibi's existing owner; Bilimbi's platform-global activate/deactivate catalog is a different business model. No new cross-tenant administrative behavior was introduced.
- Reference-unsortable columns remain unsortable. No named saved-table-view UI was invented. PC remembers one account/page arrangement; LC resets on reload, but same-record embedded query history retains its mounted arrangement.

## Executed evidence and limits

- Before/after browser sweep: shared customization/Compact/Normal/remove/re-add/reset on every operational table above; drag, row-height changes and each exposed sortable header; list search reload and available filter/page-size selections. Conditional postcode summary, workspace modal and DB step-up/schema/relationship states were opened explicitly. Drag checks target the left/right half: dropping exactly at a header midpoint may intentionally leave an adjacent column in place.
- Targeted regressions: Company and Contact party-sort aliases; SQL `name` beside an actual `c0` label; duplicate-label asymmetric values; database field-name sorts; department/address add-column suggestions; Subsidiaries independent columns; Contact default columns; shared search input padding.
- Embedded lists: 17 checks passed for bare URLs through fallback refresh, namespace isolation/clearing, actual page-2 counts (6 of 31 filtered users), reload, reference page-size alias, invalid size/page correction, Back/Forward with compact columns, and navigation between distinct companies. Subject IDs remain server-owned.
- Schedule: in America/New_York, the local 15 October window returns 32 records (page 2: seven), while the equivalent UTC window returns two. Bookmark/remount/Back preserve local bounds; an out-of-range page corrects in place, and Back/Forward leave and return normally.
- Notification rows: one-row read and visit actions updated read state; visit navigated to the linked Company; Mark all read produced zero unread rows and the shared empty state. Connections: Cancel left the connection active; confirm changed it to Revoked, removed its action and persisted `revoked_at`. Normal/18/22/26px compact states were inspected (the shortest DOM row includes its divider).
- Screenshots and short real-pointer recordings were inspected in the Amp task evidence, not committed as fixtures. Static checks: module discovery, rustfmt, workspace/all-target Clippy with warnings denied, and asset freshness. No new unit tests, page-owned assets, module dependencies, migrations, CSS classes or compiled CSS changes were needed.
- Browser checks establish controls/URL behavior and selected audited actions, not exhaustive mutation coverage, every possible catalog walk/lens/data type, or visual pixel equivalence with a running Bilimbi installation.
