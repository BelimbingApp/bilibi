//! Integration's personal-data store (ADR 05, `base/personal_data`): the
//! saved, already redacted previews of API exchanges. An exchange names no
//! person, so one is only ever a candidate, found by an email the requester
//! gave or by a phone's digits however it was formatted, and listed for the
//! operator to confirm. Only confirmed exchanges are exported or erased;
//! erasure replaces the whole preview (headers, bodies and path), never just
//! the matching text. Previews also expire after
//! `integration.payload_retention_days`. No Bilimbi counterpart.
use bilibi_database::{Change, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_personal_data::{
    Action, Candidate, Disclosure, Erased, Erasure, Exported, Handler, HandlerFuture, Holds, Kept,
    KindCount, Located, PersonalData, RecordKind, Subject, ERASED, MAX_CANDIDATES, PERSONAL_DATA,
    RECORD_KINDS,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::Client;

pub(crate) const KIND: &str = "base/integration.exchange";
const TEXT: &str = "concat_ws(' ',path,request_headers::text,request_body::text,response_headers::text,response_body::text)";

fn matching() -> String {
    format!("tenant_id=$1 AND ({TEXT} ILIKE ANY($2) OR regexp_replace({TEXT},'[^0-9]','','g') LIKE ANY($3))")
}

struct Previews;
static PREVIEWS: Previews = Previews;
impl Handler for Previews {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located> {
        Box::pin(async move {
            let (emails, phones) = subject.patterns();
            if emails.is_empty() && phones.is_empty() {
                return Ok(Located::default());
            }
            let count: i64 = client
                .query_one(
                    &format!(
                        "SELECT count(*) FROM base_integration_exchanges WHERE {}",
                        matching()
                    ),
                    &[&scope.tenant(), &emails, &phones],
                )
                .await?
                .get(0);
            let rows = client
                .query(
                    &format!("SELECT id,direction,system,operation,to_char(occurred_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI') \
                     FROM base_integration_exchanges WHERE {} ORDER BY id LIMIT $4", matching()),
                    &[&scope.tenant(), &emails, &phones, &(MAX_CANDIDATES as i64 + 1)],
                )
                .await?;
            let candidates = rows
                .iter()
                .map(|row| Candidate {
                    kind: KIND.into(),
                    id: row.get::<_, i64>(0).to_string(),
                    summary: format!(
                        "API exchange {}: {} {} {}, {} UTC",
                        row.get::<_, i64>(0),
                        row.get::<_, String>(1),
                        row.get::<_, String>(2),
                        row.get::<_, String>(3),
                        row.get::<_, String>(4)
                    ),
                })
                .collect();
            Ok(Located {
                kinds: vec![KindCount {
                    kind: KIND.into(),
                    by_person: 0,
                    by_identifier: count,
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
            grant.check_subject(scope, subject)?;
            let ids = subject.confirmed_ids(KIND);
            if ids.is_empty() {
                return Ok(Exported::default());
            }
            let rows = client
                .query(
                    "SELECT public_id,direction,system,operation,occurred_at::text,path,request_headers,request_body,response_headers,response_body \
                     FROM base_integration_exchanges WHERE tenant_id=$1 AND id=ANY($2) ORDER BY id",
                    &[&scope.tenant(), &ids],
                )
                .await?;
            let exchanges: Vec<Value> = rows
                .iter()
                .map(|row| json!({
                    "id": row.get::<_, String>(0), "direction": row.get::<_, String>(1), "system": row.get::<_, String>(2),
                    "operation": row.get::<_, String>(3), "occurred_at": row.get::<_, String>(4), "path": row.get::<_, String>(5),
                    "request_headers": row.get::<_, Option<Value>>(6), "request_body": row.get::<_, Option<Value>>(7),
                    "response_headers": row.get::<_, Option<Value>>(8), "response_body": row.get::<_, Option<Value>>(9),
                }))
                .collect();
            Ok(Exported {
                records: json!({"exchanges": exchanges}),
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
            grant.check_subject(scope, subject)?;
            let mut erased = Erased::default();
            let ids = subject.confirmed_ids(KIND);
            if ids.is_empty() {
                return Ok(erased);
            }
            let tenant = scope.tenant();
            let rows: Vec<i64> = tx
                .sql()
                .query(
                    "SELECT id FROM base_integration_exchanges WHERE tenant_id=$1 AND id=ANY($2) AND path<>$3 ORDER BY id FOR UPDATE",
                    &[&tenant, &ids, &ERASED],
                )
                .await?
                .iter()
                .map(|row| row.get(0))
                .collect();
            let held = holds.records(KIND, &rows).await?;
            let mut erase = Vec::new();
            for id in rows {
                match held.get(&id) {
                    Some(hold) => erased.kept.push(Kept::held(KIND, id, *hold)),
                    None => erase.push(id),
                }
            }
            if erase.is_empty() {
                return Ok(erased);
            }
            let count = erase.len() as i64;
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let mark = json!(ERASED);
                    let rows = sql
                        .query(
                            "UPDATE base_integration_exchanges SET path=$3,request_headers=$4,request_body=$4,response_headers=$4,response_body=$4 \
                             WHERE tenant_id=$1 AND id=ANY($2) RETURNING id",
                            &[&tenant, &erase, &ERASED, &mark],
                        )
                        .await?;
                    let changes = rows
                        .iter()
                        .map(|row| Change {
                            kind: KIND.into(),
                            id: row.get(0),
                            old: json!({"preview": "kept"}),
                            new: json!({"path": ERASED, "request_headers": ERASED, "request_body": ERASED, "response_headers": ERASED, "response_body": ERASED}),
                        })
                        .collect();
                    Ok(((), changes))
                })
            })
            .await?;
            erased.count(KIND, Action::Anonymised, count);
            Ok(erased)
        })
    }
}

inventory::submit! { Contribution::new(PERSONAL_DATA, "base/integration.previews",
Provenance { module: "base/integration", source: file!(), line: line!() },
&PersonalData { label: "API exchange previews", kinds: &[KIND], person_subjects: &[], handler: &PREVIEWS }) }
// Exchange changes never carry the preview itself.
inventory::submit! { Contribution::new(RECORD_KINDS, "base/integration.exchange",
Provenance { module: "base/integration", source: file!(), line: line!() },
&RecordKind { kind: KIND, fields: &[], subject_identifier: false, person: false }) }
