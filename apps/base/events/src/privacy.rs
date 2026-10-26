//! Events' personal-data store (ADR 05, `base/personal_data`): recorded event
//! payloads. A publisher may declare, per event type, where its payload names
//! the person (a JSON pointer to the person ID) and which payload fields are
//! personal (`PersonalEvent`). Events linked to the person that way are found
//! and erased field by field. Any other event that holds a requester's email,
//! or a phone's digits however formatted, is only a candidate for the operator
//! to confirm; a confirmed event of an undeclared type has its whole payload
//! replaced, once every subscriber has received it. Subscribers keep their own
//! copies and answer for them through their own stores. No Bilimbi counterpart.
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_module_registry::{
    Catalog, Consumer, Contribution, Entry, Provenance, RegistryError, Result as RegistryResult,
};
use bilibi_personal_data::{
    Action, Candidate, Disclosure, Erased, Erasure, Exported, Handler, HandlerFuture, Holds, Kept,
    KindCount, Located, PersonalData, RecordKind, Subject, ERASED, MAX_CANDIDATES, PERSONAL_DATA,
    RECORD_KINDS,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};
use tokio_postgres::Client;

const KIND: &str = "base/events.event";
const PAGE: i64 = 1000;
/// More linked events than this is an export too large to bundle.
const MAX_EXPORT: usize = 100_000;

/// The catalog a publisher declares an event type's personal data in:
/// `Contribution::new(PERSONAL_EVENTS, "<event name>", provenance, &PersonalEvent { .. })`.
pub const PERSONAL_EVENTS: &str = "base/events.personal";
/// Where one event type's payload holds personal data. Pointers are JSON
/// pointers into the payload (`/customer/email`).
pub struct PersonalEvent {
    /// The person the event is about, as a person ID number or string.
    pub person: Option<&'static str>,
    /// The personal fields erasure replaces with `[erased]`.
    pub fields: &'static [&'static str],
}

fn pointer_ok(pointer: &str) -> bool {
    pointer.starts_with('/') && pointer.len() <= 255 && !pointer.chars().any(char::is_control)
}
fn validate(entries: &[Entry]) -> RegistryResult<()> {
    for entry in entries {
        let declared = entry.get::<PersonalEvent>()?;
        if !entry
            .id
            .strip_prefix(entry.provenance.module)
            .is_some_and(|rest| rest.starts_with('.'))
            || declared.person.is_some_and(|p| !pointer_ok(p))
            || declared.fields.is_empty()
            || declared.fields.len() > 64
            || !declared.fields.iter().all(|f| pointer_ok(f))
        {
            return Err(RegistryError::Contribution(format!(
                "personal event {} at {}:{} must name one of {}'s own events and list 1-64 JSON pointers",
                entry.id, entry.provenance.source, entry.provenance.line, entry.provenance.module
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<PersonalEvent>(PERSONAL_EVENTS, "base/events", validate) }

type Declared = BTreeMap<&'static str, &'static PersonalEvent>;
fn declared() -> Result<&'static Declared> {
    static DECLARED: OnceLock<Arc<Declared>> = OnceLock::new();
    if let Some(declared) = DECLARED.get() {
        return Ok(declared);
    }
    let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
    let mut map = BTreeMap::new();
    for entry in catalog.entries::<PersonalEvent>(PERSONAL_EVENTS)? {
        map.insert(entry.id, entry.get::<PersonalEvent>()?);
    }
    Ok(DECLARED.get_or_init(|| Arc::new(map)))
}
/// `/a/b` as a PostgreSQL text path `{a,b}`.
fn path(pointer: &str) -> Vec<String> {
    pointer
        .trim_start_matches('/')
        .split('/')
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect()
}

/// IDs of the tenant's events linked to the person by a declared pointer,
/// above `after`, at most `limit`.
async fn linked(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    subject: &Subject,
    after: i64,
    limit: i64,
) -> Result<Vec<(i64, String)>> {
    let mut found = Vec::new();
    for (name, declaration) in declared()? {
        let Some(pointer) = declaration.person else {
            continue;
        };
        let rows = client
            .query(
                "SELECT id,name FROM base_events WHERE tenant_id=$1 AND name=$2 AND payload #>> $3::text[] = $4 AND id>$5 ORDER BY id LIMIT $6",
                &[&scope.tenant(), name, &path(pointer), &subject.person.get().to_string(), &after, &limit],
            )
            .await?;
        found.extend(rows.iter().map(|row| (row.get(0), row.get(1))));
    }
    found.sort_unstable();
    found.truncate(limit as usize);
    Ok(found)
}

struct Payloads;
static PAYLOADS: Payloads = Payloads;
impl Handler for Payloads {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located> {
        Box::pin(async move {
            let mut by_person = 0;
            for (name, declaration) in declared()? {
                let Some(pointer) = declaration.person else {
                    continue;
                };
                by_person += client
                    .query_one(
                        "SELECT count(*) FROM base_events WHERE tenant_id=$1 AND name=$2 AND payload #>> $3::text[] = $4",
                        &[&scope.tenant(), name, &path(pointer), &subject.person.get().to_string()],
                    )
                    .await?
                    .get::<_, i64>(0);
            }
            let (emails, phones) = subject.patterns();
            let (mut by_identifier, mut candidates) = (0, Vec::new());
            if !emails.is_empty() || !phones.is_empty() {
                let matching = "tenant_id=$1 AND (payload::text ILIKE ANY($2) OR regexp_replace(payload::text,'[^0-9]','','g') LIKE ANY($3))";
                by_identifier = client
                    .query_one(
                        &format!("SELECT count(*) FROM base_events WHERE {matching}"),
                        &[&scope.tenant(), &emails, &phones],
                    )
                    .await?
                    .get(0);
                candidates = client
                    .query(
                        &format!("SELECT id,name,to_char(recorded_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI') FROM base_events WHERE {matching} ORDER BY id LIMIT $4"),
                        &[&scope.tenant(), &emails, &phones, &(MAX_CANDIDATES as i64 + 1)],
                    )
                    .await?
                    .iter()
                    .map(|row| Candidate {
                        kind: KIND.into(),
                        id: row.get::<_, i64>(0).to_string(),
                        summary: format!("Event {}: {}, {} UTC", row.get::<_, i64>(0), row.get::<_, String>(1), row.get::<_, String>(2)),
                    })
                    .collect();
            }
            Ok(Located {
                kinds: vec![KindCount {
                    kind: KIND.into(),
                    by_person,
                    by_identifier,
                }],
                candidates,
                truncated: false,
            })
        })
    }
    fn export<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
        grant: &'a Disclosure,
    ) -> HandlerFuture<'a, Exported> {
        Box::pin(async move {
            grant.check(scope)?;
            let mut ids = subject.confirmed_ids(KIND);
            let mut after = 0;
            loop {
                let page = linked(client, scope, subject, after, PAGE).await?;
                let done = (page.len() as i64) < PAGE;
                after = page.last().map_or(after, |(id, _)| *id);
                ids.extend(page.into_iter().map(|(id, _)| id));
                if ids.len() > MAX_EXPORT {
                    return Err(Error::Invalid);
                }
                if done {
                    break;
                }
            }
            ids.sort_unstable();
            ids.dedup();
            let mut events = Vec::with_capacity(ids.len());
            for chunk in ids.chunks(PAGE as usize) {
                let chunk = chunk.to_vec();
                for row in client
                    .query(
                        "SELECT name,subject,recorded_at::text,payload FROM base_events WHERE tenant_id=$1 AND id=ANY($2) ORDER BY id",
                        &[&scope.tenant(), &chunk],
                    )
                    .await?
                {
                    events.push(json!({"name": row.get::<_, String>(0), "subject": row.get::<_, String>(1), "recorded_at": row.get::<_, String>(2), "payload": row.get::<_, Value>(3)}));
                }
            }
            Ok(Exported {
                records: json!({"events": events}),
                files: Vec::new(),
            })
        })
    }
    fn erase<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        grant: &'a Erasure,
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            grant.check(scope)?;
            let mut ids = subject.confirmed_ids(KIND);
            let mut after = 0;
            loop {
                let page = linked(tx.sql(), scope, subject, after, PAGE).await?;
                let done = (page.len() as i64) < PAGE;
                after = page.last().map_or(after, |(id, _)| *id);
                ids.extend(page.into_iter().map(|(id, _)| id));
                if done {
                    break;
                }
            }
            ids.sort_unstable();
            ids.dedup();
            let mut erased = Erased::default();
            let held = holds.records(KIND, &ids).await?;
            let tenant = scope.tenant();
            let declared = declared()?;
            let mut count = 0;
            for chunk in ids.chunks(PAGE as usize) {
                let chunk = chunk.to_vec();
                let rows = tx
                    .sql()
                    .query(
                        "SELECT e.id,e.name,e.payload,EXISTS (SELECT 1 FROM base_events_deliveries d WHERE d.event_id=e.id AND d.delivered_at IS NULL) \
                         FROM base_events e WHERE e.tenant_id=$1 AND e.id=ANY($2) ORDER BY e.id FOR UPDATE OF e",
                        &[&tenant, &chunk],
                    )
                    .await?;
                for row in rows {
                    let id: i64 = row.get(0);
                    if let Some(hold) = held.get(&id) {
                        erased.kept.push(Kept::held(KIND, id, *hold));
                        continue;
                    }
                    let name: String = row.get(1);
                    let mut payload: Value = row.get(2);
                    let fields: Vec<&str> = match declared.get(name.as_str()) {
                        Some(declaration) => {
                            let mut changed = Vec::new();
                            for field in declaration.fields {
                                if let Some(value) = payload.pointer_mut(field) {
                                    if !value.is_null() && *value != json!(ERASED) {
                                        *value = json!(ERASED);
                                        changed.push(*field);
                                    }
                                }
                            }
                            changed
                        }
                        None => {
                            if payload.get("erased").and_then(Value::as_str) == Some(ERASED) {
                                continue;
                            }
                            // An undeclared payload goes whole, but never under a
                            // subscriber still waiting for it: retried until delivered.
                            if row.get::<_, bool>(3) {
                                return Err(Error::Conflict);
                            }
                            payload = json!({"erased": ERASED});
                            vec!["payload"]
                        }
                    };
                    if fields.is_empty() {
                        continue;
                    }
                    let new: serde_json::Map<String, Value> = fields
                        .iter()
                        .map(|f| ((*f).to_owned(), json!(ERASED)))
                        .collect();
                    tx.mutate(move |sql| {
                        Box::pin(async move {
                            sql.execute(
                                "UPDATE base_events SET payload=$3 WHERE tenant_id=$1 AND id=$2",
                                &[&tenant, &id, &payload],
                            )
                            .await?;
                            Ok((
                                (),
                                vec![Change {
                                    kind: KIND.into(),
                                    id,
                                    old: json!({"payload": "kept"}),
                                    new: Value::Object(new),
                                }],
                            ))
                        })
                    })
                    .await?;
                    count += 1;
                }
            }
            erased.count(KIND, Action::Anonymised, count);
            Ok(erased)
        })
    }
}

inventory::submit! { Contribution::new(PERSONAL_DATA, "base/events.payloads",
Provenance { module: "base/events", source: file!(), line: line!() },
&PersonalData { label: "Event payloads", kinds: &[KIND], handler: &PAYLOADS }) }
// Event changes never carry the payload itself.
inventory::submit! { Contribution::new(RECORD_KINDS, "base/events.event",
Provenance { module: "base/events", source: file!(), line: line!() },
&RecordKind { kind: KIND, fields: &[], subject_identifier: false, person: false }) }
