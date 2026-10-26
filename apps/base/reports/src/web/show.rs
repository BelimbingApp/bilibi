//! `/reports/{id}`: one report, read first. Result runs it as the reader
//! (ask-when-run answers and drill-downs narrow it; columns the reader
//! cannot see are named as left out); Definition is the editor, live for
//! its author and read-only for others; Sharing is the author's. Copy and
//! Delete confirm through the shared dialog. Answers and the page travel in
//! the address, so a link and the CSV download repeat the same question.
use super::{
    builder::Builder, clear_flash, flash_map, index::kind_label, page_error, refusal, render,
    report_path, result, Can, Pages, Refusal,
};
use crate::{ColumnInfo, FilterDef, Kind, Report, ReportError, RunInput, RunResult};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, ActionLink, Alert, Button, ButtonVariant, Card, ColumnsOutcome, ConfirmDialog, Fact,
    Facts, Field, FlashMap, Header, Html, IconButton, IconKind, InputKind, ListColumns,
    MultiSelect, StatusKind, Tab, Tabs,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const TABS: [(&str, &str); 3] = [
    ("result", "Result"),
    ("definition", "Definition"),
    ("sharing", "Sharing"),
];
const DENIED_AUTHOR: &str = "You do not have permission to change this report.";
const DENIED_SHARE: &str = "You do not have permission to share reports.";
const STALE_DRAFT: &str = "Someone saved this report while you were editing it, so your changes were not saved. They are still in the form; Discard changes loads the saved report.";

#[derive(Clone, Copy, PartialEq)]
enum Confirm {
    Copy,
    Delete,
}

/// A drill-down: a group's own value, shown as `label: value`. It travels
/// in the address as `d<n>` (the column) and `v<n>` (its value as JSON, or
/// a date bucket's `{"min","max"}` days; no `v<n>` is the empty group), with
/// `rows=1` when the page shows the records behind it, so a link and the CSV
/// repeat it.
#[derive(Clone)]
struct Drill {
    filter: FilterDef,
    /// Filled from the reader's columns when the page draws.
    label: String,
}

/// Drill-downs one page holds at most (one per grouping).
const MAX_DRILL: usize = 2;

/// The drill-downs an address carries: a column and one scalar value, a
/// bucket's day range, or none for the empty group. The run still resolves
/// each column against the reader's own catalog and checks the value, so
/// nothing here is trusted.
pub(crate) fn drill_keys(query: &HashMap<String, String>) -> Vec<FilterDef> {
    (0..MAX_DRILL)
        .map_while(|n| {
            let column = query
                .get(&format!("d{n}"))
                .filter(|c| !c.is_empty() && c.len() <= 256)?;
            let day =
                |v: Option<&Value>| v.is_some_and(|v| v.as_str().is_some_and(|s| s.len() <= 32));
            let (test, value) = match query.get(&format!("v{n}")) {
                None => ("empty", Value::Null),
                Some(text) if text.len() <= 512 => match serde_json::from_str::<Value>(text) {
                    Ok(value @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) => {
                        ("equals", value)
                    }
                    Ok(Value::Object(range))
                        if range.len() == 2 && day(range.get("min")) && day(range.get("max")) =>
                    {
                        ("range", Value::Object(range))
                    }
                    _ => return None,
                },
                Some(_) => return None,
            };
            Some(FilterDef {
                column: column.clone(),
                test: test.into(),
                value,
                ask: false,
            })
        })
        .collect()
}

pub(crate) struct Show {
    pages: Pages,
    id: i64,
    tab: String,
    /// `a<i>` (and `a<i>_to` for a range) by filter position, as typed.
    answers: BTreeMap<String, String>,
    page: usize,
    drill: Vec<Drill>,
    report: Option<Report>,
    builder: Option<Builder>,
    hosts: BTreeMap<&'static str, (Vec<String>, ListColumns)>,
    drillable: Vec<result::Drillable>,
    /// The drawn summary's groupings, as `(spec, label, bucket)`.
    grouped: Vec<(String, String, Option<&'static str>)>,
    /// A summary shown as the records behind it (a drill-down).
    as_rows: bool,
    confirm: Option<Confirm>,
    flash: Option<(StatusKind, String)>,
}

#[derive(Template)]
#[template(path = "reports_show.html")]
struct ShowView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    facts: Html,
    tabs: Html,
    body: Html,
    dialog: Html,
}

/// The run answers an address carries, typed by each ask filter's test:
/// a list for one-of, a range from `a<i>` and `a<i>_to`, text otherwise.
/// An answer left blank is `null`, so that filter is skipped rather than
/// falling back to its stored value; an unanswered one keeps the stored value.
pub(crate) fn answers(report: &Report, typed: &BTreeMap<String, String>) -> BTreeMap<usize, Value> {
    let mut out = BTreeMap::new();
    for (i, filter) in report
        .definition
        .filters
        .iter()
        .enumerate()
        .filter(|(_, f)| f.ask)
    {
        let text = |key: String| {
            typed
                .get(&key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let answered = |key: String| typed.contains_key(&key);
        if !answered(format!("a{i}")) && !answered(format!("a{i}_to")) {
            continue;
        }
        let value = match filter.test.as_str() {
            "range" => {
                let (min, max) = (text(format!("a{i}")), text(format!("a{i}_to")));
                if min.is_none() && max.is_none() {
                    Value::Null
                } else {
                    json!({"min": min, "max": max})
                }
            }
            "one_of" => match text(format!("a{i}")) {
                Some(list) => Value::Array(
                    list.split(',')
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(|v| json!(v))
                        .collect(),
                ),
                None => Value::Null,
            },
            _ => text(format!("a{i}")).map_or(Value::Null, |v| json!(v)),
        };
        out.insert(i, value);
    }
    out
}

/// An ask filter's stored value as its box shows it: a list joined with
/// commas, a range's `min` (or `max` for the `to` box), text as is.
fn stored_text(filter: &FilterDef, to: bool) -> String {
    let text = |value: &Value| match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match (filter.test.as_str(), &filter.value) {
        ("range", range) => text(&range[if to { "max" } else { "min" }]),
        (_, Value::Array(values)) => values.iter().map(text).collect::<Vec<_>>().join(", "),
        (_, value) if !to => text(value),
        _ => String::new(),
    }
}

/// The `a<i>` keys of a query, bounded by the definition's filter limit.
pub(crate) fn answer_keys(query: &HashMap<String, String>) -> BTreeMap<String, String> {
    query
        .iter()
        .filter(|(key, value)| {
            key.strip_prefix('a')
                .map(|rest| rest.trim_end_matches("_to"))
                .and_then(|n| n.parse::<usize>().ok())
                .is_some_and(|n| n < bilibi_grid::MAX_FILTERS)
                && value.len() <= 256
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

impl Show {
    pub fn new(pages: Pages, id: i64, query: &HashMap<String, String>) -> Self {
        let mut show = Self {
            pages,
            id,
            tab: "result".into(),
            answers: BTreeMap::new(),
            page: 0,
            drill: Vec::new(),
            report: None,
            builder: None,
            hosts: BTreeMap::new(),
            drillable: Vec::new(),
            grouped: Vec::new(),
            as_rows: false,
            confirm: None,
            flash: None,
        };
        show.params(query);
        show
    }
    pub fn title(&self) -> Option<String> {
        self.report.as_ref().map(|r| r.name.clone())
    }

    fn params(&mut self, query: &HashMap<String, String>) {
        let tab = query
            .get("tab")
            .filter(|t| TABS.iter().any(|(key, _)| key == t))
            .cloned()
            .unwrap_or_else(|| "result".into());
        // Opening the editor starts from the saved report unless a changed
        // draft is waiting there.
        if tab == "definition" && self.tab != tab && !self.builder.as_ref().is_some_and(|b| b.dirty)
        {
            self.builder = None;
        }
        self.tab = tab;
        self.page = params::positive_integer(query.get("page").map(String::as_str))
            .map_or(0, |p| {
                p.saturating_sub(1).min(bilibi_grid::MAX_OFFSET as u64) as usize
            });
        self.answers = answer_keys(query);
        self.as_rows = query.get("rows").is_some_and(|v| v == "1");
        self.drill = drill_keys(query)
            .into_iter()
            .map(|filter| Drill {
                filter,
                label: String::new(),
            })
            .collect();
    }
    fn query(&self) -> HashMap<String, String> {
        // A blank answer stays: it means "no filter", not "the stored value".
        let mut query: HashMap<String, String> = self
            .answers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if self.tab != "result" {
            query.insert("tab".into(), self.tab.clone());
        }
        if self.page > 0 {
            query.insert("page".into(), (self.page + 1).to_string());
        }
        if self.as_rows {
            query.insert("rows".into(), "1".into());
        }
        for (n, drill) in self.drill.iter().enumerate() {
            query.insert(format!("d{n}"), drill.filter.column.clone());
            if !drill.filter.value.is_null() {
                query.insert(format!("v{n}"), drill.filter.value.to_string());
            }
        }
        query
    }

    /// The report as the reader may see it now. Gone or unshared is the
    /// page's 404. The editor on screen is a draft of the version it shows:
    /// when someone saves a newer one, it keeps its base version (changed or
    /// not), so its save is refused as a conflict, never applied over theirs.
    async fn load(&mut self, scope: &Scope) -> Result<Report> {
        let report = self
            .pages
            .reports
            .get(scope, self.id)
            .await
            .map_err(page_error)?;
        let fresh = self
            .report
            .as_ref()
            .is_none_or(|r| r.version != report.version);
        let drafting = self.tab == "definition" || self.builder.as_ref().is_some_and(|b| b.dirty);
        if self.builder.is_none() || (fresh && !drafting) {
            self.builder = Some(Builder::from_report(&report));
        }
        self.report = Some(report.clone());
        Ok(report)
    }

    /// Answers `Some((path, flash))` when the event leaves this page.
    async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<Option<(String, String)>> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        let report = self.load(scope).await?;
        match get("type").ok_or(Error::Invalid)? {
            "clear-flash" => clear_flash(&mut self.flash, value),
            "answer" => {
                let posted = params::form_values(value, "ask");
                self.answers = answer_keys(&posted);
                self.page = 0;
            }
            "page" => {
                self.page = params::positive_integer(get("page"))
                    .map_or(0, |p| p.saturating_sub(1) as usize);
            }
            // The pager's page-size form: a report's page size is a setting.
            "filters" => {}
            "grid" => {
                let tab = if report.kind == Kind::Rows {
                    "rows"
                } else {
                    "summary"
                };
                if let Some((_, host)) = self.hosts.get_mut(tab) {
                    if let ColumnsOutcome::Sort(_) = host.handle(&params::string_values(value)) {
                        return Err(Error::Invalid);
                    }
                }
            }
            "drill" => self.drill_line(get("line")),
            "undrill" => {
                match params::positive_integer(get("index")) {
                    Some(n) if (n as usize) <= self.drill.len() => {
                        self.drill.remove(n as usize - 1);
                    }
                    _ => {
                        self.drill.clear();
                        self.as_rows = false;
                    }
                }
                self.page = 0;
            }
            "edit_definition" if report.owned => {
                if let Some(builder) = self.builder.as_mut() {
                    builder.read(value);
                }
            }
            "add_filter" if report.owned => {
                if let Some(builder) = self.builder.as_mut() {
                    builder.add_filter();
                }
            }
            "remove_filter" if report.owned => {
                if let (Some(builder), Some(index)) = (
                    self.builder.as_mut(),
                    get("index").and_then(|i| i.parse().ok()),
                ) {
                    builder.remove_filter(index);
                }
            }
            "reset_definition" => self.builder = Some(Builder::from_report(&report)),
            "save_definition" if report.owned => {
                self.save_definition(scope, credential, &report).await?
            }
            "save_sharing" if report.owned => {
                self.save_sharing(scope, credential, &report, value).await?
            }
            "request_copy" => self.confirm = Some(Confirm::Copy),
            "request_delete" if report.owned => self.confirm = Some(Confirm::Delete),
            "cancel_confirm" => self.confirm = None,
            "copy" if self.confirm == Some(Confirm::Copy) => {
                self.confirm = None;
                match self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "copy",
                        "base.reports.copy",
                        json!({"id": report.id}),
                    )
                    .await
                {
                    Ok(result) => {
                        let id = result["id"].as_i64().ok_or(Error::Internal)?;
                        return Ok(Some((
                            format!("{}?tab=definition", report_path(id)),
                            "Your copy is saved. Change it as you like.".into(),
                        )));
                    }
                    Err(error) => self.refused(refusal(
                        error,
                        "You do not have permission to build reports.",
                    )?),
                }
            }
            "delete" if self.confirm == Some(Confirm::Delete) => {
                self.confirm = None;
                match self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "delete",
                        "base.reports.delete",
                        json!({"id": report.id, "version": report.version}),
                    )
                    .await
                {
                    Ok(_) => {
                        return Ok(Some((
                            super::INDEX.into(),
                            format!("{} was deleted.", report.name),
                        )))
                    }
                    Err(error) => self.refused(refusal(error, DENIED_AUTHOR)?),
                }
            }
            // A control drawn for an earlier state (a dialog already closed,
            // a report no longer the reader's): the redraw shows why.
            "copy" | "delete" | "edit_definition" | "add_filter" | "remove_filter"
            | "save_definition" | "save_sharing" | "request_delete" => {}
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }

    fn refused(&mut self, refusal: Refusal) {
        let message = match refusal {
            Refusal::Message(message) => message,
            Refusal::Fields(fields) => fields.into_values().flatten().collect::<Vec<_>>().join(" "),
        };
        self.flash = Some((StatusKind::Error, message));
    }

    /// Shows the records behind a drawn line (or, for `total`, behind the
    /// whole result), narrowed by that line's own groups: equals for a value,
    /// the period's first and last day for a date bucket.
    fn drill_line(&mut self, line: Option<&str>) {
        if line == Some("total") {
            self.as_rows = true;
            self.page = 0;
            return;
        }
        let Some(line) = line
            .and_then(|l| l.parse::<usize>().ok())
            .and_then(|n| self.drillable.get(n))
        else {
            return;
        };
        let depth = if line.subtotal {
            line.keys.len().saturating_sub(1)
        } else {
            line.keys.len()
        };
        let mut added = Vec::new();
        for (key, (spec, label, bucket)) in line.keys.iter().zip(&self.grouped).take(depth) {
            if self.drill.iter().any(|d| &d.filter.column == spec) {
                continue;
            }
            let shown = result::key_text(key, *bucket);
            let (test, value) = match bucket {
                _ if key.is_null() => ("empty", Value::Null),
                Some(bucket) => match result::bucket_days(key, bucket) {
                    Some((min, max)) => ("range", json!({"min": min, "max": max})),
                    None => continue,
                },
                None => ("equals", key.clone()),
            };
            added.push(Drill {
                filter: FilterDef {
                    column: spec.clone(),
                    test: test.into(),
                    value,
                    ask: false,
                },
                label: format!(
                    "{label}: {}",
                    if key.is_null() {
                        "(none)".into()
                    } else {
                        shown
                    }
                ),
            });
        }
        self.drill.extend(added);
        self.drill.truncate(MAX_DRILL);
        self.as_rows = true;
        self.page = 0;
    }

    async fn save_definition(
        &mut self,
        scope: &Scope,
        credential: &str,
        report: &Report,
    ) -> Result<()> {
        let Some(builder) = self.builder.as_mut() else {
            return Ok(());
        };
        builder.errors.clear();
        let input = builder.input(report);
        match self
            .pages
            .submit(
                scope,
                credential,
                "save_definition",
                "base.reports.update",
                input,
            )
            .await
        {
            Ok(_) => {
                self.report = None;
                self.builder = None;
                self.drill.clear();
                self.page = 0;
                self.flash = Some((StatusKind::Success, "Report saved.".into()));
            }
            Err(error) => match refusal(error, DENIED_AUTHOR)? {
                Refusal::Fields(fields) => builder.errors = fields,
                Refusal::Message(message) if message == ReportError::Conflict.to_string() => {
                    self.flash = Some((StatusKind::Error, STALE_DRAFT.into()))
                }
                Refusal::Message(message) => self.flash = Some((StatusKind::Error, message)),
            },
        }
        Ok(())
    }

    async fn save_sharing(
        &mut self,
        scope: &Scope,
        credential: &str,
        report: &Report,
        value: &Value,
    ) -> Result<()> {
        let fields = params::form_values(value, "share");
        let everyone = fields.get("everyone").map(String::as_str) == Some("true");
        let roles: Vec<i64> = value
            .get("share[roles][]")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|v| v.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        let input = json!({"id": report.id, "version": report.version, "everyone": everyone, "roles": roles});
        match self
            .pages
            .submit(
                scope,
                credential,
                "save_sharing",
                "base.reports.share",
                input,
            )
            .await
        {
            Ok(_) => {
                self.flash = Some((
                    StatusKind::Success,
                    "Sharing saved. People run it with their own access.".into(),
                ))
            }
            Err(error) => self.refused(refusal(error, DENIED_SHARE)?),
        }
        Ok(())
    }

    pub async fn render(&mut self, scope: &Scope) -> Result<String> {
        let report = self.load(scope).await?;
        let can = self.pages.can(scope).await?;
        if self.tab == "sharing" && !report.owned {
            self.tab = "result".into();
        }
        let header = self.header(&report, can)?;
        let facts = self.facts(scope, &report).await?;
        let path = report_path(report.id);
        let hrefs: Vec<(String, &str, bool)> = TABS
            .iter()
            .filter(|(key, _)| *key != "sharing" || report.owned)
            .map(|(key, label)| {
                let mut query = self.query();
                query.remove("page");
                if *key == "result" {
                    query.remove("tab");
                } else {
                    query.insert("tab".into(), (*key).into());
                }
                (params::address(&path, &query), *label, self.tab == *key)
            })
            .collect();
        let tabs = render(Tabs {
            id: "report-tabs",
            label: "Report views",
            tabs: hrefs
                .iter()
                .map(|(href, label, current)| Tab::patch(href, label, *current))
                .collect(),
            ..Tabs::default()
        })?;
        let body = match self.tab.as_str() {
            "definition" => self.definition(scope, &report, can).await?,
            "sharing" => self.sharing(scope, &report, can).await?,
            _ => self.result(scope, &report, can).await?,
        };
        let dialog = match self.confirm {
            Some(Confirm::Copy) => {
                let consequence = format!("Copy {}?", report.name);
                render(ConfirmDialog {
                    id: "report-copy-confirm",
                    consequence: &consequence,
                    detail: "You get your own copy to change. It is not shared with anyone until you share it.",
                    confirm: "Copy",
                    working: "Copying…",
                    confirm_event: "copy",
                    cancel_event: "cancel_confirm",
                })?
            }
            Some(Confirm::Delete) => {
                let consequence = format!("Delete {}?", report.name);
                render(ConfirmDialog {
                    id: "report-delete-confirm",
                    consequence: &consequence,
                    detail: "It disappears for everyone it is shared with. This cannot be undone.",
                    confirm: "Delete",
                    working: "Deleting…",
                    confirm_event: "delete",
                    cancel_event: "cancel_confirm",
                })?
            }
            None => Html::EMPTY,
        };
        ShowView {
            flash: flash_map(&self.flash),
            header,
            facts,
            tabs,
            body,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn header(&self, report: &Report, can: Can) -> Result<Html> {
        let mut actions = Html::EMPTY;
        if can.export {
            // The answers and drill-downs, so the file is the result shown.
            let mut query = self.query();
            query.retain(|k, _| k.starts_with(['a', 'd', 'v']) || k == "rows");
            let href = params::address(&format!("{}/export", report_path(report.id)), &query);
            actions.push(&render(ActionLink {
                id: "report-export",
                icon: "export",
                title: "Download the whole result as CSV",
                href: &href,
                body: Html::text("Download CSV"),
                ..ActionLink::default()
            })?);
        }
        if can.author {
            actions.push(&render(Button {
                id: "report-copy",
                button_type: "button",
                event: "request_copy",
                body: Html::text("Copy"),
                ..Button::default()
            })?);
        }
        if can.author && report.owned {
            actions.push(&render(Button {
                id: "report-delete",
                button_type: "button",
                event: "request_delete",
                variant: ButtonVariant::Danger,
                body: Html::text("Delete"),
                ..Button::default()
            })?);
        }
        let actions = if actions.is_empty() {
            actions
        } else {
            render(Row { body: actions })?
        };
        render(Header {
            title: Html::text(&report.name),
            subtitle: &report.description,
            actions,
            ..Header::default()
        })
    }

    async fn facts(&self, scope: &Scope, report: &Report) -> Result<Html> {
        let types = self
            .pages
            .reports
            .record_types(scope)
            .await
            .map_err(page_error)?;
        let label = types
            .iter()
            .find(|(id, _)| *id == report.record_type)
            .map_or(report.record_type.as_str(), |(_, l)| l.as_str());
        let sharing = if !report.owned {
            "Shared with you".to_owned()
        } else if report.shared_with_everyone {
            "Everyone who can run reports".to_owned()
        } else if report.role_ids.is_empty() {
            "Only you".to_owned()
        } else {
            format!(
                "{} role{}",
                report.role_ids.len(),
                if report.role_ids.len() == 1 { "" } else { "s" }
            )
        };
        render(Card {
            id: "report-facts",
            body: render(Facts {
                id: "report-facts-list",
                items: vec![
                    Fact {
                        title: "Records",
                        id: "report-record-type",
                        value: Html::text(label),
                    },
                    Fact {
                        title: "Kind",
                        id: "report-kind",
                        value: Html::text(kind_label(report.kind)),
                    },
                    Fact {
                        title: "Shared",
                        id: "report-sharing",
                        value: Html::text(&sharing),
                    },
                ],
            })?,
            ..Card::default()
        })
    }

    async fn result(&mut self, scope: &Scope, report: &Report, can: Can) -> Result<Html> {
        let mut out = Html::EMPTY;
        let offered = self.offered(scope, report).await;
        let asks: Vec<(usize, &FilterDef)> = report
            .definition
            .filters
            .iter()
            .enumerate()
            .filter(|(_, f)| f.ask)
            .collect();
        if !asks.is_empty() {
            out.push(&self.ask_form(&asks, &offered)?);
        }
        // A drill-down read from the address is named here, from the
        // reader's own columns; one drilled on this page keeps its label.
        for drill in self.drill.iter_mut().filter(|d| d.label.is_empty()) {
            let column = offered
                .iter()
                .find(|c| c.spec == drill.filter.column)
                .map_or(drill.filter.column.as_str(), |c| c.label.as_str());
            let value = match &drill.filter.value {
                Value::Null => "(none)".into(),
                Value::Object(range) => format!(
                    "{} to {}",
                    range.get("min").map_or_else(String::new, result::cell),
                    range.get("max").map_or_else(String::new, result::cell)
                ),
                value => result::cell(value),
            };
            drill.label = format!("{column}: {value}");
        }
        if self.as_rows && report.kind != Kind::Rows {
            out.push(&self.breadcrumb()?);
        }
        let input = RunInput {
            answers: answers(report, &self.answers),
            drill: self.drill.iter().map(|d| d.filter.clone()).collect(),
            page: self.page,
            as_rows: self.as_rows,
        };
        let run = match self.pages.reports.run(scope, report.id, &input).await {
            Ok(run) => run,
            Err(
                error @ (ReportError::NotFound | ReportError::Forbidden | ReportError::Database(_)),
            ) => return Err(page_error(error)),
            Err(ReportError::Invalid(fields)) => {
                let message = fields.into_values().flatten().collect::<Vec<_>>().join(" ");
                out.push(&alert(
                    "report-run-refused",
                    StatusKind::Error,
                    &format!("This answer does not fit the report: {message}"),
                )?);
                return Ok(out);
            }
            Err(error) => {
                out.push(&alert(
                    "report-run-refused",
                    StatusKind::Error,
                    &error.to_string(),
                )?);
                return Ok(out);
            }
        };
        if !run.left_out.is_empty() {
            let message = format!(
                "Left out because your access does not include them: {}.",
                run.left_out.join(", ")
            );
            out.push(&alert("report-left-out", StatusKind::Info, &message)?);
        }
        let body = match &run.result {
            RunResult::Rows {
                columns,
                rows,
                pages,
                total,
                page,
                page_size,
                totals,
            } => {
                if can.export && !self.as_rows {
                    let cap = self
                        .pages
                        .reports
                        .export_rows(scope)
                        .await
                        .map_err(page_error)?;
                    if *total as usize > cap {
                        let message = format!("{total} rows is more than a download holds ({cap}). Narrow it with a filter to download it.");
                        out.push(&alert("report-export-cap", StatusKind::Info, &message)?);
                    }
                }
                let builtins = result::row_columns(columns);
                let host = self.host("rows", &builtins)?;
                let sizes = [*page_size as u64];
                let list = result::RowsPage {
                    rows,
                    pages,
                    total: *total,
                    page: *page,
                    page_size: *page_size,
                    totals: totals.as_deref(),
                };
                result::rows(host, &list, &sizes)?
            }
            summary @ RunResult::Summary { groupings, .. } => {
                self.grouped = groupings
                    .iter()
                    .map(|(c, bucket)| (c.spec.clone(), c.label.clone(), *bucket))
                    .collect();
                let builtins = result::summary_columns(summary);
                let host = self.host("summary", &builtins)?;
                let (html, drillable) = result::summary(&report.name, host, summary)?;
                self.drillable = drillable;
                html
            }
        };
        let flat = matches!(run.result, RunResult::Rows { .. });
        out.push(&render(Card {
            id: "report-result",
            inner_class: if flat { "p-0" } else { "" },
            body,
            ..Card::default()
        })?);
        Ok(out)
    }

    /// The column host of a result table, kept while its columns stay the
    /// same so a reader's arrangement survives each redraw.
    fn host(
        &mut self,
        which: &'static str,
        builtins: &[bilibi_ui::BuiltinColumn],
    ) -> Result<&ListColumns> {
        let key: Vec<String> = builtins
            .iter()
            .map(|b| format!("{}={}", b.id, b.label))
            .collect();
        let stale = self.hosts.get(which).is_none_or(|(held, _)| *held != key);
        if stale {
            let host = ListColumns::mount(&format!("report-{which}"), builtins.to_vec())
                .map_err(|_| Error::Internal)?;
            self.hosts.insert(which, (key, host));
        }
        Ok(&self.hosts[which].1)
    }

    async fn offered(&self, scope: &Scope, report: &Report) -> Vec<ColumnInfo> {
        self.pages
            .reports
            .columns(scope, &report.record_type)
            .await
            .unwrap_or_default()
    }

    fn ask_form(&self, asks: &[(usize, &FilterDef)], offered: &[ColumnInfo]) -> Result<Html> {
        let mut body = Html::EMPTY;
        for (i, filter) in asks {
            let label = offered
                .iter()
                .find(|c| c.spec == filter.column)
                .map_or(filter.column.as_str(), |c| c.label.as_str());
            let key = format!("a{i}");
            // Until the reader answers, the box shows the stored value the
            // run applies, so what is shown is what runs.
            let answered =
                self.answers.contains_key(&key) || self.answers.contains_key(&format!("{key}_to"));
            let stored = |key: &str| {
                if answered {
                    self.answers.get(key).cloned().unwrap_or_default()
                } else {
                    stored_text(filter, key.ends_with("_to"))
                }
            };
            let field = Field::in_form("ask", &key).id(format!("report-ask-{i}"));
            let html = match filter.test.as_str() {
                "within" => render(
                    field
                        .label(format!("{label} within"))
                        .kind(InputKind::Select)
                        .prompt("Any time")
                        .options(choices(&[
                            ("Today", "today"),
                            ("This week", "this_week"),
                            ("Last 7 days", "last_7_days"),
                            ("Last 30 days", "last_30_days"),
                            ("This month", "this_month"),
                            ("This quarter", "this_quarter"),
                            ("This year", "this_year"),
                        ]))
                        .value(stored(&key)),
                )?,
                "range" => {
                    let mut html =
                        render(field.label(format!("{label} from")).value(stored(&key)))?;
                    let to = format!("{key}_to");
                    html.push(&render(
                        Field::in_form("ask", &to)
                            .id(format!("report-ask-{i}-to"))
                            .label(format!("{label} to"))
                            .value(stored(&to)),
                    )?);
                    html
                }
                "one_of" => render(
                    field
                        .label(format!("{label} is one of"))
                        .hint("Separate values with commas.")
                        .value(stored(&key)),
                )?,
                "contains" => render(field.label(format!("{label} contains")).value(stored(&key)))?,
                _ => render(field.label(label).value(stored(&key)))?,
            };
            body.push(&html);
        }
        let actions = render(Button {
            id: "report-ask-run",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Running…",
            body: Html::text("Run"),
            ..Button::default()
        })?;
        render(Card {
            id: "report-ask",
            title: "Ask when run",
            body: render(bilibi_ui::Form {
                id: "report-ask-form",
                event: "answer",
                change: false,
                class: "",
                body,
                actions,
            })?,
            ..Card::default()
        })
    }

    /// Where a drill-down stands: the groups it narrows to, each removable,
    /// and the way back to the summary or matrix it came from.
    fn breadcrumb(&self) -> Result<Html> {
        let mut parts = Html::EMPTY;
        if self.drill.is_empty() {
            parts.push(&render(Narrowed {
                label: "everything in the total",
                button: Html::EMPTY,
            })?);
        }
        for (n, drill) in self.drill.iter().enumerate() {
            let id = format!("report-undrill-{}", n + 1);
            let index = (n + 1).to_string();
            let label = format!("Stop narrowing to {}", drill.label);
            // Data values keep their own spelling: a badge would capitalise them.
            let button = render(IconButton {
                id: &id,
                kind: IconKind::Neutral,
                event: "undrill",
                name: "index",
                value: &index,
                ..IconButton::new("close", &label)
            })?;
            parts.push(&render(Narrowed {
                label: &drill.label,
                button,
            })?);
        }
        parts.push(&render(Button {
            id: "report-undrill-all",
            button_type: "button",
            event: "undrill",
            body: Html::text("Back to the summary"),
            ..Button::default()
        })?);
        render(Breadcrumb { body: parts })
    }

    async fn definition(&mut self, scope: &Scope, report: &Report, can: Can) -> Result<Html> {
        let offered = self.offered(scope, report).await;
        let editable = report.owned && can.author;
        let mut out = Html::EMPTY;
        if !editable {
            let message = if report.owned {
                "You can no longer change reports. Ask an administrator for permission to build reports."
            } else {
                "Only its author can change this report. Copy it to make your own version."
            };
            out.push(&alert(
                "report-definition-readonly",
                StatusKind::Info,
                message,
            )?);
        }
        let builder = self
            .builder
            .get_or_insert_with(|| Builder::from_report(report));
        out.push(&builder.render(&offered, editable)?);
        render(Card {
            id: "report-definition",
            body: out,
            ..Card::default()
        })
    }

    async fn sharing(&mut self, scope: &Scope, report: &Report, can: Can) -> Result<Html> {
        if !can.share {
            let message = if report.shared_with_everyone {
                "Shared with everyone who can run reports. Ask an administrator for permission to change who it is shared with."
            } else {
                "Ask an administrator for permission to share reports."
            };
            return render(Card {
                id: "report-sharing",
                body: alert("report-sharing-note", StatusKind::Info, message)?,
                ..Card::default()
            });
        }
        let roles = self
            .pages
            .reports
            .share_roles(scope)
            .await
            .map_err(page_error)?;
        let mut body = render(
            Field::in_form("share", "everyone")
                .id("report-share-everyone")
                .label("Everyone who can run reports")
                .kind(InputKind::Checkbox)
                .checked(report.shared_with_everyone)
                .hint("Each person runs it with their own access, so they see only the records and fields they may see."),
        )?;
        let mut picker = MultiSelect::new("report-share-roles", "share[roles]")
            .label("Roles")
            .options(
                roles
                    .iter()
                    .map(|(id, name)| bilibi_ui::Choice::new(name, id.to_string()))
                    .collect(),
            )
            .hint("People holding any of these roles can run it.");
        picker.values = report.role_ids.iter().map(i64::to_string).collect();
        picker.selection_label = ":count role|:count roles".into();
        picker.placeholder = "No roles".into();
        picker.chips = true;
        picker.wrap_chips = true;
        body.push(&render(picker)?);
        let actions = render(Button {
            id: "report-share-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save sharing"),
            ..Button::default()
        })?;
        render(Card {
            id: "report-sharing",
            body: render(bilibi_ui::Form {
                id: "report-share-form",
                event: "save_sharing",
                change: false,
                class: "",
                body,
                actions,
            })?,
            ..Card::default()
        })
    }
}

fn alert(id: &str, kind: StatusKind, message: &str) -> Result<Html> {
    render(Alert {
        kind,
        id,
        body: Html::text(message),
        ..Alert::default()
    })
}

#[derive(Template)]
#[template(
    source = r#"<div class="flex flex-wrap items-center gap-2">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct Row {
    body: Html,
}

/// One group a drill-down narrows to, with its remove button.
#[derive(Template)]
#[template(
    source = r#"<span class="inline-flex items-center gap-1 text-sm font-medium text-ink">{{ label }}{{ button|safe }}</span>"#,
    ext = "html"
)]
struct Narrowed<'a> {
    label: &'a str,
    button: Html,
}

#[derive(Template)]
#[template(
    source = r#"<nav id="report-drill" aria-label="Narrowed to" class="flex flex-wrap items-center gap-2"><span class="text-sm text-ink-muted">Records behind</span>{{ body|safe }}</nav>"#,
    ext = "html"
)]
struct Breadcrumb {
    body: Html,
}

impl LiveState<Scope, Error> for Show {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.params(values),
                LiveInput::Event(value) => {
                    if let Some((path, message)) =
                        self.event(request.scope, request.credential, value).await?
                    {
                        return Ok(LiveBody {
                            redirect: Some(path),
                            flash: Some((StatusKind::Success, message)),
                            ..LiveBody::default()
                        });
                    }
                }
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.query(),
                ..LiveBody::default()
            })
        })
    }
}
