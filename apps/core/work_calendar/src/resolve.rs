//! How a date resolves (issue 135, item 2): the calendar's exception wins,
//! then a holiday or a substitute from an included set, then the week of the
//! rule version in force. Belimbing's
//! `AttendanceCalendarResolver` resolves the same four day types.
use crate::model::{Day, DayType, Exception, Reason, Rule, Substitute, WeekDay};
use jiff::{civil::Date, tz::TimeZone, Timestamp, ToSpan};
use std::collections::{BTreeMap, BTreeSet};

/// The furthest a holiday on a rest day moves to find a working day.
pub(crate) const SUBSTITUTE_SEARCH: i64 = 14;

/// The version of `rules` (sorted by `effective_from`) in force on `date`:
/// the last that starts by then. The first version also governs any earlier
/// date, so a calendar always answers; `None` only without versions.
pub(crate) fn in_force(rules: &[Rule], date: Date) -> Option<&Rule> {
    rules
        .iter()
        .rev()
        .find(|rule| rule.effective_from <= date)
        .or(rules.first())
}

/// The version of `rules` in force at `now`: the last whose first date has
/// come in its own zone, so "today" is the calendar's date where it keeps
/// its hours, never the database session's. The first version also governs
/// before then. The calendars listing reads it the same way in SQL
/// (`store::zone_in_force`).
pub(crate) fn in_force_now(rules: &[Rule], now: Timestamp) -> Option<&Rule> {
    rules
        .iter()
        .rev()
        .find(|rule| rule.effective_from <= today(&rule.timezone, now))
        .or(rules.first())
}

/// The date at `now` in `zone`; UTC for a zone this build does not know.
pub(crate) fn today(zone: &str, now: Timestamp) -> Date {
    let zone = TimeZone::get(zone).unwrap_or(TimeZone::UTC);
    now.to_zoned(zone).date()
}

/// Everything one calendar's dates depend on, loaded for a window.
pub(crate) struct Source {
    pub substitute: Substitute,
    /// Sorted by `effective_from`; never empty.
    pub rules: Vec<Rule>,
    /// `(date, set code, name)`, sorted by date then set.
    pub holidays: Vec<(Date, String, String)>,
    pub exceptions: Vec<Exception>,
}
impl Source {
    /// The version in force on `date` (see [`in_force`]).
    pub fn rule(&self, date: Date) -> &Rule {
        in_force(&self.rules, date).unwrap_or(&self.rules[0])
    }
    fn weekly(&self, date: Date) -> &WeekDay {
        self.rule(date).week.day(date)
    }

    /// Where each holiday on a rest (or off) day moved: the next working day
    /// by the calendar's own week that is not a holiday or another holiday's
    /// substitute, within 14 days. Holidays are taken in date order.
    fn substitutes(&self) -> BTreeMap<Date, (String, String, Date)> {
        let mut moved = BTreeMap::new();
        if self.substitute == Substitute::None {
            return moved;
        }
        let dates: BTreeSet<Date> = self.holidays.iter().map(|(date, ..)| *date).collect();
        let mut seen = BTreeSet::new();
        for (holiday, set, name) in &self.holidays {
            if !seen.insert(*holiday) || !self.substitute.moves(self.weekly(*holiday).day_type) {
                continue;
            }
            for offset in 1..=SUBSTITUTE_SEARCH {
                let Ok(candidate) = holiday.checked_add(offset.days()) else {
                    break;
                };
                if self.weekly(candidate).day_type == DayType::Working
                    && !dates.contains(&candidate)
                    && !moved.contains_key(&candidate)
                {
                    moved.insert(candidate, (set.clone(), name.clone(), *holiday));
                    break;
                }
            }
        }
        moved
    }

    /// Each date from `from` to `to`, inclusive.
    pub fn days(&self, from: Date, to: Date) -> Vec<Day> {
        let moved = self.substitutes();
        let mut days = Vec::new();
        let mut date = from;
        while date <= to {
            let rule = self.rule(date);
            let weekly = rule.week.day(date);
            let exception = self
                .exceptions
                .iter()
                .find(|e| e.starts_on <= date && date <= e.ends_on);
            let holiday = self.holidays.iter().find(|(day, ..)| *day == date);
            let (day_type, periods, reason) = match (exception, holiday, moved.get(&date)) {
                (Some(e), _, _) => (
                    e.day.day_type,
                    e.day.periods.clone(),
                    Reason::Exception {
                        name: e.name.clone(),
                    },
                ),
                (None, Some((_, set, name)), _) => (
                    DayType::Holiday,
                    Vec::new(),
                    Reason::Holiday {
                        set: set.clone(),
                        name: name.clone(),
                    },
                ),
                (None, None, Some((set, name, holiday))) => (
                    DayType::Holiday,
                    Vec::new(),
                    Reason::Substitute {
                        set: set.clone(),
                        name: name.clone(),
                        holiday: *holiday,
                    },
                ),
                (None, None, None) => (weekly.day_type, weekly.periods.clone(), Reason::Week),
            };
            days.push(Day {
                date,
                day_type,
                weekly: weekly.day_type,
                periods,
                reason,
                timezone: rule.timezone.clone(),
            });
            match date.tomorrow() {
                Ok(next) => date = next,
                Err(_) => break,
            }
        }
        days
    }
}
