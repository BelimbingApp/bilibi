//! A reference to another module's record, shown and chosen the same way on
//! every page: the `RelatedRecord` pill names and links it (or shows it as
//! unavailable), and the `RecordPicker` live control finds one by name.
//! `base/records` resolves names and hits with the reader's permissions; this
//! module only draws them, so a page writes no markup of its own. No Bilimbi
//! counterpart (Bilimbi has no cross-module reference).
use crate::{forms::InputKind, Button, Field, Html, Restricted};
use askama::Template;

/// How a reference reads for this viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordState {
    /// The viewer may see it: its name and, when the kind has one, its page.
    Named { label: String, href: Option<String> },
    /// Hidden from this viewer, removed, or its module is not installed.
    Unavailable,
}

/// One reference as a pill. With a page, the name is a `RecordLink`: a link
/// that opens the record, or inside a workspace tile a control that selects
/// it in the following tile. A long name stays on one
/// line, at most 16rem (60% of a phone's width), and ends in "…", with the
/// whole name as the pill's title, so a table cell never grows to fit it.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{%- import "components.html" as ui -%}{%- import "lists.html" as lists -%}
{%- match state -%}
{%- when RecordState::Named with { label, href } -%}
{%- let record_kind = kind -%}{%- let record_href = href -%}
<span id="{{ id }}" data-related-record title="{{ label }}" class="inline-flex min-w-0 max-w-[min(16rem,60vw)] align-middle">{% call ui::badge(kind=crate::BadgeKind::Neutral, dot=false, class="min-w-0 max-w-full normal-case") %}{% match record_href %}{% when Some with (target) %}{% call lists::record_link(workspace=workspace, kind=record_kind, record_id=record_id, href=target, class="min-w-0 truncate hover:underline") %}{{ label }}{% endcall %}{% when None %}<span class="min-w-0 truncate">{{ label }}</span>{% endmatch %}{% endcall %}</span>
{%- when RecordState::Unavailable -%}
<span id="{{ id }}" data-related-record>{{ unavailable }}</span>
{%- endmatch -%}"#,
    ext = "html"
)]
pub struct RelatedRecord {
    /// A stable id for this pill on the page.
    pub id: String,
    /// The page is framed: the name selects the record in a following tile.
    pub workspace: bool,
    /// The workspace kind: the module that registered the record kind.
    pub kind: String,
    pub record_id: String,
    pub state: RecordState,
    unavailable: Html,
}
impl RelatedRecord {
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        record_id: impl Into<String>,
        state: RecordState,
    ) -> Self {
        let id = id.into();
        let marker = Restricted {
            reason: "Not available to you: hidden, removed, or its module is not installed.".into(),
            ..Restricted::new(format!("{id}-unavailable"))
        };
        Self {
            unavailable: Html::render(&marker).unwrap_or(Html::EMPTY),
            id,
            workspace: false,
            kind: kind.into(),
            record_id: record_id.into(),
            state,
        }
    }
    /// The page is framed in a workspace tile.
    pub fn workspace(mut self, framed: bool) -> Self {
        self.workspace = framed;
        self
    }
}

/// A picker hit's text: one line within the button, ending in "…" when long,
/// with the whole text as its title (a button is one fixed height).
#[derive(Template)]
#[template(
    source = r#"<span class="min-w-0 truncate" title="{{ text }}">{{ text }}</span>"#,
    ext = "html"
)]
struct HitLabel {
    text: String,
}

/// One record a picker offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerHit {
    /// `kind:id`, what choosing it raises.
    pub value: String,
    pub label: String,
    /// The kind's name for people, such as "Contact" or "Company".
    pub kind_label: String,
}

/// A live type-ahead over the kinds a field allows. It sits in the owner's
/// `Form` with `change`: typing raises the form's event, the owner searches
/// (`base/records` `search`) and renders the hits; choosing one raises
/// `pick_event` with `record=kind:id`, and the owner keeps it as `selected`.
/// The chosen `kind:id` posts with the form as `{form}[{key}]`. The owner
/// still checks it on save; a chosen value is never proof.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{%- import "forms.html" as forms -%}<div id="{{ id }}" data-record-picker class="space-y-1">
{%- if let Some(pill) = selected -%}
<div class="flex flex-wrap items-center gap-2"><span class="text-xs text-ink-muted">{{ label }}</span>{{ pill }}{{ clear }}</div>{{ hidden }}{% call forms::errors(id=id, errors=errors) %}{% endcall %}
{%- else -%}
{{ search }}
{%- if !hits.is_empty() -%}<ul id="{{ id }}-hits" aria-label="{{ label }}" class="space-y-1">{% for hit in hits %}<li>{{ hit }}</li>{% endfor %}</ul>
{%- else if !query.is_empty() -%}<p id="{{ id }}-none" class="text-xs text-ink-muted">No matching record you may see.</p>
{%- endif -%}
{%- endif -%}
</div>"#,
    ext = "html"
)]
pub struct RecordPicker {
    pub id: String,
    pub label: String,
    query: String,
    selected: Option<Html>,
    search: Html,
    hidden: Html,
    clear: Html,
    hits: Vec<Html>,
    /// Shown under the chosen record; while searching, the search field shows them.
    errors: Vec<String>,
}
impl RecordPicker {
    /// `form` and `key` name the posted fields: the typed text as
    /// `{form}[{key}_search]`, the choice as `{form}[{key}]`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        form: &str,
        key: &str,
        label: &str,
        query: &str,
        hits: &[PickerHit],
        selected: Option<(&str, RelatedRecord)>,
        events: (&str, &str),
        errors: Vec<String>,
    ) -> Self {
        let (pick_event, clear_event) = events;
        let search = Field::in_form(form, &format!("{key}_search"))
            .id(format!("{id}-search"))
            .label(label)
            .kind(InputKind::Search)
            .value(query)
            .attr("data-debounce", "300")
            .autocomplete("off")
            .hint("Type part of a name.")
            .errors(errors.clone());
        let hits = hits
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let body = Html::render(&HitLabel {
                    text: format!("{} · {}", hit.label, hit.kind_label),
                })
                .unwrap_or(Html::EMPTY);
                Html::render(&Button {
                    id: &format!("{id}-hit-{index}"),
                    button_type: "button",
                    event: pick_event,
                    name: "record",
                    value: &hit.value,
                    class: "w-full min-w-0 justify-start",
                    body,
                    ..Button::default()
                })
                .unwrap_or(Html::EMPTY)
            })
            .collect();
        let (selected, hidden, clear) = match selected {
            Some((value, pill)) => (
                Some(Html::render(&pill).unwrap_or(Html::EMPTY)),
                Html::render(
                    &Field::in_form(form, key)
                        .id(format!("{id}-value"))
                        .kind(InputKind::Hidden)
                        .value(value),
                )
                .unwrap_or(Html::EMPTY),
                Html::render(&Button {
                    id: &format!("{id}-clear"),
                    button_type: "button",
                    event: clear_event,
                    body: Html::text("Change"),
                    ..Button::default()
                })
                .unwrap_or(Html::EMPTY),
            ),
            None => (None, Html::EMPTY, Html::EMPTY),
        };
        Self {
            id: id.into(),
            label: label.into(),
            query: query.into(),
            selected,
            search: Html::render(&search).unwrap_or(Html::EMPTY),
            hidden,
            clear,
            hits,
            errors,
        }
    }
}
