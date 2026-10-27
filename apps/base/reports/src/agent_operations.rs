//! The report operation contracts (ADR 01): reads run as the caller, writes
//! are reviewed then committed in the caller's audited transaction. Pages
//! reach the same contracts. No Bilimbi counterpart.
use crate::{
    definition, Definition, Draft, FilterDef, Format, Frequency, Kind, Plan, ReportError, Reports,
    RunInput, AUTHOR, EXPORT, SCHEDULE, SHARE, VIEW,
};
use bilibi_agent_api::{
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

static BOUND: OnceLock<Arc<Reports>> = OnceLock::new();
pub(crate) fn bind(reports: Arc<Reports>) -> bilibi_module_registry::Result<()> {
    BOUND
        .set(reports)
        .map_err(|_| RegistryError::Service("duplicate reports binding".into()))
}
/// The installed facade, for callers outside a request (the dashboard
/// provider), once the service exists.
pub(crate) fn facade() -> Option<&'static Arc<Reports>> {
    BOUND.get()
}
fn bound() -> Result<&'static Arc<Reports>, AgentError> {
    BOUND.get().ok_or(AgentError::Backend(Error::Internal))
}

impl From<ReportError> for AgentError {
    fn from(error: ReportError) -> Self {
        match error {
            ReportError::Invalid(errors) => AgentError::InvalidInput(errors),
            ReportError::NotFound => AgentError::NotFound,
            ReportError::Forbidden => AgentError::Forbidden,
            ReportError::Conflict => AgentError::StalePlan,
            ReportError::Database(error) => AgentError::Backend(error),
            other => AgentError::InvalidInput(BTreeMap::from([(
                "report".into(),
                vec![other.to_string()],
            )])),
        }
    }
}

fn invalid(field: &str, message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([(field.into(), vec![message.into()])]))
}
fn id(input: &Value) -> Result<i64, AgentError> {
    input["id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| invalid("id", "is required"))
}
fn version(input: &Value) -> Result<i64, AgentError> {
    input["version"]
        .as_i64()
        .filter(|v| *v > 0)
        .ok_or_else(|| invalid("version", "is required"))
}
fn draft(input: &Value) -> Result<Draft, AgentError> {
    let text = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
    let kind = Kind::from_key(input["kind"].as_str().unwrap_or_default())
        .ok_or_else(|| invalid("kind", "must be rows or summary"))?;
    let definition =
        Definition::from_input(&input["definition"]).map_err(AgentError::InvalidInput)?;
    Ok(Draft {
        name: text("name"),
        description: text("description"),
        record_type: text("record_type"),
        kind,
        definition,
    })
}
/// `answers` as `{filter, value | values | min/max}` items, by filter position.
fn run_input(input: &Value) -> Result<RunInput, AgentError> {
    let mut answers = BTreeMap::new();
    let given = input["answers"].as_array().map_or(0, Vec::len);
    if given > bilibi_grid::MAX_FILTERS {
        return Err(invalid("answers", "takes at most one answer per filter"));
    }
    for answer in input["answers"].as_array().into_iter().flatten() {
        let position = answer["filter"]
            .as_u64()
            .and_then(|p| usize::try_from(p).ok())
            .ok_or_else(|| invalid("answers", "each answer names its filter position"))?;
        let value = if let Some(values) = answer.get("values") {
            values.clone()
        } else if answer.get("min").is_some() || answer.get("max").is_some() {
            json!({"min": answer.get("min").cloned().unwrap_or(Value::Null), "max": answer.get("max").cloned().unwrap_or(Value::Null)})
        } else {
            answer.get("value").cloned().unwrap_or(Value::Null)
        };
        answers.insert(position, value);
    }
    // The browser's drill filters: a group's value, an empty group, or a
    // date bucket's first and last day (`min`/`max`, as answers write them).
    let drill = match input.get("drill").and_then(Value::as_array) {
        Some(items) => items
            .iter()
            .map(|item| {
                let column = item["column"].as_str().unwrap_or_default().to_owned();
                let value = match item["test"].as_str() {
                    Some("equals") => item["value"].clone(),
                    Some("empty") => Value::Null,
                    Some("range") => json!({"min": item["min"], "max": item["max"]}),
                    _ => return Err(invalid("drill", "test must be equals, empty or range")),
                };
                Ok(FilterDef {
                    column,
                    test: item["test"].as_str().unwrap_or_default().to_owned(),
                    value,
                    ask: false,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        None if input.get("drill").is_some() => {
            return Err(invalid("drill", "must be a list of filters"))
        }
        None => Vec::new(),
    };
    if drill.len() > 2 {
        return Err(invalid("drill", "takes at most two filters"));
    }
    let page = input["page"]
        .as_u64()
        .and_then(|p| usize::try_from(p).ok())
        .unwrap_or(0);
    Ok(RunInput {
        answers,
        drill,
        page,
        as_rows: input["as_rows"].as_bool() == Some(true),
    })
}
fn listed(report: &crate::Report) -> Value {
    match report.builtin {
        Some(key) => {
            json!({"builtin": key, "name": report.name, "description": report.description, "record_type": report.record_type, "kind": report.kind.key(), "owned": false})
        }
        None => {
            json!({"id": report.id, "name": report.name, "description": report.description, "record_type": report.record_type, "kind": report.kind.key(), "owned": report.owned, "version": report.version, "updated_at": report.updated_at})
        }
    }
}
/// A built-in report's key, when the input names one instead of an id.
fn builtin(input: &Value) -> Option<&str> {
    input["builtin"].as_str().filter(|key| !key.is_empty())
}

enum Read {
    List,
    Get,
    Run,
    Fields,
    Exports,
    Subscriptions,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/reports"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let reports = bound()?;
            let value = match self {
                Self::List => {
                    let list = reports.list(scope).await?;
                    let builtins = reports.builtins(scope).await?;
                    json!({"reports": list.iter().map(listed).collect::<Vec<_>>(), "built_in": builtins.iter().map(listed).collect::<Vec<_>>()})
                }
                Self::Get => {
                    let report = match builtin(input) {
                        Some(key) => reports.builtin(scope, key).await?,
                        None => reports.get(scope, id(input)?).await?,
                    };
                    serde_json::to_value(report).map_err(|_| Error::Internal)?
                }
                Self::Subscriptions => {
                    json!({"subscriptions": reports.subscriptions(scope, id(input)?).await?})
                }
                Self::Exports => {
                    let report = input["id"].as_i64().filter(|id| *id > 0);
                    json!({"exports": reports.exports(scope, report).await?})
                }
                Self::Fields => match input["record_type"].as_str().filter(|t| !t.is_empty()) {
                    None => {
                        let types = reports.record_types(scope).await?;
                        json!({"record_types": types.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>()})
                    }
                    Some(record_type) => {
                        json!({"record_type": record_type, "columns": reports.columns(scope, record_type).await?})
                    }
                },
                Self::Run => {
                    let run = match builtin(input) {
                        Some(key) => reports.run_builtin(scope, key, &run_input(input)?).await?,
                        None => reports.run(scope, id(input)?, &run_input(input)?).await?,
                    };
                    serde_json::to_value(run).map_err(|_| Error::Internal)?
                }
            };
            Ok(value.into())
        })
    }
}

enum Write {
    Create,
    Update,
    Copy,
    Share,
    Delete,
    PrepareExport,
    Subscribe,
    Pause,
    Unsubscribe,
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "base/reports"
    }
    /// The review: what will be saved, checked against the caller's catalog,
    /// and the version a later commit must still match.
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let reports = bound()?;
            let value = match self {
                Self::Create => {
                    let draft = draft(input)?;
                    reports.check(scope, &draft).await?;
                    json!({"summary": format!("Save the new report {}", draft.name.trim()), "report": crate::after(&draft)})
                }
                Self::Update => {
                    let draft = draft(input)?;
                    let current = reports.get(scope, id(input)?).await?;
                    if !current.owned {
                        return Err(ReportError::NotOwner.into());
                    }
                    reports.check(scope, &draft).await?;
                    json!({"summary": format!("Change the report {}", current.name), "version": current.version, "before": crate::snapshot(&current), "after": crate::after(&draft)})
                }
                Self::Copy => {
                    let current = match builtin(input) {
                        Some(key) => reports.builtin(scope, key).await?,
                        None => reports.get(scope, id(input)?).await?,
                    };
                    let draft = crate::copy_of(&current, input["name"].as_str());
                    reports.check(scope, &draft).await?;
                    json!({"summary": format!("Save a copy of {} as {}", current.name, draft.name), "report": crate::after(&draft)})
                }
                Self::Share => {
                    let current = reports.get(scope, id(input)?).await?;
                    if !current.owned {
                        return Err(ReportError::NotOwner.into());
                    }
                    json!({"summary": format!("Change who can run {}", current.name), "version": current.version,
                        "before": {"shared_with_everyone": current.shared_with_everyone, "roles": current.role_ids},
                        "after": {"shared_with_everyone": input["everyone"].as_bool() == Some(true), "roles": input["roles"]}})
                }
                Self::Delete => {
                    let current = reports.get(scope, id(input)?).await?;
                    if !current.owned {
                        return Err(ReportError::NotOwner.into());
                    }
                    json!({"summary": format!("Delete the report {}; people it is shared with lose it too", current.name), "version": current.version})
                }
                Self::Subscribe => {
                    let current = reports.get(scope, id(input)?).await?;
                    let plan = plan(input)?;
                    json!({"summary": format!("Deliver {} {} at {} as {}{}", current.name, plan.frequency.key(), clock(plan.at_minute), plan.format.key().to_uppercase(), if plan.roles.is_empty() { String::new() } else { format!(", also to {} role(s)", plan.roles.len()) })})
                }
                Self::Pause => {
                    let paused = input["paused"].as_bool() == Some(true);
                    json!({"summary": if paused { "Pause this scheduled report; deliveries not started are cancelled" } else { "Resume this scheduled report" }})
                }
                Self::Unsubscribe => {
                    json!({"summary": "Delete this scheduled report and its delivery history"})
                }
                Self::PrepareExport => {
                    let current = reports.get(scope, id(input)?).await?;
                    let format = export_format(input)?;
                    json!({"summary": format!("Prepare {} as {} in the background; you get a notice when it is ready", current.name, format.key().to_uppercase())})
                }
            };
            Ok(value.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let reports = bound()?;
            let value = match self {
                Self::Create => json!({"id": reports.create(tx, scope, &draft(input)?).await?}),
                Self::Update => {
                    let id = id(input)?;
                    reports
                        .update(tx, scope, id, version(input)?, &draft(input)?)
                        .await?;
                    json!({"id": id})
                }
                Self::Copy => {
                    let copy = match builtin(input) {
                        Some(key) => {
                            reports
                                .copy_builtin(tx, scope, key, input["name"].as_str())
                                .await?
                        }
                        None => {
                            reports
                                .copy(tx, scope, id(input)?, input["name"].as_str())
                                .await?
                        }
                    };
                    json!({"id": copy})
                }
                Self::Share => {
                    let id = id(input)?;
                    let roles: Vec<i64> = input["roles"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_i64)
                        .collect();
                    reports
                        .share(
                            tx,
                            scope,
                            id,
                            version(input)?,
                            input["everyone"].as_bool() == Some(true),
                            &roles,
                        )
                        .await?;
                    json!({"id": id})
                }
                Self::Delete => {
                    let id = id(input)?;
                    reports.delete(tx, scope, id, version(input)?).await?;
                    json!({"id": id, "deleted": true})
                }
                Self::Subscribe => {
                    let subscription = reports
                        .subscribe(tx, scope, id(input)?, &plan(input)?)
                        .await?;
                    json!({"subscription": subscription})
                }
                Self::Pause => {
                    let subscription = subscription_id(input)?;
                    reports
                        .pause(
                            tx,
                            scope,
                            subscription,
                            version(input)?,
                            input["paused"].as_bool() == Some(true),
                        )
                        .await?;
                    json!({"subscription": subscription})
                }
                Self::Unsubscribe => {
                    let subscription = subscription_id(input)?;
                    reports
                        .unsubscribe(tx, scope, subscription, version(input)?)
                        .await?;
                    json!({"subscription": subscription, "deleted": true})
                }
                Self::PrepareExport => {
                    let answers = run_input(input)?.answers;
                    let export = reports
                        .prepare_export(tx, scope, id(input)?, &answers, export_format(input)?)
                        .await?;
                    json!({"export": export, "state": "queued", "next": "Read base.reports.exports for its state; a ready one has a path to download."})
                }
            };
            Ok(value.into())
        })
    }
}

fn empty_schema() -> Value {
    json!({"type":"object","properties":{}})
}
fn id_schema() -> Value {
    json!({"type":"object","required":["id"],"properties":{"id":{"type":"integer","minimum":1,"description":"The report's id from base.reports.list."}}})
}
fn fields_schema() -> Value {
    json!({"type":"object","properties":{"record_type":{"type":"string","maxLength":64,"description":"Left out: the record types you can report on. Given: that type's columns."}}})
}
fn fields_output() -> Value {
    json!({"type":"object","description":"record_types (id, label); or, for one record_type, columns (spec, label, ty): the column paths a definition may use, own fields first, then fields one link away like group.name."})
}
fn report_schema() -> Value {
    json!({"type":"object","properties":{
        "id":{"type":"integer","minimum":1,"description":"A saved report's id from base.reports.list."},
        "builtin":{"type":"string","maxLength":64,"description":"Instead of id: a built-in report's key from base.reports.list."}
    }})
}
fn run_schema() -> Value {
    json!({"type":"object","properties":{
        "id":{"type":"integer","minimum":1},
        "builtin":{"type":"string","maxLength":64,"description":"Instead of id: a built-in report's key."},
        "answers":{"type":"array","description":"Values for the report's ask-when-run filters, by the filter's position in the definition (from 0). Left out: the stored default.","items":{"type":"object","required":["filter"],"properties":{
            "filter":{"type":"integer","minimum":0,"maximum":15},
            "value":{"type":"string","maxLength":256},
            "values":{"type":"array","items":{"type":"string","maxLength":256}},
            "min":{"type":"string","maxLength":32},
            "max":{"type":"string","maxLength":32}
        }}},
        "drill":{"type":"array","description":"Up to two filters that narrow to one group of a summary, as the page drills: {column, test: equals, value} for a group's value, {column, test: empty} for the empty group, or {column, test: range, min, max} with YYYY-MM-DD days for a date bucket (a month is its first and last day).","items":{"type":"object","required":["column","test"],"properties":{
            "column":{"type":"string","maxLength":160},"test":{"type":"string","enum":["equals","empty","range"]},"value":{"type":"string","maxLength":256},
            "min":{"type":"string","maxLength":32},"max":{"type":"string","maxLength":32}
        }}},
        "page":{"type":"integer","minimum":0,"maximum":1000,"description":"A list report's page, from 0."},
        "as_rows":{"type":"boolean","description":"For a summary or matrix: list the records behind it (with drill, the records of one group) page by page instead."}
    }})
}
fn draft_schema(with_id: bool) -> Value {
    let mut schema = json!({"type":"object","required":["name","record_type","kind","definition"],"properties":{
        "name":{"type":"string","maxLength":120},
        "description":{"type":"string","maxLength":1000},
        "record_type":{"type":"string","maxLength":64,"description":"A Grid table id the caller can read, such as the ones base.grid lists."},
        "kind":{"type":"string","enum":["rows","summary","matrix"]},
        "definition":definition::schema()
    }});
    if with_id {
        schema["required"] = json!(["id", "version", "name", "record_type", "kind", "definition"]);
        schema["properties"]["id"] = json!({"type":"integer","minimum":1});
        schema["properties"]["version"] = json!({"type":"integer","minimum":1,"description":"The version base.reports.get returned."});
    }
    schema
}
fn update_schema() -> Value {
    draft_schema(true)
}
fn create_schema() -> Value {
    draft_schema(false)
}
fn subscription_id(input: &Value) -> Result<i64, AgentError> {
    input["subscription"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| invalid("subscription", "is required"))
}
fn clock(minute: i16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}
/// A schedule from `frequency`, `at` (HH:MM in your time zone), `weekday`
/// (1 Monday … 7 Sunday) or `monthday` (1–28), `format`, `include_me`,
/// `skip_empty` and `roles`.
pub(crate) fn plan(input: &Value) -> Result<Plan, AgentError> {
    let frequency = Frequency::from_key(input["frequency"].as_str().unwrap_or_default())
        .ok_or_else(|| invalid("frequency", "must be daily, weekly or monthly"))?;
    let at = input["at"].as_str().unwrap_or_default();
    let at_minute = at
        .split_once(':')
        .and_then(|(h, m)| Some((h.parse::<i16>().ok()?, m.parse::<i16>().ok()?)))
        .filter(|(h, m)| (0..24).contains(h) && (0..60).contains(m))
        .map(|(h, m)| h * 60 + m)
        .ok_or_else(|| invalid("at", "must be a time like 08:30"))?;
    let day = |key: &str| input[key].as_i64().and_then(|d| i16::try_from(d).ok());
    Ok(Plan {
        frequency,
        at_minute,
        weekday: (frequency == Frequency::Weekly)
            .then(|| day("weekday"))
            .flatten(),
        monthday: (frequency == Frequency::Monthly)
            .then(|| day("monthday"))
            .flatten(),
        format: export_format(input)?,
        include_owner: input["include_me"].as_bool() != Some(false),
        skip_empty: input["skip_empty"].as_bool() != Some(false),
        roles: input["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect(),
    })
}
fn subscribe_schema() -> Value {
    json!({"type":"object","required":["id","frequency","at"],"properties":{
        "id":{"type":"integer","minimum":1,"description":"The report to deliver."},
        "frequency":{"type":"string","enum":["daily","weekly","monthly"]},
        "at":{"type":"string","maxLength":5,"description":"Time of day in your time zone, HH:MM."},
        "weekday":{"type":"integer","minimum":1,"maximum":7,"description":"Weekly: 1 Monday … 7 Sunday."},
        "monthday":{"type":"integer","minimum":1,"maximum":28,"description":"Monthly: the day of the month."},
        "format":{"type":"string","enum":["csv","pdf"]},
        "include_me":{"type":"boolean","description":"Left out: true."},
        "skip_empty":{"type":"boolean","description":"Left out: true, so an empty result sends nothing."},
        "roles":{"type":"array","items":{"type":"integer","minimum":1},"description":"Role ids whose holders also receive it, each with their own access. Needs reports.share."}
    }})
}
fn pause_schema() -> Value {
    json!({"type":"object","required":["subscription","version","paused"],"properties":{"subscription":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":1},"paused":{"type":"boolean"}}})
}
fn unsubscribe_schema() -> Value {
    json!({"type":"object","required":["subscription","version"],"properties":{"subscription":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":1}}})
}
fn subscriptions_output() -> Value {
    json!({"type":"object","description":"subscriptions to the report that are yours: id, frequency, at_minute, weekday, monthday, timezone, format, include_owner, skip_empty, paused, roles, next_run_at, last_run_at, last_state and last_reason (your latest delivery), version."})
}
fn export_format(input: &Value) -> Result<Format, AgentError> {
    Format::from_key(input["format"].as_str().unwrap_or("csv"))
        .ok_or_else(|| invalid("format", "must be csv or pdf"))
}
fn prepare_schema() -> Value {
    let mut schema = run_schema();
    schema["required"] = json!(["id"]);
    if let Some(properties) = schema["properties"].as_object_mut() {
        properties.remove("builtin");
    }
    schema["properties"]["format"] =
        json!({"type":"string","enum":["csv","pdf"],"description":"Left out: csv."});
    schema
}
fn exports_schema() -> Value {
    json!({"type":"object","properties":{"id":{"type":"integer","minimum":1,"description":"A report id to list only its exports."}}})
}
fn exports_output() -> Value {
    json!({"type":"object","description":"exports, newest first: id, report_id, report_name, format, state (queued, ready, failed, expired), filename, rows, message (why it failed), created_at, expires_at, path (a ready one's download address, for you only)."})
}
fn copy_schema() -> Value {
    json!({"type":"object","properties":{"id":{"type":"integer","minimum":1},"builtin":{"type":"string","maxLength":64,"description":"Instead of id: a built-in report's key."},"name":{"type":"string","maxLength":120,"description":"Left out: Copy of the original name."}}})
}
fn share_schema() -> Value {
    json!({"type":"object","required":["id","version"],"properties":{
        "id":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":1},
        "everyone":{"type":"boolean","description":"true lets everyone in the company with reports.view run it."},
        "roles":{"type":"array","items":{"type":"integer","minimum":1},"description":"Role ids whose holders may run it. Replaces the current list; empty shares with no role."}
    }})
}
fn version_schema() -> Value {
    json!({"type":"object","required":["id","version"],"properties":{"id":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":1}}})
}
fn list_output() -> Value {
    json!({"type":"object","description":"reports: id, name, description, record_type, kind (rows or summary), owned (you may change it), version, updated_at."})
}
fn get_output() -> Value {
    json!({"type":"object","description":"The report: id, owner_id, owned, name, description, record_type, kind, definition (columns, filters, groupings, measures, sort, chart), shared_with_everyone, role_ids, version, updated_at."})
}
fn run_output() -> Value {
    json!({"type":"object","description":"report; left_out (column paths you cannot see, dropped from the result); timezone; kind. Rows: columns (spec, label, ty), rows as [key, cells], total, page, page_size. Summary: groupings as [column, bucket], measures as [column, agg], lines as [keys, subtotal, rows, values], total as [rows, values], chart."})
}
fn write_output() -> Value {
    json!({"type":"object","description":"The review: summary and the before/after facts, with the version the commit must match. The commit answers the report id."})
}

const GUIDE: &str = "A report is a saved question over one record type, never saved data: every run reads as the person running it, so they see only the rows and fields their own access allows. Read base.reports.list (your reports, and built_in ones modules supply, which you can run and copy but not change), then base.reports.get for a definition and base.reports.run for its result. A rows report lists columns page by page; a summary groups by up to two columns (a date column by day, week, month, quarter or year, in the company's time zone) and totals measures (count, sum, avg, min, max) with subtotals. Filters test a column with equals, one_of, contains, range, within (a period like this_month), empty or not_empty; a filter marked ask takes the reader's value at run time through answers. Columns the reader cannot see are left out and listed in left_out; a filter on such a column refuses the run. Build with base.reports.create: base.reports.fields lists the record types and each one's column paths, change your own with update, share them with share, and copy anyone's you can run. Export a whole result with base.reports.prepare_export (csv or pdf); it runs as you in the background and base.reports.exports gives its download path when ready. Have a report delivered on a schedule with base.reports.subscribe; each recipient gets the result their own access allows. Every write is reviewed first; update, share and delete take the version from get.";

inventory::submit! { Contribution::new(DEFINITIONS, "base/reports", Provenance { module: "base/reports", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key:"base.reports.list", title:"Reports", summary:"List the reports you can run: your own and those shared with you.", keywords:&["report","reports","list","saved"], capability:VIEW, input:empty_schema, output:list_output, handler:Handler::Read(&Read::List), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.get", title:"Report definition", summary:"Read one report's definition: record type, columns, filters, groupings, measures, sort and chart.", keywords:&["report","definition","get"], capability:VIEW, input:report_schema, output:get_output, handler:Handler::Read(&Read::Get), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.fields", title:"Report fields", summary:"The record types you can report on, or one type's columns with their paths and types, for building a report.", keywords:&["report","fields","columns","record","type"], capability:VIEW, input:fields_schema, output:fields_output, handler:Handler::Read(&Read::Fields), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.run", title:"Run a report", summary:"Run a report as yourself: a page of rows or a grouped summary with totals, answering its ask-when-run filters.", keywords:&["report","run","summary","total","group"], capability:VIEW, input:run_schema, output:run_output, handler:Handler::Read(&Read::Run), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.create", title:"Build a report", summary:"Save a new report over one record type. Review the definition first.", keywords:&["report","create","build","new"], capability:AUTHOR, input:create_schema, output:write_output, handler:Handler::SafeWrite(&Write::Create), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.update", title:"Change a report", summary:"Replace your own report's name, description and definition at its version.", keywords:&["report","update","change","edit"], capability:AUTHOR, input:update_schema, output:write_output, handler:Handler::SafeWrite(&Write::Update), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.copy", title:"Copy a report", summary:"Save your own copy of a report you can run, to change it.", keywords:&["report","copy","duplicate"], capability:AUTHOR, input:copy_schema, output:write_output, handler:Handler::SafeWrite(&Write::Copy), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.share", title:"Share a report", summary:"Choose who may run your report: everyone, chosen roles, or only you. They run it with their own access.", keywords:&["report","share","role","everyone"], capability:SHARE, input:share_schema, output:write_output, handler:Handler::SafeWrite(&Write::Share), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.exports", title:"Your report exports", summary:"List the exports you asked for, their state, and where a ready one downloads.", keywords:&["report","export","download","csv","pdf"], capability:VIEW, input:exports_schema, output:exports_output, handler:Handler::Read(&Read::Exports), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.prepare_export", title:"Export a report", summary:"Prepare a report's whole result as CSV or PDF in the background, run as you; the bell says when it is ready.", keywords:&["report","export","csv","pdf","download"], capability:EXPORT, input:prepare_schema, output:write_output, handler:Handler::SafeWrite(&Write::PrepareExport), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.subscriptions", title:"Your report schedules", summary:"List your scheduled deliveries of a report, when they next run and how the last went.", keywords:&["report","schedule","subscription","deliver","email"], capability:VIEW, input:id_schema, output:subscriptions_output, handler:Handler::Read(&Read::Subscriptions), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.subscribe", title:"Schedule a report", summary:"Have a report delivered daily, weekly or monthly to you and, optionally, to roles; each recipient gets their own result by bell and email link.", keywords:&["report","schedule","subscribe","deliver","email"], capability:SCHEDULE, input:subscribe_schema, output:write_output, handler:Handler::SafeWrite(&Write::Subscribe), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.pause_subscription", title:"Pause or resume a report schedule", summary:"Pause a scheduled report (cancelling deliveries not started) or resume it.", keywords:&["report","schedule","pause","resume"], capability:SCHEDULE, input:pause_schema, output:write_output, handler:Handler::SafeWrite(&Write::Pause), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.unsubscribe", title:"Delete a report schedule", summary:"Stop and delete one of your scheduled reports.", keywords:&["report","schedule","unsubscribe","delete"], capability:SCHEDULE, input:unsubscribe_schema, output:write_output, handler:Handler::SafeWrite(&Write::Unsubscribe), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.reports.delete", title:"Delete a report", summary:"Delete your own report; it disappears for everyone it was shared with.", keywords:&["report","delete","remove"], capability:AUTHOR, input:version_schema, output:write_output, handler:Handler::SafeWrite(&Write::Delete), channels:Channels::Every, step_up:&[] },
    ],
    guides: &[Guide { key:"base.reports", title:"Build and run reports", summary:"Saved questions over records, run with each reader's own access.", keywords:&["report","reports","summary","chart","export"], capability:VIEW, body:GUIDE }],
}) }
