//! Presentation composes the shared primitives; all stored strings are escaped.
use crate::{Diagnostics, Health, Recorded, Regression, Store};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_ui::{
    choices, Alert, Card, FilterToolbar, FlexRow, FlexTable, Header, Html, IconButton, IconContext,
    ListColumns, ListState, PageCounts, Pagination, StatusKind, TableEmpty, ToolbarControl,
};

pub(super) struct Data {
    pub diagnostics: Diagnostics,
    pub samples: Result<crate::Page>,
    pub regressions: Result<Vec<Regression>>,
    pub display: DisplayContext,
}
pub(super) fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
fn status(health: Health) -> &'static str {
    match health {
        Health::Available => "Available",
        Health::Degraded => "Degraded",
        Health::Unavailable => "Unavailable",
    }
}

#[derive(Template)]
#[template(
    source = r#"<div id="performance-health" class="mb-2 grid gap-2 sm:grid-cols-5">{% for card in cards %}{{ card|safe }}{% endfor %}</div>"#,
    ext = "html"
)]
struct HealthCards {
    cards: Vec<Html>,
}
#[derive(Template)]
#[template(
    source = r#"<p class="text-xs font-semibold text-muted">{{ label }}</p><p class="mt-1 text-sm tabular-nums text-ink">{{ value }}</p>"#,
    ext = "html"
)]
struct Metric<'a> {
    label: &'a str,
    value: String,
}
#[derive(Template)]
#[template(
    source = r#"<span class="tabular-nums">{{ text }}</span>"#,
    ext = "html"
)]
struct Number {
    text: String,
}
#[derive(Template)]
#[template(
    source = r#"<time id="{{ id }}" datetime="{{ iso }}" data-format="datetime" data-precision="minute" data-mode="{{ mode }}" data-text-company="{{ company }}" data-text-utc="{{ utc }}" data-follow-shell="true" data-hook="DateTime" class="tabular-nums">{{ text }}</time>"#,
    ext = "html"
)]
struct When {
    id: String,
    iso: String,
    text: String,
    company: String,
    utc: String,
    mode: &'static str,
}
fn health(data: &Data) -> Result<Html> {
    let d = &data.diagnostics;
    let mut cards = Vec::new();
    for (id, label, value) in [
        (
            "performance-recorder",
            "Recorder",
            status(d.recorder).into(),
        ),
        (
            "performance-store",
            "History store",
            if d.store == Store::Available {
                "Available"
            } else {
                "Unavailable"
            }
            .into(),
        ),
        (
            "performance-samples",
            "Visible samples",
            data.samples
                .as_ref()
                .map(|p| p.total.to_string())
                .unwrap_or_else(|_| "—".into()),
        ),
        (
            "performance-pending",
            "Recorder backlog",
            d.stats.pending.to_string(),
        ),
        (
            "performance-dropped",
            "Dropped samples",
            d.stats.dropped.to_string(),
        ),
    ] {
        cards.push(render(Card {
            id,
            inner_class: "p-3",
            body: render(Metric { label, value })?,
            ..Card::default()
        })?);
    }
    render(HealthCards { cards })
}
fn number(text: String) -> Result<Html> {
    render(Number { text })
}
fn sample_row(recorded: &Recorded, display: &DisplayContext) -> Result<FlexRow> {
    let sample = &recorded.sample;
    let at = jiff::Timestamp::try_from(sample.observed_at).map_err(|_| Error::Internal)?;
    let when = display.render(at, Format::DateTime, Precision::Minute);
    let mut row = FlexRow::new(recorded.id.to_string())
        .slot(
            "observed_at",
            render(When {
                id: format!("performance-observed-{}", recorded.id),
                iso: when.iso,
                text: when.text,
                company: when.company,
                utc: when.utc,
                mode: when.mode.as_str(),
            })?,
        )
        .slot("kind", Html::text(sample.kind.as_str()))
        .slot("identity", Html::text(&sample.identity))
        .slot("outcome", Html::text(sample.outcome.as_str()))
        .slot("duration", number(format!("{} ms", sample.duration_ms))?)
        .slot(
            "database",
            number(format!(
                "{} / {} ms",
                sample.db_count, sample.db_duration_ms
            ))?,
        );
    row.dom_id = Some(format!("samples-{}", recorded.id));
    Ok(row)
}
fn regression_row(regression: &Regression) -> Result<FlexRow> {
    Ok(FlexRow::new(format!(
        "{}:{}",
        regression.kind.as_str(),
        regression.identity
    ))
    .slot("identity", Html::text(&regression.identity))
    .slot(
        "baseline",
        number(format!("{:.1} ms", regression.baseline_average_ms))?,
    )
    .slot(
        "current",
        number(format!("{:.1} ms", regression.current_p95_ms))?,
    )
    .slot(
        "change",
        number(format!("{:.1}%", regression.delta_percent))?,
    ))
}
fn table<'a>(
    columns: &'a ListColumns,
    rows: Vec<FlexRow>,
    event: &'a str,
    caption: &'a str,
    empty: &str,
) -> FlexTable<'a> {
    let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows);
    table.event = event;
    table.caption = caption;
    table.framed = false;
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.empty = Some(TableEmpty::Content(Html::text(empty)));
    table
}
fn notice(id: &str, text: &str) -> Result<Html> {
    render(Alert {
        id,
        kind: StatusKind::Error,
        body: Html::text(text),
        class: "mb-2",
    })
}
pub(super) fn page(
    list: &ListState,
    samples: &ListColumns,
    regressions: &ListColumns,
    data: &Data,
) -> Result<String> {
    let mut body = render(Header {
        title: Html::text("Performance"),
        subtitle: "See which pages and background jobs are slow.",
        title_actions: render(IconButton {
            id: "performance-pin",
            context: IconContext::Inline,
            nav_pin: "nav-admin-system-performance",
            ..IconButton::new("bilimbi-pin", "Pin Performance to sidebar")
        })?,
        ..Header::default()
    })?;
    body.push(&health(data)?);
    if !data.diagnostics.recording && data.diagnostics.store == Store::Available {
        body.push(&render(bilibi_ui::PanelNotice {
            id: "performance-disabled",
            body: Html::text("Recording is off; earlier history is still shown."),
            ..bilibi_ui::PanelNotice::default()
        })?);
    }
    let regression_rows = data
        .regressions
        .as_ref()
        .map(|rows| rows.iter().map(regression_row).collect())
        .unwrap_or_else(|_| Ok(vec![]))?;
    let regression_body = render(table(
        regressions,
        regression_rows,
        "regressions_grid",
        "Recent regressions",
        if data.regressions.is_ok() {
            "Nothing has slowed down recently."
        } else {
            "Regression diagnostics are unavailable."
        },
    ))?;
    body.push(&render(Card {
        id: "performance-regressions",
        title: "Recent regressions",
        inner_class: "p-0",
        class: "mb-2",
        body: regression_body,
        ..Card::default()
    })?);
    body.push(&render(FilterToolbar::new(
        "performance-filters",
        vec![
            ToolbarControl::search(
                "performance-identity",
                "identity",
                "Route or worker",
                &list.search,
                "Exact identity",
            ),
            ToolbarControl::select(
                "performance-kind",
                "kind",
                "Kind",
                &list.filters["kind"],
                choices(&[
                    ("All", ""),
                    ("Request", "request"),
                    ("Live event", "event"),
                    ("Job", "job"),
                    ("Runtime", "runtime"),
                ]),
            ),
            ToolbarControl::select(
                "performance-outcome",
                "outcome",
                "Outcome",
                &list.filters["outcome"],
                choices(&[
                    ("All", ""),
                    ("OK", "ok"),
                    ("Error", "error"),
                    ("Cancelled", "cancelled"),
                    ("Discarded", "discarded"),
                ]),
            ),
        ],
    ))?);
    if data.samples.is_err() {
        body.push(&notice(
            "performance-unavailable",
            "Performance history is unavailable. It will retry on the next refresh.",
        )?);
    }
    let sample_rows = data
        .samples
        .as_ref()
        .map(|p| {
            p.entries
                .iter()
                .map(|s| sample_row(s, &data.display))
                .collect()
        })
        .unwrap_or_else(|_| Ok(vec![]))?;
    let mut history = render(table(
        samples,
        sample_rows,
        "samples_grid",
        "Performance history",
        if data.samples.is_ok() {
            "No matching performance history."
        } else {
            "History unavailable."
        },
    ))?;
    if let Ok(page) = &data.samples {
        history.push(&render(Pagination::new(
            "performance-pagination",
            PageCounts::new(list.page, list.page_size, page.total as u64),
            &super::SIZES,
        ))?);
    }
    body.push(&render(Card {
        id: "performance-history",
        inner_class: "p-0",
        body: history,
        ..Card::default()
    })?);
    Ok(render(bilibi_ui::Page {
        id: "performance-index",
        body,
        ..bilibi_ui::Page::default()
    })?
    .as_str()
    .to_owned())
}
