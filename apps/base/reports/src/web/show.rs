//! `/reports/{id}`: one report, read first. Result runs it as the reader
//! (ask-when-run answers and drill-downs narrow it; columns the reader
//! cannot see are named as left out); Definition is the editor, live for
//! its author and read-only for others; Sharing is the author's. Copy and
//! Delete confirm through the shared dialog. Answers and the page travel in
//! the address, so a link and the CSV download repeat the same question.
use super::{
    builder::Builder, clear_flash, flash_map, index::kind_label, page_error, refusal, render,
    report_path, result, schedule, Can, Pages, Refusal,
};
use crate::{ColumnInfo, FilterDef, Kind, Report, ReportError, RunInput, RunResult};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, ActionLink, Alert, Badge, BadgeKind, Button, ButtonVariant, Card, ColumnsOutcome,
    ConfirmDialog, Fact, Facts, Field, FlashMap, Header, Html, IconButton, IconKind, InputKind,
    ListColumns, MultiSelect, StatusKind, Tab, Tabs,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const TABS: [(&str, &str); 4] = [
    ("result", "Result"),
    ("definition", "Definition"),
    ("schedule", "Schedule"),
    ("sharing", "Sharing"),
];
const DENIED_AUTHOR: &str = "You do not have permission to change this report.";
const DENIED_SHARE: &str = "You do not have permission to share reports.";
const STALE_DRAFT: &str = "Someone saved this report while you were editing it, so your changes were not saved. They are still in the form; Discard changes loads the saved report.";
const STALE_SHARING: &str = "Someone saved this report while you were changing who it is shared with, so your sharing was not saved. Your choices are still in the form; Discard changes loads the saved sharing.";

/// Who the Sharing form shares with, at the version it was opened at, so a
/// save from an older form is refused as a conflict, never applied over a
/// newer one.
#[derive(Clone)]
struct Sharing {
    version: i64,
    everyone: bool,
    roles: Vec<i64>,
    /// Refused as stale: the reader's choices, kept until saved or discarded.
    dirty: bool,
}

impl Sharing {
    fn from_report(report: &Report) -> Self {
        Self {
            version: report.version,
            everyone: report.shared_with_everyone,
            roles: report.role_ids.clone(),
            dirty: false,
        }
    }
}

/// A confirmation the dialog is asking. A delete holds the version the
/// reader saw when they asked, so a report saved meanwhile refuses it as a
/// conflict instead of being deleted unseen.
#[derive(Clone, Copy, PartialEq)]
enum Confirm {
    Copy,
    Delete(i64),
    /// A schedule of the reader's, by id and version.
    Unsubscribe(i64, i64),
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
    /// A built-in report's key, read-only and copyable; `None` for a stored one.
    builtin: Option<String>,
    tab: String,
    /// `a<i>` (and `a<i>_to` for a range) by filter position, as typed.
    answers: BTreeMap<String, String>,
    page: usize,
    drill: Vec<Drill>,
    report: Option<Report>,
    builder: Option<Builder>,
    sharing: Option<Sharing>,
    hosts: BTreeMap<&'static str, (Vec<String>, ListColumns)>,
    drillable: Vec<result::Drillable>,
    /// The drawn summary's groupings, as `(spec, label, bucket)`.
    grouped: Vec<(String, String, Option<&'static str>)>,
    /// A summary or matrix shown as the records behind it (a drill-down).
    as_rows: bool,
    /// The last run was too heavy for a page: downloads, which read under
    /// the same bounds, are not offered; Prepare is.
    too_heavy: bool,
    confirm: Option<Confirm>,
    flash: Option<(StatusKind, String)>,
    schedule: schedule::Draft,
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
    /// A built-in report, at `/built-in-reports/{key}`.
    pub fn builtin(pages: Pages, key: String, query: &HashMap<String, String>) -> Self {
        let mut show = Self::new(pages, 0, query);
        show.builtin = Some(key);
        show
    }

    pub fn new(pages: Pages, id: i64, query: &HashMap<String, String>) -> Self {
        let mut show = Self {
            pages,
            id,
            builtin: None,
            tab: "result".into(),
            answers: BTreeMap::new(),
            page: 0,
            drill: Vec::new(),
            report: None,
            builder: None,
            sharing: None,
            hosts: BTreeMap::new(),
            drillable: Vec::new(),
            grouped: Vec::new(),
            as_rows: false,
            too_heavy: false,
            confirm: None,
            flash: None,
            schedule: schedule::Draft::default(),
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
        if tab == "sharing" && self.tab != tab && !self.sharing.as_ref().is_some_and(|s| s.dirty) {
            self.sharing = None;
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
        let report = match &self.builtin {
            Some(key) => self.pages.reports.builtin(scope, key).await,
            None => self.pages.reports.get(scope, self.id).await,
        }
        .map_err(page_error)?;
        let fresh = self
            .report
            .as_ref()
            .is_none_or(|r| r.version != report.version);
        let drafting = self.tab == "definition" || self.builder.as_ref().is_some_and(|b| b.dirty);
        if self.builder.is_none() || (fresh && !drafting) {
            self.builder = Some(Builder::from_report(&report));
        }
        // The Sharing form keeps the version it was drawn at the same way.
        let sharing = self.tab == "sharing" || self.sharing.as_ref().is_some_and(|s| s.dirty);
        if self.sharing.is_none() || (fresh && !sharing) {
            self.sharing = Some(Sharing::from_report(&report));
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
    ) -> Result<Option<(String, Option<String>)>> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        let shown = self.report.as_ref().map(|r| r.version);
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
            // The table on screen: a summary's records once drilled into.
            "grid" => {
                let tab = if report.kind == Kind::Rows || self.as_rows {
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
            // Discard changes is a submit of the Sharing form (see `sharing`), so
            // the browser resets the form's controls with the server's draft.
            "save_sharing" if get("discard").is_some() => {
                self.sharing = Some(Sharing::from_report(&report))
            }
            "edit_schedule" => self.schedule.read(value),
            "subscribe" => {
                self.schedule.errors.clear();
                let input = self.schedule.input(report.id);
                match self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "subscribe",
                        "base.reports.subscribe",
                        input,
                    )
                    .await
                {
                    Ok(_) => {
                        self.schedule = schedule::Draft::default();
                        self.flash = Some((
                            StatusKind::Success,
                            "Scheduled. The bell and an email tell you each time it is delivered."
                                .into(),
                        ));
                    }
                    Err(error) => {
                        match refusal(error, "You do not have permission to schedule reports.")? {
                            Refusal::Fields(fields) => self.schedule.errors = fields,
                            Refusal::Message(message) => {
                                self.flash = Some((StatusKind::Error, message))
                            }
                        }
                    }
                }
            }
            "pause_schedule" => {
                if let Some((id, version, Some(paused))) = schedule::parse(get("schedule")) {
                    let input = json!({"subscription": id, "version": version, "paused": paused});
                    match self
                        .pages
                        .submit(
                            scope,
                            credential,
                            "pause_schedule",
                            "base.reports.pause_subscription",
                            input,
                        )
                        .await
                    {
                        Ok(_) => {
                            self.flash = Some((
                                StatusKind::Success,
                                if paused {
                                    "Schedule paused."
                                } else {
                                    "Schedule resumed."
                                }
                                .into(),
                            ))
                        }
                        Err(error) => self.refused(refusal(
                            error,
                            "You do not have permission to schedule reports.",
                        )?),
                    }
                }
            }
            "request_unsubscribe" => {
                if let Some((id, version, _)) = schedule::parse(get("schedule")) {
                    self.confirm = Some(Confirm::Unsubscribe(id, version));
                }
            }
            "unsubscribe" => {
                if let Some(Confirm::Unsubscribe(id, version)) = self.confirm.take() {
                    let input = json!({"subscription": id, "version": version});
                    match self
                        .pages
                        .submit(
                            scope,
                            credential,
                            "unsubscribe",
                            "base.reports.unsubscribe",
                            input,
                        )
                        .await
                    {
                        Ok(_) => {
                            self.flash = Some((StatusKind::Success, "Schedule deleted.".into()))
                        }
                        Err(error) => self.refused(refusal(
                            error,
                            "You do not have permission to schedule reports.",
                        )?),
                    }
                }
            }
            // Checked before the browser is sent to the file, so a PDF this
            // result cannot make is refused here in words, not as a 403.
            "download_pdf" if report.builtin.is_none() => {
                let input = RunInput {
                    answers: answers(&report, &self.answers),
                    drill: self.drill.iter().map(|d| d.filter.clone()).collect(),
                    page: 0,
                    as_rows: self.as_rows,
                    background: false,
                };
                match self
                    .pages
                    .reports
                    .exportable(scope, report.id, &input, crate::Format::Pdf)
                    .await
                {
                    Ok(()) => return Ok(Some((self.export_href(&report, true), None))),
                    Err(ReportError::Database(error)) => return Err(error),
                    Err(ReportError::Forbidden) => self.refused(Refusal::Message(
                        "You do not have permission to export reports.".into(),
                    )),
                    Err(refused) => self.refused(Refusal::Message(refused.to_string())),
                }
            }
            "prepare_export" => {
                let format = match get("format") {
                    Some("pdf") => "pdf",
                    _ => "csv",
                };
                let input = json!({"id": report.id, "format": format, "answers": agent_answers(&answers(&report, &self.answers))});
                match self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "prepare_export",
                        "base.reports.prepare_export",
                        input,
                    )
                    .await
                {
                    Ok(_) => {
                        self.flash = Some((
                            StatusKind::Success,
                            "Preparing your export. The bell tells you when it is ready.".into(),
                        ))
                    }
                    Err(error) => self.refused(refusal(
                        error,
                        "You do not have permission to export reports.",
                    )?),
                }
            }
            "save_sharing" if report.owned => self.save_sharing(scope, credential, value).await?,
            "request_copy" => self.confirm = Some(Confirm::Copy),
            "request_delete" if report.owned => {
                self.confirm = Some(Confirm::Delete(shown.unwrap_or(report.version)))
            }
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
                        match report.builtin {
                            Some(key) => json!({"builtin": key}),
                            None => json!({"id": report.id}),
                        },
                    )
                    .await
                {
                    Ok(result) => {
                        let id = result["id"].as_i64().ok_or(Error::Internal)?;
                        return Ok(Some((
                            format!("{}?tab=definition", report_path(id)),
                            Some("Your copy is saved. Change it as you like.".into()),
                        )));
                    }
                    Err(error) => self.refused(refusal(
                        error,
                        "You do not have permission to build reports.",
                    )?),
                }
            }
            "delete" if matches!(self.confirm, Some(Confirm::Delete(_))) => {
                let Some(Confirm::Delete(version)) = self.confirm.take() else {
                    return Ok(None);
                };
                match self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "delete",
                        "base.reports.delete",
                        json!({"id": report.id, "version": version}),
                    )
                    .await
                {
                    Ok(_) => {
                        return Ok(Some((
                            super::INDEX.into(),
                            Some(format!("{} was deleted.", report.name)),
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

    /// Narrows the run to a drawn summary line's own groups. Bucketed
    /// groups (a month, a quarter) are not drill targets.
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

    async fn save_sharing(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let Some(sharing) = self.sharing.as_mut() else {
            return Ok(());
        };
        let everyone = value.get("share[everyone]").and_then(Value::as_str) == Some("true");
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
        let input = json!({"id": self.id, "version": sharing.version, "everyone": everyone, "roles": roles});
        sharing.everyone = everyone;
        sharing.roles = roles;
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
                self.report = None;
                self.sharing = None;
                self.flash = Some((
                    StatusKind::Success,
                    "Sharing saved. People run it with their own access.".into(),
                ))
            }
            Err(error) => match refusal(error, DENIED_SHARE)? {
                Refusal::Message(message) if message == ReportError::Conflict.to_string() => {
                    if let Some(sharing) = self.sharing.as_mut() {
                        sharing.dirty = true;
                    }
                    self.flash = Some((StatusKind::Error, STALE_SHARING.into()))
                }
                refusal => self.refused(refusal),
            },
        }
        Ok(())
    }

    pub async fn render(&mut self, scope: &Scope) -> Result<String> {
        let report = self.load(scope).await?;
        let can = self.pages.can(scope).await?;
        if (self.tab == "sharing" && !report.owned)
            || (self.tab == "schedule" && !schedule::offered(&report, can))
        {
            self.tab = "result".into();
        }
        let facts = self.facts(scope, &report).await?;
        let path = page_path(&report);
        let hrefs: Vec<(String, &str, bool)> = TABS
            .iter()
            .filter(|(key, _)| {
                (*key != "sharing" || report.owned)
                    && (*key != "schedule" || schedule::offered(&report, can))
            })
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
            "schedule" => self.schedules(scope, &report, can).await?,
            _ => self.result(scope, &report, can).await?,
        };
        // After the body: a run too heavy for the page withdraws the downloads.
        let header = self.header(&report, can)?;
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
            Some(Confirm::Unsubscribe(..)) => render(ConfirmDialog {
                id: "report-unsubscribe-confirm",
                consequence: "Delete this schedule?",
                detail:
                    "Its deliveries stop, for everyone it reached, and its history goes with it.",
                confirm: "Delete",
                working: "Deleting…",
                confirm_event: "unsubscribe",
                cancel_event: "cancel_confirm",
            })?,
            Some(Confirm::Delete(_)) => {
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

    /// The file of the result shown: its answers, drill-downs and records
    /// view in the address.
    fn export_href(&self, report: &Report, pdf: bool) -> String {
        let mut query = self.query();
        query.retain(|k, _| k.starts_with(['a', 'd', 'v']) || k == "rows");
        if pdf {
            query.insert("format".into(), "pdf".into());
        }
        params::address(&format!("{}/export", report_path(report.id)), &query)
    }

    fn header(&self, report: &Report, can: Can) -> Result<Html> {
        let mut actions = Html::EMPTY;
        if can.export && report.builtin.is_none() && !self.too_heavy {
            actions.push(&render(ActionLink {
                id: "report-export",
                icon: "export",
                title: "Download the whole result as CSV",
                href: &self.export_href(report, false),
                body: Html::text("Download CSV"),
                ..ActionLink::default()
            })?);
            actions.push(&render(Button {
                id: "report-export-pdf",
                button_type: "button",
                event: "download_pdf",
                working: "Checking…",
                body: Html::text("Download PDF"),
                ..Button::default()
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
            render(Row {
                parts: vec![actions],
            })?
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
            out.push(&self.breadcrumb(report.kind)?);
        }
        let input = RunInput {
            answers: answers(report, &self.answers),
            drill: self.drill.iter().map(|d| d.filter.clone()).collect(),
            page: self.page,
            as_rows: self.as_rows,
            background: false,
        };
        let run = match match report.builtin {
            Some(key) => self.pages.reports.run_builtin(scope, key, &input).await,
            None => self.pages.reports.run(scope, report.id, &input).await,
        } {
            // A drill-down's lighter read says nothing about the whole result.
            Ok(run) if self.as_rows => run,
            Ok(run) => {
                self.too_heavy = false;
                run
            }
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
            // Too heavy for a page: offer the background export right here.
            Err(error @ ReportError::TooHeavy(_)) => {
                self.too_heavy = true;
                out.push(&alert(
                    "report-run-refused",
                    StatusKind::Warning,
                    &error.to_string(),
                )?);
                if can.export && report.builtin.is_none() {
                    out.push(&self.exports(scope, report).await?);
                }
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
                        let message = format!("{total} rows is more than a download holds ({cap}). Prepare it as an export instead; you get a notice when it is ready.");
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
                let matrix = (report.kind == Kind::Matrix)
                    .then(|| result::matrix_columns(summary))
                    .flatten();
                if report.kind == Kind::Matrix && matrix.is_none() {
                    let message = format!("More than {} values run across, so this matrix is shown as a summary. Filter it, or group across a field with fewer values.", result::MAX_ACROSS);
                    out.push(&alert("report-matrix-wide", StatusKind::Info, &message)?);
                }
                let (html, drillable) = match matrix {
                    Some(builtins) => {
                        let host = self.host("matrix", &builtins)?;
                        result::matrix(&report.name, host, summary)?
                    }
                    None => {
                        let builtins = result::summary_columns(summary);
                        let host = self.host("summary", &builtins)?;
                        result::summary(&report.name, host, summary)?
                    }
                };
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
        if can.export && !self.as_rows && report.builtin.is_none() {
            out.push(&self.exports(scope, report).await?);
        }
        Ok(out)
    }

    async fn schedules(&mut self, scope: &Scope, report: &Report, can: Can) -> Result<Html> {
        let display = self.pages.display(scope).await?;
        let subscriptions = self
            .pages
            .reports
            .subscriptions(scope, report.id)
            .await
            .map_err(page_error)?;
        let roles = if can.share {
            self.pages
                .reports
                .share_roles(scope)
                .await
                .map_err(page_error)?
        } else {
            Vec::new()
        };
        self.host("schedules", &schedule::columns())?;
        let host = &self.hosts["schedules"].1;
        schedule::tab(host, &display, &self.schedule, &subscriptions, &roles, can)
    }

    /// The reader's prepared exports of this report, and the buttons that
    /// ask for one.
    async fn exports(&mut self, scope: &Scope, report: &Report) -> Result<Html> {
        let display = self.pages.display(scope).await?;
        let list = self
            .pages
            .reports
            .exports(scope, Some(report.id))
            .await
            .map_err(page_error)?;
        let mut rows = Vec::new();
        for export in &list {
            let id = export.id.to_string();
            let when = match super::instant(&display, &export.created_at) {
                Some(i) => {
                    let cell = format!("report-export-at-{id}");
                    render(super::index::UpdatedCell { id: &cell, i })?
                }
                None => Html::text(&export.created_at),
            };
            let state = match export.state {
                "ready" => render(Badge::status(BadgeKind::Success, "Ready"))?,
                "failed" => render(Badge::status(BadgeKind::Danger, "Failed"))?,
                "expired" => render(Badge::status(BadgeKind::Neutral, "Expired"))?,
                _ => render(Badge::status(BadgeKind::Warning, "Preparing"))?,
            };
            let file = match (&export.path, &export.filename) {
                (Some(path), Some(name)) => render(ActionLink {
                    id: &format!("report-export-file-{id}"),
                    icon: "export",
                    title: "Download this export",
                    href: path,
                    body: Html::text(name),
                    ..ActionLink::default()
                })?,
                _ => Html::text(export.message.as_deref().unwrap_or("")),
            };
            let mut row = bilibi_ui::FlexRow::new(id.clone())
                .slot("asked", when)
                .slot("format", Html::text(&export.format.to_uppercase()))
                .slot("state", state)
                .slot(
                    "rows",
                    Html::text(&export.rows.map(|r| r.to_string()).unwrap_or_default()),
                )
                .slot("file", file);
            row.dom_id = Some(format!("report-export-{id}"));
            rows.push(row);
        }
        let builtins = vec![
            bilibi_ui::BuiltinColumn::new("asked", "Asked"),
            bilibi_ui::BuiltinColumn::new("format", "Format"),
            bilibi_ui::BuiltinColumn::new("state", "State"),
            bilibi_ui::BuiltinColumn::new("rows", "Rows").right(),
            bilibi_ui::BuiltinColumn::new("file", "File"),
        ];
        let host = self.host("exports", &builtins)?;
        let mut table = bilibi_ui::FlexTable::new("report-exports", host.flex_columns(), rows);
        table.zoom = host.zoom;
        table.caption = "Your exports of this report";
        table.framed = false;
        table.empty = Some(bilibi_ui::TableEmpty::Content(Html::text(
            "Prepare the whole result in the background when it is too large to download at once. It is kept for a few days.",
        )));
        let mut actions = render(Button {
            id: "report-prepare-csv",
            button_type: "button",
            event: "prepare_export",
            name: "format",
            value: "csv",
            working: "Asking…",
            body: Html::text("Prepare CSV"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "report-prepare-pdf",
            button_type: "button",
            event: "prepare_export",
            name: "format",
            value: "pdf",
            working: "Asking…",
            body: Html::text("Prepare PDF"),
            ..Button::default()
        })?);
        let mut body = render(Row {
            parts: vec![actions],
        })?;
        body.push(&render(table)?);
        render(Card {
            id: "report-exports-card",
            title: "Exports",
            body,
            ..Card::default()
        })
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
                submit_event: "",
            })?,
            ..Card::default()
        })
    }

    /// Where a drill-down stands: the groups it narrows to, each removable,
    /// and the way back to the summary or matrix it came from.
    fn breadcrumb(&self, kind: Kind) -> Result<Html> {
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
            body: Html::text(if kind == Kind::Matrix {
                "Back to the matrix"
            } else {
                "Back to the summary"
            }),
            ..Button::default()
        })?);
        render(Breadcrumb { parts: vec![parts] })
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
        let sharing = self
            .sharing
            .get_or_insert_with(|| Sharing::from_report(report))
            .clone();
        let mut body = render(
            Field::in_form("share", "everyone")
                .id("report-share-everyone")
                .label("Everyone who can run reports")
                .kind(InputKind::Checkbox)
                .checked(sharing.everyone)
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
        picker.values = sharing.roles.iter().map(i64::to_string).collect();
        picker.selection_label = ":count role|:count roles".into();
        picker.placeholder = "No roles".into();
        picker.chips = true;
        picker.wrap_chips = true;
        body.push(&render(picker)?);
        let mut actions = render(Button {
            id: "report-share-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save sharing"),
            ..Button::default()
        })?;
        if sharing.dirty {
            actions.push(&render(Button {
                id: "report-share-reset",
                button_type: "submit",
                name: "discard",
                value: "true",
                body: Html::text("Discard changes"),
                ..Button::default()
            })?);
        }
        render(Card {
            id: "report-sharing",
            body: render(bilibi_ui::Form {
                id: "report-share-form",
                event: "save_sharing",
                change: false,
                class: "",
                body,
                actions,
                submit_event: "",
            })?,
            ..Card::default()
        })
    }
}

/// Run answers in the agent dialect (`definition::schema`'s run input).
fn agent_answers(answers: &std::collections::BTreeMap<usize, Value>) -> Value {
    Value::Array(
        answers
            .iter()
            .map(|(i, value)| match value {
                Value::Array(values) => json!({"filter": i, "values": values}),
                Value::Object(range) => {
                    let mut answer = json!({"filter": i});
                    for key in ["min", "max"] {
                        if let Some(Value::String(bound)) = range.get(key) {
                            answer[key] = json!(bound);
                        }
                    }
                    answer
                }
                other => json!({"filter": i, "value": crate::cell(other)}),
            })
            .collect(),
    )
}

/// The page's own address: a stored report's, or a built-in's.
fn page_path(report: &Report) -> String {
    match report.builtin {
        Some(key) => format!("{}/{key}", super::BUILTIN_BASE),
        None => report_path(report.id),
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
    source = r#"{% import "components.html" as ui %}{% call ui::cluster(parts=parts) %}{% endcall %}"#,
    ext = "html"
)]
struct Row {
    parts: Vec<Html>,
}

/// One group a drill-down narrows to, with its remove button.
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::removable(label=label, button=button, strong=true) %}{% endcall %}"#,
    ext = "html"
)]
struct Narrowed<'a> {
    label: &'a str,
    button: Html,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::narrowing(id="report-drill", label="Narrowed to", lead="Records behind", parts=parts) %}{% endcall %}"#,
    ext = "html"
)]
struct Breadcrumb {
    parts: Vec<Html>,
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
                            flash: message.map(|message| (StatusKind::Success, message)),
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
