//! Presentation composes the shared primitives; all stored strings are escaped.
use crate::{Diagnostics, Health, Recorded, Regression, Store};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_ui::{
    choices, params, Alert, Card, FilterToolbar, FlexRow, FlexTable, Header, Html, IconButton,
    IconContext, ListColumns, ListState, PageCounts, Pagination, StatusKind, Tab, TableEmpty, Tabs,
    ToolbarControl,
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
#[derive(Template)]
#[template(
    source = r#"<p id="performance-status" role="status" class="mb-2 text-xs text-ink-muted">{{ text }}</p>"#,
    ext = "html"
)]
struct StatusText<'a> {
    text: &'a str,
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
fn grouped(locale: &str, value: u64) -> String {
    i64::try_from(value)
        .ok()
        .and_then(|value| {
            bilibi_locale::Numbers::new(locale)
                .ok()
                .map(|numbers| numbers.integer(value))
        })
        .unwrap_or_else(|| value.to_string())
}
fn waiting_phrase(locale: &str, value: u64) -> String {
    if value == 1 {
        "1 sample is waiting to be saved".to_owned()
    } else {
        format!("{} samples are waiting to be saved", grouped(locale, value))
    }
}
fn dropped_phrase(locale: &str, value: u64) -> String {
    if value == 1 {
        "1 sample was dropped".to_owned()
    } else {
        format!("{} samples were dropped", grouped(locale, value))
    }
}
fn kept_phrase(locale: &str, value: u64, earlier: bool) -> String {
    if value == 0 {
        return if earlier {
            "no earlier samples kept".to_owned()
        } else {
            "no samples kept".to_owned()
        };
    }
    let word = match (value == 1, earlier) {
        (true, true) => "earlier sample kept",
        (true, false) => "sample kept",
        (false, true) => "earlier samples kept",
        (false, false) => "samples kept",
    };
    format!("{} {word}", grouped(locale, value))
}
fn sentence(parts: Vec<String>) -> String {
    let mut parts = parts.into_iter();
    let Some(first) = parts.next() else {
        return String::new();
    };
    let mut chars = first.chars();
    let mut text = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default();
    for part in parts {
        text.push_str(" · ");
        text.push_str(&part);
    }
    text
}
/// One line for the five old facts. A warning only when the recorder or the
/// history store is down, or when samples are waiting or were dropped.
fn status_text(diagnostics: &Diagnostics, locale: &str) -> String {
    let waiting = diagnostics.stats.pending as u64;
    let dropped = diagnostics.stats.dropped;
    let store_down = diagnostics.store == Store::Unavailable;
    let recorder_down = diagnostics.recorder == Health::Unavailable;
    let calm = !recorder_down && !store_down && waiting == 0 && dropped == 0;
    let mut parts = Vec::new();
    if calm {
        parts.push(
            if diagnostics.recording {
                "Recording normally"
            } else {
                "Recording is off"
            }
            .to_owned(),
        );
    } else if !diagnostics.recording && !store_down {
        parts.push("Recording is off".to_owned());
    }
    if recorder_down {
        parts.push("the recorder is not running".to_owned());
    } else if store_down {
        parts.push("the recorder is running".to_owned());
    }
    if store_down {
        parts.push("history cannot be read".to_owned());
    }
    if waiting > 0 {
        parts.push(waiting_phrase(locale, waiting));
    }
    if dropped > 0 {
        parts.push(dropped_phrase(locale, dropped));
    }
    if let Some(samples) = diagnostics.samples {
        let samples = u64::try_from(samples).unwrap_or(0);
        parts.push(kept_phrase(locale, samples, !diagnostics.recording));
    }
    if waiting == 0 {
        parts.push("none waiting".to_owned());
    }
    if dropped == 0 {
        parts.push("none dropped".to_owned());
    }
    sentence(parts)
}
fn attention(diagnostics: &Diagnostics) -> Option<StatusKind> {
    if diagnostics.recorder == Health::Unavailable || diagnostics.store == Store::Unavailable {
        Some(StatusKind::Error)
    } else if diagnostics.stats.pending > 0 || diagnostics.stats.dropped > 0 {
        Some(StatusKind::Warning)
    } else {
        None
    }
}
fn status(data: &Data) -> Result<Html> {
    let text = status_text(&data.diagnostics, data.display.locale);
    match attention(&data.diagnostics) {
        Some(kind) => render(Alert {
            id: "performance-status",
            kind,
            body: Html::text(&text),
            class: "mb-2",
        }),
        None => render(StatusText { text: &text }),
    }
}
fn tabs(list: &ListState, current: &str) -> Result<Html> {
    let tab_paths: Vec<(String, String, &str, &str)> = super::TABS
        .iter()
        .map(|(id, label)| {
            let mut query = list.to_params();
            if *id != super::TABS[0].0 {
                query.insert("tab".into(), (*id).into());
            }
            (
                format!("performance-tab-{id}"),
                params::address(super::PATH, &query),
                *id,
                *label,
            )
        })
        .collect();
    render(Tabs {
        id: "performance-tabs",
        label: "Performance views",
        class: "mb-2",
        tabs: tab_paths
            .iter()
            .map(|(dom, href, id, label)| Tab {
                id: dom,
                ..Tab::patch(href, label, current == *id)
            })
            .collect(),
    })
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
fn samples_empty(list: &ListState, ok: bool) -> &'static str {
    if !ok {
        "History is unavailable."
    } else if list.search.is_empty() && list.filters.values().all(|value| value.is_empty()) {
        "No samples have been kept."
    } else {
        "No samples match these filters."
    }
}
pub(super) fn page(
    list: &ListState,
    samples: &ListColumns,
    regressions: &ListColumns,
    data: &Data,
    tab: &str,
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
    body.push(&status(data)?);
    body.push(&tabs(list, tab)?);
    if tab == super::TABS[1].0 {
        body.push(&render(FilterToolbar::new(
            "performance-filters",
            vec![
                ToolbarControl::search(
                    "performance-identity",
                    "identity",
                    "Page address or job name",
                    &list.search,
                    "Full page address or job name",
                )
                .hint("Type the whole page address or job name, not part of it."),
                ToolbarControl::select(
                    "performance-kind",
                    "kind",
                    "Kind of sample",
                    &list.filters["kind"],
                    choices(&[
                        ("All kinds", ""),
                        ("Request", "request"),
                        ("Live event", "event"),
                        ("Job", "job"),
                        ("Runtime", "runtime"),
                    ]),
                ),
                ToolbarControl::select(
                    "performance-outcome",
                    "outcome",
                    "How it ended",
                    &list.filters["outcome"],
                    choices(&[
                        ("All outcomes", ""),
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
                "Sample history is unavailable. It will retry on the next refresh.",
            )?);
        }
        let sample_rows = data
            .samples
            .as_ref()
            .map(|page| {
                page.entries
                    .iter()
                    .map(|sample| sample_row(sample, &data.display))
                    .collect()
            })
            .unwrap_or_else(|_| Ok(vec![]))?;
        let mut history = render(table(
            samples,
            sample_rows,
            "samples_grid",
            "All samples",
            samples_empty(list, data.samples.is_ok()),
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
    } else {
        let regression_rows = data
            .regressions
            .as_ref()
            .map(|rows| rows.iter().map(regression_row).collect())
            .unwrap_or_else(|_| Ok(vec![]))?;
        let regression_body = render(table(
            regressions,
            regression_rows,
            "regressions_grid",
            "Recent slowdowns",
            if data.regressions.is_ok() {
                "Nothing has slowed down recently."
            } else {
                "Slowdown checks are unavailable."
            },
        ))?;
        body.push(&render(Card {
            id: "performance-regressions",
            title: "Recent slowdowns",
            inner_class: "p-0",
            body: regression_body,
            ..Card::default()
        })?);
    }
    Ok(render(bilibi_ui::Page {
        id: "performance-index",
        body,
        ..bilibi_ui::Page::default()
    })?
    .as_str()
    .to_owned())
}
