use super::*;
use askama::Template;
use bilibi_audit::console::ConsoleResult;
use bilibi_datetime::{Format, Precision};
use bilibi_ui::{
    flex_table::{FlexColumn, FlexRow, FlexTable, Instant},
    list_columns::ListColumns,
    Alert, BackLink, Button, ButtonVariant, ConfirmDialog, CreateButton, EmptyState, Field,
    FilterToolbar, FlashGroup, FlashMap, Header, Icon, IconButton, IconKind, InputKind, PageCounts,
    Pagination, StatusKind, TableEmpty, ToolbarControl,
};
use serde_json::Value;

#[derive(Template)]
#[template(path = "database_queries.html")]
struct IndexView {
    flash: Html,
    header: Html,
    toolbar: Html,
    table: Html,
    pagination: Html,
    confirm: Html,
}
#[derive(Template)]
#[template(
    source = r#"<a href="{{ href }}" class="font-medium text-action hover:underline">{{ name }}</a>"#,
    ext = "html"
)]
struct QueryLink<'a> {
    href: &'a str,
    name: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<span class="block max-w-xs truncate text-ink-subtle" title="{{ value }}">{{ value }}</span>"#,
    ext = "html"
)]
struct Description<'a> {
    value: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<div class="inline-flex items-center gap-1.5">{{ duplicate }}{{ delete }}</div>"#,
    ext = "html"
)]
struct RowActions {
    duplicate: Html,
    delete: Html,
}

#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}<span class="whitespace-nowrap text-ink-subtle">{% call flex::instant(id=id, i=time) %}{% endcall %}</span>"#,
    ext = "html"
)]
struct DateCell {
    id: String,
    time: Instant,
}

fn flash(success: &str, error: &str) -> Result<Html> {
    fragment(&FlashGroup {
        id: "flash-group",
        flash: FlashMap {
            success,
            error,
            ..Default::default()
        },
    })
}
fn confirm(pending: bool, name: &str) -> Result<Html> {
    if !pending {
        return Ok(Html::EMPTY);
    }
    fragment(&ConfirmDialog {
        id: "delete-query-confirm",
        consequence: &format!("Query “{name}” will be deleted."),
        detail: "Its saved SQL and parameters are removed. This cannot be undone.",
        confirm: "Delete",
        working: "Deleting…",
        confirm_event: "delete",
        cancel_event: "cancel_delete",
    })
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn index(
    pages: &Pages,
    scope: &Scope,
    list: &ListState,
    columns: &ListColumns,
    found: queries::QueryPage,
    editor: bool,
    pending: Option<&queries::SavedQuery>,
    success: &str,
    error: &str,
) -> Result<String> {
    let client = pages.db.get().await?;
    let locale = pages.locales.resolve(&**client, scope, None).await?.locale;
    let display = pages
        .display
        .display(&**client, scope, None, locale)
        .await?;
    let mut rows = Vec::new();
    // Rendering a column event must retain the add-column query and offers.
    // This static catalogue changes only through ListColumns::handle.
    for query in &found.entries {
        let key = query.id.to_string();
        let mut result = FlexRow::new(&key)
            .slot(
                "name",
                fragment(&QueryLink {
                    href: &format!("{ROOT}/{}", query.slug),
                    name: &query.name,
                })?,
            )
            .slot(
                "description",
                fragment(&Description {
                    value: if query.description.is_empty() {
                        "—"
                    } else {
                        &query.description
                    },
                })?,
            );
        result.dom_id = Some(format!("query-row-{}", query.id));
        for (key, at) in [
            ("created_at", query.created_at),
            ("updated_at", query.updated_at),
        ] {
            let stamp = jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?;
            let time = display.render(stamp, Format::DateTime, Precision::Minute);
            result = result.slot(
                key,
                fragment(&DateCell {
                    id: format!(
                        "{}-{}",
                        if key == "created_at" {
                            "created"
                        } else {
                            "updated"
                        },
                        query.id
                    ),
                    time: Instant::new(
                        &time.iso,
                        &time.text,
                        &time.company,
                        &time.utc,
                        time.mode.as_str(),
                    ),
                })?,
            );
        }
        if editor {
            result.actions = fragment(&RowActions {
                duplicate: fragment(&IconButton {
                    id: &format!("duplicate-query-{}", query.id),
                    event: "duplicate",
                    name: "id",
                    value: &key,
                    ..IconButton::new("duplicate", "Duplicate query")
                })?,
                delete: fragment(&IconButton {
                    id: &format!("delete-query-{}", query.id),
                    kind: IconKind::Danger,
                    event: "request_delete",
                    name: "id",
                    value: &key,
                    ..IconButton::new("delete", "Delete query")
                })?,
            })?;
        }
        rows.push(result);
    }
    let mut table = FlexTable::new("database-queries-table", columns.flex_columns(), rows)
        .sorted(list.sort_by.as_deref(), list.sort_dir);
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.framed = false;
    table.actions = editor;
    table.caption = "Database queries";
    table.empty = Some(TableEmpty::Content(Html::text("No queries found.")));
    let create = if editor {
        fragment(&CreateButton {
            noun: "Query",
            href: &format!("{ROOT}/_new"),
            id: "btn-create-query",
            ..Default::default()
        })?
    } else {
        Html::EMPTY
    };
    IndexView {
        flash: flash(success, error)?,
        header: fragment(&Header {
            title: Html::text("Database Queries"),
            subtitle: "User-defined SQL queries rendered as browsable pages",
            actions: create,
            title_actions: Html::EMPTY,
        })?,
        toolbar: fragment(&FilterToolbar::new(
            "database-queries-filters",
            vec![ToolbarControl::search(
                "search-input",
                "search",
                "Search database queries",
                &list.search,
                "Search by name or description...",
            )],
        ))?,
        table: fragment(&table)?,
        pagination: fragment(&Pagination::new(
            "database-queries-pagination",
            PageCounts::new(list.page, list.page_size, found.total as u64),
            &SIZES,
        ))?,
        confirm: confirm(
            pending.is_some(),
            pending.map_or("", |query| query.name.as_str()),
        )?,
    }
    .render()
    .map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(path = "database_query.html")]
struct DetailView {
    flash: Html,
    header: Html,
    run: Html,
    prompt: Html,
    sql: Html,
    caution: Html,
    error: Html,
    parameters: Vec<Html>,
    results: Html,
    confirm: Html,
}
#[derive(Template)]
#[template(
    source = r#"<div class="flex items-center gap-3"><form id="query-title-form" data-event="change_field" data-change class="inline-block">{{ name }}</form>{% if dirty %}<span class="inline-flex items-center rounded-full bg-warning-surface px-2 py-0.5 text-xs font-medium text-warning-ink border border-warning-line">Unsaved changes</span>{% endif %}</div>"#,
    ext = "html"
)]
struct Title {
    name: Html,
    dirty: bool,
}
#[derive(Template)]
#[template(
    source = r#"<form id="query-desc-form" data-event="change_field" data-change class="w-full">{{ description }}</form>"#,
    ext = "html"
)]
struct DescriptionForm {
    description: Html,
}
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::header(subtitle_html=description, actions=actions) %}{{ title }}{% endcall %}"#,
    ext = "html"
)]
struct QueryHeader {
    title: Html,
    description: Html,
    actions: Html,
}
#[derive(Template)]
#[template(
    source = r#"<span id="query-results-summary">{{ text }}</span>"#,
    ext = "html"
)]
struct Summary {
    text: String,
}
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::section_heading(id="query-results-heading", title="Results", description_html=summary) %}{% endcall %}"#,
    ext = "html"
)]
struct ResultsHeading {
    summary: Html,
}
#[derive(Template)]
#[template(path = "database_query_results.html")]
struct ResultsView {
    heading: Html,
    table: Html,
    pagination: Html,
}

fn action(id: &str, event: &str, label: &str, icon: &str, variant: ButtonVariant) -> Result<Html> {
    let mut body = fragment(&Icon {
        name: icon,
        class: "size-4 mr-1.5",
    })?;
    body.push(&Html::text(label));
    fragment(&Button {
        id,
        event,
        body,
        variant,
        ..Default::default()
    })
}
fn editor_field(name: &str, id: &str, value: &str, placeholder: &str, class: &str) -> Field {
    Field::named(name)
        .id(id)
        .value(value)
        .placeholder(placeholder)
        .attr("data-debounce", "150")
        .wrapper_class("mb-0")
        .class(class)
}
pub(super) fn detail(state: &detail::Detail, editor: bool) -> Result<String> {
    let mut actions = fragment(&BackLink {
        id: "database-query-back",
        href: ROOT,
        title: "Back to database queries",
        ..Default::default()
    })?;
    if editor {
        if state.saved.is_some() {
            actions.push(&Html::text(" "));
            actions.push(&action(
                "btn-duplicate-query",
                "duplicate",
                "Duplicate",
                "duplicate",
                ButtonVariant::Secondary,
            )?);
            actions.push(&Html::text(" "));
            actions.push(&action(
                "btn-delete-query",
                "request_delete",
                "Delete",
                "delete",
                ButtonVariant::Danger,
            )?);
        } else {
            actions.push(&Html::text(" "));
            actions.push(&action(
                "btn-discard-query",
                "delete",
                "Discard",
                "close",
                ButtonVariant::Secondary,
            )?);
        }
        actions.push(&Html::text(" "));
        actions.push(&action(
            "btn-save-query",
            "save",
            "Save",
            "save",
            ButtonVariant::Primary,
        )?);
    }
    let title = fragment(&Title {
        name: fragment(&editor_field("name", "query-name-input", &state.draft.name, "Untitled Query", "bg-transparent text-xl font-bold text-ink border-0 border-b border-transparent hover:border-high-contrast-line focus:border-brand-strong focus:ring-0 px-1 py-0.5"))?,
        dirty: state.dirty,
    })?;
    let description = fragment(&DescriptionForm { description: fragment(&editor_field(
        "description", "query-description-input", &state.draft.description, "Add a description...",
        "w-full bg-transparent text-sm text-ink-subtle border-0 border-b border-transparent hover:border-high-contrast-line focus:border-brand-strong focus:ring-0 px-1 py-0.5",
    ))? })?;
    let prompt = editor_field("prompt", "query-prompt-textarea", &state.draft.prompt, "Describe the data or query in natural language...", "block w-full rounded-md border border-line bg-surface px-3 py-1.5 text-sm text-ink placeholder:text-ink-faint focus:border-brand-strong focus:outline-none focus:ring-1 focus:ring-brand-strong/30").kind(InputKind::Textarea).attr("rows", "2");
    let sql = editor_field("sql_query", "query-sql-textarea", &state.draft.sql_query, "SELECT * FROM users WHERE id = :id LIMIT 25;", "block w-full rounded-md border border-line bg-surface-sunken font-mono text-xs text-ink px-3 py-1.5 placeholder:text-ink-faint focus:border-brand-strong focus:outline-none focus:ring-1 focus:ring-brand-strong/30").kind(InputKind::Textarea).attr("rows", "5");
    let parameters = state.detected.iter().map(|key| fragment(&Field::in_form("parameters", key)
        .id(format!("param-input-{key}")).label(format!(":{key}")).label_class("block text-xs font-mono text-ink mb-1")
        .wrapper_class("mb-0").placeholder(format!("Value for :{key}"))
        .value(state.parameters.get(key).and_then(|v| v.as_deref()).unwrap_or(""))
        .class("block w-full rounded-md border border-line bg-surface px-2.5 py-1.5 text-xs text-ink placeholder:text-ink-faint focus:border-brand-strong focus:outline-none focus:ring-1 focus:ring-brand-strong/30")))
        .collect::<Result<Vec<_>>>()?;
    DetailView {
        flash: flash(&state.success, "")?,
        header: fragment(&QueryHeader { title, description, actions })?,
        run: {
            let mut body = fragment(&Icon { name: "play", class: "size-3.5 mr-1" })?;
            body.push(&Html::text("Run Query"));
            fragment(&Button { id: "btn-run-query", event: "run_query", class: "px-3 py-1 text-xs", variant: ButtonVariant::Primary, body, ..Default::default() })?
        },
        prompt: fragment(&prompt)?, sql: fragment(&sql)?, parameters,
        caution: fragment(&Alert { id: "database-query-reach-caution", kind: StatusKind::Warning, class: "mt-2", body: Html::text("Not filtered to one company: SQL run here reads across every company and tenant in the database.") })?,
        error: if state.error.is_empty() { Html::EMPTY } else { fragment(&Alert { id: "query-execution-error", kind: StatusKind::Error, class: "", body: Html::text(&state.error) })? },
        results: match &state.result { Some(result) => results(state, result)?, None => Html::EMPTY },
        confirm: confirm(state.pending, &state.draft.name)?,
    }.render().map_err(|_| Error::Internal)
}
#[derive(Template)]
#[template(
    source = r#"<span class="block max-w-sm truncate whitespace-nowrap font-mono text-xs{% if missing %} text-ink-faint{% endif %}">{{ text }}</span>"#,
    ext = "html"
)]
struct ResultCell {
    text: String,
    missing: bool,
}
fn results(state: &detail::Detail, result: &ConsoleResult) -> Result<Html> {
    // Result names may contain spaces, quotes, duplicate names or Unicode. DOM
    // identities use positions; the original names only reach C9's bound sort.
    let all = result
        .columns
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut column = FlexColumn::new(&format!("c{i}"), &format!("c{i}"), name).fixed();
            column.sort_key = Some(name.clone());
            column.sortable = !result.columns[..i].contains(name);
            column
        })
        .collect();
    let columns = state
        .columns
        .as_ref()
        .map_or(all, ListColumns::flex_columns);
    let mut rows = Vec::new();
    for (i, values) in result.rows.iter().enumerate() {
        let mut row = FlexRow::new(i.to_string());
        row.dom_id = Some(format!("result-row-{i}"));
        for (index, value) in values.iter().enumerate() {
            row = row.slot(
                &format!("c{index}"),
                fragment(&ResultCell {
                    text: match value {
                        Value::Null => "—".into(),
                        Value::String(value) => value.clone(),
                        _ => value.to_string(),
                    },
                    missing: value.is_null(),
                })?,
            );
        }
        rows.push(row);
    }
    let mut table = FlexTable::new("query-results-table", columns, rows)
        .sorted(state.sort.as_deref(), state.sort_dir());
    table.event = "results_grid";
    table.caption = "Query results";
    table.framed = false;
    table.empty = Some(TableEmpty::State(EmptyState::missing(
        "No rows returned",
        "The query ran and matched nothing.",
    )));
    if let Some(columns) = &state.columns {
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
    }
    let mut pagination = Pagination::new(
        "query-results-pagination",
        PageCounts::new(
            result.page as u64,
            result.per_page as u64,
            result.total as u64,
        ),
        &SIZES,
    );
    pagination.filters_event = "result_page_size";
    pagination.page_event = "page_results";
    fragment(&ResultsView {
        heading: fragment(&ResultsHeading {
            summary: fragment(&Summary {
                text: format!(
                    "{} columns · {} total rows",
                    result.columns.len(),
                    result.total
                ),
            })?,
        })?,
        table: fragment(&table)?,
        pagination: fragment(&pagination)?,
    })
}
