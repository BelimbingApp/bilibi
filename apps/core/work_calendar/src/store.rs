//! Reads and audited writes of calendars, rule versions, exceptions and
//! holiday sets. No Bilimbi counterpart. Writes are reached through the
//! `core.work_calendar.*` contracts (ADR 01): `prepare` validates the input,
//! `review` checks it against stored state (a preview stops there), and
//! `apply` locks the row, reviews again against the version its plan read
//! and writes, recording each `Change` and a `core/work_calendar.changed`
//! event per calendar whose dates moved.
//!
//! Installation-wide holiday sets (issue 210) are rows of the platform
//! operator's tenant marked `installation`: every tenant reads and includes
//! them, only the operator writes them, and a change to one is told to the
//! other tenants by the `publish` job.
use crate::{
    model::{
        date, day_entry, invalid, name, valid_code, Calendar, CalendarEntry, Choice, Exception,
        FieldError, Holiday, HolidaySet, HolidaySetEntry, Listing, Rows, Rule, Substitute, Week,
        WeekDay, WriteError, WriteResult, MAX_CHOICES, MAX_ROWS,
    },
    timetable::{exception, reach},
    WorkCalendars, CHANGED, INSTALLATION_MANAGE, MANAGE,
};
use bilibi_authz::{authorize_key, authorize_operator};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_events::NewEvent;
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

/// Most holidays one write adds or removes.
const MAX_HOLIDAYS: usize = 400;

/// `search` as an `ILIKE` pattern matching it anywhere, its wildcards escaped.
fn pattern(search: &str) -> String {
    let mut escaped = String::new();
    for c in search.trim().chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    format!("%{escaped}%")
}
fn offset(page: i64, size: i64) -> i64 {
    (page - 1) * size
}
fn parse_date(text: &str) -> Result<Date> {
    text.parse().map_err(|_| Error::Internal)
}
/// A range of dates as SQL bounds; an open end is infinite.
fn bounds(from: Option<Date>, to: Option<Date>) -> (String, String) {
    (
        from.map_or_else(|| "-infinity".into(), |d| d.to_string()),
        to.map_or_else(|| "infinity".into(), |d| d.to_string()),
    )
}

/// The zone of calendar `c`'s rule version in force on the database's date
/// (the first version's before it starts), as one SQL subquery: the
/// calendars listing and their Grid table read it alike, and it matches
/// [`crate::resolve::in_force`] on the same date.
macro_rules! zone_in_force {
    () => {
        "(SELECT r.timezone FROM core_work_calendar_rules r WHERE r.tenant_id=c.tenant_id AND r.calendar_id=c.id ORDER BY r.effective_from<=CURRENT_DATE DESC, CASE WHEN r.effective_from<=CURRENT_DATE THEN r.effective_from END DESC NULLS LAST, r.effective_from LIMIT 1)"
    };
}
pub(crate) use zone_in_force;

pub(crate) async fn calendars(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    listing: &Listing,
) -> Result<Rows<CalendarEntry>> {
    if !listing.valid() {
        return Err(Error::Invalid);
    }
    let pattern = pattern(&listing.search);
    let total: i64 = client
        .query_one(
            "SELECT count(*) FROM core_work_calendars WHERE tenant_id=$1 AND (code ILIKE $2 OR name ILIKE $2)",
            &[&tenant, &pattern],
        )
        .await?
        .get(0);
    let rows = client
        .query(
            concat!(
                "SELECT c.code,c.name,c.status,",
                zone_in_force!(),
                ",\
             ARRAY(SELECT s.code FROM core_work_calendar_includes i JOIN core_work_calendar_holiday_sets s ON s.tenant_id=i.set_tenant_id AND s.id=i.holiday_set_id WHERE i.tenant_id=c.tenant_id AND i.calendar_id=c.id ORDER BY s.code LIMIT $5),\
             (SELECT count(*) FROM core_work_calendar_includes i WHERE i.tenant_id=c.tenant_id AND i.calendar_id=c.id) \
             FROM core_work_calendars c WHERE c.tenant_id=$1 AND (c.code ILIKE $2 OR c.name ILIKE $2) ORDER BY c.code LIMIT $3 OFFSET $4"
            ),
            &[&tenant, &pattern, &listing.page_size, &offset(listing.page, listing.page_size), &MAX_ROWS],
        )
        .await?
        .iter()
        .map(|row| CalendarEntry {
            code: row.get(0),
            name: row.get(1),
            status: row.get(2),
            timezone: row.get::<_, Option<String>>(3).unwrap_or_default(),
            holiday_sets: Rows {
                rows: row.get(4),
                total: row.get(5),
            },
        })
        .collect();
    Ok(Rows { rows, total })
}
/// The tenant's own holiday sets and the installation-wide ones, whose codes
/// never collide (`claim`).
pub(crate) async fn holiday_sets(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    listing: &Listing,
) -> Result<Rows<HolidaySetEntry>> {
    if !listing.valid() {
        return Err(Error::Invalid);
    }
    let pattern = pattern(&listing.search);
    let total: i64 = client
        .query_one(
            "SELECT count(*) FROM core_work_calendar_holiday_sets WHERE (tenant_id=$1 OR installation) AND (code ILIKE $2 OR name ILIKE $2)",
            &[&tenant, &pattern],
        )
        .await?
        .get(0);
    let rows = client
        .query(
            "SELECT code,name,country_iso,admin1_code,installation FROM core_work_calendar_holiday_sets WHERE (tenant_id=$1 OR installation) AND (code ILIKE $2 OR name ILIKE $2) ORDER BY code,tenant_id LIMIT $3 OFFSET $4",
            &[&tenant, &pattern, &listing.page_size, &offset(listing.page, listing.page_size)],
        )
        .await?
        .iter()
        .map(|row| HolidaySetEntry {
            code: row.get(0),
            name: row.get(1),
            country_iso: row.get(2),
            admin1_code: row.get(3),
            installation: row.get(4),
        })
        .collect();
    Ok(Rows { rows, total })
}
pub(crate) async fn choices(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    search: &str,
) -> Result<Vec<Choice>> {
    if search.chars().count() > 200 {
        return Err(Error::Invalid);
    }
    Ok(client
        .query(
            "SELECT code,name FROM core_work_calendars WHERE tenant_id=$1 AND status='active' AND (code ILIKE $2 OR name ILIKE $2) ORDER BY name,code LIMIT $3",
            &[&tenant, &pattern(search), &MAX_CHOICES],
        )
        .await?
        .iter()
        .map(|row| Choice {
            code: row.get(0),
            name: row.get(1),
        })
        .collect())
}

/// A calendar's own row: what fences, previews and writes need.
struct CalendarRow {
    id: i64,
    name: String,
    status: String,
    substitute: Substitute,
    version: i64,
}
impl CalendarRow {
    fn to_json(&self, code: &str) -> Value {
        json!({"code": code, "name": self.name, "status": self.status, "substitute": self.substitute.as_str(), "version": self.version})
    }
}
async fn calendar_row(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    code: &str,
) -> Result<CalendarRow> {
    let row = client
        .query_opt(
            "SELECT id,name,status,substitute,version FROM core_work_calendars WHERE tenant_id=$1 AND code=$2",
            &[&tenant, &code],
        )
        .await?
        .ok_or(Error::NotFound)?;
    Ok(CalendarRow {
        id: row.get(0),
        name: row.get(1),
        status: row.get(2),
        substitute: Substitute::parse(row.get(3)).ok_or(Error::Internal)?,
        version: row.get(4),
    })
}
/// A holiday set's own row: the tenant's, or an installation-wide one kept by
/// the platform operator (`tenant` is the tenant keeping it).
struct SetRow {
    id: i64,
    tenant: i64,
    installation: bool,
    name: String,
    country_iso: Option<String>,
    admin1_code: Option<String>,
    version: i64,
}
/// The set `code` names for `tenant`: its own, else an installation-wide one.
/// A tenant's codes and the installation-wide codes never collide (`claim`).
async fn set_row(client: &(impl GenericClient + Sync), tenant: i64, code: &str) -> Result<SetRow> {
    let row = client
        .query_opt(
            "SELECT id,name,country_iso,admin1_code,version,tenant_id,installation FROM core_work_calendar_holiday_sets WHERE code=$2 AND (tenant_id=$1 OR installation) ORDER BY tenant_id=$1 DESC LIMIT 1",
            &[&tenant, &code],
        )
        .await?
        .ok_or(Error::NotFound)?;
    Ok(SetRow {
        id: row.get(0),
        name: row.get(1),
        country_iso: row.get(2),
        admin1_code: row.get(3),
        version: row.get(4),
        tenant: row.get(5),
        installation: row.get(6),
    })
}
/// The set a write names: `NotFound` becomes a refusal on `field`.
async fn named_set(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    code: &str,
    field: &str,
) -> WriteResult<SetRow> {
    match set_row(client, tenant, code).await {
        Err(Error::NotFound) => Err(invalid(
            field,
            "is not a holiday set of this tenant or an installation-wide one",
        )),
        read => Ok(read?),
    }
}
/// Whether `code` is free for a new holiday set. A tenant's set may not take
/// a code its own sets or an installation-wide set hold; an installation-wide
/// set may not take one any tenant holds, so a code names one set for every
/// reader. That check reads every tenant's codes on purpose, for the
/// platform operator alone.
async fn claim(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    code: &str,
    installation: bool,
) -> WriteResult<()> {
    let taken = if installation {
        client
            .query_opt(
                "SELECT 1 FROM core_work_calendar_holiday_sets WHERE code=$1 LIMIT 1",
                &[&code],
            )
            .await?
    } else {
        client
            .query_opt(
                "SELECT 1 FROM core_work_calendar_holiday_sets WHERE code=$2 AND (tenant_id=$1 OR installation) LIMIT 1",
                &[&tenant, &code],
            )
            .await?
    };
    match taken {
        None => Ok(()),
        Some(_) if installation => Err(invalid(
            "code",
            "is already used by a holiday set of this or another tenant",
        )),
        Some(_) => Err(invalid(
            "code",
            "is already used by another holiday set, of this tenant or installation-wide",
        )),
    }
}
/// Proves `scope` may change `set`. Its own tenant's set needs only the
/// write's `admin.work-calendar.manage`. An installation-wide set is the
/// platform operator's alone, with `admin.work-calendar.holiday-sets.manage`
/// as well; every other tenant reads it and may copy it.
async fn keep(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    set: &SetRow,
) -> WriteResult<()> {
    if set.tenant != scope.tenant() {
        return Err(invalid(
            "code",
            "is an installation-wide holiday set the platform operator keeps: copy it into a set of your own (core.work_calendar.holiday_set.copy) to change it",
        ));
    }
    if set.installation {
        authorize_operator(client, scope, INSTALLATION_MANAGE).await?;
    }
    Ok(())
}
/// How many calendars of other tenants include an installation-wide set
/// `tenant` keeps, and in how many tenants: those a holiday change is told
/// to. Counts only, so the keeper never reads another tenant's codes.
pub(crate) async fn elsewhere(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    set: i64,
) -> Result<(i64, i64)> {
    let row = client
        .query_one(
            "SELECT count(*),count(DISTINCT tenant_id) FROM core_work_calendar_includes WHERE set_tenant_id=$1 AND holiday_set_id=$2 AND tenant_id<>$1",
            &[&tenant, &set],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}
/// Codes of the tenant's calendars including a set, the first [`MAX_ROWS`].
async fn including(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    set: i64,
) -> Result<Rows<String>> {
    let total: i64 = client
        .query_one(
            "SELECT count(*) FROM core_work_calendar_includes WHERE tenant_id=$1 AND holiday_set_id=$2",
            &[&tenant, &set],
        )
        .await?
        .get(0);
    let rows = client
        .query(
            "SELECT c.code FROM core_work_calendar_includes i JOIN core_work_calendars c ON c.tenant_id=i.tenant_id AND c.id=i.calendar_id WHERE i.tenant_id=$1 AND i.holiday_set_id=$2 ORDER BY c.code LIMIT $3",
            &[&tenant, &set, &MAX_ROWS],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    Ok(Rows { rows, total })
}
/// "a, b and 3 more", for a preview.
fn named(rows: &Rows<String>) -> String {
    if rows.total == 0 {
        return "none".into();
    }
    let shown: Vec<&str> = rows.rows.iter().take(10).map(String::as_str).collect();
    match rows.total - shown.len() as i64 {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}
async fn exception_row(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    calendar: i64,
    id: i64,
) -> Result<Option<Exception>> {
    client
        .query_opt(
            "SELECT id,starts_on::text,ends_on::text,name,day_type,periods FROM core_work_calendar_exceptions WHERE tenant_id=$1 AND calendar_id=$2 AND id=$3",
            &[&tenant, &calendar, &id],
        )
        .await?
        .as_ref()
        .map(exception)
        .transpose()
}

/// One calendar: its first [`MAX_ROWS`] rule versions and included sets, and
/// page `page` of its exceptions.
pub(crate) async fn calendar(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    code: &str,
    page: i64,
) -> Result<Calendar> {
    if !(1..=100_000).contains(&page) {
        return Err(Error::Invalid);
    }
    let row = calendar_row(client, tenant, code).await?;
    let id = row.id;
    let count = |table: &'static str| async move {
        Ok::<i64, Error>(
            client
                .query_one(
                    &format!("SELECT count(*) FROM {table} WHERE tenant_id=$1 AND calendar_id=$2"),
                    &[&tenant, &id],
                )
                .await?
                .get(0),
        )
    };
    let rules = client
        .query(
            "SELECT effective_from::text,timezone,week FROM core_work_calendar_rules WHERE tenant_id=$1 AND calendar_id=$2 ORDER BY effective_from LIMIT $3",
            &[&tenant, &id, &MAX_ROWS],
        )
        .await?
        .iter()
        .map(|row| {
            Ok(Rule {
                effective_from: parse_date(row.get(0))?,
                timezone: row.get(1),
                week: Week::from_stored(&row.get(2))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let holiday_sets = client
        .query(
            "SELECT s.code FROM core_work_calendar_includes i JOIN core_work_calendar_holiday_sets s ON s.tenant_id=i.set_tenant_id AND s.id=i.holiday_set_id WHERE i.tenant_id=$1 AND i.calendar_id=$2 ORDER BY s.code LIMIT $3",
            &[&tenant, &id, &MAX_ROWS],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let exceptions = client
        .query(
            "SELECT id,starts_on::text,ends_on::text,name,day_type,periods FROM core_work_calendar_exceptions WHERE tenant_id=$1 AND calendar_id=$2 ORDER BY starts_on LIMIT $3 OFFSET $4",
            &[&tenant, &id, &MAX_ROWS, &offset(page, MAX_ROWS)],
        )
        .await?
        .iter()
        .map(exception)
        .collect::<Result<Vec<_>>>()?;
    Ok(Calendar {
        code: code.to_owned(),
        name: row.name,
        status: row.status,
        substitute: row.substitute,
        version: row.version,
        rules: Rows {
            rows: rules,
            total: count("core_work_calendar_rules").await?,
        },
        holiday_sets: Rows {
            rows: holiday_sets,
            total: count("core_work_calendar_includes").await?,
        },
        exceptions: Rows {
            rows: exceptions,
            total: count("core_work_calendar_exceptions").await?,
        },
    })
}
/// A holiday set, the tenant's or an installation-wide one: page `page` of
/// its holidays from `from` to `to` (all dates when omitted) and the first
/// [`MAX_ROWS`] of the tenant's calendars including it.
pub(crate) async fn holiday_set(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    code: &str,
    from: Option<Date>,
    to: Option<Date>,
    page: i64,
) -> Result<HolidaySet> {
    if !(1..=100_000).contains(&page) {
        return Err(Error::Invalid);
    }
    let row = set_row(client, tenant, code).await?;
    let (from, to) = bounds(from, to);
    let total: i64 = client
        .query_one(
            "SELECT count(*) FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date BETWEEN $3::text::date AND $4::text::date",
            &[&row.tenant, &row.id, &from, &to],
        )
        .await?
        .get(0);
    let holidays = client
        .query(
            "SELECT date::text,name FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date BETWEEN $3::text::date AND $4::text::date ORDER BY date LIMIT $5 OFFSET $6",
            &[&row.tenant, &row.id, &from, &to, &MAX_ROWS, &offset(page, MAX_ROWS)],
        )
        .await?
        .iter()
        .map(|row| {
            Ok(Holiday {
                date: parse_date(row.get(0))?,
                name: row.get(1),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(HolidaySet {
        code: code.to_owned(),
        name: row.name,
        country_iso: row.country_iso,
        admin1_code: row.admin1_code,
        installation: row.installation,
        version: row.version,
        calendars: including(client, tenant, row.id).await?,
        holidays: Rows {
            rows: holidays,
            total,
        },
    })
}

/// One reviewed write, as its contract names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Create,
    Update,
    Rule,
    AddException,
    RemoveException,
    CreateSet,
    UpdateSet,
    Holidays,
    Include,
    CopySet,
}
/// A write's validated input.
pub(crate) enum Prepared {
    Create {
        code: String,
        name: String,
        timezone: String,
        substitute: Substitute,
        effective_from: Date,
        week: Week,
    },
    Update {
        code: String,
        version: i64,
        name: Option<String>,
        substitute: Option<Substitute>,
        status: Option<&'static str>,
    },
    Rule {
        code: String,
        version: i64,
        effective_from: Date,
        timezone: Option<String>,
        week: Week,
    },
    AddException {
        code: String,
        version: i64,
        starts_on: Date,
        ends_on: Date,
        name: String,
        day: WeekDay,
    },
    RemoveException {
        code: String,
        version: i64,
        id: i64,
    },
    CreateSet {
        code: String,
        name: String,
        country_iso: Option<String>,
        admin1_code: Option<String>,
        installation: bool,
    },
    UpdateSet {
        code: String,
        version: i64,
        name: Option<String>,
        place: Option<(Option<String>, Option<String>)>,
    },
    Holidays {
        code: String,
        version: i64,
        add: Vec<Holiday>,
        remove: Vec<Date>,
    },
    Include {
        code: String,
        version: i64,
        set: String,
        included: bool,
    },
    /// A new tenant set holding `source`'s holidays from `from` to `to`.
    CopySet {
        code: String,
        name: String,
        source: String,
        from: Option<Date>,
        to: Option<Date>,
    },
}

fn code(input: &Value, field: &str, errors: &mut Vec<FieldError>) -> String {
    let code = input[field].as_str().unwrap_or_default().trim().to_owned();
    if !valid_code(&code) {
        errors.push(FieldError::new(
            field,
            "must be 1–64 lowercase letters, digits, _ or -, starting with a letter or digit",
        ));
    }
    code
}
fn version(input: &Value, errors: &mut Vec<FieldError>) -> i64 {
    match input["version"].as_i64() {
        Some(version) if version >= 1 => version,
        _ => {
            errors.push(FieldError::new("version", "must be the version you read"));
            0
        }
    }
}
fn timezone(value: &Value, errors: &mut Vec<FieldError>) -> Option<String> {
    match value.as_str() {
        Some(zone) if bilibi_datetime::valid_timezone(zone) => Some(zone.to_owned()),
        _ => {
            errors.push(FieldError::new(
                "timezone",
                "must be an IANA time zone, such as Asia/Kuala_Lumpur",
            ));
            None
        }
    }
}
fn place(input: &Value, errors: &mut Vec<FieldError>) -> (Option<String>, Option<String>) {
    let text = |field: &str| {
        input[field]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let country = text("country_iso").map(|iso| iso.to_ascii_uppercase());
    let admin1 = text("admin1_code");
    if country
        .as_deref()
        .is_some_and(|iso| iso.len() != 2 || !iso.bytes().all(|b| b.is_ascii_alphabetic()))
    {
        errors.push(FieldError::new(
            "country_iso",
            "must be a two-letter country code",
        ));
    }
    if admin1.is_some() && country.is_none() {
        errors.push(FieldError::new("admin1_code", "needs a country"));
    }
    (country, admin1)
}

impl WorkCalendars {
    /// Parse and validate a write's input; nothing is read but the tenant's
    /// default zone and the Geonames reference data.
    pub(crate) async fn prepare(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        action: Action,
        input: &Value,
    ) -> WriteResult<Prepared> {
        let mut errors = Vec::new();
        let prepared = match action {
            Action::Create => {
                let code = code(input, "code", &mut errors);
                let name = name(&input["name"], "name", &mut errors);
                let timezone = match &input["timezone"] {
                    Value::Null => self.tenant_zone(client, scope).await?,
                    zone => timezone(zone, &mut errors).unwrap_or_default(),
                };
                let substitute = match &input["substitute"] {
                    Value::Null => Some(Substitute::None),
                    value => substitute(value, &mut errors),
                };
                let effective_from = match &input["effective_from"] {
                    Value::Null => Some(today(client).await?),
                    value => date(value, "effective_from", &mut errors),
                };
                let week = Week::parse(&input["week"], &mut errors);
                match (name, substitute, effective_from, week) {
                    (Some(name), Some(substitute), Some(effective_from), Some(week)) => {
                        Prepared::Create {
                            code,
                            name,
                            timezone,
                            substitute,
                            effective_from,
                            week,
                        }
                    }
                    _ => return Err(WriteError::Invalid(errors)),
                }
            }
            Action::Update => {
                let (code, version) = (
                    code(input, "code", &mut errors),
                    version(input, &mut errors),
                );
                let name = (!input["name"].is_null())
                    .then(|| name(&input["name"], "name", &mut errors))
                    .flatten();
                let substitute = (!input["substitute"].is_null())
                    .then(|| substitute(&input["substitute"], &mut errors))
                    .flatten();
                let status = match input["status"].as_str() {
                    None if input["status"].is_null() => None,
                    Some("active") => Some("active"),
                    Some("retired") => Some("retired"),
                    _ => {
                        errors.push(FieldError::new("status", "must be active or retired"));
                        None
                    }
                };
                Prepared::Update {
                    code,
                    version,
                    name,
                    substitute,
                    status,
                }
            }
            Action::Rule => {
                let (code, version) = (
                    code(input, "code", &mut errors),
                    version(input, &mut errors),
                );
                let effective_from = date(&input["effective_from"], "effective_from", &mut errors);
                let timezone = (!input["timezone"].is_null())
                    .then(|| timezone(&input["timezone"], &mut errors))
                    .flatten();
                let week = Week::parse(&input["week"], &mut errors);
                match (effective_from, week) {
                    (Some(effective_from), Some(week)) => Prepared::Rule {
                        code,
                        version,
                        effective_from,
                        timezone,
                        week,
                    },
                    _ => return Err(WriteError::Invalid(errors)),
                }
            }
            Action::AddException => {
                let (code, version) = (
                    code(input, "code", &mut errors),
                    version(input, &mut errors),
                );
                let starts_on = date(&input["starts_on"], "starts_on", &mut errors);
                let ends_on = match &input["ends_on"] {
                    Value::Null => starts_on,
                    value => date(value, "ends_on", &mut errors),
                };
                let name = name(&input["name"], "name", &mut errors);
                let day = day_entry(input, "exception", &mut errors);
                if let (Some(starts_on), Some(ends_on)) = (starts_on, ends_on) {
                    if ends_on < starts_on {
                        errors.push(FieldError::new("ends_on", "must not be before starts_on"));
                    } else if starts_on
                        .until(ends_on)
                        .map_or(true, |span| span.get_days() >= 366)
                    {
                        errors.push(FieldError::new(
                            "ends_on",
                            "a range covers at most 366 days",
                        ));
                    }
                }
                match (starts_on, ends_on, name, day) {
                    (Some(starts_on), Some(ends_on), Some(name), Some(day)) => {
                        Prepared::AddException {
                            code,
                            version,
                            starts_on,
                            ends_on,
                            name,
                            day,
                        }
                    }
                    _ => return Err(WriteError::Invalid(errors)),
                }
            }
            Action::RemoveException => {
                let (code, version) = (
                    code(input, "code", &mut errors),
                    version(input, &mut errors),
                );
                let id = input["exception_id"].as_i64().filter(|id| *id > 0);
                if id.is_none() {
                    errors.push(FieldError::new("exception_id", "must be an exception's id"));
                }
                Prepared::RemoveException {
                    code,
                    version,
                    id: id.unwrap_or_default(),
                }
            }
            Action::CreateSet => {
                let code = code(input, "code", &mut errors);
                let name = name(&input["name"], "name", &mut errors);
                let (country_iso, admin1_code) = place(input, &mut errors);
                self.check_place(country_iso.as_deref(), admin1_code.as_deref(), &mut errors)
                    .await?;
                let installation = match &input["installation"] {
                    Value::Null => false,
                    Value::Bool(installation) => *installation,
                    _ => {
                        errors.push(FieldError::new("installation", "must be true or false"));
                        false
                    }
                };
                match name {
                    Some(name) => Prepared::CreateSet {
                        code,
                        name,
                        country_iso,
                        admin1_code,
                        installation,
                    },
                    None => return Err(WriteError::Invalid(errors)),
                }
            }
            Action::CopySet => {
                let code = code(input, "code", &mut errors);
                let name = name(&input["name"], "name", &mut errors);
                let source = self::code(input, "source", &mut errors);
                let mut optional = |field: &str| match &input[field] {
                    Value::Null => None,
                    value => date(value, field, &mut errors),
                };
                let (from, to) = (optional("from"), optional("to"));
                if from.zip(to).is_some_and(|(from, to)| to < from) {
                    errors.push(FieldError::new("to", "must not be before from"));
                }
                match name {
                    Some(name) => Prepared::CopySet {
                        code,
                        name,
                        source,
                        from,
                        to,
                    },
                    None => return Err(WriteError::Invalid(errors)),
                }
            }
            Action::UpdateSet => {
                let (code, version) = (
                    code(input, "code", &mut errors),
                    version(input, &mut errors),
                );
                let name = (!input["name"].is_null())
                    .then(|| name(&input["name"], "name", &mut errors))
                    .flatten();
                let place = match input.get("country_iso") {
                    Some(_) => {
                        let (country, admin1) = place(input, &mut errors);
                        self.check_place(country.as_deref(), admin1.as_deref(), &mut errors)
                            .await?;
                        Some((country, admin1))
                    }
                    None => None,
                };
                Prepared::UpdateSet {
                    code,
                    version,
                    name,
                    place,
                }
            }
            Action::Holidays => {
                let (code, version) = (
                    code(input, "code", &mut errors),
                    version(input, &mut errors),
                );
                let items = |field: &str| input[field].as_array().cloned().unwrap_or_default();
                let (add, remove) = (items("add"), items("remove"));
                if add.len() > MAX_HOLIDAYS || remove.len() > MAX_HOLIDAYS {
                    errors.push(FieldError::new(
                        "add",
                        format!("at most {MAX_HOLIDAYS} holidays a write"),
                    ));
                }
                let add: Vec<Holiday> = add
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        let field = format!("add.{index}");
                        Some(Holiday {
                            date: date(&item["date"], &format!("{field}.date"), &mut errors)?,
                            name: name(&item["name"], &format!("{field}.name"), &mut errors)?,
                        })
                    })
                    .collect();
                let listed = remove.len();
                let remove: Vec<Date> = remove
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| date(item, &format!("remove.{index}"), &mut errors))
                    .collect();
                let mut dates: Vec<Date> = add.iter().map(|h| h.date).collect();
                dates.sort();
                if dates.windows(2).any(|pair| pair[0] == pair[1]) {
                    errors.push(FieldError::new("add", "names each date once"));
                }
                // Positions match the input only when every date parsed.
                let positioned = if remove.len() == listed {
                    &remove[..]
                } else {
                    &[]
                };
                for (index, day) in positioned.iter().enumerate() {
                    if remove[..index].contains(day) {
                        errors.push(FieldError::new(
                            &format!("remove.{index}"),
                            "is already removed earlier in this list",
                        ));
                    } else if dates.binary_search(day).is_ok() {
                        errors.push(FieldError::new(
                            &format!("remove.{index}"),
                            "is also in add: rename a holiday by adding it",
                        ));
                    }
                }
                if add.is_empty() && remove.is_empty() {
                    errors.push(FieldError::new("add", "add or remove at least one holiday"));
                }
                Prepared::Holidays {
                    code,
                    version,
                    add,
                    remove,
                }
            }
            Action::Include => {
                let (code, version) = (
                    code(input, "code", &mut errors),
                    version(input, &mut errors),
                );
                let set = self::code(input, "holiday_set", &mut errors);
                let included = input["included"].as_bool().unwrap_or(true);
                Prepared::Include {
                    code,
                    version,
                    set,
                    included,
                }
            }
        };
        if errors.is_empty() {
            Ok(prepared)
        } else {
            Err(WriteError::Invalid(errors))
        }
    }

    async fn tenant_zone(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<String> {
        let zone = self
            .settings
            .get(
                client,
                scope,
                bilibi_settings::Target::Tenant,
                bilibi_datetime::TIMEZONE_KEY,
            )
            .await?
            .value;
        Ok(zone
            .as_str()
            .filter(|zone| bilibi_datetime::valid_timezone(zone))
            .unwrap_or("UTC")
            .to_owned())
    }
    async fn check_place(
        &self,
        country: Option<&str>,
        admin1: Option<&str>,
        errors: &mut Vec<FieldError>,
    ) -> Result<()> {
        let Some(country) = country else {
            return Ok(());
        };
        if self.geonames.get_country(country).await?.is_none() {
            errors.push(FieldError::new(
                "country_iso",
                "is not a known country (import countries first)",
            ));
        }
        if let Some(code) = admin1 {
            match self.geonames.get_admin1(code).await? {
                Some(division) if division.country_iso == country => {}
                Some(_) => {
                    errors.push(FieldError::new("admin1_code", "belongs to another country"))
                }
                None => errors.push(FieldError::new("admin1_code", "is not a known division")),
            }
        }
        Ok(())
    }

    /// Check a prepared write against stored state, as a preview shows it.
    /// `apply` runs it again under the row locks, fencing against `read`: the
    /// version its plan read of the calendar or set it writes (`plan_read`).
    pub(crate) async fn review(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        prepared: &Prepared,
        read: Option<i64>,
    ) -> WriteResult<Value> {
        let tenant = scope.tenant();
        let fenced = |stored: i64, version: i64| {
            if read.unwrap_or(stored) == version {
                Ok(())
            } else {
                Err(WriteError::Stale)
            }
        };
        Ok(match prepared {
            Prepared::Create {
                code,
                name,
                timezone,
                effective_from,
                ..
            } => {
                if client
                    .query_opt(
                        "SELECT 1 FROM core_work_calendars WHERE tenant_id=$1 AND code=$2",
                        &[&tenant, &code],
                    )
                    .await?
                    .is_some()
                {
                    return Err(invalid("code", "is already used by another calendar"));
                }
                json!({"summary": format!("Create calendar {name} ({code}) in {timezone}, from {effective_from}")})
            }
            Prepared::Update {
                code,
                version,
                name,
                substitute,
                status,
            } => {
                let calendar = known(calendar_row(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                let mut changes = Vec::new();
                if let Some(name) = name {
                    changes.push(format!("name {name}"));
                }
                if let Some(substitute) = substitute {
                    changes.push(format!("substitutes {}", substitute.as_str()));
                }
                if let Some(status) = status {
                    changes.push(format!("status {status}"));
                }
                json!({"summary": format!("Update calendar {code}: {}", if changes.is_empty() { "nothing".into() } else { changes.join(", ") }), "before": calendar.to_json(code)})
            }
            Prepared::Rule {
                code,
                version,
                effective_from,
                ..
            } => {
                let calendar = known(calendar_row(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                let replaces = client
                    .query_opt(
                        "SELECT 1 FROM core_work_calendar_rules WHERE tenant_id=$1 AND calendar_id=$2 AND effective_from=$3::text::date",
                        &[&tenant, &calendar.id, &effective_from.to_string()],
                    )
                    .await?
                    .is_some();
                json!({"summary": format!("{} the week of calendar {code} from {effective_from}", if replaces { "Replace" } else { "Set" })})
            }
            Prepared::AddException {
                code,
                version,
                starts_on,
                ends_on,
                name,
                day,
            } => {
                let calendar = known(calendar_row(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                if let Some(other) = client
                    .query_opt(
                        "SELECT name,starts_on::text,ends_on::text FROM core_work_calendar_exceptions WHERE tenant_id=$1 AND calendar_id=$2 AND starts_on<=$4::text::date AND ends_on>=$3::text::date ORDER BY starts_on LIMIT 1",
                        &[&tenant, &calendar.id, &starts_on.to_string(), &ends_on.to_string()],
                    )
                    .await?
                {
                    return Err(invalid(
                        "starts_on",
                        format!(
                            "overlaps the exception {} ({} to {})",
                            other.get::<_, &str>(0),
                            other.get::<_, &str>(1),
                            other.get::<_, &str>(2)
                        ),
                    ));
                }
                json!({"summary": format!("Make {starts_on} to {ends_on} {} on calendar {code}: {name}", day.day_type.as_str())})
            }
            Prepared::RemoveException { code, version, id } => {
                let calendar = known(calendar_row(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                let Some(found) = exception_row(client, tenant, calendar.id, *id).await? else {
                    return Err(invalid(
                        "exception_id",
                        "is not an exception of this calendar",
                    ));
                };
                json!({"summary": format!("Remove the exception {} ({} to {}) from calendar {code}", found.name, found.starts_on, found.ends_on)})
            }
            Prepared::CreateSet {
                code,
                name,
                installation,
                ..
            } => {
                if *installation {
                    authorize_operator(client, scope, INSTALLATION_MANAGE).await?;
                }
                claim(client, tenant, code, *installation).await?;
                json!({"summary": format!("Create {}holiday set {name} ({code})", if *installation { "installation-wide " } else { "" })})
            }
            Prepared::UpdateSet { code, version, .. } => {
                let set = known(set_row(client, tenant, code).await)?;
                keep(client, scope, &set).await?;
                fenced(set.version, *version)?;
                let calendars = including(client, tenant, set.id).await?;
                json!({"summary": format!("Update holiday set {code}"), "calendars": calendars.rows, "calendars_total": calendars.total})
            }
            Prepared::Holidays {
                code,
                version,
                add,
                remove,
            } => {
                let set = known(set_row(client, tenant, code).await)?;
                keep(client, scope, &set).await?;
                fenced(set.version, *version)?;
                // A removal names a date the set holds: one it does not would
                // remove nothing.
                let wanted: Vec<String> = remove.iter().map(Date::to_string).collect();
                let held: Vec<String> = client
                    .query(
                        "SELECT date::text FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date=ANY($3::text[]::date[])",
                        &[&tenant, &set.id, &wanted],
                    )
                    .await?
                    .iter()
                    .map(|row| row.get(0))
                    .collect();
                let absent: Vec<FieldError> = wanted
                    .iter()
                    .enumerate()
                    .filter(|(_, day)| !held.contains(day))
                    .map(|(index, _)| {
                        FieldError::new(&format!("remove.{index}"), "is not a holiday of this set")
                    })
                    .collect();
                if !absent.is_empty() {
                    return Err(WriteError::Invalid(absent));
                }
                let calendars = including(client, tenant, set.id).await?;
                let mut summary = format!(
                    "Holiday set {code}: {} added or renamed, {} removed; calendars that include it: {}",
                    add.len(),
                    remove.len(),
                    named(&calendars)
                );
                let (others, tenants) = match set.installation {
                    true => elsewhere(client, tenant, set.id).await?,
                    false => (0, 0),
                };
                if others > 0 {
                    summary.push_str(&format!("; and {others} calendars in {tenants} other tenants, told through core/work_calendar.changed"));
                }
                json!({"summary": summary, "calendars": calendars.rows, "calendars_total": calendars.total, "other_tenant_calendars": others, "other_tenants": tenants})
            }
            Prepared::Include {
                code,
                version,
                set,
                included,
            } => {
                let calendar = known(calendar_row(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                let found = named_set(client, tenant, set, "holiday_set").await?;
                let kind = if found.installation {
                    "installation-wide holiday set"
                } else {
                    "holiday set"
                };
                json!({"summary": format!("{} {kind} {set} {} calendar {code}", if *included { "Include" } else { "Leave out" }, if *included { "in" } else { "of" })})
            }
            Prepared::CopySet {
                code,
                name,
                source,
                from,
                to,
            } => {
                claim(client, tenant, code, false).await?;
                let origin = named_set(client, tenant, source, "source").await?;
                let (first, last) = bounds(*from, *to);
                let count: i64 = client
                    .query_one(
                        "SELECT count(*) FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date BETWEEN $3::text::date AND $4::text::date",
                        &[&origin.tenant, &origin.id, &first, &last],
                    )
                    .await?
                    .get(0);
                if count > MAX_HOLIDAYS as i64 {
                    return Err(invalid(
                        "to",
                        format!("covers {count} holidays; a copy takes at most {MAX_HOLIDAYS}: narrow from and to, then add the rest with core.work_calendar.holiday_set.holidays"),
                    ));
                }
                json!({"summary": format!("Copy {count} holidays of {}holiday set {source} into a new holiday set {name} ({code})", if origin.installation { "installation-wide " } else { "" }), "holidays": count})
            }
        })
    }

    /// Lock, review again and write, in the caller's audited transaction.
    pub(crate) async fn apply(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        prepared: Prepared,
    ) -> WriteResult<Value> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        let tenant = scope.tenant();
        // Only the tenant keeping a set writes it; one it reads (its own or
        // an installation-wide one) is locked shared.
        let lock_set =
            "SELECT id,version FROM core_work_calendar_holiday_sets WHERE tenant_id=$1 AND code=$2 FOR UPDATE";
        let share_set = "SELECT id FROM core_work_calendar_holiday_sets WHERE code=$2 AND (tenant_id=$1 OR installation) ORDER BY tenant_id=$1 DESC LIMIT 1 FOR SHARE";
        let lock_calendar =
            "SELECT id,version FROM core_work_calendars WHERE tenant_id=$1 AND code=$2 FOR UPDATE";
        // Serialize writers of one calendar or set before reviewing. A
        // holiday write locks its set, and an inclusion its calendar and then
        // the set (shared), so whichever commits second reads the other's
        // rows and records `changed` for the pair. The row the write is
        // fenced on comes back with its version.
        let locked = match &prepared {
            Prepared::Create { .. } | Prepared::CreateSet { .. } | Prepared::CopySet { .. } => {
                tx.sql()
                    .execute(
                        "SELECT pg_advisory_xact_lock(91351,$1::integer)",
                        &[&i32::try_from(tenant).map_err(|_| Error::Invalid)?],
                    )
                    .await?;
                // A code names one set for every reader (`claim`): a new
                // installation-wide set holds its code's lock alone, while
                // tenants' new sets share it, so tenants never wait on (or
                // deadlock with) each other.
                let code_lock = match &prepared {
                    Prepared::CreateSet {
                        code,
                        installation: true,
                        ..
                    } => Some(("SELECT pg_advisory_xact_lock(91352,hashtext($1))", code)),
                    Prepared::CreateSet { code, .. } | Prepared::CopySet { code, .. } => Some((
                        "SELECT pg_advisory_xact_lock_shared(91352,hashtext($1))",
                        code,
                    )),
                    _ => None,
                };
                if let Some((statement, code)) = code_lock {
                    tx.sql().execute(statement, &[code]).await?;
                }
                if let Prepared::CopySet { source, .. } = &prepared {
                    tx.sql().query_opt(share_set, &[&tenant, source]).await?;
                }
                None
            }
            Prepared::UpdateSet { code, .. } | Prepared::Holidays { code, .. } => tx
                .sql()
                .query_opt(lock_set, &[&tenant, code])
                .await?
                .map(|row| ("set", row)),
            Prepared::Include { code, set, .. } => {
                let calendar = tx.sql().query_opt(lock_calendar, &[&tenant, code]).await?;
                tx.sql().query_opt(share_set, &[&tenant, set]).await?;
                calendar.map(|row| ("calendar", row))
            }
            Prepared::Update { code, .. }
            | Prepared::Rule { code, .. }
            | Prepared::AddException { code, .. }
            | Prepared::RemoveException { code, .. } => tx
                .sql()
                .query_opt(lock_calendar, &[&tenant, code])
                .await?
                .map(|row| ("calendar", row)),
        };
        let read = match locked {
            Some((kind, row)) => Some(plan_read(tx.sql(), kind, row.get(0), row.get(1)).await?),
            None => None,
        };
        self.review(tx.sql(), scope, &prepared, read).await?;
        let mut changed: Vec<(String, Option<Date>, Option<Date>)> = Vec::new();
        let result = match prepared {
            Prepared::Create {
                code,
                name,
                timezone,
                substitute,
                effective_from,
                week,
            } => {
                let (stored_week, from) = (week.to_stored(), effective_from.to_string());
                let calendar = code.clone();
                tx.mutate(move |sql| Box::pin(async move {
                    let id: i64 = sql.query_one("INSERT INTO core_work_calendars(tenant_id,code,name,substitute) VALUES($1,$2,$3,$4) RETURNING id", &[&tenant, &code, &name, &substitute.as_str()]).await?.get(0);
                    let rule: i64 = sql.query_one("INSERT INTO core_work_calendar_rules(tenant_id,calendar_id,effective_from,timezone,week) VALUES($1,$2,$3::text::date,$4,$5) RETURNING id", &[&tenant, &id, &from, &timezone, &stored_week]).await?.get(0);
                    Ok(((), vec![
                        Change { kind: "core/work_calendar.calendar".into(), id, old: json!({}), new: json!({"code": code, "name": name, "status": "active", "substitute": substitute.as_str()}) },
                        Change { kind: "core/work_calendar.rule".into(), id: rule, old: json!({}), new: json!({"calendar": code, "effective_from": from, "timezone": timezone, "week": stored_week}) },
                    ]))
                })).await?;
                json!({"code": calendar})
            }
            Prepared::Update {
                code,
                name,
                substitute,
                status,
                ..
            } => {
                let before = known(calendar_row(tx.sql(), tenant, &code).await)?;
                let id = before.id;
                let old = json!({"name": before.name, "status": before.status, "substitute": before.substitute.as_str()});
                let new = json!({
                    "name": name.clone().unwrap_or(before.name.clone()),
                    "status": status.unwrap_or(before.status.as_str()),
                    "substitute": substitute.unwrap_or(before.substitute).as_str(),
                });
                if substitute.is_some_and(|s| s != before.substitute) {
                    changed.push((code.clone(), None, None));
                }
                let stored = new.clone();
                tx.mutate(move |sql| Box::pin(async move {
                    sql.execute("UPDATE core_work_calendars SET name=$3,status=$4,substitute=$5,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id, &stored["name"].as_str(), &stored["status"].as_str(), &stored["substitute"].as_str()]).await?;
                    Ok(((), vec![Change { kind: "core/work_calendar.calendar".into(), id, old, new: stored }]))
                })).await?;
                new
            }
            Prepared::Rule {
                code,
                effective_from,
                timezone,
                week,
                ..
            } => {
                let id = known(calendar_row(tx.sql(), tenant, &code).await)?.id;
                let from = effective_from.to_string();
                // The version in force on the date, else the first.
                let (in_force, first) = {
                    let row = tx.sql().query_one(
                        "SELECT (SELECT timezone FROM core_work_calendar_rules WHERE tenant_id=$1 AND calendar_id=$2 ORDER BY effective_from<=$3::text::date DESC, CASE WHEN effective_from<=$3::text::date THEN effective_from END DESC NULLS LAST, effective_from LIMIT 1),(SELECT min(effective_from)::text FROM core_work_calendar_rules WHERE tenant_id=$1 AND calendar_id=$2)",
                        &[&tenant, &id, &from],
                    ).await?;
                    let first: Option<&str> = row.get(1);
                    (
                        row.get::<_, Option<String>>(0),
                        first.map(parse_date).transpose()?,
                    )
                };
                let timezone = timezone.or(in_force).unwrap_or_else(|| "UTC".into());
                // The first version also governs every earlier date, so
                // setting or replacing it moves those dates too.
                let lower = first
                    .filter(|first| effective_from > *first)
                    .map(|_| effective_from);
                changed.push((code.clone(), lower, None));
                let stored_week = week.to_stored();
                let reply = json!({"code": code, "effective_from": from, "timezone": timezone, "week": week.to_json()});
                tx.mutate(move |sql| Box::pin(async move {
                    let old = sql.query_opt("SELECT timezone,week FROM core_work_calendar_rules WHERE tenant_id=$1 AND calendar_id=$2 AND effective_from=$3::text::date", &[&tenant, &id, &from]).await?.map(|row| json!({"timezone": row.get::<_, String>(0), "week": row.get::<_, Value>(1)}));
                    let rule: i64 = sql.query_one("INSERT INTO core_work_calendar_rules(tenant_id,calendar_id,effective_from,timezone,week) VALUES($1,$2,$3::text::date,$4,$5) ON CONFLICT (tenant_id,calendar_id,effective_from) DO UPDATE SET timezone=EXCLUDED.timezone,week=EXCLUDED.week RETURNING id", &[&tenant, &id, &from, &timezone, &stored_week]).await?.get(0);
                    sql.execute("UPDATE core_work_calendars SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                    Ok(((), vec![Change { kind: "core/work_calendar.rule".into(), id: rule, old: old.unwrap_or(json!({})), new: json!({"calendar": code, "effective_from": from, "timezone": timezone, "week": stored_week}) }]))
                })).await?;
                reply
            }
            Prepared::AddException {
                code,
                starts_on,
                ends_on,
                name,
                day,
                ..
            } => {
                let id = known(calendar_row(tx.sql(), tenant, &code).await)?.id;
                changed.push((code.clone(), Some(starts_on), Some(ends_on)));
                let (from, until, periods) = (
                    starts_on.to_string(),
                    ends_on.to_string(),
                    crate::model::periods_json(&day.periods),
                );
                let facts = json!({"calendar": code, "starts_on": from, "ends_on": until, "name": name, "type": day.day_type.as_str(), "periods": periods});
                let new = facts.clone();
                let exception: i64 = tx.mutate(move |sql| Box::pin(async move {
                    let exception: i64 = sql.query_one("INSERT INTO core_work_calendar_exceptions(tenant_id,calendar_id,starts_on,ends_on,name,day_type,periods) VALUES($1,$2,$3::text::date,$4::text::date,$5,$6,$7) RETURNING id", &[&tenant, &id, &from, &until, &name, &day.day_type.as_str(), &periods]).await?.get(0);
                    sql.execute("UPDATE core_work_calendars SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                    Ok((exception, vec![Change { kind: "core/work_calendar.exception".into(), id: exception, old: json!({}), new }]))
                })).await?;
                let mut reply = facts;
                reply["id"] = json!(exception);
                reply
            }
            Prepared::RemoveException {
                code,
                id: exception,
                ..
            } => {
                let id = known(calendar_row(tx.sql(), tenant, &code).await)?.id;
                let found = exception_row(tx.sql(), tenant, id, exception)
                    .await?
                    .ok_or(WriteError::NotFound)?;
                changed.push((code.clone(), Some(found.starts_on), Some(found.ends_on)));
                // The same facts the add path records, periods included.
                let old = json!({"calendar": code, "starts_on": found.starts_on.to_string(), "ends_on": found.ends_on.to_string(), "name": found.name, "type": found.day.day_type.as_str(), "periods": crate::model::periods_json(&found.day.periods)});
                tx.mutate(move |sql| Box::pin(async move {
                    sql.execute("DELETE FROM core_work_calendar_exceptions WHERE tenant_id=$1 AND calendar_id=$2 AND id=$3", &[&tenant, &id, &exception]).await?;
                    sql.execute("UPDATE core_work_calendars SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                    Ok(((), vec![Change { kind: "core/work_calendar.exception".into(), id: exception, old, new: json!({}) }]))
                })).await?;
                json!({"removed": exception})
            }
            Prepared::CreateSet {
                code,
                name,
                country_iso,
                admin1_code,
                installation,
            } => {
                let reply = json!({"code": code, "name": name, "country_iso": country_iso, "admin1_code": admin1_code, "installation": installation});
                let new = reply.clone();
                tx.mutate(move |sql| Box::pin(async move {
                    let id: i64 = sql.query_one("INSERT INTO core_work_calendar_holiday_sets(tenant_id,code,name,country_iso,admin1_code,installation) VALUES($1,$2,$3,$4,$5,$6) RETURNING id", &[&tenant, &code, &name, &country_iso, &admin1_code, &installation]).await?.get(0);
                    Ok(((), vec![Change { kind: "core/work_calendar.holiday_set".into(), id, old: json!({}), new }]))
                })).await?;
                reply
            }
            Prepared::UpdateSet {
                code, name, place, ..
            } => {
                let before = known(set_row(tx.sql(), tenant, &code).await)?;
                let id = before.id;
                let old = json!({"name": before.name, "country_iso": before.country_iso, "admin1_code": before.admin1_code});
                let (country_iso, admin1_code) =
                    place.unwrap_or((before.country_iso.clone(), before.admin1_code.clone()));
                let new = json!({"name": name.unwrap_or(before.name.clone()), "country_iso": country_iso, "admin1_code": admin1_code});
                let stored = new.clone();
                tx.mutate(move |sql| Box::pin(async move {
                    sql.execute("UPDATE core_work_calendar_holiday_sets SET name=$3,country_iso=$4,admin1_code=$5,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id, &stored["name"].as_str(), &stored["country_iso"].as_str(), &stored["admin1_code"].as_str()]).await?;
                    Ok(((), vec![Change { kind: "core/work_calendar.holiday_set".into(), id, old, new: stored }]))
                })).await?;
                new
            }
            Prepared::Holidays {
                code, add, remove, ..
            } => {
                let set = known(set_row(tx.sql(), tenant, &code).await)?;
                let id = set.id;
                let touched: Vec<Date> = add
                    .iter()
                    .map(|h| h.date)
                    .chain(remove.iter().copied())
                    .collect();
                let added = add.len();
                let removed: usize = tx.mutate(move |sql| Box::pin(async move {
                    let mut changes = Vec::new();
                    for holiday in &remove {
                        let day = holiday.to_string();
                        if let Some(row) = sql.query_opt("DELETE FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date=$3::text::date RETURNING id,name", &[&tenant, &id, &day]).await? {
                            changes.push(Change { kind: "core/work_calendar.holiday".into(), id: row.get(0), old: json!({"set": code, "date": day, "name": row.get::<_, String>(1)}), new: json!({}) });
                        }
                    }
                    let removed = changes.len();
                    for holiday in &add {
                        let day = holiday.date.to_string();
                        let old = sql.query_opt("SELECT name FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date=$3::text::date", &[&tenant, &id, &day]).await?.map(|row| json!({"set": code, "date": day, "name": row.get::<_, String>(0)}));
                        let row: i64 = sql.query_one("INSERT INTO core_work_calendar_holidays(tenant_id,holiday_set_id,date,name) VALUES($1,$2,$3::text::date,$4) ON CONFLICT (tenant_id,holiday_set_id,date) DO UPDATE SET name=EXCLUDED.name RETURNING id", &[&tenant, &id, &day, &holiday.name]).await?.get(0);
                        changes.push(Change { kind: "core/work_calendar.holiday".into(), id: row, old: old.unwrap_or(json!({})), new: json!({"set": code, "date": day, "name": holiday.name}) });
                    }
                    sql.execute("UPDATE core_work_calendar_holiday_sets SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                    Ok((removed, changes))
                })).await?;
                if let (Some(first), Some(last)) = (touched.iter().min(), touched.iter().max()) {
                    for (calendar, to) in reach(tx.sql(), tenant, Some(id), None, *last).await? {
                        changed.push((calendar, Some(*first), Some(to)));
                    }
                    // Other tenants' calendars hear of an installation-wide
                    // change from the `publish` job, queued with it. Under
                    // the set's lock no inclusion can slip in unseen: one
                    // waiting on it reads these holidays and tells itself.
                    if set.installation && elsewhere(tx.sql(), tenant, id).await?.0 > 0 {
                        self.enqueue_publish(tx, id, *first, *last).await?;
                    }
                }
                json!({"added_or_renamed": added, "removed": removed})
            }
            Prepared::Include {
                code,
                set,
                included,
                ..
            } => {
                let id = known(calendar_row(tx.sql(), tenant, &code).await)?.id;
                let found = known(set_row(tx.sql(), tenant, &set).await)?;
                let (set_id, keeper, installation) = (found.id, found.tenant, found.installation);
                let already = tx
                    .sql()
                    .query_opt(
                        "SELECT 1 FROM core_work_calendar_includes WHERE tenant_id=$1 AND calendar_id=$2 AND holiday_set_id=$3",
                        &[&tenant, &id, &set_id],
                    )
                    .await?
                    .is_some();
                if already != included {
                    let span = tx
                        .sql()
                        .query_one(
                            "SELECT min(date)::text,max(date)::text FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2",
                            &[&keeper, &set_id],
                        )
                        .await?;
                    let (first, last): (Option<&str>, Option<&str>) = (span.get(0), span.get(1));
                    let (first, last) = (
                        first.map(parse_date).transpose()?,
                        last.map(parse_date).transpose()?,
                    );
                    let facts =
                        json!({"calendar": code, "holiday_set": set, "installation": installation});
                    tx.mutate(move |sql| Box::pin(async move {
                        if included {
                            sql.execute("INSERT INTO core_work_calendar_includes(tenant_id,calendar_id,holiday_set_id,set_tenant_id,set_installation) VALUES($1,$2,$3,$4,$5)", &[&tenant, &id, &set_id, &keeper, &installation]).await?;
                        } else {
                            sql.execute("DELETE FROM core_work_calendar_includes WHERE tenant_id=$1 AND calendar_id=$2 AND holiday_set_id=$3", &[&tenant, &id, &set_id]).await?;
                        }
                        sql.execute("UPDATE core_work_calendars SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                        let (old, new) = if included { (json!({}), facts) } else { (facts, json!({})) };
                        Ok(((), vec![Change { kind: "core/work_calendar.include".into(), id: set_id, old, new }]))
                    })).await?;
                    if let (Some(first), Some(last)) = (first, last) {
                        for (calendar, to) in reach(tx.sql(), tenant, None, Some(id), last).await? {
                            changed.push((calendar, Some(first), Some(to)));
                        }
                    }
                }
                json!({"code": code, "holiday_set": set, "included": included})
            }
            Prepared::CopySet {
                code,
                name,
                source,
                from,
                to,
            } => {
                let origin = known(set_row(tx.sql(), tenant, &source).await)?;
                let (first, last) = bounds(from, to);
                let reply = json!({"code": code, "name": name, "source": source, "country_iso": origin.country_iso, "admin1_code": origin.admin1_code});
                let new = reply.clone();
                // No calendar includes the new set yet, so no date moves.
                let copied: usize = tx.mutate(move |sql| Box::pin(async move {
                    let id: i64 = sql.query_one("INSERT INTO core_work_calendar_holiday_sets(tenant_id,code,name,country_iso,admin1_code) VALUES($1,$2,$3,$4,$5) RETURNING id", &[&tenant, &code, &name, &origin.country_iso, &origin.admin1_code]).await?.get(0);
                    let mut changes = vec![Change { kind: "core/work_calendar.holiday_set".into(), id, old: json!({}), new }];
                    for row in sql.query("INSERT INTO core_work_calendar_holidays(tenant_id,holiday_set_id,date,name) SELECT $1,$2,date,name FROM core_work_calendar_holidays WHERE tenant_id=$3 AND holiday_set_id=$4 AND date BETWEEN $5::text::date AND $6::text::date ORDER BY date RETURNING id,date::text,name", &[&tenant, &id, &origin.tenant, &origin.id, &first, &last]).await? {
                        changes.push(Change { kind: "core/work_calendar.holiday".into(), id: row.get(0), old: json!({}), new: json!({"set": code, "date": row.get::<_, String>(1), "name": row.get::<_, String>(2)}) });
                    }
                    Ok((changes.len() - 1, changes))
                })).await?;
                let mut reply = reply;
                reply["copied"] = json!(copied);
                reply
            }
        };
        for (calendar, from, to) in changed {
            self.record_changed(tx, &calendar, from, to, (from, to))
                .await?;
        }
        Ok(result)
    }

    /// Records `core/work_calendar.changed` for `calendar` in the transaction's
    /// tenant. One event per logical change: a plan may change one calendar
    /// several times in its transaction, so the key names the change's range
    /// (`keyed`) too; recording the very same change twice is a no-op, and
    /// the same key with another range is `Conflict`.
    pub(crate) async fn record_changed(
        &self,
        tx: &mut MutationTransaction<'_>,
        calendar: &str,
        from: Option<Date>,
        to: Option<Date>,
        keyed: (Option<Date>, Option<Date>),
    ) -> Result<()> {
        let trace = tx.context().trace_id.clone();
        let day = |date: Option<Date>| date.map_or("-".to_owned(), |d| d.to_string());
        let key = format!("{trace}:{calendar}:{}:{}", day(keyed.0), day(keyed.1));
        self.events
            .record(
                tx,
                NewEvent {
                    name: CHANGED,
                    subject: calendar,
                    key: &key,
                    payload: json!({"calendar": calendar, "from": from.map(|d| d.to_string()), "to": to.map(|d| d.to_string())}),
                },
            )
            .await?;
        Ok(())
    }
}

fn substitute(value: &Value, errors: &mut Vec<FieldError>) -> Option<Substitute> {
    let parsed = value.as_str().and_then(Substitute::parse);
    if parsed.is_none() {
        errors.push(FieldError::new(
            "substitute",
            "must be none, rest or rest_and_off",
        ));
    }
    parsed
}
/// The version of a calendar or set its plan read: the one stored when this
/// transaction first locked it for a write. A plan commits its operations in
/// one transaction, each reviewed against the version read before the plan,
/// so a later write to the same calendar or set is fenced against that, not
/// the version an earlier write of the plan advanced; a change committed by
/// anyone else since still makes it stale. A transaction-local setting keeps
/// the versions, so PostgreSQL drops them when the transaction ends.
async fn plan_read(
    client: &(impl GenericClient + Sync),
    kind: &str,
    id: i64,
    stored: i64,
) -> Result<i64> {
    let key = format!("{kind}:{id}");
    let setting: Option<String> = client
        .query_one(
            "SELECT current_setting('bilibi_work_calendar.plan_read',true)",
            &[],
        )
        .await?
        .get(0);
    // Empty (or absent) when no write of this transaction has locked one.
    let mut read: serde_json::Map<String, Value> = setting
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if let Some(version) = read.get(&key).and_then(Value::as_i64) {
        return Ok(version);
    }
    read.insert(key, json!(stored));
    client
        .execute(
            "SELECT set_config('bilibi_work_calendar.plan_read',$1,true)",
            &[&Value::Object(read).to_string()],
        )
        .await?;
    Ok(stored)
}
/// The database's date: what defaults a write's dates and picks the rule
/// version "in force today" in listings.
pub(crate) async fn today(client: &(impl GenericClient + Sync)) -> Result<Date> {
    client
        .query_one("SELECT CURRENT_DATE::text", &[])
        .await?
        .get::<_, &str>(0)
        .parse()
        .map_err(|_| Error::Internal)
}
fn known<T>(read: Result<T>) -> WriteResult<T> {
    match read {
        Ok(value) => Ok(value),
        Err(Error::NotFound) => Err(WriteError::NotFound),
        Err(error) => Err(error.into()),
    }
}
