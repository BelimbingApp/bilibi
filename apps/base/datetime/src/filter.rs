//! The compact period filter, the Rust counterpart of Bilimbi's
//! `<.period_filter>` and `UI.PeriodFilterComponent` at the pinned source:
//! a preset select and, for a custom range, a compact modal whose dates stay a
//! draft until Apply range succeeds. The page keeps the applied [`Period`] in
//! its URL and owns this component's transient state (open, draft, error),
//! because the server decides whether the dialog exists. Each filter's events
//! carry its id, so several filters can share a page.
//!
//! Wire it like this: build one with [`PeriodFilter::new`] from the URL's
//! period and the request clock, render it beside the filter toolbar, and
//! hand every live event to [`PeriodFilter::event_checked`] (a filter ignores another
//! filter's events); an [`Outcome::Applied`] replaces the URL period and
//! resets the page, an [`Outcome::Update`] only re-renders.
use crate::{Clock, Period, Preset};
use askama::Template;
use bilibi_ui::{params, Choice, Field, InputKind};
use serde_json::Value;
use std::collections::HashMap;

const SELECT: &str = "period_select";
const APPLY: &str = "period_apply";
const CANCEL: &str = "period_cancel";
/// The select's rendered value while editing an applied custom range, so the
/// patcher restores the applied option on Cancel.
const CHANGE_CUSTOM: &str = "change_custom";
const SELECTION_FORM: &str = "period_filter";
const RANGE_FORM: &str = "range";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The applied period changed: put it in the URL and reset the page.
    Applied(Period),
    /// Only the component's own state changed.
    Update,
}

/// Period selection with its private custom-range draft.
#[derive(Template, Clone, Debug)]
#[template(path = "period_filter.html")]
pub struct PeriodFilter {
    pub id: String,
    pub label: String,
    pub period: Period,
    /// The calendar zone the dates are read in (named in the select's title).
    pub timezone: String,
    open: bool,
    draft_from: String,
    draft_to: String,
    error: Option<&'static str>,
}

impl PeriodFilter {
    pub fn new(id: impl Into<String>, period: Period, clock: &Clock) -> Self {
        Self {
            id: id.into(),
            label: "Period".into(),
            period,
            timezone: clock.timezone.clone(),
            open: false,
            draft_from: String::new(),
            draft_to: String::new(),
            error: None,
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// The applied period after the page re-read its URL or clock; the dialog
    /// and draft, if open, are kept.
    pub fn set(&mut self, period: Period, clock: &Clock) {
        self.period = period;
        self.timezone = clock.timezone.clone();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn error(&self) -> Option<&'static str> {
        self.error
    }

    /// The selection form's change event; posts `period_filter[period]`.
    pub fn select_event(&self) -> String {
        format!("{SELECT}:{}", self.id)
    }

    /// The range form's submit event; posts `range[from]` and `range[to]`.
    pub fn apply_event(&self) -> String {
        format!("{APPLY}:{}", self.id)
    }

    /// Escape, the backdrop and Cancel: the applied state stays as it was.
    pub fn cancel_event(&self) -> String {
        format!("{CANCEL}:{}", self.id)
    }

    /// Handles one of this component's events; `None` is someone else's event.
    ///
    /// Deprecated: a posted value over 1 KiB is silently read as blank. Use
    /// [`PeriodFilter::event_checked`], which refuses it.
    pub fn event(&mut self, kind: &str, payload: &Value, clock: &Clock) -> Option<Outcome> {
        self.handle(kind, payload, clock, |payload, form| {
            Ok(params::form_values(payload, form))
        })
        .unwrap_or(None)
    }

    /// Like [`PeriodFilter::event`], but a posted value over the form limit is
    /// refused, not read as blank.
    pub fn event_checked(
        &mut self,
        kind: &str,
        payload: &Value,
        clock: &Clock,
    ) -> Result<Option<Outcome>, params::TooLong> {
        self.handle(kind, payload, clock, |payload, form| {
            params::form_values_checked(payload, form, params::FIELD_LIMIT)
        })
    }

    fn handle(
        &mut self,
        kind: &str,
        payload: &Value,
        clock: &Clock,
        read: impl Fn(&Value, &str) -> Result<HashMap<String, String>, params::TooLong>,
    ) -> Result<Option<Outcome>, params::TooLong> {
        let Some((name, id)) = kind.split_once(':') else {
            return Ok(None);
        };
        if id != self.id {
            return Ok(None);
        }
        Ok(match name {
            SELECT => {
                let posted = read(payload, SELECTION_FORM)?;
                let period = posted.get("period").map(String::as_str).unwrap_or("");
                if period == Preset::Custom.as_str() || period == CHANGE_CUSTOM {
                    self.open();
                    return Ok(Some(Outcome::Update));
                }
                let period = period.to_owned();
                let applied =
                    Period::parse(|key| (key == "period").then(|| period.clone()), clock.today);
                self.close();
                Some(Outcome::Applied(applied))
            }
            CANCEL => {
                self.close();
                Some(Outcome::Update)
            }
            APPLY => {
                let posted = read(payload, RANGE_FORM)?;
                let from = posted.get("from").cloned().unwrap_or_default();
                let to = posted.get("to").cloned().unwrap_or_default();
                match Period::custom(&from, &to) {
                    Ok(applied) => {
                        self.close();
                        Some(Outcome::Applied(applied))
                    }
                    Err(error) => {
                        self.open = true;
                        self.error = Some(error);
                        self.draft_from = from;
                        self.draft_to = to;
                        Some(Outcome::Update)
                    }
                }
            }
            _ => None,
        })
    }

    fn open(&mut self) {
        self.open = true;
        self.error = None;
        self.draft_from = self.period.from.to_string();
        self.draft_to = self.period.to.to_string();
    }

    fn close(&mut self) {
        self.open = false;
        self.error = None;
    }

    fn selected(&self) -> &'static str {
        match (self.open, self.period.preset) {
            (true, Preset::Custom) => CHANGE_CUSTOM,
            (true, _) => Preset::Custom.as_str(),
            (false, preset) => preset.as_str(),
        }
    }

    /// The preset select, labelled for screen readers only.
    pub fn selection(&self) -> Field {
        let mut options: Vec<Choice> = Preset::ALL
            .iter()
            .map(|(label, preset)| Choice::new(*label, preset.as_str()))
            .collect();
        if self.period.preset == Preset::Custom {
            options.push(Choice::new("Change dates…", CHANGE_CUSTOM));
        }
        Field::in_form(SELECTION_FORM, "period")
            .id(format!("{}-period", self.id))
            .kind(InputKind::Select)
            .label(self.label.clone())
            .label_class("sr-only")
            .wrapper_class("mb-0")
            .value(self.selected())
            .options(options)
            .attr(
                "title",
                &format!(
                    "{} – {} ({})",
                    self.period.from, self.period.to, self.timezone
                ),
            )
    }

    pub fn range_id(&self) -> String {
        format!("{}-range", self.id)
    }

    pub fn from_field(&self) -> Field {
        self.date_field("from", "From", &self.draft_from)
    }

    pub fn to_field(&self) -> Field {
        self.date_field("to", "To", &self.draft_to)
    }

    fn date_field(&self, key: &str, label: &str, value: &str) -> Field {
        Field::in_form(RANGE_FORM, key)
            .id(format!("{}-draft-{key}", self.id))
            .kind(InputKind::Date)
            .label(label)
            .wrapper_class("mb-0")
            .value(value)
            .required()
    }
}
