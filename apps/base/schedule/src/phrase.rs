//! Mirrors Bilimbi Schedule.Web.IndexLive cron presentation, extended for the viewer clock.
//! Plain words for a cron, and the same schedule read in another time zone.
//!
//! Words follow the viewer's top-bar zone when the instants are exactly one
//! cron there. A fixed hour across a daylight-saving change, or a half-hour
//! offset that gives different minutes to different hours, stays in the stored
//! zone: the words name that zone instead of showing a wrong conversion.
use crate::cron::Cron;
use jiff::{tz::TimeZone, Span, Timestamp};
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

pub(crate) struct Shown {
    /// What the schedule column shows.
    pub words: String,
    /// Matching cron and zone, with the saved clock when converted.
    pub tooltip: String,
}

pub(crate) fn show(expression: &str, stored_zone: &str, viewer_zone: &str) -> Shown {
    let tooltip = format!("{expression} · {stored_zone}");
    let Ok(cron) = Cron::parse(expression) else {
        return Shown {
            words: "Custom schedule".into(),
            tooltip,
        };
    };
    let stored_words = words(&cron);
    if stored_zone == viewer_zone {
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
    if let Some(converted) = project(&cron, &stored_tz, &viewer_tz) {
        return Shown {
            words: words(&converted),
            tooltip: format!(
                "{} · {viewer_zone} (saved: {tooltip})",
                canonical(&converted)
            ),
        };
    }
    let label = zone_label(stored_zone);
    Shown {
        words: format!("{stored_words} ({label}; shown in saved time zone)"),
        tooltip: format!(
            "{tooltip}. Shown in {label}. It does not match one schedule in your time zone."
        ),
    }
}

/// A sentence for a confirmation: the stored wording, then the stored zone.
pub(crate) fn sentence(expression: &str, timezone: &str) -> String {
    let words = Cron::parse(expression)
        .map(|cron| words(&cron))
        .unwrap_or_else(|_| "Custom schedule".into());
    if words == "Custom schedule" {
        format!("{expression}, {timezone}")
    } else {
        format!("{words} ({timezone})")
    }
}

/// The cron a person typed, as a canonical five-field expression. Words we
/// show round-trip; a five-field cron is accepted as typed.
pub(crate) fn interpret(text: &str) -> Option<String> {
    let text = strip_zone_suffix(text.trim());
    if text.is_empty() || text.chars().count() > 120 {
        return None;
    }
    let cron = Cron::parse(text).ok().or_else(|| parse_words(text))?;
    let expression = canonical(&cron);
    (expression.len() <= 64).then_some(expression)
}

pub(crate) fn canonical(cron: &Cron) -> String {
    format!(
        "{} {} {} {} {}",
        encode(&cron.minute_values(), 0, 59),
        encode(&cron.hour_values(), 0, 23),
        encode(&cron.day_values(), 1, 31),
        encode(&cron.month_values(), 1, 12),
        encode(&cron.weekday_values(), 0, 6),
    )
}

fn zone_label(zone: &str) -> &str {
    match zone {
        "Etc/UTC" | "UTC" => "UTC",
        other => other,
    }
}

fn strip_zone_suffix(text: &str) -> &str {
    match text.rfind(" (") {
        Some(open) if text.ends_with(')') => text[..open].trim(),
        _ => text,
    }
}

fn words(cron: &Cron) -> String {
    let minutes = cron.minute_values();
    let hours = cron.hour_values();
    let days = cron.day_values();
    let months = cron.month_values();
    let weekdays = cron.weekday_values();
    let open = full(&days, 1, 31) && full(&months, 1, 12) && full(&weekdays, 0, 6);
    if full(&minutes, 0, 59) && full(&hours, 0, 23) && open {
        return "Every minute".into();
    }
    if full(&hours, 0, 23) && open {
        if let Some(step) = every_n(&minutes, 60) {
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
        if let Some(step) = every_n(&hours, 24) {
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
    if minutes.len() == 1 && hours.len() == 1 && full(&months, 1, 12) && full(&days, 1, 31) {
        let time = time_of(hours[0], minutes[0]);
        if let Some(when) = weekday_phrase(&weekdays) {
            return format!("{when} at {time}");
        }
    }
    if minutes.len() == 1
        && hours.len() == 1
        && full(&months, 1, 12)
        && full(&weekdays, 0, 6)
        && !full(&days, 1, 31)
    {
        return format!(
            "{} at {}",
            monthday_phrase(&days),
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
    "Custom schedule".into()
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

fn parse_words(text: &str) -> Option<Cron> {
    let lower = text.to_ascii_lowercase();
    if let Some(cron) = parse_monthdays(&lower) {
        return Some(cron);
    }
    if lower == "every minute" {
        return assemble(
            &all(0, 59),
            &all(0, 23),
            &all(1, 31),
            &all(1, 12),
            &all(0, 6),
        );
    }
    let rest = lower.strip_prefix("every ")?;
    if let Some(step) = rest.strip_suffix(" minutes").and_then(|n| n.parse().ok()) {
        if (2..60).contains(&step) && 60u8.is_multiple_of(step) {
            return assemble(
                &stepped(0, 59, step),
                &all(0, 23),
                &all(1, 31),
                &all(1, 12),
                &all(0, 6),
            );
        }
    }
    if rest == "hour" {
        return at_hours(0, &stepped(0, 23, 1));
    }
    if let Some(step) = rest.strip_suffix(" hours").and_then(|n| n.parse().ok()) {
        if (2..24).contains(&step) && 24u8.is_multiple_of(step) {
            return at_hours(0, &stepped(0, 23, step));
        }
    }
    if let Some(minute) = rest
        .strip_prefix("hour at minute ")
        .and_then(|n| n.parse().ok())
    {
        if minute < 60 {
            return at_hours(minute, &all(0, 23));
        }
    }
    if let Some((step, minute)) = hours_at_minute(rest) {
        return at_hours(minute, &stepped(0, 23, step));
    }
    if let Some(times) = rest.strip_prefix("day at ") {
        let times = times.replace(" and ", ", ");
        let clocks = times
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(parse_time)
            .collect::<Option<Vec<_>>>()?;
        let clocks = clocks.into_iter().collect::<BTreeSet<_>>();
        let hours = clocks.iter().map(|time| time.0).collect::<BTreeSet<_>>();
        let minutes = clocks.iter().map(|time| time.1).collect::<BTreeSet<_>>();
        if clocks.len() != hours.len() * minutes.len() {
            return None;
        }
        return assemble(
            &minutes.into_iter().collect::<Vec<_>>(),
            &hours.into_iter().collect::<Vec<_>>(),
            &all(1, 31),
            &all(1, 12),
            &all(0, 6),
        );
    }
    if let Some(time) = rest.strip_prefix("weekday at ").and_then(parse_time) {
        return at_weekdays(time, &[1, 2, 3, 4, 5]);
    }
    if let Some((days, time)) = rest.split_once(" at ") {
        let time = parse_time(time)?;
        let weekdays = parse_weekdays(days)?;
        return at_weekdays(time, &weekdays);
    }
    None
}

fn hours_at_minute(text: &str) -> Option<(u8, u8)> {
    let (step, minute) = text.split_once(" hours at minute ")?;
    let step = step.parse().ok()?;
    let minute = minute.parse().ok()?;
    ((2..24).contains(&step) && 24u8.is_multiple_of(step) && minute < 60).then_some((step, minute))
}

fn parse_monthdays(lower: &str) -> Option<Cron> {
    let rest = lower
        .strip_prefix("on day ")
        .or_else(|| lower.strip_prefix("on days "))?;
    let (days, time) = rest.split_once(" of every month at ")?;
    let (hour, minute) = parse_time(time)?;
    let days = parse_numbers(days)?;
    if days.is_empty() || days.iter().any(|day| !(1..=31).contains(day)) {
        return None;
    }
    assemble(&[minute], &[hour], &days, &all(1, 12), &all(0, 6))
}

fn at_hours(minute: u8, hours: &[u8]) -> Option<Cron> {
    assemble(&[minute], hours, &all(1, 31), &all(1, 12), &all(0, 6))
}

fn at_weekdays(time: (u8, u8), weekdays: &[u8]) -> Option<Cron> {
    assemble(&[time.1], &[time.0], &all(1, 31), &all(1, 12), weekdays)
}

fn parse_time(text: &str) -> Option<(u8, u8)> {
    let (clock, suffix) = text.trim().rsplit_once(' ')?;
    let (hour, minute) = clock.split_once(':')?;
    let hour: u8 = hour.parse().ok()?;
    let minute: u8 = minute.parse().ok()?;
    if !(1..=12).contains(&hour) || minute > 59 {
        return None;
    }
    let hour = match suffix {
        "am" if hour == 12 => 0,
        "am" => hour,
        "pm" if hour == 12 => 12,
        "pm" => hour + 12,
        _ => return None,
    };
    Some((hour, minute))
}

fn parse_weekdays(text: &str) -> Option<Vec<u8>> {
    if text == "day" || text == "weekday" {
        return None;
    }
    let mut days = Vec::new();
    for part in text.replace(" and ", ", ").split(',') {
        let name = part.trim();
        if name.is_empty() {
            continue;
        }
        let index = WEEKDAYS
            .iter()
            .position(|day| day.eq_ignore_ascii_case(name))?;
        let index = u8::try_from(index).ok()?;
        if !days.contains(&index) {
            days.push(index);
        }
    }
    if days.is_empty() {
        return None;
    }
    days.sort_unstable();
    Some(days)
}

fn parse_numbers(text: &str) -> Option<Vec<u8>> {
    let mut numbers = Vec::new();
    for part in text.replace(" and ", ", ").split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let number = part.parse().ok()?;
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    numbers.sort_unstable();
    (!numbers.is_empty()).then_some(numbers)
}

fn all(min: u8, max: u8) -> Vec<u8> {
    (min..=max).collect()
}

fn stepped(min: u8, max: u8, step: u8) -> Vec<u8> {
    (min..=max).step_by(usize::from(step)).collect()
}

fn assemble(
    minutes: &[u8],
    hours: &[u8],
    days: &[u8],
    months: &[u8],
    weekdays: &[u8],
) -> Option<Cron> {
    let expression = format!(
        "{} {} {} {} {}",
        encode(minutes, 0, 59),
        encode(hours, 0, 23),
        encode(days, 1, 31),
        encode(months, 1, 12),
        encode(weekdays, 0, 6),
    );
    let cron = Cron::parse(&expression).ok()?;
    (cron.minute_values() == minutes
        && cron.hour_values() == hours
        && cron.day_values() == days
        && cron.month_values() == months
        && cron.weekday_values() == weekdays)
        .then_some(cron)
}

fn encode(values: &[u8], min: u8, max: u8) -> String {
    if values.is_empty() {
        return min.to_string();
    }
    if full(values, min, max) {
        return "*".into();
    }
    if let Some(step) = arithmetic_step(values) {
        let start = values[0];
        let end = *values.last().unwrap();
        if step == 1 {
            return format!("{start}-{end}");
        }
        let covers = u16::from(end) + u16::from(step) > u16::from(max)
            && values.len() == usize::from((max - start) / step) + 1;
        if covers && start == min {
            return format!("*/{step}");
        }
        if covers {
            return format!("{start}/{step}");
        }
        return format!("{start}-{end}/{step}");
    }
    let mut parts = Vec::new();
    let mut index = 0;
    while index < values.len() {
        let start = values[index];
        let mut end = start;
        while index + 1 < values.len() && values[index + 1] == end + 1 {
            index += 1;
            end = values[index];
        }
        if start == end {
            parts.push(start.to_string());
        } else {
            parts.push(format!("{start}-{end}"));
        }
        index += 1;
    }
    parts.join(",")
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

#[derive(Clone, Eq, PartialEq)]
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

fn project(cron: &Cron, stored: &TimeZone, viewer: &TimeZone) -> Option<Cron> {
    let deltas = offset_deltas(stored, viewer)?;
    if deltas.is_empty() {
        return None;
    }
    if deltas.len() == 1 && deltas.contains(&0) {
        return Some(cron.clone());
    }
    let pattern = Pattern::from(cron);
    let mut shifted = None;
    for delta in deltas {
        let next = shift(&pattern, delta)?;
        if shifted.as_ref().is_some_and(|seen| seen != &next) {
            return None;
        }
        shifted = Some(next);
    }
    let shifted = shifted?;
    if shifted == pattern {
        return Some(cron.clone());
    }
    assemble(
        &shifted.minutes,
        &shifted.hours,
        &shifted.days,
        &shifted.months,
        &shifted.weekdays,
    )
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

fn offset_deltas(stored: &TimeZone, viewer: &TimeZone) -> Option<BTreeSet<i32>> {
    // Inspect every transition over a full Gregorian cycle, including the
    // recurring future rules. A current offset alone would mislabel DST clocks.
    let start = Timestamp::now();
    let end = start
        .to_zoned(TimeZone::UTC)
        .checked_add(Span::new().years(400))
        .ok()?
        .timestamp();
    let mut deltas = BTreeSet::from([delta_at(stored, viewer, start)?]);
    for zone in [stored, viewer] {
        for transition in zone.following(start) {
            if transition.timestamp() > end {
                break;
            }
            let before = transition
                .timestamp()
                .checked_sub(Span::new().seconds(1))
                .ok()?;
            deltas.insert(delta_at(stored, viewer, before)?);
            deltas.insert(delta_at(stored, viewer, transition.timestamp())?);
        }
    }
    Some(deltas)
}

fn delta_at(stored: &TimeZone, viewer: &TimeZone, at: Timestamp) -> Option<i32> {
    let from = stored.to_offset(at).seconds();
    let to = viewer.to_offset(at).seconds();
    if from.rem_euclid(60) != 0 || to.rem_euclid(60) != 0 {
        return None;
    }
    Some((to - from) / 60)
}
