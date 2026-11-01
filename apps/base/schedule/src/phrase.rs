//! Cron presentation, extended for the viewer clock.
//! Plain words for a cron, and the same schedule read in the viewer's zone.
//!
//! Words follow the viewer's top-bar zone at the offset it has now, and name
//! that zone, because a daylight-saving change moves the clock time. When no
//! single clock time reads right in the viewer's zone (a half-hour offset that
//! gives different minutes to different hours, or days that do not shift as
//! one), the words stay in the saved zone and name it.
use crate::cron::Cron;
use jiff::{tz::TimeZone, Timestamp};
use std::collections::BTreeSet;

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const CUSTOM: &str = "Custom schedule";

pub(crate) struct Shown {
    /// What the schedule column shows.
    pub words: String,
    /// The exact saved cron and zone.
    pub tooltip: String,
}

pub(crate) fn show(
    expression: &str,
    stored_zone: &str,
    viewer_zone: &str,
    now: Timestamp,
) -> Shown {
    let tooltip = format!("{expression} · {stored_zone}");
    let Ok(cron) = Cron::parse(expression) else {
        return Shown {
            words: CUSTOM.into(),
            tooltip,
        };
    };
    let pattern = Pattern::from(&cron);
    let stored_words = words(&pattern);
    if stored_zone == viewer_zone || stored_words == CUSTOM {
        let stored_words =
            if pattern.minutes.len() == 1 && pattern.hours.len() == 1 && stored_words != CUSTOM {
                format!("{stored_words} {}", zone_label(stored_zone))
            } else {
                stored_words
            };
        return Shown {
            words: stored_words,
            tooltip,
        };
    }
    let (Ok(stored_tz), Ok(viewer_tz)) = (TimeZone::get(stored_zone), TimeZone::get(viewer_zone))
    else {
        return Shown {
            words: stored_words,
            tooltip,
        };
    };
    let converted = delta_at(&stored_tz, &viewer_tz, now)
        .and_then(|delta| shift(&pattern, delta))
        .map(|converted| (words(&converted), converted))
        .filter(|(text, _)| text != CUSTOM);
    let Some((text, converted)) = converted else {
        return Shown {
            words: format!(
                "{stored_words} ({}; shown in saved time zone)",
                zone_label(stored_zone)
            ),
            tooltip,
        };
    };
    let words = if full(&converted.hours, 0, 23) {
        text
    } else {
        format!("{text} {}", offset_label(&viewer_tz, now))
    };
    Shown { words, tooltip }
}

fn zone_label(zone: &str) -> &str {
    match zone {
        "Etc/UTC" | "UTC" => "UTC",
        other => other,
    }
}

/// The zone's abbreviation at `now` (EDT, UTC), or `GMT+8` when it has none.
fn offset_label(zone: &TimeZone, now: Timestamp) -> String {
    let info = zone.to_offset_info(now);
    let abbreviation = info.abbreviation();
    if !abbreviation.is_empty() && abbreviation.chars().all(|c| c.is_ascii_alphabetic()) {
        return abbreviation.to_owned();
    }
    let minutes = info.offset().seconds() / 60;
    let (sign, minutes) = if minutes < 0 {
        ('-', -minutes)
    } else {
        ('+', minutes)
    };
    match minutes % 60 {
        0 => format!("GMT{sign}{}", minutes / 60),
        rest => format!("GMT{sign}{}:{rest:02}", minutes / 60),
    }
}

fn words(pattern: &Pattern) -> String {
    let Pattern {
        minutes,
        hours,
        days,
        months,
        weekdays,
    } = pattern;
    let open = full(days, 1, 31) && full(months, 1, 12) && full(weekdays, 0, 6);
    if full(minutes, 0, 59) && full(hours, 0, 23) && open {
        return "Every minute".into();
    }
    if full(hours, 0, 23) && open {
        if let Some(step) = every_n(minutes, 60) {
            return format!("Every {step} minutes");
        }
        if minutes.len() == 1 {
            return if minutes[0] == 0 {
                "Every hour".into()
            } else {
                format!("Every hour at minute {}", minutes[0])
            };
        }
    }
    if minutes.len() == 1 && open {
        if let Some(step) = every_n(hours, 24) {
            return if minutes[0] == 0 {
                if step == 1 {
                    "Every hour".into()
                } else {
                    format!("Every {step} hours")
                }
            } else if step == 1 {
                format!("Every hour at minute {}", minutes[0])
            } else {
                format!("Every {step} hours at minute {}", minutes[0])
            };
        }
    }
    if minutes.len() == 1 && hours.len() == 1 && full(months, 1, 12) && full(days, 1, 31) {
        let time = time_of(hours[0], minutes[0]);
        if let Some(when) = weekday_phrase(weekdays) {
            return format!("{when} at {time}");
        }
    }
    if minutes.len() == 1
        && hours.len() == 1
        && full(months, 1, 12)
        && full(weekdays, 0, 6)
        && !full(days, 1, 31)
    {
        return format!(
            "{} at {}",
            monthday_phrase(days),
            time_of(hours[0], minutes[0])
        );
    }
    if hours.len() * minutes.len() <= 8 && open {
        let times = join_and(
            &hours
                .iter()
                .flat_map(|hour| minutes.iter().map(move |minute| time_of(*hour, *minute)))
                .collect::<Vec<_>>(),
        );
        return format!("Every day at {times}");
    }
    CUSTOM.into()
}

fn weekday_phrase(days: &[u8]) -> Option<String> {
    if full(days, 0, 6) {
        return Some("Every day".into());
    }
    if days == [1, 2, 3, 4, 5] {
        return Some("Every weekday".into());
    }
    let names = days
        .iter()
        .map(|day| WEEKDAYS.get(usize::from(*day)).copied())
        .collect::<Option<Vec<_>>>()?;
    let labels = names
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    Some(format!("Every {}", join_and(&labels)))
}

fn monthday_phrase(days: &[u8]) -> String {
    let labels = days.iter().map(|day| day.to_string()).collect::<Vec<_>>();
    if days.len() == 1 {
        format!("On day {} of every month", labels[0])
    } else {
        format!("On days {} of every month", join_and(&labels))
    }
}

fn time_of(hour: u8, minute: u8) -> String {
    let suffix = if hour < 12 { "AM" } else { "PM" };
    let hour = match hour % 12 {
        0 => 12,
        hour => hour,
    };
    format!("{hour}:{minute:02} {suffix}")
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        many => {
            let last = many.last().unwrap();
            format!("{}, and {last}", many[..many.len() - 1].join(", "))
        }
    }
}

/// A complete residue class that tiles `span`, such as every 5 minutes.
fn every_n(values: &[u8], span: u8) -> Option<u8> {
    let step = arithmetic_step(values)?;
    if step <= 1 || !span.is_multiple_of(step) {
        return None;
    }
    let start = values[0];
    if start != 0 {
        return None;
    }
    let expected = (0..span / step)
        .map(|i| start + i * step)
        .collect::<Vec<_>>();
    (expected == values).then_some(step)
}

fn full(values: &[u8], min: u8, max: u8) -> bool {
    values.len() == usize::from(max - min) + 1
}

fn arithmetic_step(values: &[u8]) -> Option<u8> {
    if values.len() < 2 {
        return None;
    }
    let step = values[1].checked_sub(values[0])?;
    if step == 0 {
        return None;
    }
    values
        .windows(2)
        .all(|pair| pair[1].checked_sub(pair[0]) == Some(step))
        .then_some(step)
}

#[derive(Clone)]
struct Pattern {
    minutes: Vec<u8>,
    hours: Vec<u8>,
    days: Vec<u8>,
    months: Vec<u8>,
    weekdays: Vec<u8>,
}

impl Pattern {
    fn from(cron: &Cron) -> Self {
        Self {
            minutes: cron.minute_values(),
            hours: cron.hour_values(),
            days: cron.day_values(),
            months: cron.month_values(),
            weekdays: cron.weekday_values(),
        }
    }
    fn open_days(&self) -> bool {
        full(&self.days, 1, 31) && full(&self.months, 1, 12) && full(&self.weekdays, 0, 6)
    }
}

fn shift(pattern: &Pattern, delta: i32) -> Option<Pattern> {
    if delta == 0
        || (full(&pattern.minutes, 0, 59) && full(&pattern.hours, 0, 23) && pattern.open_days())
    {
        return Some(pattern.clone());
    }
    let pairs = pattern
        .hours
        .iter()
        .flat_map(|hour| {
            pattern.minutes.iter().map(move |minute| {
                let total = i32::from(*hour) * 60 + i32::from(*minute) + delta;
                let day = total.div_euclid(24 * 60);
                let minute_of_day = total.rem_euclid(24 * 60);
                (
                    day,
                    u8::try_from(minute_of_day / 60).unwrap_or(0),
                    u8::try_from(minute_of_day % 60).unwrap_or(0),
                )
            })
        })
        .collect::<Vec<_>>();
    let day_shifts = pairs.iter().map(|pair| pair.0).collect::<BTreeSet<_>>();
    let (hours, minutes) = clock_fields(&pairs)?;
    if day_shifts.len() != 1 {
        if !pattern.open_days() {
            return None;
        }
        return Some(Pattern {
            minutes,
            hours,
            ..pattern.clone()
        });
    }
    let day = *day_shifts.iter().next()?;
    if day == 0 || pattern.open_days() {
        return Some(Pattern {
            minutes,
            hours,
            ..pattern.clone()
        });
    }
    if full(&pattern.days, 1, 31) && full(&pattern.months, 1, 12) {
        let weekdays = pattern
            .weekdays
            .iter()
            .map(|day_index| u8::try_from((i32::from(*day_index) + day).rem_euclid(7)).unwrap_or(0))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        return Some(Pattern {
            minutes,
            hours,
            weekdays,
            ..pattern.clone()
        });
    }
    if full(&pattern.months, 1, 12) && full(&pattern.weekdays, 0, 6) {
        let mut days = BTreeSet::new();
        for date in &pattern.days {
            let shifted = i32::from(*date) + day;
            if *date > 28 || !(1..=28).contains(&shifted) {
                return None;
            }
            days.insert(u8::try_from(shifted).unwrap_or(0));
        }
        return Some(Pattern {
            minutes,
            hours,
            days: days.into_iter().collect(),
            ..pattern.clone()
        });
    }
    None
}

fn clock_fields(pairs: &[(i32, u8, u8)]) -> Option<(Vec<u8>, Vec<u8>)> {
    let unique = pairs
        .iter()
        .map(|pair| (pair.1, pair.2))
        .collect::<BTreeSet<_>>();
    if unique.len() != pairs.len() {
        return None;
    }
    let hours = unique.iter().map(|pair| pair.0).collect::<BTreeSet<_>>();
    let minutes = unique.iter().map(|pair| pair.1).collect::<BTreeSet<_>>();
    if unique.len() != hours.len() * minutes.len() {
        return None;
    }
    Some((hours.into_iter().collect(), minutes.into_iter().collect()))
}

fn delta_at(stored: &TimeZone, viewer: &TimeZone, at: Timestamp) -> Option<i32> {
    let from = stored.to_offset(at).seconds();
    let to = viewer.to_offset(at).seconds();
    if from.rem_euclid(60) != 0 || to.rem_euclid(60) != 0 {
        return None;
    }
    Some((to - from) / 60)
}

/// A fixed-time cron expressed in the editor's zone, preserving its calendar
/// where one cron can represent it. Refuse month-boundary conversions that
/// cannot be represented honestly, just as the read presentation does.
pub(crate) fn editable(
    expression: &str,
    stored_zone: &str,
    viewer_zone: &str,
    now: Timestamp,
) -> Option<String> {
    let cron = Cron::parse(expression).ok()?;
    if cron.minute_values().len() != 1 || cron.hour_values().len() != 1 {
        return None;
    }
    let from = TimeZone::get(stored_zone).ok()?;
    let to = TimeZone::get(viewer_zone).ok()?;
    let pattern = shift(&Pattern::from(&cron), delta_at(&from, &to, now)?)?;
    let field = |values: &[u8], min, max| {
        if full(values, min, max) {
            "*".to_owned()
        } else {
            values
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",")
        }
    };
    Some(format!(
        "{} {} {} {} {}",
        pattern.minutes[0],
        pattern.hours[0],
        field(&pattern.days, 1, 31),
        field(&pattern.months, 1, 12),
        field(&pattern.weekdays, 0, 6)
    ))
}
