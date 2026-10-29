//! The week editor of the create and set-week dialogs (a type and opening
//! hours per weekday, posted as `<form>[monday_type]` and
//! `<form>[monday_periods]`), and how a day reads elsewhere on the pages.
use super::*;
use crate::{DayType, Period, Week, WeekDay};
use bilibi_ui::{Choice, DayKind, Field, InputKind};
use serde_json::json;

/// Each weekday as the forms name it and as people read it, Monday first.
pub(super) const DAYS: [(&str, &str); 7] = [
    ("monday", "Monday"),
    ("tuesday", "Tuesday"),
    ("wednesday", "Wednesday"),
    ("thursday", "Thursday"),
    ("friday", "Friday"),
    ("saturday", "Saturday"),
    ("sunday", "Sunday"),
];
/// The type choices, in the order a person meets them.
pub(super) fn types() -> Vec<Choice> {
    [
        DayType::Working,
        DayType::Off,
        DayType::Rest,
        DayType::Holiday,
    ]
    .into_iter()
    .map(|t| Choice::new(kind(t).label(), t.as_str()))
    .collect()
}
pub(super) fn is_day(name: &str) -> bool {
    DAYS.iter().any(|(day, _)| *day == name)
}
/// Whether a form key is one of the editor's fields (`monday_periods`).
pub(super) fn is_field(key: &str) -> bool {
    key.split_once('_')
        .is_some_and(|(day, part)| is_day(day) && matches!(part, "type" | "periods"))
}
/// A weekday field's words: `monday_type` is "Monday", `monday_periods`
/// "Monday hours".
pub(super) fn label(key: &str) -> Option<String> {
    let (day, part) = key.split_once('_')?;
    let (_, name) = DAYS.iter().find(|(d, _)| *d == day)?;
    Some(match part {
        "periods" => format!("{name} hours"),
        _ => (*name).to_owned(),
    })
}
pub(super) fn kind(day_type: DayType) -> DayKind {
    match day_type {
        DayType::Working => DayKind::Working,
        DayType::Rest => DayKind::Rest,
        DayType::Off => DayKind::Off,
        DayType::Holiday => DayKind::Holiday,
    }
}
/// A period as people read it: `09:00–13:00`.
pub(super) fn period(period: &Period) -> String {
    period.text().replacen('-', "–", 1)
}
/// A day as a table cell reads it: its hours, or its type.
pub(super) fn summary(day: &WeekDay) -> String {
    if day.day_type == DayType::Working {
        day.periods
            .iter()
            .map(period)
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        kind(day.day_type).label().to_owned()
    }
}
/// Hours as typed into the editor: `09:00-13:00, 14:00-18:00`.
fn typed(periods: &[Period]) -> String {
    periods
        .iter()
        .map(|p| p.text())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A new calendar's week: Monday to Friday 09:00–18:00, Saturday off, Sunday
/// rest.
pub(super) fn defaults() -> HashMap<String, String> {
    let mut values = HashMap::new();
    for (index, (day, _)) in DAYS.iter().enumerate() {
        let day_type = match index {
            5 => "off",
            6 => "rest",
            _ => "working",
        };
        values.insert(format!("{day}_type"), day_type.into());
        if index < 5 {
            values.insert(format!("{day}_periods"), "09:00-18:00".into());
        }
    }
    values
}
/// The editor's values for a stored week.
pub(super) fn values(week: &Week) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for ((day, _), weekday) in DAYS.iter().zip(&week.0) {
        values.insert(format!("{day}_type"), weekday.day_type.as_str().into());
        values.insert(format!("{day}_periods"), typed(&weekday.periods));
    }
    values
}
/// Opening hours as the contracts take them: one `HH:MM-HH:MM` per period.
/// An en dash is read as the hyphen it stands for.
pub(super) fn periods(text: &str) -> Vec<String> {
    text.split(',')
        .map(|period| period.trim().replace('–', "-"))
        .filter(|period| !period.is_empty())
        .collect()
}
/// The `week` a contract takes. Only a working day sends its hours, so hours
/// left in a day changed to rest are not refused.
pub(super) fn input(posted: &HashMap<String, String>) -> Value {
    let get = |key: String| posted.get(&key).map_or("", String::as_str);
    let mut week = serde_json::Map::new();
    for (day, _) in DAYS {
        let day_type = get(format!("{day}_type"));
        let hours = if day_type == "working" {
            periods(get(format!("{day}_periods")))
        } else {
            Vec::new()
        };
        week.insert(day.into(), json!({"type": day_type, "periods": hours}));
    }
    Value::Object(week)
}

#[derive(Template)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% for row in rows %}{% call forms::field_row(fields=row) %}{% endcall %}{% endfor %}"#,
    ext = "html"
)]
struct Rows {
    rows: Vec<Vec<Html>>,
}
/// Seven rows of a type and its hours, with each field's refusals.
pub(super) fn editor(
    form: &str,
    values: &HashMap<String, String>,
    errors: &BTreeMap<String, Vec<String>>,
) -> Result<Html> {
    let value = |key: &str| values.get(key).cloned().unwrap_or_default();
    let errors = |key: &str| errors.get(key).cloned().unwrap_or_default();
    let mut rows = Vec::with_capacity(DAYS.len());
    for (day, name) in DAYS {
        let (type_key, hours_key) = (format!("{day}_type"), format!("{day}_periods"));
        rows.push(vec![
            render(
                Field::in_form(form, &type_key)
                    .id(format!("{form}-{day}-type"))
                    .label(name)
                    .kind(InputKind::Select)
                    .options(types())
                    .value(value(&type_key))
                    .errors(errors(&type_key)),
            )?,
            render(
                Field::in_form(form, &hours_key)
                    .id(format!("{form}-{day}-periods"))
                    .label(format!("{name} hours"))
                    .value(value(&hours_key))
                    .placeholder("09:00-13:00, 14:00-18:00")
                    .errors(errors(&hours_key)),
            )?,
        ]);
    }
    render(Rows { rows })
}
