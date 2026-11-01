//! Maps cron wall-clock minutes to instants in one IANA zone. This is the only
//! place a DST gap or fold is decided, and every function takes the clock as an
//! argument so a caller can replay any moment (the walkthrough does).
//!
//! * A **fold** (clocks repeat) has two instants for one wall time. Both are
//!   real occurrences: `previous` returns the latest one that has already
//!   happened and `next` the earliest one still ahead, so a poll before the
//!   repeat fires the first and a poll after it fires the second. (`previous`
//!   keeps the exact rule; `next` is display only and also reports the
//!   repeat while the first pass is running.)
//! * A **gap** (clocks skip) has no instant. The occurrence is dropped, never
//!   shifted: `next` resumes from the instant clocks resume and `previous`
//!   searches before the gap.
use crate::cron::Cron;
use jiff::{
    civil::{Date, DateTime},
    tz::{AmbiguousOffset, TimeZone},
    Span, Timestamp,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// A gap or calendar edge kept the search from settling in a few steps.
    Unresolvable,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the schedule time could not be resolved in its zone")
    }
}

/// What one wall-clock minute means in a zone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Local {
    Instant(Timestamp),
    /// The minute happens twice; `earlier` is on the pre-transition offset.
    Fold {
        earlier: Timestamp,
        later: Timestamp,
    },
    /// The minute never happens. `resumes` is the first wall time that does.
    Gap {
        resumes: DateTime,
    },
}

/// Gap and fold both resolve on the wall clock of `zone`.
pub fn resolve(zone: &TimeZone, wall: DateTime) -> Result<Local, Error> {
    let ambiguous = zone.to_ambiguous_timestamp(wall);
    let at = |offset: jiff::tz::Offset| offset.to_timestamp(wall).map_err(|_| Error::Unresolvable);
    Ok(match ambiguous.offset() {
        AmbiguousOffset::Unambiguous { offset } => Local::Instant(at(offset)?),
        AmbiguousOffset::Fold { before, after } => Local::Fold {
            earlier: at(before)?,
            later: at(after)?,
        },
        AmbiguousOffset::Gap { after, .. } => {
            // On the post-gap offset the skipped minute lies just before the
            // transition, so the transition that follows it ends the gap.
            let transition = zone
                .following(at(after)?)
                .next()
                .ok_or(Error::Unresolvable)?
                .timestamp();
            Local::Gap {
                resumes: transition.to_zoned(zone.clone()).datetime(),
            }
        }
    })
}

const ATTEMPTS: usize = 8;
const MINUTE: fn() -> Span = || Span::new().minutes(1);

fn last_day() -> DateTime {
    Date::constant(9999, 12, 31).at(23, 59, 0, 0)
}
fn first_day() -> DateTime {
    Date::constant(1, 1, 1).at(0, 0, 0, 0)
}

/// Earliest occurrence at or after `now`; of a fold's two passes only one
/// strictly ahead counts.
///
/// Wall-clock search alone would skip the repeat of a fold that is still ahead
/// when `now` is between its two passes, so that repeat is considered as well.
pub fn next(cron: &Cron, zone: &TimeZone, now: Timestamp) -> Result<Timestamp, Error> {
    let found = next_by_wall_clock(cron, zone, now)?;
    Ok(match upcoming_repeat(cron, zone, now) {
        Some(repeat) => found.min(repeat),
        None => found,
    })
}

fn next_by_wall_clock(cron: &Cron, zone: &TimeZone, now: Timestamp) -> Result<Timestamp, Error> {
    let mut from = now.to_zoned(zone.clone()).datetime();
    for _ in 0..ATTEMPTS {
        let wall = cron
            .next_at_or_after(from, last_day())
            .ok_or(Error::Unresolvable)?;
        match resolve(zone, wall)? {
            Local::Instant(at) => return Ok(at),
            Local::Fold { earlier, later } => {
                // Strictly ahead: the repeat that already started is not "next".
                if let Some(at) = [earlier, later].into_iter().filter(|at| *at > now).min() {
                    return Ok(at);
                }
                from = wall
                    .checked_add(MINUTE())
                    .map_err(|_| Error::Unresolvable)?;
            }
            Local::Gap { resumes } => from = resumes,
        }
    }
    Err(Error::Unresolvable)
}

/// First occurrence in the second pass of the next fold, when clocks go back.
fn upcoming_repeat(cron: &Cron, zone: &TimeZone, now: Timestamp) -> Option<Timestamp> {
    let transition = zone.following(now).next()?;
    let start = transition.timestamp();
    let after = transition.offset();
    let before = zone.to_offset(start.checked_sub(Span::new().seconds(1)).ok()?);
    let repeated = i64::from(before.seconds() - after.seconds());
    if repeated <= 0 {
        return None;
    }
    let first = start.to_zoned(zone.clone()).datetime();
    let last = first.checked_add(Span::new().seconds(repeated - 60)).ok()?;
    let wall = cron.next_at_or_after(first, last)?;
    after.to_timestamp(wall).ok()
}

/// Latest occurrence at or before `now`.
pub fn previous(cron: &Cron, zone: &TimeZone, now: Timestamp) -> Result<Timestamp, Error> {
    let mut from = now.to_zoned(zone.clone()).datetime();
    for _ in 0..ATTEMPTS {
        let wall = cron
            .prev_at_or_before(from, first_day())
            .ok_or(Error::Unresolvable)?;
        let step_back = || wall.checked_sub(MINUTE()).map_err(|_| Error::Unresolvable);
        match resolve(zone, wall)? {
            Local::Instant(at) => return Ok(at),
            Local::Fold { earlier, later } => {
                // Only repeats that have happened; the later one wins once it has.
                if let Some(at) = [earlier, later].into_iter().filter(|at| *at <= now).max() {
                    return Ok(at);
                }
                from = step_back()?;
            }
            Local::Gap { .. } => from = step_back()?,
        }
    }
    Err(Error::Unresolvable)
}
