//! Reads and audited writes of calendars, rule versions, exceptions and
//! holiday sets. No Bilimbi counterpart. Writes are reached through the
//! `core.work_calendar.*` contracts (ADR 01): `prepare` validates the input,
//! `review` checks it against stored state (a preview stops there), and
//! `apply` locks the row, reviews again and writes, recording each `Change`
//! and a `core/work_calendar.changed` event per calendar whose dates moved.
use crate::{
    model::{
        date, day_entry, invalid, name, valid_code, Calendar, Choice, FieldError, Holiday,
        HolidaySet, Rule, Substitute, Summary, Week, WeekDay, WriteError, WriteResult,
    },
    timetable::exception,
    WorkCalendars, CHANGED, MANAGE,
};
use bilibi_authz::authorize_key;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_events::NewEvent;
use bilibi_tenancy::Scope;
use jiff::{civil::Date, ToSpan};
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

/// Most holidays one write adds or removes.
const MAX_HOLIDAYS: usize = 400;

pub(crate) async fn summary(client: &(impl GenericClient + Sync), tenant: i64) -> Result<Summary> {
    let calendars = client
        .query(
            "SELECT code,name,status FROM core_work_calendars WHERE tenant_id=$1 ORDER BY code",
            &[&tenant],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect();
    let holiday_sets = client
        .query(
            "SELECT code,name,country_iso,admin1_code FROM core_work_calendar_holiday_sets WHERE tenant_id=$1 ORDER BY code",
            &[&tenant],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect();
    Ok(Summary {
        calendars,
        holiday_sets,
    })
}
pub(crate) async fn choices(
    client: &(impl GenericClient + Sync),
    tenant: i64,
) -> Result<Vec<Choice>> {
    Ok(client
        .query(
            "SELECT code,name FROM core_work_calendars WHERE tenant_id=$1 AND status='active' ORDER BY name,code",
            &[&tenant],
        )
        .await?
        .iter()
        .map(|row| Choice {
            code: row.get(0),
            name: row.get(1),
        })
        .collect())
}

pub(crate) async fn calendar(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    code: &str,
) -> Result<(i64, Calendar)> {
    let row = client
        .query_opt(
            "SELECT id,code,name,status,substitute,version FROM core_work_calendars WHERE tenant_id=$1 AND code=$2",
            &[&tenant, &code],
        )
        .await?
        .ok_or(Error::NotFound)?;
    let id: i64 = row.get(0);
    let rules = client
        .query(
            "SELECT effective_from::text,timezone,week FROM core_work_calendar_rules WHERE tenant_id=$1 AND calendar_id=$2 ORDER BY effective_from",
            &[&tenant, &id],
        )
        .await?
        .iter()
        .map(|row| {
            Ok(Rule {
                effective_from: row.get::<_, &str>(0).parse().map_err(|_| Error::Internal)?,
                timezone: row.get(1),
                week: Week::from_stored(&row.get(2))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let holiday_sets = client
        .query(
            "SELECT s.code FROM core_work_calendar_includes i JOIN core_work_calendar_holiday_sets s ON s.tenant_id=i.tenant_id AND s.id=i.holiday_set_id WHERE i.tenant_id=$1 AND i.calendar_id=$2 ORDER BY s.code",
            &[&tenant, &id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let exceptions = client
        .query(
            "SELECT id,starts_on::text,ends_on::text,name,day_type,periods FROM core_work_calendar_exceptions WHERE tenant_id=$1 AND calendar_id=$2 ORDER BY starts_on",
            &[&tenant, &id],
        )
        .await?
        .iter()
        .map(exception)
        .collect::<Result<Vec<_>>>()?;
    Ok((
        id,
        Calendar {
            code: row.get(1),
            name: row.get(2),
            status: row.get(3),
            substitute: Substitute::parse(row.get(4)).ok_or(Error::Internal)?,
            version: row.get(5),
            rules,
            holiday_sets,
            exceptions,
        },
    ))
}
/// A holiday set and its holidays from `from` to `to` (all when omitted).
pub(crate) async fn holiday_set(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    code: &str,
    from: Option<Date>,
    to: Option<Date>,
) -> Result<(i64, HolidaySet)> {
    let row = client
        .query_opt(
            "SELECT id,code,name,country_iso,admin1_code,version FROM core_work_calendar_holiday_sets WHERE tenant_id=$1 AND code=$2",
            &[&tenant, &code],
        )
        .await?
        .ok_or(Error::NotFound)?;
    let id: i64 = row.get(0);
    let (from, to) = (
        from.map(|d| d.to_string())
            .unwrap_or_else(|| "-infinity".into()),
        to.map(|d| d.to_string())
            .unwrap_or_else(|| "infinity".into()),
    );
    let holidays = client
        .query(
            "SELECT date::text,name FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date BETWEEN $3::text::date AND $4::text::date ORDER BY date",
            &[&tenant, &id, &from, &to],
        )
        .await?
        .iter()
        .map(|row| {
            Ok(Holiday {
                date: row.get::<_, &str>(0).parse().map_err(|_| Error::Internal)?,
                name: row.get(1),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let calendars = client
        .query(
            "SELECT c.code FROM core_work_calendar_includes i JOIN core_work_calendars c ON c.tenant_id=i.tenant_id AND c.id=i.calendar_id WHERE i.tenant_id=$1 AND i.holiday_set_id=$2 ORDER BY c.code",
            &[&tenant, &id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    Ok((
        id,
        HolidaySet {
            code: row.get(1),
            name: row.get(2),
            country_iso: row.get(3),
            admin1_code: row.get(4),
            version: row.get(5),
            calendars,
            holidays,
        },
    ))
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
                match name {
                    Some(name) => Prepared::CreateSet {
                        code,
                        name,
                        country_iso,
                        admin1_code,
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
    /// `apply` runs it again under the row lock.
    pub(crate) async fn review(
        &self,
        client: &(impl GenericClient + Sync),
        tenant: i64,
        prepared: &Prepared,
    ) -> WriteResult<Value> {
        let fenced = |stored: i64, version: i64| {
            if stored == version {
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
                if exists(client, "core_work_calendars", tenant, code).await? {
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
                let (_, calendar) = known(calendar(client, tenant, code).await)?;
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
                json!({"summary": format!("Update calendar {code}: {}", if changes.is_empty() { "nothing".into() } else { changes.join(", ") }), "before": calendar.to_json()})
            }
            Prepared::Rule {
                code,
                version,
                effective_from,
                ..
            } => {
                let (_, calendar) = known(calendar(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                let replaces = calendar
                    .rules
                    .iter()
                    .any(|rule| rule.effective_from == *effective_from);
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
                let (_, calendar) = known(calendar(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                if let Some(other) = calendar
                    .exceptions
                    .iter()
                    .find(|e| e.starts_on <= *ends_on && e.ends_on >= *starts_on)
                {
                    return Err(invalid(
                        "starts_on",
                        format!(
                            "overlaps the exception {} ({} to {})",
                            other.name, other.starts_on, other.ends_on
                        ),
                    ));
                }
                json!({"summary": format!("Make {starts_on} to {ends_on} {} on calendar {code}: {name}", day.day_type.as_str())})
            }
            Prepared::RemoveException { code, version, id } => {
                let (_, calendar) = known(calendar(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                let Some(found) = calendar.exceptions.iter().find(|e| e.id == *id) else {
                    return Err(invalid(
                        "exception_id",
                        "is not an exception of this calendar",
                    ));
                };
                json!({"summary": format!("Remove the exception {} ({} to {}) from calendar {code}", found.name, found.starts_on, found.ends_on)})
            }
            Prepared::CreateSet { code, name, .. } => {
                if exists(client, "core_work_calendar_holiday_sets", tenant, code).await? {
                    return Err(invalid("code", "is already used by another holiday set"));
                }
                json!({"summary": format!("Create holiday set {name} ({code})")})
            }
            Prepared::UpdateSet { code, version, .. } => {
                let (_, set) = known(holiday_set(client, tenant, code, None, None).await)?;
                fenced(set.version, *version)?;
                json!({"summary": format!("Update holiday set {code}"), "calendars": set.calendars})
            }
            Prepared::Holidays {
                code,
                version,
                add,
                remove,
            } => {
                let (_, set) = known(holiday_set(client, tenant, code, None, None).await)?;
                fenced(set.version, *version)?;
                json!({"summary": format!("Holiday set {code}: {} added or renamed, {} removed; calendars that include it: {}", add.len(), remove.len(), if set.calendars.is_empty() { "none".into() } else { set.calendars.join(", ") }), "calendars": set.calendars})
            }
            Prepared::Include {
                code,
                version,
                set,
                included,
            } => {
                let (_, calendar) = known(calendar(client, tenant, code).await)?;
                fenced(calendar.version, *version)?;
                if !exists(client, "core_work_calendar_holiday_sets", tenant, set).await? {
                    return Err(invalid(
                        "holiday_set",
                        "is not a holiday set of this tenant",
                    ));
                }
                json!({"summary": format!("{} holiday set {set} {} calendar {code}", if *included { "Include" } else { "Leave out" }, if *included { "in" } else { "of" })})
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
        // Serialize writers of one calendar or set before reviewing.
        match &prepared {
            Prepared::Create { .. } | Prepared::CreateSet { .. } => {
                tx.sql()
                    .execute(
                        "SELECT pg_advisory_xact_lock(91351,$1::integer)",
                        &[&i32::try_from(tenant).map_err(|_| Error::Invalid)?],
                    )
                    .await?;
            }
            Prepared::UpdateSet { code, .. } | Prepared::Holidays { code, .. } => {
                tx.sql()
                    .query_opt("SELECT id FROM core_work_calendar_holiday_sets WHERE tenant_id=$1 AND code=$2 FOR UPDATE", &[&tenant, code])
                    .await?;
            }
            Prepared::Update { code, .. }
            | Prepared::Rule { code, .. }
            | Prepared::AddException { code, .. }
            | Prepared::RemoveException { code, .. }
            | Prepared::Include { code, .. } => {
                tx.sql()
                    .query_opt("SELECT id FROM core_work_calendars WHERE tenant_id=$1 AND code=$2 FOR UPDATE", &[&tenant, code])
                    .await?;
            }
        }
        self.review(tx.sql(), tenant, &prepared).await?;
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
                let (id, before) = known(calendar(tx.sql(), tenant, &code).await)?;
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
                let (id, before) = known(calendar(tx.sql(), tenant, &code).await)?;
                let timezone = timezone.unwrap_or_else(|| {
                    before
                        .rules
                        .iter()
                        .rev()
                        .find(|rule| rule.effective_from <= effective_from)
                        .or(before.rules.first())
                        .map_or("UTC".into(), |rule| rule.timezone.clone())
                });
                let (stored_week, from) = (week.to_stored(), effective_from.to_string());
                changed.push((code.clone(), Some(effective_from), None));
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
                let (id, _) = known(calendar(tx.sql(), tenant, &code).await)?;
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
                let (id, before) = known(calendar(tx.sql(), tenant, &code).await)?;
                let found = before
                    .exceptions
                    .iter()
                    .find(|e| e.id == exception)
                    .ok_or(WriteError::NotFound)?;
                changed.push((code.clone(), Some(found.starts_on), Some(found.ends_on)));
                let old = json!({"calendar": code, "starts_on": found.starts_on.to_string(), "ends_on": found.ends_on.to_string(), "name": found.name, "type": found.day.day_type.as_str()});
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
            } => {
                let reply = json!({"code": code, "name": name, "country_iso": country_iso, "admin1_code": admin1_code});
                let new = reply.clone();
                tx.mutate(move |sql| Box::pin(async move {
                    let id: i64 = sql.query_one("INSERT INTO core_work_calendar_holiday_sets(tenant_id,code,name,country_iso,admin1_code) VALUES($1,$2,$3,$4,$5) RETURNING id", &[&tenant, &code, &name, &country_iso, &admin1_code]).await?.get(0);
                    Ok(((), vec![Change { kind: "core/work_calendar.holiday_set".into(), id, old: json!({}), new }]))
                })).await?;
                reply
            }
            Prepared::UpdateSet {
                code, name, place, ..
            } => {
                let (id, before) = known(holiday_set(tx.sql(), tenant, &code, None, None).await)?;
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
                let (id, before) = known(holiday_set(tx.sql(), tenant, &code, None, None).await)?;
                let touched: Vec<Date> = add
                    .iter()
                    .map(|h| h.date)
                    .chain(remove.iter().copied())
                    .collect();
                if let (Some(first), Some(last)) = (touched.iter().min(), touched.iter().max()) {
                    let last = last
                        .checked_add(crate::resolve::SUBSTITUTE_SEARCH.days())
                        .unwrap_or(*last);
                    for calendar in &before.calendars {
                        changed.push((calendar.clone(), Some(*first), Some(last)));
                    }
                }
                let (added, removed) = (add.len(), remove.len());
                tx.mutate(move |sql| Box::pin(async move {
                    let mut changes = Vec::new();
                    for holiday in &remove {
                        let day = holiday.to_string();
                        if let Some(row) = sql.query_opt("DELETE FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date=$3::text::date RETURNING id,name", &[&tenant, &id, &day]).await? {
                            changes.push(Change { kind: "core/work_calendar.holiday".into(), id: row.get(0), old: json!({"set": code, "date": day, "name": row.get::<_, String>(1)}), new: json!({}) });
                        }
                    }
                    for holiday in &add {
                        let day = holiday.date.to_string();
                        let old = sql.query_opt("SELECT name FROM core_work_calendar_holidays WHERE tenant_id=$1 AND holiday_set_id=$2 AND date=$3::text::date", &[&tenant, &id, &day]).await?.map(|row| json!({"set": code, "date": day, "name": row.get::<_, String>(0)}));
                        let row: i64 = sql.query_one("INSERT INTO core_work_calendar_holidays(tenant_id,holiday_set_id,date,name) VALUES($1,$2,$3::text::date,$4) ON CONFLICT (tenant_id,holiday_set_id,date) DO UPDATE SET name=EXCLUDED.name RETURNING id", &[&tenant, &id, &day, &holiday.name]).await?.get(0);
                        changes.push(Change { kind: "core/work_calendar.holiday".into(), id: row, old: old.unwrap_or(json!({})), new: json!({"set": code, "date": day, "name": holiday.name}) });
                    }
                    sql.execute("UPDATE core_work_calendar_holiday_sets SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                    Ok(((), changes))
                })).await?;
                json!({"added_or_renamed": added, "removed": removed})
            }
            Prepared::Include {
                code,
                set,
                included,
                ..
            } => {
                let (id, before) = known(calendar(tx.sql(), tenant, &code).await)?;
                let (set_id, holidays) =
                    known(holiday_set(tx.sql(), tenant, &set, None, None).await)?;
                let already = before.holiday_sets.contains(&set);
                if already != included {
                    if let (Some(first), Some(last)) =
                        (holidays.holidays.first(), holidays.holidays.last())
                    {
                        let last = last
                            .date
                            .checked_add(crate::resolve::SUBSTITUTE_SEARCH.days())
                            .unwrap_or(last.date);
                        changed.push((code.clone(), Some(first.date), Some(last)));
                    }
                    let facts = json!({"calendar": code, "holiday_set": set});
                    tx.mutate(move |sql| Box::pin(async move {
                        if included {
                            sql.execute("INSERT INTO core_work_calendar_includes(tenant_id,calendar_id,holiday_set_id) VALUES($1,$2,$3)", &[&tenant, &id, &set_id]).await?;
                        } else {
                            sql.execute("DELETE FROM core_work_calendar_includes WHERE tenant_id=$1 AND calendar_id=$2 AND holiday_set_id=$3", &[&tenant, &id, &set_id]).await?;
                        }
                        sql.execute("UPDATE core_work_calendars SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                        let (old, new) = if included { (json!({}), facts) } else { (facts, json!({})) };
                        Ok(((), vec![Change { kind: "core/work_calendar.include".into(), id: set_id, old, new }]))
                    })).await?;
                }
                json!({"code": code, "holiday_set": set, "included": included})
            }
        };
        let trace = tx.context().trace_id.clone();
        for (calendar, from, to) in changed {
            let key = format!("{trace}:{calendar}");
            self.events
                .record(
                    tx,
                    NewEvent {
                        name: CHANGED,
                        subject: &calendar,
                        key: &key,
                        payload: json!({"calendar": calendar, "from": from.map(|d| d.to_string()), "to": to.map(|d| d.to_string())}),
                    },
                )
                .await?;
        }
        Ok(result)
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
async fn today(client: &(impl GenericClient + Sync)) -> Result<Date> {
    client
        .query_one("SELECT CURRENT_DATE::text", &[])
        .await?
        .get::<_, &str>(0)
        .parse()
        .map_err(|_| Error::Internal)
}
async fn exists(
    client: &(impl GenericClient + Sync),
    table: &'static str,
    tenant: i64,
    code: &str,
) -> Result<bool> {
    Ok(client
        .query_opt(
            &format!("SELECT 1 FROM {table} WHERE tenant_id=$1 AND code=$2"),
            &[&tenant, &code],
        )
        .await?
        .is_some())
}
fn known<T>(read: Result<T>) -> WriteResult<T> {
    match read {
        Ok(value) => Ok(value),
        Err(Error::NotFound) => Err(WriteError::NotFound),
        Err(error) => Err(error.into()),
    }
}
