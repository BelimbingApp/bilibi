//! Business-time arithmetic (issue 135, item 8). `Timetable::load` reads one
//! calendar's window (at most 731 days) in three indexed queries and answers
//! in memory: it is the per-request cache, so the next request sees a saved
//! change. No Bilimbi counterpart.
//!
//! Periods are wall-clock minutes in the zone of the rule version in force,
//! turned into UTC instants by `Period::bounds`' rules: a time in a
//! daylight-saving gap starts at the first valid instant, a time in a fold
//! uses the earlier one. Business time is real elapsed time, so a period
//! across a clock change is an hour longer or shorter.
use crate::{
    model::{periods_from_json, Day, DayType, Exception, Rule, Substitute, Week, WeekDay},
    resolve::{Source, SUBSTITUTE_SEARCH},
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use jiff::{
    civil::Date,
    tz::{AmbiguousOffset, TimeZone},
    SignedDuration, Timestamp, ToSpan,
};
use tokio_postgres::GenericClient;

/// The widest window one load reads.
pub const MAX_WINDOW_DAYS: i64 = 731;
/// The furthest a search looks ahead.
pub const SEARCH_DAYS: i64 = 366;

/// Why an answer could not be given.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TimeError {
    /// The answer lies past the loaded window or 366 days ahead (or the
    /// calendar has no working time at all). Load a later window.
    #[error("The answer lies beyond the loaded window or the search limit")]
    Beyond,
    #[error("The start is after the end")]
    Backwards,
    #[error("A duration must not be negative")]
    Negative,
}

/// One calendar's business time for a window, or the clock (always open).
pub struct Timetable {
    window: Option<Window>,
}
struct Window {
    days: Vec<Day>,
    start: Timestamp,
    end: Timestamp,
    /// Opening intervals `[start, end)`, sorted and merged.
    open: Vec<(Timestamp, Timestamp)>,
}

/// A civil time of `date` plus `minutes` (past 1440 is the next day), read in
/// `tz`: a gap moves to its first valid instant, a fold takes the earlier.
pub(crate) fn instant(tz: &TimeZone, date: Date, minutes: u16) -> Result<Timestamp> {
    let date = if minutes >= 1440 {
        date.tomorrow().map_err(|_| Error::Invalid)?
    } else {
        date
    };
    let minutes = minutes % 1440;
    let civil = date.at(
        i8::try_from(minutes / 60).map_err(|_| Error::Invalid)?,
        i8::try_from(minutes % 60).map_err(|_| Error::Invalid)?,
        0,
        0,
    );
    let at = match tz.to_ambiguous_timestamp(civil).offset() {
        AmbiguousOffset::Unambiguous { offset } => offset.to_timestamp(civil),
        AmbiguousOffset::Fold { before, .. } => before.to_timestamp(civil),
        AmbiguousOffset::Gap { after, .. } => {
            let earlier = after.to_timestamp(civil).map_err(|_| Error::Invalid)?;
            return tz
                .following(earlier)
                .next()
                .map(|transition| transition.timestamp())
                .ok_or(Error::Invalid);
        }
    };
    at.map_err(|_| Error::Invalid)
}
fn zone(name: &str) -> Result<TimeZone> {
    TimeZone::get(name).map_err(|_| Error::Internal)
}
fn shift(date: Date, days: i64) -> Result<Date> {
    date.checked_add(days.days()).map_err(|_| Error::Invalid)
}

impl Timetable {
    /// Always open: for a caller that falls back to clock time when no
    /// calendar is set, so it keeps one code path.
    pub fn clock() -> Self {
        Self { window: None }
    }

    /// Calendar `code` of the scope's tenant from `from` to `to` (inclusive,
    /// at most 731 days). Retired calendars still compute. An unknown code is
    /// `NotFound`; a reversed or too wide window is `Invalid`.
    pub async fn load(
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        code: &str,
        from: Date,
        to: Date,
    ) -> Result<Self> {
        let source = source(client, scope.tenant(), code, from, to).await?;
        let days = source.days(shift(from, -1)?, to);
        let start = instant(&zone(&source.rule(from).timezone)?, from, 0)?;
        let end = instant(&zone(&source.rule(to).timezone)?, shift(to, 1)?, 0)?;
        let mut open = Vec::new();
        for day in &days {
            if day.day_type != DayType::Working {
                continue;
            }
            let tz = zone(&day.timezone)?;
            for period in &day.periods {
                let (from, until) = (
                    instant(&tz, day.date, period.start)?.max(start),
                    instant(&tz, day.date, period.end)?.min(end),
                );
                if from < until {
                    open.push((from, until));
                }
            }
        }
        open.sort();
        let mut merged: Vec<(Timestamp, Timestamp)> = Vec::with_capacity(open.len());
        for (from, until) in open {
            match merged.last_mut() {
                Some(last) if from <= last.1 => last.1 = last.1.max(until),
                _ => merged.push((from, until)),
            }
        }
        Ok(Self {
            window: Some(Window {
                days: days.into_iter().skip(1).collect(),
                start,
                end,
                open: merged,
            }),
        })
    }

    /// The resolved dates of the window, `from` first; empty for the clock.
    pub fn days(&self) -> &[Day] {
        self.window.as_ref().map_or(&[], |w| &w.days)
    }
    pub fn day(&self, date: Date) -> Option<&Day> {
        self.days().iter().find(|day| day.date == date)
    }

    fn inside(window: &Window, t: Timestamp) -> std::result::Result<(), TimeError> {
        if t < window.start || t >= window.end {
            Err(TimeError::Beyond)
        } else {
            Ok(())
        }
    }
    /// The furthest instant a search from `t` may reach.
    fn limit(window: &Window, t: Timestamp) -> Timestamp {
        t.checked_add(SignedDuration::from_hours(SEARCH_DAYS * 24))
            .map_or(window.end, |far| far.min(window.end))
    }

    pub fn is_open(&self, t: Timestamp) -> std::result::Result<bool, TimeError> {
        let Some(window) = &self.window else {
            return Ok(true);
        };
        Self::inside(window, t)?;
        let after = window.open.partition_point(|(start, _)| *start <= t);
        Ok(after > 0 && t < window.open[after - 1].1)
    }

    /// `t` if open, else the start of the next opening.
    pub fn next_open(&self, t: Timestamp) -> std::result::Result<Timestamp, TimeError> {
        let Some(window) = &self.window else {
            return Ok(t);
        };
        if self.is_open(t)? {
            return Ok(t);
        }
        let next = window.open.partition_point(|(start, _)| *start <= t);
        match window.open.get(next) {
            Some((start, _)) if *start < Self::limit(window, t) => Ok(*start),
            _ => Err(TimeError::Beyond),
        }
    }

    /// `d` of business time after `t`, counted from `t` or, if closed, from
    /// the next opening. A result exactly at a closing time stays there ("due
    /// 18:00 today", not "09:00 tomorrow"); `add(t, 0)` is `t`.
    pub fn add(
        &self,
        t: Timestamp,
        d: SignedDuration,
    ) -> std::result::Result<Timestamp, TimeError> {
        if d.is_negative() {
            return Err(TimeError::Negative);
        }
        if d.is_zero() {
            return Ok(t);
        }
        let Some(window) = &self.window else {
            return t.checked_add(d).map_err(|_| TimeError::Beyond);
        };
        let limit = Self::limit(window, t);
        let mut cursor = self.next_open(t)?;
        let mut index = window.open.partition_point(|(start, _)| *start <= cursor) - 1;
        let mut remaining = d;
        loop {
            let (_, close) = window.open[index];
            let available = cursor.duration_until(close);
            if remaining <= available {
                let due = cursor
                    .checked_add(remaining)
                    .map_err(|_| TimeError::Beyond)?;
                return if due <= limit {
                    Ok(due)
                } else {
                    Err(TimeError::Beyond)
                };
            }
            remaining -= available;
            index += 1;
            match window.open.get(index) {
                Some((start, _)) if *start < limit => cursor = *start,
                _ => return Err(TimeError::Beyond),
            }
        }
    }

    /// Business time in `[a, b)`.
    pub fn elapsed(
        &self,
        a: Timestamp,
        b: Timestamp,
    ) -> std::result::Result<SignedDuration, TimeError> {
        if a > b {
            return Err(TimeError::Backwards);
        }
        let Some(window) = &self.window else {
            return Ok(a.duration_until(b));
        };
        if a < window.start || b > window.end {
            return Err(TimeError::Beyond);
        }
        Ok(window
            .open
            .iter()
            .filter(|(start, close)| *start < b && *close > a)
            .map(|(start, close)| (*start).max(a).duration_until((*close).min(b)))
            .sum())
    }

    /// The `n`th working day after `date`; `n = 0` is `date` itself when it
    /// is a working day, else the next one. Take the time from `day`.
    pub fn add_working_days(&self, date: Date, n: u32) -> std::result::Result<Date, TimeError> {
        let Some(window) = &self.window else {
            return date
                .checked_add(i64::from(n).days())
                .map_err(|_| TimeError::Beyond);
        };
        let first = window.days.first().ok_or(TimeError::Beyond)?.date;
        let last = window.days.last().ok_or(TimeError::Beyond)?.date;
        if date < first || date > last {
            return Err(TimeError::Beyond);
        }
        let mut counted = 0;
        for day in window.days.iter().filter(|day| day.date >= date) {
            if date
                .until(day.date)
                .map_or(true, |span| span.get_days() > SEARCH_DAYS as i32)
            {
                break;
            }
            if day.day_type != DayType::Working {
                continue;
            }
            if n == 0 {
                return Ok(day.date);
            }
            if day.date > date {
                counted += 1;
                if counted == n {
                    return Ok(day.date);
                }
            }
        }
        Err(TimeError::Beyond)
    }
}

/// The three queries: the calendar with its rule versions, the holidays of
/// its included sets, and its exceptions in the window.
async fn source(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    code: &str,
    from: Date,
    to: Date,
) -> Result<Source> {
    if from > to
        || from
            .until(to)
            .map_or(true, |span| i64::from(span.get_days()) >= MAX_WINDOW_DAYS)
    {
        return Err(Error::Invalid);
    }
    let rows = client
        .query(
            "SELECT c.id,c.substitute,r.effective_from::text,r.timezone,r.week FROM core_work_calendars c JOIN core_work_calendar_rules r ON r.tenant_id=c.tenant_id AND r.calendar_id=c.id WHERE c.tenant_id=$1 AND c.code=$2 ORDER BY r.effective_from",
            &[&tenant, &code],
        )
        .await?;
    let first = rows.first().ok_or(Error::NotFound)?;
    let (calendar, substitute): (i64, &str) = (first.get(0), first.get(1));
    let substitute = Substitute::parse(substitute).ok_or(Error::Internal)?;
    let rules = rows
        .iter()
        .map(|row| {
            Ok(Rule {
                effective_from: row.get::<_, &str>(2).parse().map_err(|_| Error::Internal)?,
                timezone: row.get(3),
                week: Week::from_stored(&row.get(4))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // Substitutes chain: an earlier holiday's substitute can take the day a
    // later one would move to, however far back the chain runs. Allocation
    // starts where no earlier holiday can reach the window, so a date
    // resolves the same whatever window asks.
    let earliest = match substitute {
        Substitute::None => shift(from, -1)?,
        _ => {
            start(
                client,
                tenant,
                calendar,
                shift(from, -(SUBSTITUTE_SEARCH + 1))?,
            )
            .await?
        }
    };
    let (earliest, before) = (earliest.to_string(), shift(from, -1)?.to_string());
    let last = to.to_string();
    let holidays = client
        .query(
            "SELECT h.date::text,s.code,h.name FROM core_work_calendar_includes i JOIN core_work_calendar_holiday_sets s ON s.tenant_id=i.tenant_id AND s.id=i.holiday_set_id JOIN core_work_calendar_holidays h ON h.tenant_id=i.tenant_id AND h.holiday_set_id=i.holiday_set_id WHERE i.tenant_id=$1 AND i.calendar_id=$2 AND h.date BETWEEN $3::text::date AND $4::text::date ORDER BY h.date,s.code",
            &[&tenant, &calendar, &earliest, &last],
        )
        .await?
        .iter()
        .map(|row| {
            Ok((
                row.get::<_, &str>(0).parse().map_err(|_| Error::Internal)?,
                row.get(1),
                row.get(2),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let exceptions = client
        .query(
            "SELECT id,starts_on::text,ends_on::text,name,day_type,periods FROM core_work_calendar_exceptions WHERE tenant_id=$1 AND calendar_id=$2 AND starts_on<=$4::text::date AND ends_on>=$3::text::date ORDER BY starts_on",
            &[&tenant, &calendar, &before, &last],
        )
        .await?
        .iter()
        .map(exception)
        .collect::<Result<Vec<_>>>()?;
    Ok(Source {
        substitute,
        rules,
        holidays,
        exceptions,
    })
}
/// Where substitute allocation may start for dates after `cut + 14`: the
/// latest holiday on or before `cut` preceded by more than
/// [`SUBSTITUTE_SEARCH`] days without a holiday, since no substitute moves
/// further than that; `cut` when no holiday falls on or before it.
async fn start(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    calendar: i64,
    cut: Date,
) -> Result<Date> {
    let row = client
        .query_one(
            "SELECT max(date)::text FROM (SELECT date,lag(date) OVER (ORDER BY date) AS prev FROM (SELECT DISTINCT h.date FROM core_work_calendar_includes i JOIN core_work_calendar_holidays h ON h.tenant_id=i.tenant_id AND h.holiday_set_id=i.holiday_set_id WHERE i.tenant_id=$1 AND i.calendar_id=$2 AND h.date<=$3::text::date) d) g WHERE prev IS NULL OR date-prev>$4",
            &[&tenant, &calendar, &cut.to_string(), &(SUBSTITUTE_SEARCH as i32)],
        )
        .await?;
    match row.get::<_, Option<&str>>(0) {
        Some(text) => Ok(text.parse::<Date>().map_err(|_| Error::Internal)?.min(cut)),
        None => Ok(cut),
    }
}
/// The last date a change to holidays on or before `last` can move, for
/// each calendar including `set` (or the one `calendar`): a substitute moves
/// at most [`SUBSTITUTE_SEARCH`] days, but each can displace the next
/// holiday's, so the reach runs to that past the end of the chain of
/// holidays (each within that many days of the one before) from `last`. A
/// calendar that substitutes nothing moves only the holidays' own dates.
pub(crate) async fn reach(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    set: Option<i64>,
    calendar: Option<i64>,
    last: Date,
) -> Result<Vec<(String, Date)>> {
    client
        .query(
            "WITH cal AS (SELECT id,code,substitute FROM core_work_calendars WHERE tenant_id=$1 AND (id=$3 OR id IN (SELECT calendar_id FROM core_work_calendar_includes WHERE tenant_id=$1 AND holiday_set_id=$2))), d AS (SELECT DISTINCT i.calendar_id,h.date FROM cal JOIN core_work_calendar_includes i ON i.tenant_id=$1 AND i.calendar_id=cal.id JOIN core_work_calendar_holidays h ON h.tenant_id=i.tenant_id AND h.holiday_set_id=i.holiday_set_id AND h.date>=$4::text::date), g AS (SELECT calendar_id,date,lead(date) OVER (PARTITION BY calendar_id ORDER BY date) AS next FROM d) SELECT cal.code,cal.substitute,(SELECT min(g.date)::text FROM g WHERE g.calendar_id=cal.id AND (g.next IS NULL OR g.next-g.date>$5)) FROM cal ORDER BY cal.code",
            &[&tenant, &set, &calendar, &last.to_string(), &(SUBSTITUTE_SEARCH as i32)],
        )
        .await?
        .iter()
        .map(|row| {
            let code: String = row.get(0);
            let substitute = Substitute::parse(row.get(1)).ok_or(Error::Internal)?;
            let end: Option<&str> = row.get(2);
            let end = match end {
                Some(text) => text.parse::<Date>().map_err(|_| Error::Internal)?.max(last),
                None => last,
            };
            let to = match substitute {
                Substitute::None => last,
                _ => end.checked_add(SUBSTITUTE_SEARCH.days()).unwrap_or(end),
            };
            Ok((code, to))
        })
        .collect()
}
pub(crate) fn exception(row: &tokio_postgres::Row) -> Result<Exception> {
    Ok(Exception {
        id: row.get("id"),
        starts_on: row
            .get::<_, &str>("starts_on")
            .parse()
            .map_err(|_| Error::Internal)?,
        ends_on: row
            .get::<_, &str>("ends_on")
            .parse()
            .map_err(|_| Error::Internal)?,
        name: row.get("name"),
        day: WeekDay {
            day_type: DayType::parse(row.get("day_type")).ok_or(Error::Internal)?,
            periods: periods_from_json(&row.get("periods"))?,
        },
    })
}
