//! `core.work_calendar.*`: the contracts every door calls (ADR 01). Reads
//! list and show calendars, resolve days and answer business-time questions;
//! reviewed writes build calendars, their weeks, exceptions and holiday sets
//! (entering a year of holidays is one call). No Bilimbi counterpart.
use crate::{
    store::Action, Calendar, Listing, TimeError, Timetable, WorkCalendars, WriteError, LIST,
    MANAGE, MAX_PAGE_SIZE, MAX_ROWS, MAX_WINDOW_DAYS, SEARCH_DAYS, VIEW,
};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, RegistryError};
use bilibi_tenancy::Scope;
use jiff::{civil::Date, SignedDuration, Timestamp, ToSpan};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

static CALENDARS: OnceLock<Arc<WorkCalendars>> = OnceLock::new();
pub(crate) fn bind(calendars: Arc<WorkCalendars>) -> bilibi_module_registry::Result<()> {
    CALENDARS
        .set(calendars)
        .map_err(|_| RegistryError::Service("duplicate work calendar agent binding".into()))
}
fn calendars() -> bilibi_agent_api::Result<&'static Arc<WorkCalendars>> {
    CALENDARS.get().ok_or(Error::Internal.into())
}
fn field(name: &str, message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([(name.into(), vec![message.into()])]))
}
fn refusal(error: WriteError) -> AgentError {
    match error {
        WriteError::Access(error) => error.into(),
        WriteError::NotFound => field("code", "is not a calendar or holiday set of this tenant"),
        WriteError::Stale => Error::Conflict.into(),
        WriteError::Invalid(errors) => {
            let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for e in errors {
                fields.entry(e.field).or_default().push(e.message);
            }
            AgentError::InvalidInput(fields)
        }
    }
}
fn read_refusal(error: Error, name: &str) -> AgentError {
    match error {
        Error::NotFound => field(name, "is not a calendar or holiday set of this tenant"),
        Error::Invalid => field(
            name,
            "is not a valid window: from on or before to, at most 731 days",
        ),
        error => error.into(),
    }
}
fn date(input: &Value, name: &str) -> bilibi_agent_api::Result<Date> {
    input[name]
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| field(name, "must be a date (YYYY-MM-DD)"))
}
fn instant(input: &Value, name: &str) -> bilibi_agent_api::Result<Timestamp> {
    input[name]
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| {
            field(
                name,
                "must be an instant with an offset, such as 2026-11-02T09:00:00+08:00",
            )
        })
}
fn shift(date: Date, days: i64) -> bilibi_agent_api::Result<Date> {
    date.checked_add(days.days())
        .map_err(|_| field("date", "is out of range"))
}
/// An optional whole number from `min` to `max`, `default` when omitted.
fn number(
    input: &Value,
    name: &str,
    min: i64,
    max: i64,
    default: i64,
) -> bilibi_agent_api::Result<i64> {
    match input.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_i64()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| field(name, &format!("must be a whole number from {min} to {max}"))),
    }
}
fn page(input: &Value) -> bilibi_agent_api::Result<i64> {
    number(input, "page", 1, 100_000, 1)
}
fn listing(input: &Value) -> bilibi_agent_api::Result<Listing> {
    let search = match input.get("search") {
        None | Some(Value::Null) => String::new(),
        Some(value) => value
            .as_str()
            .filter(|text| text.chars().count() <= 200)
            .ok_or_else(|| field("search", "must be text of at most 200 characters"))?
            .to_owned(),
    };
    Ok(Listing {
        search,
        page: page(input)?,
        page_size: number(input, "page_size", 1, MAX_PAGE_SIZE, MAX_PAGE_SIZE)?,
    })
}
/// The UTC date of an instant: a window around it covers its civil date in
/// any zone.
fn utc_date(at: Timestamp) -> Date {
    at.to_zoned(jiff::tz::TimeZone::UTC).date()
}

#[derive(Clone, Copy)]
enum Read {
    List,
    HolidaySets,
    Get,
    Days,
    BusinessTime,
    HolidaySet,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "core/work_calendar"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let calendars = calendars()?;
            let code = input["code"].as_str().unwrap_or_default();
            let data = match self {
                Self::List => {
                    let listing = listing(input)?;
                    let page = calendars.calendars(scope, &listing).await?;
                    json!({
                        "calendars": page.rows.iter().map(|c| json!({"code": c.code, "name": c.name, "status": c.status})).collect::<Vec<_>>(),
                        "total": page.total, "page": listing.page, "page_size": listing.page_size,
                    })
                }
                Self::HolidaySets => {
                    let listing = listing(input)?;
                    let page = calendars.holiday_sets(scope, &listing).await?;
                    json!({
                        "holiday_sets": page.rows.iter().map(|s| json!({"code": s.code, "name": s.name, "country_iso": s.country_iso, "admin1_code": s.admin1_code})).collect::<Vec<_>>(),
                        "total": page.total, "page": listing.page, "page_size": listing.page_size,
                    })
                }
                Self::Get => calendars
                    .calendar(scope, code, page(input)?)
                    .await
                    .map(|calendar: Calendar| calendar.to_json())
                    .map_err(|error| read_refusal(error, "code"))?,
                Self::HolidaySet => {
                    let optional = |name: &str| -> bilibi_agent_api::Result<Option<Date>> {
                        match input.get(name) {
                            None | Some(Value::Null) => Ok(None),
                            Some(_) => date(input, name).map(Some),
                        }
                    };
                    calendars
                        .holiday_set(
                            scope,
                            code,
                            optional("from")?,
                            optional("to")?,
                            page(input)?,
                        )
                        .await
                        .map_err(|error| read_refusal(error, "code"))?
                        .to_json()
                }
                Self::Days => {
                    let (from, to) = (date(input, "from")?, date(input, "to")?);
                    let days = calendars
                        .days(scope, code, from, to)
                        .await
                        .map_err(|error| read_refusal(error, "code"))?;
                    json!({"code": code, "days": days.iter().map(|day| day.to_json()).collect::<Vec<_>>()})
                }
                Self::BusinessTime => business_time(calendars, scope, code, input).await?,
            };
            Ok(Output::Data(Data::Json(data)))
        })
    }
}

/// One arithmetic question on a window loaded around it. `Beyond` is an
/// answer (`"beyond": true`), not a failure.
async fn business_time(
    calendars: &WorkCalendars,
    scope: &Scope,
    code: &str,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let operation = input["operation"].as_str().unwrap_or_default();
    let (from, to) = match operation {
        "is_open" | "next_open" | "add" => {
            let at = utc_date(instant(input, "at")?);
            (shift(at, -1)?, shift(at, SEARCH_DAYS + 1)?)
        }
        "elapsed" => (
            shift(utc_date(instant(input, "at")?), -1)?,
            shift(utc_date(instant(input, "until")?), 1)?,
        ),
        "add_working_days" => {
            let day = date(input, "date")?;
            (day, shift(day, SEARCH_DAYS)?)
        }
        _ => {
            return Err(field(
                "operation",
                "must be is_open, next_open, add, elapsed or add_working_days",
            ))
        }
    };
    if from
        .until(to)
        .map_or(true, |span| i64::from(span.get_days()) >= MAX_WINDOW_DAYS)
    {
        return Err(field("until", "must be within 729 days of at"));
    }
    let timetable: Timetable = calendars
        .timetable(scope, code, from, to)
        .await
        .map_err(|error| read_refusal(error, "code"))?;
    let answer: Result<Value, TimeError> = match operation {
        "is_open" => timetable
            .is_open(instant(input, "at")?)
            .map(|open| json!(open)),
        "next_open" => timetable
            .next_open(instant(input, "at")?)
            .map(|at| json!(at.to_string())),
        "add" => {
            let duration: SignedDuration = input["duration"]
                .as_str()
                .and_then(|text| text.parse().ok())
                .ok_or_else(|| {
                    field("duration", "must be an ISO 8601 duration, such as PT8H30M")
                })?;
            timetable
                .add(instant(input, "at")?, duration)
                .map(|at| json!(at.to_string()))
        }
        "elapsed" => timetable
            .elapsed(instant(input, "at")?, instant(input, "until")?)
            .map(|d| json!({"duration": format!("{d:#}"), "seconds": d.as_secs()})),
        _ => {
            let days = input["days"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| field("days", "must be a whole number from 0"))?;
            timetable
                .add_working_days(date(input, "date")?, days)
                .map(|day| json!(day.to_string()))
        }
    };
    match answer {
        Ok(result) => {
            Ok(json!({"code": code, "operation": operation, "result": result, "beyond": false}))
        }
        Err(TimeError::Beyond) => {
            Ok(json!({"code": code, "operation": operation, "result": null, "beyond": true}))
        }
        Err(TimeError::Backwards) => Err(field("until", "must not be before at")),
        Err(TimeError::Negative) => Err(field("duration", "must not be negative")),
    }
}

struct Write(Action);
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "core/work_calendar"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let calendars = calendars()?;
            let client = calendars.db.get().await.map_err(Error::from)?;
            let scope = calendars.authentication.reprove(&**client, scope).await?;
            let prepared = calendars
                .prepare(&**client, &scope, self.0, input)
                .await
                .map_err(refusal)?;
            Ok(calendars
                .review(&**client, scope.tenant(), &prepared, None)
                .await
                .map_err(refusal)?
                .into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let calendars = calendars()?;
            let scope = calendars.authentication.reprove(tx.sql(), scope).await?;
            let prepared = calendars
                .prepare(tx.sql(), &scope, self.0, input)
                .await
                .map_err(refusal)?;
            Ok(calendars
                .apply(tx, &scope, prepared)
                .await
                .map_err(refusal)?
                .into())
        })
    }
}

fn day_schema() -> Value {
    json!({"type":"object","required":["type"],"properties":{
        "type":{"type":"string","enum":["working","rest","off","holiday"]},
        "periods":{"type":"array","items":{"type":"string","maxLength":11},"description":"Up to four opening periods HH:MM-HH:MM in the calendar's zone, working days only; an end at or before the start is the next day"}
    }})
}
fn week_schema() -> Value {
    let mut days = serde_json::Map::new();
    for name in crate::model::WEEKDAYS {
        days.insert(name.into(), day_schema());
    }
    json!({"type":"object","required":crate::model::WEEKDAYS,"properties":days})
}
fn code_schema(description: &str) -> Value {
    json!({"type":"string","maxLength":64,"description":description})
}
fn read_schema(operation: Read) -> Value {
    let page = json!({"type":"integer","minimum":1,"maximum":100_000,"description":format!("Default 1; {MAX_ROWS} a page")});
    match operation {
        Read::List | Read::HolidaySets => json!({"type":"object","properties":{
            "search":{"type":"string","maxLength":200,"description":"Part of a code or name; default all"},
            "page":{"type":"integer","minimum":1,"maximum":100_000,"description":"Default 1"},
            "page_size":{"type":"integer","minimum":1,"maximum":MAX_PAGE_SIZE,"description":format!("Default {MAX_PAGE_SIZE}")}
        }}),
        Read::Get => {
            json!({"type":"object","required":["code"],"properties":{"code":code_schema("A calendar code from core.work_calendar.list"),"page":page}})
        }
        Read::HolidaySet => json!({"type":"object","required":["code"],"properties":{
            "code":code_schema("A holiday set code from core.work_calendar.holiday_set.list"),
            "from":{"type":"string","format":"date"},"to":{"type":"string","format":"date"},"page":page
        }}),
        Read::Days => json!({"type":"object","required":["code","from","to"],"properties":{
            "code":code_schema("A calendar code"),
            "from":{"type":"string","format":"date"},"to":{"type":"string","format":"date","description":"At most 730 days after from"}
        }}),
        Read::BusinessTime => json!({"type":"object","required":["code","operation"],"properties":{
            "code":code_schema("A calendar code"),
            "operation":{"type":"string","enum":["is_open","next_open","add","elapsed","add_working_days"]},
            "at":{"type":"string","format":"date-time","description":"is_open, next_open, add, elapsed: the instant, with an offset"},
            "until":{"type":"string","format":"date-time","description":"elapsed: the end, not before at"},
            "duration":{"type":"string","maxLength":40,"description":"add: business time to add, ISO 8601 (PT8H30M)"},
            "date":{"type":"string","format":"date","description":"add_working_days: the start date"},
            "days":{"type":"integer","minimum":0,"maximum":366,"description":"add_working_days: 0 is the date itself or the next working day"}
        }}),
    }
}
fn write_schema(action: Action) -> Value {
    let version = json!({"type":"integer","minimum":1,"description":"The version you read; a stale version is refused"});
    let calendar = code_schema("A calendar code");
    match action {
        Action::Create => json!({"type":"object","required":["code","name","week"],"properties":{
            "code":code_schema("Stable key other modules store: lowercase letters, digits, _ and -"),
            "name":{"type":"string","maxLength":120},
            "timezone":{"type":"string","maxLength":64,"description":"IANA zone; default the tenant's localization.timezone"},
            "substitute":{"type":"string","enum":["none","rest","rest_and_off"],"description":"Where a holiday on a rest (or off) day moves; default none"},
            "effective_from":{"type":"string","format":"date","description":"First date of the week; default today. It also governs earlier dates"},
            "week":week_schema()
        }}),
        Action::Update => json!({"type":"object","required":["code","version"],"properties":{
            "code":calendar,"version":version,"name":{"type":"string","maxLength":120},
            "substitute":{"type":"string","enum":["none","rest","rest_and_off"]},
            "status":{"type":"string","enum":["active","retired"],"description":"A retired calendar leaves pickers but still computes"}
        }}),
        Action::Rule => {
            json!({"type":"object","required":["code","version","effective_from","week"],"properties":{
                "code":calendar,"version":version,
                "effective_from":{"type":"string","format":"date","description":"The week applies from this date until the next version; the same date replaces that version"},
                "timezone":{"type":"string","maxLength":64,"description":"Default: the zone in force on that date"},
                "week":week_schema()
            }})
        }
        Action::AddException => {
            json!({"type":"object","required":["code","version","starts_on","name","type"],"properties":{
                "code":calendar,"version":version,
                "starts_on":{"type":"string","format":"date"},"ends_on":{"type":"string","format":"date","description":"Default starts_on; at most 366 days"},
                "name":{"type":"string","maxLength":120,"description":"Why: Year-end shutdown, Special working Saturday, Half day"},
                "type":{"type":"string","enum":["working","rest","off","holiday"]},
                "periods":{"type":"array","items":{"type":"string","maxLength":11},"description":"Working only: opening periods; a short one is a half day"}
            }})
        }
        Action::RemoveException => {
            json!({"type":"object","required":["code","version","exception_id"],"properties":{
                "code":calendar,"version":version,"exception_id":{"type":"integer","minimum":1,"description":"From core.work_calendar.get"}
            }})
        }
        Action::CreateSet => json!({"type":"object","required":["code","name"],"properties":{
            "code":code_schema("Stable key: lowercase letters, digits, _ and -"),
            "name":{"type":"string","maxLength":120},
            "country_iso":{"type":"string","maxLength":2,"description":"Optional ISO country, checked against imported countries"},
            "admin1_code":{"type":"string","maxLength":20,"description":"Optional division (state), such as MY.14; needs country_iso"}
        }}),
        Action::UpdateSet => json!({"type":"object","required":["code","version"],"properties":{
            "code":code_schema("A holiday set code"),"version":version,"name":{"type":"string","maxLength":120},
            "country_iso":{"type":"string","maxLength":2,"description":"Give it to change the place; an empty string clears it"},
            "admin1_code":{"type":"string","maxLength":20,"description":"With country_iso; empty or omitted for none"}
        }}),
        Action::Holidays => json!({"type":"object","required":["code","version"],"properties":{
            "code":code_schema("A holiday set code"),"version":version,
            "add":{"type":"array","items":{"type":"object","required":["date","name"],"properties":{"date":{"type":"string","format":"date"},"name":{"type":"string","maxLength":120}}},"description":"Up to 400 whole-day holidays; an existing date is renamed"},
            "remove":{"type":"array","items":{"type":"string","format":"date"}}
        }}),
        Action::Include => {
            json!({"type":"object","required":["code","version","holiday_set"],"properties":{
                "code":calendar,"version":version,"holiday_set":code_schema("A holiday set code"),
                "included":{"type":"boolean","description":"Default true; false leaves the set out"}
            }})
        }
    }
}
fn read_output() -> Value {
    json!({"type":"object","description":"Calendars and holiday sets as stored. Lists give one page and its total; a calendar's rules, holiday_sets and exceptions and a set's calendars and holidays are each {rows, total}, exceptions and holidays a page of 400 at a time. Days carry date, type (working, rest, off, holiday), weekly (the week's type underneath), periods, reason (week, holiday, substitute, exception) and timezone; business_time gives result and beyond (true when the answer lies past the 366-day search or the calendar has no working time)."})
}
fn write_output() -> Value {
    json!({"type":"object","description":"The stored facts after the write. Each write that moves dates records core/work_calendar.changed per affected calendar."})
}

macro_rules! read {
    ($key:literal, $title:literal, $summary:literal, $cap:expr, $op:expr, $schema:ident) => {
        Operation {
            key: $key,
            title: $title,
            summary: $summary,
            keywords: &["calendar", "working hours", "holiday", "business time"],
            capability: $cap,
            input: $schema,
            output: read_output,
            handler: Handler::Read(&$op),
            channels: Channels::Every,
            step_up: &[],
        }
    };
}
macro_rules! write {
    ($key:literal, $title:literal, $summary:literal, $op:expr, $schema:ident) => {
        Operation {
            key: $key,
            title: $title,
            summary: $summary,
            keywords: &["calendar", "working hours", "holiday", "exception"],
            capability: MANAGE,
            input: $schema,
            output: write_output,
            handler: Handler::SafeWrite(&$op),
            channels: Channels::Every,
            step_up: &[],
        }
    };
}
fn list_schema() -> Value {
    read_schema(Read::List)
}
fn sets_schema() -> Value {
    read_schema(Read::HolidaySets)
}
fn get_schema() -> Value {
    read_schema(Read::Get)
}
fn days_schema() -> Value {
    read_schema(Read::Days)
}
fn business_schema() -> Value {
    read_schema(Read::BusinessTime)
}
fn set_schema() -> Value {
    read_schema(Read::HolidaySet)
}
fn create_schema() -> Value {
    write_schema(Action::Create)
}
fn update_schema() -> Value {
    write_schema(Action::Update)
}
fn rule_schema() -> Value {
    write_schema(Action::Rule)
}
fn add_exception_schema() -> Value {
    write_schema(Action::AddException)
}
fn remove_exception_schema() -> Value {
    write_schema(Action::RemoveException)
}
fn create_set_schema() -> Value {
    write_schema(Action::CreateSet)
}
fn update_set_schema() -> Value {
    write_schema(Action::UpdateSet)
}
fn holidays_schema() -> Value {
    write_schema(Action::Holidays)
}
fn include_schema() -> Value {
    write_schema(Action::Include)
}

static LIST_OP: Read = Read::List;
static SETS_OP: Read = Read::HolidaySets;
static GET_OP: Read = Read::Get;
static DAYS_OP: Read = Read::Days;
static BUSINESS_OP: Read = Read::BusinessTime;
static SET_OP: Read = Read::HolidaySet;
static CREATE: Write = Write(Action::Create);
static UPDATE: Write = Write(Action::Update);
static RULE: Write = Write(Action::Rule);
static ADD_EXCEPTION: Write = Write(Action::AddException);
static REMOVE_EXCEPTION: Write = Write(Action::RemoveException);
static CREATE_SET: Write = Write(Action::CreateSet);
static UPDATE_SET: Write = Write(Action::UpdateSet);
static HOLIDAYS: Write = Write(Action::Holidays);
static INCLUDE: Write = Write(Action::Include);

inventory::submit! { Contribution::new(DEFINITIONS, "core/work_calendar", crate::provenance!(), &Definitions {
    operations: &[
        read!("core.work_calendar.list", "List work calendars", "This tenant's work calendars (code, name, status), a page at a time, with the total.", LIST, LIST_OP, list_schema),
        read!("core.work_calendar.holiday_set.list", "List holiday sets", "This tenant's holiday sets (code, name, country, division), a page at a time, with the total.", LIST, SETS_OP, sets_schema),
        read!("core.work_calendar.get", "Get a work calendar", "One calendar: its weekly rule versions, included holiday sets and dated exceptions, with the version to write against.", VIEW, GET_OP, get_schema),
        read!("core.work_calendar.days", "Resolve calendar days", "Each date in a range (at most 731 days) as working, rest, off or holiday, with its opening periods and why.", VIEW, DAYS_OP, days_schema),
        read!("core.work_calendar.business_time", "Business-time arithmetic", "Ask a calendar: is it open at an instant, when does it next open, what instant is a duration of business time later, how much business time lies between two instants, which is the nth working day after a date.", VIEW, BUSINESS_OP, business_schema),
        read!("core.work_calendar.holiday_set.get", "Get a holiday set", "One holiday set with its holidays (optionally from and to a date) and the calendars that include it.", VIEW, SET_OP, set_schema),
        write!("core.work_calendar.create", "Create a work calendar", "A new calendar with its zone, its first weekly rule and its holiday substitution rule.", CREATE, create_schema),
        write!("core.work_calendar.update", "Update a work calendar", "Rename a calendar, change where holidays on rest days move, or retire or reactivate it.", UPDATE, update_schema),
        write!("core.work_calendar.rule.set", "Set a calendar's week from a date", "A new weekly rule (zone, day types and opening periods) from a date; earlier dates keep their rules.", RULE, rule_schema),
        write!("core.work_calendar.exception.add", "Add a calendar exception", "A date or range that closes, opens with its own hours, or shortens a day (a half day).", ADD_EXCEPTION, add_exception_schema),
        write!("core.work_calendar.exception.remove", "Remove a calendar exception", "Remove one dated exception from a calendar.", REMOVE_EXCEPTION, remove_exception_schema),
        write!("core.work_calendar.holiday_set.create", "Create a holiday set", "A tenant holiday set, optionally for a country and division (state).", CREATE_SET, create_set_schema),
        write!("core.work_calendar.holiday_set.update", "Update a holiday set", "Rename a holiday set or change its country and division.", UPDATE_SET, update_set_schema),
        write!("core.work_calendar.holiday_set.holidays", "Add or remove holidays", "Add (or rename) and remove whole-day holidays of a set, up to 400 a call: a year at a time.", HOLIDAYS, holidays_schema),
        write!("core.work_calendar.include", "Include a holiday set in a calendar", "Include a holiday set in a calendar, or leave it out.", INCLUDE, include_schema),
    ],
    guides: &[Guide { key: "core.work_calendar", title: "Work calendars", summary: "Build a calendar, enter holidays and ask business-time questions.", keywords: &["calendar", "working hours", "holiday", "business time", "sla"], capability: VIEW,
        body: "A work calendar says when a tenant does business. Every date resolves to working (with opening periods), rest (the weekly rest day), off (another non-working day) or holiday: a dated exception wins, then a holiday (or a substitute for one) from an included holiday set, then the weekly rule in force. Build one with core.work_calendar.create (code, name, week; the zone defaults to the tenant's). Change the week from a date with core.work_calendar.rule.set; earlier dates keep their rules. Create a holiday set per country or state with core.work_calendar.holiday_set.create, enter a year of holidays in one core.work_calendar.holiday_set.holidays call, and include it with core.work_calendar.include. A shutdown, a special working day or a half day is core.work_calendar.exception.add. Writes need the version from core.work_calendar.get or .holiday_set.get; plan, show the review, then commit. One plan may write a calendar or set several times: each of those writes names the version you read. Check the result with core.work_calendar.days, and answer due times with core.work_calendar.business_time: add PT8H from an instant gives the due time (a result at a closing time stays there), elapsed measures business time between two instants, add_working_days counts working days. beyond true means the answer lies more than 366 days ahead or the calendar has no working time." }],
}) }
