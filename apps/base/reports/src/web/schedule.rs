//! The Schedule tab of a report: the reader's own scheduled deliveries of
//! it, and the form that adds one. Every write is a contract
//! (`base.reports.subscribe`, `pause_subscription`, `unsubscribe`); each
//! recipient later receives the result their own access allows.
use super::{instant, render, Can};
use crate::{Report, Subscription};
use bilibi_database::Result;
use bilibi_datetime::DisplayContext;
use bilibi_ui::{
    choices, error_sentence, BuiltinColumn, Button, ButtonVariant, Card, ErrorSummary, Field,
    FlexRow, FlexTable, Html, IconButton, IconKind, InputKind, ListColumns, MultiSelect,
    TableEmpty,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const WEEKDAYS: [(&str, &str); 7] = [
    ("Monday", "1"),
    ("Tuesday", "2"),
    ("Wednesday", "3"),
    ("Thursday", "4"),
    ("Friday", "5"),
    ("Saturday", "6"),
    ("Sunday", "7"),
];

/// The add form's draft, held between round trips.
pub(crate) struct Draft {
    frequency: String,
    at: String,
    weekday: String,
    monthday: String,
    format: String,
    include_me: bool,
    skip_empty: bool,
    roles: Vec<String>,
    pub errors: BTreeMap<String, Vec<String>>,
}
impl Default for Draft {
    fn default() -> Self {
        Self {
            frequency: "weekly".into(),
            at: "08:00".into(),
            weekday: "1".into(),
            monthday: "1".into(),
            format: "csv".into(),
            include_me: true,
            skip_empty: true,
            roles: Vec::new(),
            errors: BTreeMap::new(),
        }
    }
}

impl Draft {
    pub fn read(&mut self, value: &Value) {
        let fields = bilibi_ui::params::form_values(value, "sched");
        let get = |key: &str| fields.get(key).cloned().unwrap_or_default();
        self.frequency = get("frequency");
        self.at = get("at");
        self.weekday = get("weekday");
        self.monthday = get("monthday");
        self.format = get("format");
        self.include_me = get("include_me") == "true";
        self.skip_empty = get("skip_empty") == "true";
        self.roles = value
            .get("sched[roles][]")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
    }
    /// The contract's input, in the agent dialect.
    pub fn input(&self, report: i64) -> Value {
        let mut input = json!({
            "id": report, "frequency": self.frequency, "at": self.at,
            "format": self.format, "include_me": self.include_me, "skip_empty": self.skip_empty,
            "roles": self.roles.iter().filter_map(|r| r.parse::<i64>().ok()).collect::<Vec<_>>(),
        });
        // Only the day this frequency uses: the contract takes no nulls.
        let day = match self.frequency.as_str() {
            "weekly" => Some(("weekday", &self.weekday)),
            "monthly" => Some(("monthday", &self.monthday)),
            _ => None,
        };
        if let Some((key, value)) = day {
            if let Ok(day) = value.parse::<i64>() {
                input[key] = json!(day);
            }
        }
        input
    }
}

/// "Weekly on Monday at 08:00 (Asia/Kuala_Lumpur)".
fn when(subscription: &Subscription) -> String {
    let at = format!(
        "{:02}:{:02}",
        subscription.at_minute / 60,
        subscription.at_minute % 60
    );
    let day = match (subscription.weekday, subscription.monthday) {
        (Some(d), _) => format!(" on {}", WEEKDAYS.get(d as usize - 1).map_or("", |w| w.0)),
        (_, Some(d)) => format!(" on day {d}"),
        _ => String::new(),
    };
    let frequency = match subscription.frequency {
        crate::Frequency::Daily => "Daily",
        crate::Frequency::Weekly => "Weekly",
        crate::Frequency::Monthly => "Monthly",
    };
    format!("{frequency}{day} at {at} ({})", subscription.timezone)
}

/// The tab: the reader's schedules of this report, then the add form.
pub(crate) fn tab(
    host: &ListColumns,
    display: &DisplayContext,
    draft: &Draft,
    subscriptions: &[Subscription],
    roles: &[(i64, String)],
    can: Can,
) -> Result<Html> {
    let mut rows = Vec::new();
    for subscription in subscriptions {
        let id = subscription.id.to_string();
        let key = format!("{}:{}", subscription.id, subscription.version);
        let next = match instant(display, &subscription.next_run_at) {
            Some(i) => {
                let cell = format!("report-schedule-next-{id}");
                render(super::index::UpdatedCell { id: &cell, i })?
            }
            None => Html::text(&subscription.next_run_at),
        };
        let last = match (&subscription.last_state, &subscription.last_reason) {
            (Some(state), Some(reason)) => Html::text(&format!("{state}: {reason}")),
            (Some(state), None) => Html::text(state),
            _ => Html::text("Not yet"),
        };
        let recipients = match (subscription.include_owner, subscription.roles.len()) {
            (true, 0) => "You".to_owned(),
            (true, n) => format!("You and {n} role{}", if n == 1 { "" } else { "s" }),
            (false, n) => format!("{n} role{}", if n == 1 { "" } else { "s" }),
        };
        let (pause_icon, pause_label, pause_value) = if subscription.paused {
            ("hero-play", "Resume this schedule", format!("{key}:false"))
        } else {
            ("hero-pause", "Pause this schedule", format!("{key}:true"))
        };
        let mut actions = render(IconButton {
            id: &format!("report-schedule-pause-{id}"),
            kind: IconKind::Neutral,
            event: "pause_schedule",
            name: "schedule",
            value: &pause_value,
            ..IconButton::new(pause_icon, pause_label)
        })?;
        actions.push(&render(IconButton {
            id: &format!("report-schedule-delete-{id}"),
            kind: IconKind::Danger,
            event: "request_unsubscribe",
            name: "schedule",
            value: &key,
            ..IconButton::new("delete", "Delete this schedule")
        })?);
        let mut row = FlexRow::new(id.clone())
            .slot("when", Html::text(&when(subscription)))
            .slot("format", Html::text(&subscription.format.to_uppercase()))
            .slot("recipients", Html::text(&recipients))
            .slot(
                "next",
                if subscription.paused {
                    Html::text("Paused")
                } else {
                    next
                },
            )
            .slot("last", last);
        row.actions = actions;
        row.dom_id = Some(format!("report-schedule-{id}"));
        rows.push(row);
    }
    let mut table = FlexTable::new("report-schedules", host.flex_columns(), rows);
    table.zoom = host.zoom;
    table.caption = "Your schedules of this report";
    table.framed = false;
    table.actions = true;
    table.empty = Some(TableEmpty::Content(Html::text(
        "Not scheduled. Add a schedule below to receive this report by bell and email.",
    )));
    let mut out = render(Card {
        id: "report-schedules-card",
        title: "Your schedules",
        inner_class: "p-0",
        body: render(table)?,
        ..Card::default()
    })?;
    out.push(&form(draft, roles, can)?);
    Ok(out)
}

/// The columns of the schedules table.
pub(crate) fn columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("when", "When"),
        BuiltinColumn::new("format", "Format"),
        BuiltinColumn::new("recipients", "To"),
        BuiltinColumn::new("next", "Next"),
        BuiltinColumn::new("last", "Last delivery to you"),
    ]
}

fn form(draft: &Draft, roles: &[(i64, String)], can: Can) -> Result<Html> {
    let field = |key: &str, label: &str| {
        Field::in_form("sched", key)
            .id(format!("sched-{}", key.replace('_', "-")))
            .label(label)
    };
    let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
    let mut body = Html::EMPTY;
    if !draft.errors.is_empty() {
        let mut summary = ErrorSummary::new("report-schedule-errors");
        for (key, messages) in &draft.errors {
            for message in messages {
                summary.push(format!("sched-{key}"), error_sentence(&label(key), message));
            }
        }
        body.push(&render(summary)?);
    }
    let mut fields = vec![
        render(
            field("frequency", "How often")
                .kind(InputKind::Select)
                .options(choices(&[
                    ("Daily", "daily"),
                    ("Weekly", "weekly"),
                    ("Monthly", "monthly"),
                ]))
                .value(&draft.frequency),
        )?,
        render(
            field("at", "At")
                .kind(InputKind::Time)
                .value(&draft.at)
                .errors(errors("at"))
                .hint("In your time zone."),
        )?,
    ];
    match draft.frequency.as_str() {
        "weekly" => fields.push(render(
            field("weekday", "On")
                .kind(InputKind::Select)
                .options(choices(&WEEKDAYS))
                .value(&draft.weekday)
                .errors(errors("weekday")),
        )?),
        "monthly" => {
            let days: Vec<(String, String)> = (1..=28)
                .map(|d| (format!("Day {d}"), d.to_string()))
                .collect();
            fields.push(render(
                field("monthday", "On")
                    .kind(InputKind::Select)
                    .options(choices(&days))
                    .value(&draft.monthday)
                    .errors(errors("monthday")),
            )?)
        }
        _ => {}
    }
    fields.push(render(
        field("format", "As")
            .kind(InputKind::Select)
            .options(choices(&[("CSV", "csv"), ("PDF", "pdf")]))
            .value(&draft.format),
    )?);
    body.push(&render(Grid { fields })?);
    body.push(&render(
        field("include_me", "Send it to me")
            .kind(InputKind::Checkbox)
            .checked(draft.include_me),
    )?);
    body.push(&render(
        field("skip_empty", "Skip a delivery when the result is empty")
            .kind(InputKind::Checkbox)
            .checked(draft.skip_empty),
    )?);
    if can.share {
        let mut picker = MultiSelect::new("sched-roles", "sched[roles]")
            .label("Also to the holders of")
            .options(roles.iter().map(|(id, name)| bilibi_ui::Choice::new(name, id.to_string())).collect())
            .hint("Each person receives the result their own access allows, and nothing once they lose it.")
            .errors(errors("roles"));
        picker.values = draft.roles.clone();
        picker.selection_label = ":count role|:count roles".into();
        picker.placeholder = "No roles".into();
        picker.chips = true;
        picker.wrap_chips = true;
        body.push(&render(picker)?);
    }
    let actions = render(Button {
        id: "sched-save",
        button_type: "button",
        event: "subscribe",
        variant: ButtonVariant::Primary,
        working: "Scheduling…",
        body: Html::text("Add schedule"),
        ..Button::default()
    })?;
    render(Card {
        id: "report-schedule-form",
        title: "Add a schedule",
        body: render(bilibi_ui::Form {
            id: "report-schedule",
            event: "edit_schedule",
            change: true,
            class: "",
            body,
            actions,
        })?,
        ..Card::default()
    })
}

fn label(key: &str) -> String {
    match key {
        "at" => "The time",
        "weekday" => "The day",
        "monthday" => "The day",
        "roles" => "Roles",
        "recipients" => "Recipients",
        "frequency" => "How often",
        _ => "The schedule",
    }
    .into()
}

#[derive(askama::Template)]
#[template(
    source = r#"<div class="grid gap-x-3 sm:grid-cols-2 lg:grid-cols-4">{% for field in fields %}{{ field|safe }}{% endfor %}</div>"#,
    ext = "html"
)]
struct Grid {
    fields: Vec<Html>,
}

/// A schedule's id and version from a control's value (`id:version`, and
/// `:paused` for the pause control).
pub(crate) fn parse(value: Option<&str>) -> Option<(i64, i64, Option<bool>)> {
    let mut parts = value?.split(':');
    let id = parts.next()?.parse().ok()?;
    let version = parts.next()?.parse().ok()?;
    let paused = parts.next().map(|p| p == "true");
    Some((id, version, paused))
}

/// Whether a report's Schedule tab is offered: the reader may schedule.
pub(crate) fn offered(report: &Report, can: Can) -> bool {
    can.schedule && report.builtin.is_none()
}
