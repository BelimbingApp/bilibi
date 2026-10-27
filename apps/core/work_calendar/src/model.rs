//! The shapes a calendar is made of and the rules that keep them valid. No
//! Bilimbi counterpart; the four day types are Belimbing's
//! `AttendanceCalendarResolver` (Normal, Rest, Off, Holiday).
use bilibi_database::Error;
use jiff::civil::Date;
use serde_json::{json, Value};

/// What a date is for payroll, attendance and leave. Fixed in code: a
/// Domain cannot add one the others would not understand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DayType {
    /// Open, with opening periods.
    Working,
    /// The weekly rest day.
    Rest,
    /// Another non-working day, such as Saturday in a five-day week.
    Off,
    Holiday,
}
impl DayType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Rest => "rest",
            Self::Off => "off",
            Self::Holiday => "holiday",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "working" => Self::Working,
            "rest" => Self::Rest,
            "off" => Self::Off,
            "holiday" => Self::Holiday,
            _ => return None,
        })
    }
}

/// Where a holiday that falls on a weekly rest (or off) day moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Substitute {
    None,
    /// A holiday on a rest day moves to the next working day.
    Rest,
    /// A holiday on a rest or off day moves to the next working day.
    RestAndOff,
}
impl Substitute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rest => "rest",
            Self::RestAndOff => "rest_and_off",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "none" => Self::None,
            "rest" => Self::Rest,
            "rest_and_off" => Self::RestAndOff,
            _ => return None,
        })
    }
    pub(crate) fn moves(self, weekly: DayType) -> bool {
        matches!(
            (self, weekly),
            (Self::Rest | Self::RestAndOff, DayType::Rest) | (Self::RestAndOff, DayType::Off)
        )
    }
}

/// An opening period in wall-clock minutes from the start of the day it
/// begins on, half-open: `09:00-18:00` closes at 18:00. `end` past 1440 runs
/// into the next day (`22:00-02:00` is 1320–1560).
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct Period {
    pub start: u16,
    pub end: u16,
}
impl Period {
    /// `HH:MM-HH:MM`; an end at or before the start is on the next day, and
    /// `24:00` is midnight at the end of the day.
    pub fn parse(text: &str) -> Option<Self> {
        let (start, end) = text.trim().split_once('-')?;
        let minute = |part: &str| -> Option<u16> {
            let (hours, minutes) = part.trim().split_once(':')?;
            if hours.len() != 2 || minutes.len() != 2 {
                return None;
            }
            let (hours, minutes): (u16, u16) = (hours.parse().ok()?, minutes.parse().ok()?);
            (hours <= 24 && minutes < 60 && (hours < 24 || minutes == 0))
                .then_some(hours * 60 + minutes)
        };
        let (start, mut end) = (minute(start)?, minute(end)?);
        if start >= 1440 {
            return None;
        }
        if end <= start {
            end += 1440;
        }
        Some(Self { start, end })
    }
    pub fn text(self) -> String {
        let clock = |minutes: u16| {
            let minutes = if minutes > 1440 {
                minutes - 1440
            } else {
                minutes
            };
            format!("{:02}:{:02}", minutes / 60, minutes % 60)
        };
        format!("{}-{}", clock(self.start), clock(self.end))
    }
    pub(crate) fn from_pair(value: &Value) -> Option<Self> {
        let pair = value.as_array()?;
        let (start, end) = (pair.first()?.as_u64()?, pair.get(1)?.as_u64()?);
        let period = Self {
            start: u16::try_from(start).ok()?,
            end: u16::try_from(end).ok()?,
        };
        (pair.len() == 2 && valid(&[period])).then_some(period)
    }
}
/// Up to four periods, each 1 minute to 24 hours starting within the day,
/// sorted and not overlapping.
pub(crate) fn valid(periods: &[Period]) -> bool {
    periods.len() <= 4
        && periods
            .iter()
            .all(|p| p.start < 1440 && p.end > p.start && p.end - p.start <= 1440)
        && periods.windows(2).all(|pair| pair[0].end <= pair[1].start)
}
pub(crate) fn periods_json(periods: &[Period]) -> Value {
    json!(periods
        .iter()
        .map(|p| json!([p.start, p.end]))
        .collect::<Vec<_>>())
}
pub(crate) fn periods_from_json(value: &Value) -> Result<Vec<Period>, Error> {
    value
        .as_array()
        .ok_or(Error::Internal)?
        .iter()
        .map(|pair| Period::from_pair(pair).ok_or(Error::Internal))
        .collect()
}

/// One weekday of a rule version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WeekDay {
    pub day_type: DayType,
    pub periods: Vec<Period>,
}
pub const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
/// Monday to Sunday.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Week(pub [WeekDay; 7]);
impl Week {
    pub(crate) fn day(&self, date: Date) -> &WeekDay {
        &self.0[usize::from(date.weekday().to_monday_zero_offset().unsigned_abs())]
    }
    pub(crate) fn to_stored(&self) -> Value {
        json!(self
            .0
            .iter()
            .map(
                |day| json!({"type": day.day_type.as_str(), "periods": periods_json(&day.periods)})
            )
            .collect::<Vec<_>>())
    }
    pub(crate) fn from_stored(value: &Value) -> Result<Self, Error> {
        let days = value
            .as_array()
            .filter(|days| days.len() == 7)
            .ok_or(Error::Internal)?;
        let days: Vec<WeekDay> = days
            .iter()
            .map(|day| {
                Ok(WeekDay {
                    day_type: day["type"]
                        .as_str()
                        .and_then(DayType::parse)
                        .ok_or(Error::Internal)?,
                    periods: periods_from_json(&day["periods"])?,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self(days.try_into().map_err(|_| Error::Internal)?))
    }
    /// `{"monday": {"type": "working", "periods": ["09:00-13:00", ...]}, ...}`
    pub fn to_json(&self) -> Value {
        let mut week = serde_json::Map::new();
        for (name, day) in WEEKDAYS.iter().zip(&self.0) {
            week.insert(
                (*name).into(),
                json!({"type": day.day_type.as_str(), "periods": day.periods.iter().map(|p| p.text()).collect::<Vec<_>>()}),
            );
        }
        Value::Object(week)
    }
    /// Reads the shape `to_json` writes. Every weekday is required; a working
    /// day needs one to four periods and any other day none, and a day's
    /// late hours may not run into the next day's.
    pub fn parse(value: &Value, errors: &mut Vec<FieldError>) -> Option<Self> {
        let Some(week) = value.as_object() else {
            errors.push(FieldError::new("week", "must name each weekday"));
            return None;
        };
        if let Some(extra) = week.keys().find(|key| !WEEKDAYS.contains(&key.as_str())) {
            errors.push(FieldError::new("week", format!("{extra} is not a weekday")));
        }
        let mut days = Vec::with_capacity(7);
        for name in WEEKDAYS {
            let field = format!("week.{name}");
            let Some(day) = week.get(name) else {
                errors.push(FieldError::new(&field, "is required"));
                continue;
            };
            if let Some(day) = day_entry(day, &field, errors) {
                days.push(day);
            }
        }
        let week = Self(days.try_into().ok()?);
        for (index, name) in WEEKDAYS.iter().enumerate() {
            let next = &week.0[(index + 1) % 7];
            let late = week.0[index]
                .periods
                .iter()
                .map(|p| p.end)
                .max()
                .unwrap_or(0);
            let early = next.periods.iter().map(|p| p.start).min().unwrap_or(1440);
            if late > 1440 + early {
                errors.push(FieldError::new(
                    &format!("week.{name}"),
                    "runs into the next day's opening hours",
                ));
            }
        }
        errors.is_empty().then_some(week)
    }
}
/// One day's type and periods, as a weekday or an exception gives them.
pub(crate) fn day_entry(
    value: &Value,
    field: &str,
    errors: &mut Vec<FieldError>,
) -> Option<WeekDay> {
    let Some(day_type) = value["type"].as_str().and_then(DayType::parse) else {
        errors.push(FieldError::new(
            &format!("{field}.type"),
            "must be working, rest, off or holiday",
        ));
        return None;
    };
    let texts = match &value["periods"] {
        Value::Null => Vec::new(),
        Value::Array(items) => items.clone(),
        _ => {
            errors.push(FieldError::new(
                &format!("{field}.periods"),
                "must be a list",
            ));
            return None;
        }
    };
    let mut periods = Vec::with_capacity(texts.len());
    for text in &texts {
        match text.as_str().and_then(Period::parse) {
            Some(period) => periods.push(period),
            None => {
                errors.push(FieldError::new(
                    &format!("{field}.periods"),
                    "each period is HH:MM-HH:MM",
                ));
                return None;
            }
        }
    }
    periods.sort();
    let field = format!("{field}.periods");
    match day_type {
        DayType::Working if periods.is_empty() => {
            errors.push(FieldError::new(&field, "a working day needs opening hours"));
            None
        }
        DayType::Working if !valid(&periods) => {
            errors.push(FieldError::new(
                &field,
                "at most four periods, each under 24 hours, not overlapping",
            ));
            None
        }
        DayType::Working => Some(WeekDay { day_type, periods }),
        _ if !periods.is_empty() => {
            errors.push(FieldError::new(
                &field,
                "only a working day has opening hours",
            ));
            None
        }
        _ => Some(WeekDay {
            day_type,
            periods: Vec::new(),
        }),
    }
}

/// Why a date resolved as it did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reason {
    /// The week of the rule version in force.
    Week,
    Holiday {
        set: String,
        name: String,
    },
    /// A holiday on a rest (or off) day, moved here from `holiday`.
    Substitute {
        set: String,
        name: String,
        holiday: Date,
    },
    Exception {
        name: String,
    },
}
impl Reason {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Week => json!({"kind": "week"}),
            Self::Holiday { set, name } => json!({"kind": "holiday", "set": set, "name": name}),
            Self::Substitute { set, name, holiday } => {
                json!({"kind": "substitute", "set": set, "name": name, "holiday": holiday.to_string()})
            }
            Self::Exception { name } => json!({"kind": "exception", "name": name}),
        }
    }
}

/// One resolved date.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Day {
    pub date: Date,
    pub day_type: DayType,
    /// The type the week gives the date underneath (a holiday on a rest day).
    pub weekly: DayType,
    /// Empty unless `day_type` is working.
    pub periods: Vec<Period>,
    pub reason: Reason,
    /// The zone the periods are read in.
    pub timezone: String,
}
impl Day {
    pub fn to_json(&self) -> Value {
        json!({
            "date": self.date.to_string(), "type": self.day_type.as_str(), "weekly": self.weekly.as_str(),
            "periods": self.periods.iter().map(|p| p.text()).collect::<Vec<_>>(),
            "reason": self.reason.to_json(), "timezone": self.timezone,
        })
    }
}

/// A calendar a picker offers, or a module stores by `code`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    pub code: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub effective_from: Date,
    pub timezone: String,
    pub week: Week,
}
#[derive(Clone, Debug)]
pub struct Exception {
    pub id: i64,
    pub starts_on: Date,
    pub ends_on: Date,
    pub name: String,
    pub day: WeekDay,
}
#[derive(Clone, Debug)]
pub struct Calendar {
    pub code: String,
    pub name: String,
    pub status: String,
    pub substitute: Substitute,
    pub version: i64,
    pub rules: Vec<Rule>,
    /// Codes of the holiday sets it includes.
    pub holiday_sets: Vec<String>,
    pub exceptions: Vec<Exception>,
}
impl Calendar {
    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code, "name": self.name, "status": self.status,
            "substitute": self.substitute.as_str(), "version": self.version,
            "rules": self.rules.iter().map(|rule| json!({"effective_from": rule.effective_from.to_string(), "timezone": rule.timezone, "week": rule.week.to_json()})).collect::<Vec<_>>(),
            "holiday_sets": self.holiday_sets,
            "exceptions": self.exceptions.iter().map(|e| json!({
                "id": e.id, "starts_on": e.starts_on.to_string(), "ends_on": e.ends_on.to_string(), "name": e.name,
                "type": e.day.day_type.as_str(), "periods": e.day.periods.iter().map(|p| p.text()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }
}
#[derive(Clone, Debug)]
pub struct Holiday {
    pub date: Date,
    pub name: String,
}
#[derive(Clone, Debug)]
pub struct HolidaySet {
    pub code: String,
    pub name: String,
    pub country_iso: Option<String>,
    pub admin1_code: Option<String>,
    pub version: i64,
    /// Codes of the calendars that include it.
    pub calendars: Vec<String>,
    pub holidays: Vec<Holiday>,
}
impl HolidaySet {
    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code, "name": self.name, "country_iso": self.country_iso, "admin1_code": self.admin1_code,
            "version": self.version, "calendars": self.calendars,
            "holidays": self.holidays.iter().map(|h| json!({"date": h.date.to_string(), "name": h.name})).collect::<Vec<_>>(),
        })
    }
}
/// The tenant's calendars and holiday sets, for a list.
#[derive(Clone, Debug)]
pub struct Summary {
    pub calendars: Vec<(String, String, String)>,
    pub holiday_sets: Vec<(String, String, Option<String>, Option<String>)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}
impl FieldError {
    pub fn new(field: &str, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("Not found")]
    NotFound,
    #[error("The record changed. Reload before saving")]
    Stale,
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    #[error(transparent)]
    Access(#[from] Error),
}
pub type WriteResult<T> = std::result::Result<T, WriteError>;
impl From<tokio_postgres::Error> for WriteError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Access(error.into())
    }
}
pub(crate) fn invalid(field: &str, message: impl Into<String>) -> WriteError {
    WriteError::Invalid(vec![FieldError::new(field, message)])
}

/// Codes are stable keys other modules store: lowercase letters, digits, `_`
/// and `-`, 1–64 characters.
pub(crate) fn valid_code(code: &str) -> bool {
    let mut bytes = code.bytes();
    code.len() <= 64
        && bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
pub(crate) fn name(value: &Value, field: &str, errors: &mut Vec<FieldError>) -> Option<String> {
    match value.as_str().map(str::trim) {
        Some(name)
            if !name.is_empty()
                && name.chars().count() <= 120
                && !name.chars().any(char::is_control) =>
        {
            Some(name.to_owned())
        }
        _ => {
            errors.push(FieldError::new(field, "must be 1–120 characters"));
            None
        }
    }
}
pub(crate) fn date(value: &Value, field: &str, errors: &mut Vec<FieldError>) -> Option<Date> {
    match value.as_str().and_then(|text| text.parse::<Date>().ok()) {
        Some(date) => Some(date),
        None => {
            errors.push(FieldError::new(field, "must be a date (YYYY-MM-DD)"));
            None
        }
    }
}
