//! Standard five-field cron over civil (wall-clock) time. It knows nothing about
//! zones: `recurrence` maps these wall times to instants, so DST gaps and folds
//! are decided in exactly one place.
//!
//! Grammar per field: `*`, `n`, `a-b`, each with an optional `/step`, in a comma
//! list; `n/step` runs from `n` to the field maximum. Months accept `JAN`..`DEC`
//! and weekdays `SUN`..`SAT` (0 and 7 are Sunday). Macros (`@daily`), `?`, `L`,
//! `W`, `#`, seconds and year fields are refused. As in Bilimbi's Crontab
//! library, every field must match: a restricted day-of-month and a restricted
//! weekday are ANDed, not ORed as in Vixie cron.
use jiff::civil::{Date, DateTime};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CronError {
    /// Not exactly five whitespace-separated fields (or a macro).
    Fields,
    /// The named field is malformed or out of range.
    Field(&'static str),
    /// No date ever matches (for example `0 0 31 2 *`).
    Never,
}
impl std::fmt::Display for CronError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fields => f.write_str("a standard five-field cron expression is required"),
            Self::Field(name) => write!(f, "the {name} field is invalid"),
            Self::Never => f.write_str("the expression never matches a date"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cron {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
}

struct Spec {
    name: &'static str,
    min: u32,
    max: u32,
    names: &'static [&'static str],
    first_name: u32,
}
const MINUTE: Spec = Spec {
    name: "minute",
    min: 0,
    max: 59,
    names: &[],
    first_name: 0,
};
const HOUR: Spec = Spec {
    name: "hour",
    min: 0,
    max: 23,
    names: &[],
    first_name: 0,
};
const DAY: Spec = Spec {
    name: "day-of-month",
    min: 1,
    max: 31,
    names: &[],
    first_name: 1,
};
const MONTH: Spec = Spec {
    name: "month",
    min: 1,
    max: 12,
    names: &[
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ],
    first_name: 1,
};
const WEEKDAY: Spec = Spec {
    name: "day-of-week",
    min: 0,
    max: 7,
    names: &["sun", "mon", "tue", "wed", "thu", "fri", "sat"],
    first_name: 0,
};

fn value(spec: &Spec, text: &str) -> Result<u32, CronError> {
    let bad = CronError::Field(spec.name);
    let number = if text.bytes().all(|b| b.is_ascii_digit()) && !text.is_empty() {
        text.parse::<u32>().map_err(|_| bad)?
    } else {
        let lower = text.to_ascii_lowercase();
        let index = spec
            .names
            .iter()
            .position(|name| *name == lower)
            .ok_or(bad)?;
        spec.first_name + index as u32
    };
    if (spec.min..=spec.max).contains(&number) {
        Ok(number)
    } else {
        Err(bad)
    }
}
/// Bit `n` is set for every allowed value `n` (weekday 7 folds onto 0).
fn field(spec: &Spec, text: &str) -> Result<u64, CronError> {
    let bad = CronError::Field(spec.name);
    let mut bits = 0u64;
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step: u32 = if step.bytes().all(|b| b.is_ascii_digit()) && !step.is_empty() {
                    step.parse().map_err(|_| bad)?
                } else {
                    return Err(bad);
                };
                if step == 0 || step > spec.max {
                    return Err(bad);
                }
                (range, Some(step))
            }
            None => (item, None),
        };
        let (low, high) = if range == "*" {
            (spec.min, if spec.max == 7 { 6 } else { spec.max })
        } else if let Some((low, high)) = range.split_once('-') {
            (value(spec, low)?, value(spec, high)?)
        } else {
            let low = value(spec, range)?;
            // `n/step` continues to the field maximum; plain `n` is one value.
            (low, if step.is_some() { spec.max } else { low })
        };
        if low > high {
            return Err(bad);
        }
        let mut current = low;
        while current <= high {
            bits |= 1
                << if spec.max == 7 && current == 7 {
                    0
                } else {
                    current
                };
            current += step.unwrap_or(1);
        }
    }
    Ok(bits)
}

impl Cron {
    pub fn parse(expression: &str) -> Result<Self, CronError> {
        let parts: Vec<&str> = expression.split_whitespace().collect();
        if parts.len() != 5 || parts[0].starts_with('@') {
            return Err(CronError::Fields);
        }
        let cron = Self {
            minutes: field(&MINUTE, parts[0])?,
            hours: field(&HOUR, parts[1])? as u32,
            days: field(&DAY, parts[2])? as u32,
            months: field(&MONTH, parts[3])? as u16,
            weekdays: field(&WEEKDAY, parts[4])? as u8,
        };
        // Every Gregorian calendar repeats within 400 years, so a date that has
        // no match there never will (`0 0 30 2 *`, `0 0 31 4,6 *`).
        let start = Date::constant(2000, 1, 1).at(0, 0, 0, 0);
        let end = Date::constant(2400, 1, 1).at(0, 0, 0, 0);
        if cron.next_at_or_after(start, end).is_none() {
            return Err(CronError::Never);
        }
        Ok(cron)
    }

    fn day_matches(&self, date: Date) -> bool {
        self.months & (1 << date.month()) != 0
            && self.days & (1 << date.day()) != 0
            && self.weekdays & (1 << date.weekday().to_sunday_zero_offset()) != 0
    }
    /// Whether the wall-clock minute matches. Seconds are ignored.
    pub fn matches(&self, at: DateTime) -> bool {
        self.day_matches(at.date())
            && self.hours & (1 << at.hour()) != 0
            && self.minutes & (1 << at.minute()) != 0
    }

    /// Earliest matching minute in `from..=until` (`from` rounds up to a whole
    /// minute). The search is bounded by the caller; it never loops unbounded.
    pub fn next_at_or_after(&self, from: DateTime, until: DateTime) -> Option<DateTime> {
        let from = ceil_minute(from)?;
        let mut date = from.date();
        let mut floor = Some((from.hour() as u32, from.minute() as u32));
        while date <= until.date() {
            if self.months & (1 << date.month()) == 0 {
                date = first_of_next_month(date)?;
                floor = None;
                continue;
            }
            if self.day_matches(date) {
                let (start_hour, start_minute) = floor.unwrap_or((0, 0));
                for hour in start_hour..24 {
                    if self.hours & (1 << hour) == 0 {
                        continue;
                    }
                    let minimum = if hour == start_hour { start_minute } else { 0 };
                    let bits = self.minutes >> minimum;
                    if bits != 0 {
                        let minute = minimum + bits.trailing_zeros();
                        let found = date.at(hour as i8, minute as i8, 0, 0);
                        return (found <= until).then_some(found);
                    }
                }
            }
            date = date.tomorrow().ok()?;
            floor = None;
        }
        None
    }

    pub(crate) fn minute_values(&self) -> Vec<u8> {
        bits(self.minutes, 0, 59)
    }
    pub(crate) fn hour_values(&self) -> Vec<u8> {
        bits(u64::from(self.hours), 0, 23)
    }
    pub(crate) fn day_values(&self) -> Vec<u8> {
        bits(u64::from(self.days), 1, 31)
    }
    pub(crate) fn month_values(&self) -> Vec<u8> {
        bits(u64::from(self.months), 1, 12)
    }
    pub(crate) fn weekday_values(&self) -> Vec<u8> {
        bits(u64::from(self.weekdays), 0, 6)
    }

    /// Latest matching minute in `since..=from` (`from` rounds down).
    pub fn prev_at_or_before(&self, from: DateTime, since: DateTime) -> Option<DateTime> {
        let from = from.with().second(0).subsec_nanosecond(0).build().ok()?;
        let mut date = from.date();
        let mut ceiling = Some((from.hour() as u32, from.minute() as u32));
        while date >= since.date() {
            if self.months & (1 << date.month()) == 0 {
                date = date.first_of_month().yesterday().ok()?;
                ceiling = None;
                continue;
            }
            if self.day_matches(date) {
                let (end_hour, end_minute) = ceiling.unwrap_or((23, 59));
                for hour in (0..=end_hour).rev() {
                    if self.hours & (1 << hour) == 0 {
                        continue;
                    }
                    let maximum = if hour == end_hour { end_minute } else { 59 };
                    let bits = self.minutes & (u64::MAX >> (63 - maximum));
                    if bits != 0 {
                        let minute = 63 - bits.leading_zeros();
                        let found = date.at(hour as i8, minute as i8, 0, 0);
                        return (found >= since).then_some(found);
                    }
                }
            }
            date = date.yesterday().ok()?;
            ceiling = None;
        }
        None
    }
}

fn bits(bits: u64, min: u32, max: u32) -> Vec<u8> {
    (min..=max)
        .filter(|n| bits & (1u64 << n) != 0)
        .map(|n| n as u8)
        .collect()
}

fn ceil_minute(at: DateTime) -> Option<DateTime> {
    let floor = at.with().second(0).subsec_nanosecond(0).build().ok()?;
    if floor == at {
        Some(at)
    } else {
        floor.checked_add(jiff::Span::new().minutes(1)).ok()
    }
}
fn first_of_next_month(date: Date) -> Option<Date> {
    date.last_of_month().tomorrow().ok()
}
