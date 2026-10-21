# Design Library

The five source routes are discovered from this module. Access requires
`admin.system.design-library.view`; the contributed System Viewer role receives
that capability. The menu lives under Administration → System → Design Library.

The adapter lives in `base/design_library` so shared `base/ui` does not acquire
identity, database or authorization dependencies. It composes the production
Base UI models/macros, including its own shell navigation, inline controls,
dialogs, tables and the U7 `FlexSample`. All example changes stay in the mounted
page's memory. The specimen POST changes no business data, returns 303 to
Components, and displays the source's informational message. The host applies
the route capability, Origin and CSRF checks, including when framed.

`source.json` pins the original Bilimbi files and their hashes. The templates
preserve the source's text, family anchors and slots, using the shared compact UI scale; `sample.json` and
`controls.json` hold its copied sample form values and control declarations.
The page is split at family/card boundaries because Askama inlines macros;
large input templates otherwise exceed the compiler's memory limit. Every
control declaration renders through its shared typed model in `controls.rs`.

Data display demonstrates the canonical list toolbar: search first, filter boxes to its right, wrapping below search on narrow screens. Its company status filter works together with search, sort and pagination.

Inputs keeps the single select, checkbox and radio group in Choice controls. The separate Multi-select card demonstrates the canonical inset label, selected count and chips: single-line overflow by default, explicit wrapping, and an empty selection. The live-state preview reads the first multi-select. Code input shows the device code and a bare 2FA code field.

For manual runtime evidence, follow
[`design-library-v1.json`](../../../../fixtures/parity/design-library-v1.json) and
[`runtime-parity.md`](../../../../docs/runtime-parity.md). Use an isolated migrated
database holding the walkthrough fixture (`cargo run -p bilibi-walkthrough-fixture`),
then visit `/system/design-library/components` as its `admin@example.test`. Try the action link, modal/confirmation flow, live form, the File input card
(stream two small files, Keep files, and watch a file over 256 KiB refused),
inline facts, company search/paging, FlexTable notch and Graphic's icon
filter/copy tiles.
Theme and Design Spec preserve the source's foundations and accepted decisions.
No unit tests or test-only infrastructure belong to this module.
