//! Agent operations (ADR 01) over the privacy facade: the same calls the
//! Personal data pages make through their contracts. Agents may enter, verify
//! and refuse requests and manage holds, and approve access requests for their
//! person. An erasure is sent for approval (`base.privacy.erasure.submit`) and
//! decided through Workflow's approval case contract
//! `base.privacy.erasure.decide`, which an independent person's agent may call
//! with that person's Step-up code.
use crate::{
    ConfirmedRecord, HoldTarget, NewRequest, Privacy, RequestKind, RequestView, FIELD_REVEAL,
    HOLD_MANAGE, HOLD_VIEW, REQUEST_APPROVE, REQUEST_RECEIVE, REQUEST_VIEW, RETENTION_VIEW,
    STEP_UP_REVEAL,
};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_personal_data::Identifier;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

static PRIVACY: OnceLock<Arc<Privacy>> = OnceLock::new();
/// Bound once at boot by the `base/privacy.agent` service.
pub(crate) fn bind(privacy: Arc<Privacy>) -> bool {
    PRIVACY.set(privacy).is_ok()
}
fn privacy() -> bilibi_agent_api::Result<&'static Arc<Privacy>> {
    PRIVACY.get().ok_or(Error::Internal.into())
}
fn field_error(field: &str, message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([(field.into(), vec![message.into()])]))
}
fn id(input: &Value, field: &str) -> bilibi_agent_api::Result<i64> {
    input[field]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| field_error(field, "must be a positive integer"))
}
fn text<'a>(input: &'a Value, field: &str) -> bilibi_agent_api::Result<&'a str> {
    input[field]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| field_error(field, "is required"))
}

/// A request as agents and pages read it: never the identifier values.
pub(crate) fn request_json(request: &RequestView) -> Value {
    json!({
        "id": request.id, "kind": request.kind.as_str(), "person_id": request.person.get(),
        "identifiers": request.identifiers.iter().map(|i| match i { Identifier::Email(_) => "email", Identifier::Phone(_) => "phone" }).collect::<Vec<_>>(),
        "company_id": request.company.get(), "status": request.status, "version": request.version,
        "received_by": request.received_by, "received_at": request.received_at, "due_at": request.due_at,
        "verified_by": request.verified_by, "approved_by": request.approved_by, "approved_at": request.approved_at,
        "approval_request": request.approval_request, "approval_awaiting": request.approval_awaiting,
        "refused_reason": request.refused_reason, "completed_at": request.completed_at, "late": request.late,
        "finish_attempts": request.finish_attempts, "finish_error": request.finish_error, "erased": request.erased,
        "confirmed": request.confirmed,
        "has_export": request.export_artifact.is_some(), "has_certificate": request.certificate_artifact.is_some(),
        "stores": request.stores.iter().map(|s| json!({
            "store": s.store, "label": s.label, "state": s.state, "attempts": s.attempts,
            "last_error": s.last_error, "answered_at": s.answered_at, "result": s.result,
        })).collect::<Vec<_>>(),
    })
}

enum Read {
    Requests,
    Request,
    Locate,
    Holds,
    Retention,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/privacy"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let privacy = privacy()?;
            let client = privacy.db.get().await.map_err(Error::from)?;
            let data = match self {
                Self::Requests => {
                    let open = input["open"].as_bool().unwrap_or(false);
                    let search = input["search"].as_str().unwrap_or("");
                    let offset = input["offset"].as_i64().unwrap_or(0);
                    let limit = input["limit"].as_i64().unwrap_or(100);
                    let (rows, total) = privacy
                        .requests(&**client, scope, search, open, offset, limit)
                        .await?;
                    json!({"requests": rows.iter().map(request_json).collect::<Vec<_>>(), "total": total})
                }
                Self::Request => {
                    json!({"request": request_json(&privacy.request(&**client, scope, id(input, "id")?).await?)})
                }
                Self::Locate => {
                    let found = privacy.locate(&client, scope, id(input, "id")?).await?;
                    json!({"stores": found.iter().map(|(store, label, located)| json!({
                        "store": store, "label": label, "kinds": located.kinds,
                        "candidates": located.candidates, "truncated": located.truncated,
                    })).collect::<Vec<_>>()})
                }
                Self::Holds => {
                    let page = privacy
                        .holds(
                            &**client,
                            scope,
                            input["search"].as_str().unwrap_or(""),
                            input["offset"].as_i64().unwrap_or(0),
                            input["limit"].as_i64().unwrap_or(100),
                        )
                        .await?;
                    json!(page)
                }
                Self::Retention => {
                    let company = match input.get("company_id") {
                        Some(_) => Some(
                            OrganisationId::new(id(input, "company_id")?)
                                .map_err(|_| field_error("company_id", "is invalid"))?,
                        ),
                        None => None,
                    };
                    let (company, classes) =
                        privacy.retention_classes(&client, scope, company).await?;
                    json!({"company_id": company.map(OrganisationId::get), "classes": classes})
                }
            };
            Ok(Output::Data(Data::Json(data)))
        })
    }
}

fn kind(input: &Value) -> bilibi_agent_api::Result<RequestKind> {
    match input["kind"].as_str() {
        Some("access") => Ok(RequestKind::Access),
        Some("portability") => Ok(RequestKind::Portability),
        Some("erasure") => Ok(RequestKind::Erasure),
        _ => Err(field_error(
            "kind",
            "must be access, portability or erasure",
        )),
    }
}
fn new_request(input: &Value) -> bilibi_agent_api::Result<NewRequest> {
    let person = PersonId::new(id(input, "person_id")?)
        .map_err(|_| field_error("person_id", "is invalid"))?;
    let company = OrganisationId::new(id(input, "company_id")?)
        .map_err(|_| field_error("company_id", "is invalid"))?;
    let mut identifiers = Vec::new();
    for (field, make) in [
        ("emails", Identifier::Email as fn(String) -> Identifier),
        ("phones", Identifier::Phone),
    ] {
        if let Some(values) = input.get(field) {
            let values = values
                .as_array()
                .ok_or_else(|| field_error(field, "must be a list"))?;
            for value in values {
                identifiers.push(make(
                    value
                        .as_str()
                        .ok_or_else(|| field_error(field, "must be text"))?
                        .to_owned(),
                ));
            }
        }
    }
    Ok(NewRequest {
        kind: kind(input)?,
        person,
        identifiers,
        company,
    })
}
fn hold_target(input: &Value) -> bilibi_agent_api::Result<HoldTarget> {
    match (input.get("person_id"), input.get("record_kind")) {
        (Some(_), None) => Ok(HoldTarget::Person {
            person: PersonId::new(id(input, "person_id")?)
                .map_err(|_| field_error("person_id", "is invalid"))?,
        }),
        (None, Some(_)) => Ok(HoldTarget::Record {
            kind: text(input, "record_kind")?.to_owned(),
            id: id(input, "record_id")?,
        }),
        _ => Err(field_error(
            "person_id",
            "give exactly one of person_id or record_kind with record_id",
        )),
    }
}
fn refusal(error: Error) -> AgentError {
    match error {
        Error::Conflict => field_error("version", "the request or hold changed; read it again"),
        error => error.into(),
    }
}

enum Write {
    Receive,
    Verify,
    Approve,
    Submit,
    Refuse,
    PlaceHold,
    ReleaseHold,
    Reveal,
}
fn reveal_record(input: &Value) -> bilibi_agent_api::Result<Value> {
    match &input["record"] {
        record @ (Value::Number(_) | Value::String(_)) => Ok(record.clone()),
        _ => Err(field_error("record", "must be the record's key")),
    }
}
/// The records the operator confirms, as `base.privacy.request.locate` listed them.
fn confirm(input: &Value) -> bilibi_agent_api::Result<Vec<ConfirmedRecord>> {
    match input.get("confirm") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(list) => serde_json::from_value(list.clone())
            .map_err(|_| field_error("confirm", "must be a list of {store, kind, id}")),
    }
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "base/privacy"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let privacy = privacy()?;
            let client = privacy.db.get().await.map_err(Error::from)?;
            let facts = match self {
                Self::Receive => {
                    let new = new_request(input)?;
                    json!({"summary": format!("Record a {} request for person {}, filed under company {}", new.kind.as_str(), new.person.get(), new.company.get())})
                }
                Self::Verify | Self::Approve | Self::Submit | Self::Refuse => {
                    let request = privacy.request(&**client, scope, id(input, "id")?).await?;
                    let verb = match self {
                        Self::Verify => "Verify",
                        Self::Approve => "Approve and run",
                        Self::Submit => "Send for approval",
                        _ => "Refuse",
                    };
                    json!({"summary": format!("{verb} {} request {} for person {} (now {})", request.kind.as_str(), request.id, request.person.get(), request.status), "request": request_json(&request)})
                }
                Self::PlaceHold => {
                    let target = hold_target(input)?;
                    json!({"summary": format!("Place a legal hold on {}", match &target { HoldTarget::Person { person } => format!("person {person}"), HoldTarget::Record { kind, id } => format!("{kind} {id}") })})
                }
                Self::ReleaseHold => {
                    json!({"summary": format!("Release legal hold {}", id(input, "id")?)})
                }
                Self::Reveal => {
                    json!({"summary": format!("Reveal {}.{} of record {} (recorded with the reason)", text(input, "table")?, text(input, "field")?, reveal_record(input)?), "step_up": [STEP_UP_REVEAL]})
                }
            };
            Ok(facts.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let privacy = privacy()?;
            let version = || {
                input["version"]
                    .as_i64()
                    .ok_or_else(|| field_error("version", "is required"))
            };
            let result = match self {
                Self::Receive => request_json(
                    &privacy
                        .receive_in(tx, scope, new_request(input)?)
                        .await
                        .map_err(refusal)?,
                ),
                Self::Verify => request_json(
                    &privacy
                        .verify_in(
                            tx,
                            scope,
                            id(input, "id")?,
                            version()?,
                            text(input, "verification")?,
                            &confirm(input)?,
                        )
                        .await
                        .map_err(|error| match error {
                            Error::Invalid => field_error(
                                "confirm",
                                "names a record that is not a current candidate, or the note is invalid",
                            ),
                            error => refusal(error),
                        })?,
                ),
                Self::Approve => request_json(
                    &privacy
                        .approve_in(tx, scope, id(input, "id")?, version()?)
                        .await
                        .map_err(refusal)?,
                ),
                Self::Submit => request_json(
                    &privacy
                        .submit_erasure_in(tx, scope, id(input, "id")?, version()?)
                        .await
                        .map_err(refusal)?,
                ),
                Self::Refuse => request_json(
                    &privacy
                        .refuse_in(
                            tx,
                            scope,
                            id(input, "id")?,
                            version()?,
                            text(input, "reason")?,
                        )
                        .await
                        .map_err(refusal)?,
                ),
                Self::PlaceHold => json!(privacy
                    .place_hold_in(tx, scope, hold_target(input)?, text(input, "reason")?)
                    .await
                    .map_err(refusal)?),
                Self::ReleaseHold => json!(privacy
                    .release_hold_in(tx, scope, id(input, "id")?, text(input, "reason")?)
                    .await
                    .map_err(refusal)?),
                Self::Reveal => json!({"value": privacy
                    .reveal_field(scope, text(input, "table")?, text(input, "field")?, &reveal_record(input)?, text(input, "reason")?)
                    .await?}),
            };
            Ok(result.into())
        })
    }
}

fn list_schema() -> Value {
    json!({"type":"object","properties":{
        "open":{"type":"boolean","description":"Only requests not yet completed or refused"},
        "search":{"type":"string","maxLength":100,"description":"A request or person ID, a kind or a status"},
        "offset":{"type":"integer","minimum":0,"description":"Requests to skip; default 0"},
        "limit":{"type":"integer","minimum":1,"maximum":500,"description":"Default 100"}}})
}
fn holds_schema() -> Value {
    json!({"type":"object","properties":{
        "search":{"type":"string","maxLength":100,"description":"A hold, person or record ID, or a record kind"},
        "offset":{"type":"integer","minimum":0,"description":"Holds to skip; default 0"},
        "limit":{"type":"integer","minimum":1,"maximum":500,"description":"Default 100"}}})
}
fn retention_list_schema() -> Value {
    json!({"type":"object","properties":{
        "company_id":{"type":"integer","minimum":1,"description":"The company whose settings to show; default the tenant's primary company"}}})
}
fn id_schema() -> Value {
    json!({"type":"object","required":["id"],"properties":{"id":{"type":"integer","minimum":1}}})
}
fn receive_schema() -> Value {
    json!({"type":"object","required":["kind","person_id","company_id"],"properties":{
        "kind":{"type":"string","enum":["access","portability","erasure"]},
        "person_id":{"type":"integer","minimum":1,"description":"The Core person the request is about"},
        "company_id":{"type":"integer","minimum":1,"description":"The company the export or certificate is filed under"},
        "emails":{"type":"array","items":{"type":"string"},"description":"Emails the requester gave"},
        "phones":{"type":"array","items":{"type":"string"},"description":"Phone numbers the requester gave"}}})
}
fn verify_schema() -> Value {
    json!({"type":"object","required":["id","version","verification"],"properties":{
        "id":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":0},
        "verification":{"type":"string","maxLength":1000,"description":"How the requester's identity was checked"},
        "confirm":{"type":"array","items":{"type":"object","required":["store","kind","id"],"properties":{"store":{"type":"string"},"kind":{"type":"string"},"id":{"type":"string"}}},
            "description":"The candidates from base.privacy.request.locate that are this person's records; only these, and records linked to the person, are acted on"}}})
}
fn approve_schema() -> Value {
    json!({"type":"object","required":["id","version"],"properties":{"id":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":0}}})
}
fn refuse_schema() -> Value {
    json!({"type":"object","required":["id","version","reason"],"properties":{
        "id":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":0},"reason":{"type":"string","maxLength":1000}}})
}
fn place_schema() -> Value {
    json!({"type":"object","required":["reason"],"properties":{
        "person_id":{"type":"integer","minimum":1,"description":"Hold everything about this person (give this or record_kind with record_id)"},
        "record_kind":{"type":"string","maxLength":255,"description":"The record's audit kind, such as person or affiliation"},
        "record_id":{"type":"integer","minimum":1},
        "reason":{"type":"string","maxLength":255,"description":"A case reference, never personal data"}}})
}
fn release_schema() -> Value {
    json!({"type":"object","required":["id","reason"],"properties":{"id":{"type":"integer","minimum":1},"reason":{"type":"string","maxLength":255}}})
}
fn reveal_schema() -> Value {
    json!({"type":"object","required":["table","field","record","reason"],"properties":{
        "table":{"type":"string","maxLength":200,"description":"The Grid table, such as persons"},
        "field":{"type":"string","maxLength":200,"description":"The masked field, such as email"},
        "record":{"type":"integer","description":"The record's key"},
        "reason":{"type":"string","maxLength":255,"description":"A case or ticket reference, recorded with the reveal"}}})
}
fn reveal_output() -> Value {
    json!({"type":"object","description":"The field's full value (null when empty). The reveal is recorded with the reason."})
}
fn retention_output() -> Value {
    json!({"type":"object","description":"Every record class: what starts its clock, purge or anonymise, the company's sweep switch, period and legal minimum (set in System Settings), and the last sweep."})
}
fn requests_output() -> Value {
    json!({"type":"object","description":"Requests newest first: kind, person, status, version, due date, late, and each store's answer (counts, records kept and why, deferrals). Identifier values are never returned."})
}
fn request_output() -> Value {
    json!({"type":"object","description":"One request with each store's answer."})
}
fn locate_output() -> Value {
    json!({"type":"object","description":"Per store and record kind: records linked to the person, and records matched only by an email or phone the request carries, each listed as a candidate to confirm when verifying."})
}
fn holds_output() -> Value {
    json!({"type":"object","description":"Legal holds, active first: target, reason, who placed and released it."})
}

macro_rules! read {
    ($key:literal, $title:literal, $summary:literal, $cap:expr, $input:expr, $output:expr, $handler:expr) => {
        Operation {
            key: $key,
            title: $title,
            summary: $summary,
            keywords: &["privacy", "personal data", "pdpa", "gdpr"],
            capability: $cap,
            input: $input,
            output: $output,
            handler: Handler::Read($handler),
            channels: Channels::Every,
            step_up: &[],
        }
    };
}
macro_rules! write {
    ($key:literal, $title:literal, $summary:literal, $cap:expr, $input:expr, $handler:expr, $step_up:expr) => {
        Operation {
            key: $key,
            title: $title,
            summary: $summary,
            keywords: &[
                "privacy",
                "personal data",
                "pdpa",
                "gdpr",
                "erasure",
                "access request",
            ],
            capability: $cap,
            input: $input,
            output: request_output,
            handler: Handler::SafeWrite($handler),
            channels: Channels::Every,
            step_up: $step_up,
        }
    };
}
inventory::submit! { Contribution::new(DEFINITIONS, "base/privacy", Provenance { module: "base/privacy", source: file!(), line: line!() }, &Definitions {
    operations: &[
        read!("base.privacy.request.list", "List data-subject requests", "This tenant's access, portability and erasure requests with their progress.", REQUEST_VIEW, list_schema, requests_output, &Read::Requests),
        read!("base.privacy.request.get", "Read a data-subject request", "One request and each store's answer.", REQUEST_VIEW, id_schema, request_output, &Read::Request),
        read!("base.privacy.request.locate", "Find a request's personal data", "What every store holds about the request's person, including matches found only by email or phone, which verifying confirms.", REQUEST_VIEW, id_schema, locate_output, &Read::Locate),
        read!("base.privacy.hold.list", "List legal holds", "One page of this tenant's legal holds on people and records, active first, with the total.", HOLD_VIEW, holds_schema, holds_output, &Read::Holds),
        Operation {
            key: "base.privacy.field.reveal",
            title: "Reveal a masked field",
            summary: "The full value of one masked field of one record, with a reason; the reveal is recorded.",
            keywords: &["privacy", "personal data", "mask", "reveal"],
            capability: FIELD_REVEAL,
            input: reveal_schema,
            output: reveal_output,
            handler: Handler::SafeWrite(&Write::Reveal),
            channels: Channels::Every,
            step_up: &[STEP_UP_REVEAL],
        },
        read!("base.privacy.retention.list", "List retention rules", "Every record class with one company's retention settings and the last sweep.", RETENTION_VIEW, retention_list_schema, retention_output, &Read::Retention),
        write!("base.privacy.request.receive", "Record a data-subject request", "Record an access, portability or erasure request for a person; it is due after privacy.request.due_days.", REQUEST_RECEIVE, receive_schema, &Write::Receive, &[]),
        write!("base.privacy.request.verify", "Verify a requester's identity", "Record how the requester's identity was checked and confirm which records found only by email or phone are the person's.", REQUEST_RECEIVE, verify_schema, &Write::Verify, &[]),
        write!("base.privacy.request.approve", "Approve a data-subject request", "Approve and run a verified access or portability request in every store, for a person who can file under its company. An erasure is sent with base.privacy.erasure.submit instead.", REQUEST_APPROVE, approve_schema, &Write::Approve, &[]),
        write!("base.privacy.erasure.submit", "Send an erasure for approval", "Send a verified erasure request to Workflow's approval case privacy.erasure; someone independent of it decides with base.privacy.erasure.decide, and the erasure runs when it is approved.", REQUEST_RECEIVE, approve_schema, &Write::Submit, &[]),
        write!("base.privacy.request.refuse", "Refuse a data-subject request", "Refuse a request that is not yet approved, with the reason.", REQUEST_RECEIVE, refuse_schema, &Write::Refuse, &[]),
        write!("base.privacy.hold.place", "Place a legal hold", "Stop erasure and retention sweeps for a person or one record.", HOLD_MANAGE, place_schema, &Write::PlaceHold, &[]),
        write!("base.privacy.hold.release", "Release a legal hold", "Release an active legal hold; the next erasure or sweep acts on what it kept.", HOLD_MANAGE, release_schema, &Write::ReleaseHold, &[]),
    ],
    guides: &[Guide { key: "base.privacy", title: "Data-subject requests", summary: "Answer a person's access or erasure request in every store.", keywords: &["privacy", "pdpa", "gdpr", "erasure", "access request", "legal hold"], capability: REQUEST_VIEW,
        body: "Record the request with base.privacy.request.receive (kind, person_id, company_id, and the emails or phones the person gave). Read base.privacy.request.locate to see what every store holds; records found only by email or phone are listed as candidates. Record how the person's identity was checked with base.privacy.request.verify, passing in confirm only the candidates that are this person's; nothing else found only by an identifier is touched. A person with privacy.request.approve, or you as their agent, then approves an access or portability request with base.privacy.request.approve. An erasure is sent for approval with base.privacy.erasure.submit and decided on Waiting for my approval or with base.privacy.erasure.decide, by someone independent of it: not the data subject or any account linked to them, not whoever received or verified it, and never while impersonating. Each decision asks for that person's Step-up code: when a commit answers step_up_required, ask them for their current authenticator code. A rejected or recalled erasure returns to whoever received it, to send again or refuse. Running answers each store in the background; read base.privacy.request.get until status is completed. A store that fails stays pending and the request shows late. Legal holds (base.privacy.hold.place) keep what they name through erasure; the certificate lists every store, what was kept and why." }],
}) }
