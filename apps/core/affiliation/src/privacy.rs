//! Affiliation's personal-data store (ADR 05, `base/privacy`): the person's
//! affiliations with companies. Erasure keeps each affiliation, its company
//! link and its dates, so the company's records stay whole, and clears the
//! job title, work email and work phone. No Bilimbi counterpart.
use bilibi_database::{Change, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_privacy::{
    Action, Erased, ErasedRecord, Exported, Handler, HandlerFuture, Holds, Identifier, Kept,
    KeptReason, KindAction, KindCount, Located, PersonalData, Subject, PERSONAL_DATA,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::Client;

const KIND: &str = "affiliation";
const FIELDS: [&str; 3] = ["title", "work_email", "work_phone"];

fn identifiers(subject: &Subject) -> (Vec<String>, Vec<String>) {
    let (mut emails, mut phones) = (Vec::new(), Vec::new());
    for identifier in &subject.identifiers {
        match identifier {
            Identifier::Email(value) => emails.push(value.to_lowercase()),
            Identifier::Phone(value) => phones.push(value.clone()),
        }
    }
    (emails, phones)
}

struct Affiliations;
static AFFILIATIONS: Affiliations = Affiliations;
impl Handler for Affiliations {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located> {
        Box::pin(async move {
            let (emails, phones) = identifiers(subject);
            let row = client
                .query_one(
                    "SELECT count(*) FILTER (WHERE person_id=$2), \
                     count(*) FILTER (WHERE person_id<>$2 AND (lower(work_email)=ANY($3) OR work_phone=ANY($4))) \
                     FROM person_affiliations WHERE tenant_id=$1",
                    &[&scope.tenant(), &subject.person.get(), &emails, &phones],
                )
                .await?;
            Ok(Located {
                kinds: vec![KindCount {
                    kind: KIND.into(),
                    by_person: row.get(0),
                    by_identifier: row.get(1),
                }],
            })
        })
    }
    fn export<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Exported> {
        Box::pin(async move {
            let rows = client
                .query(
                    "SELECT id,organisation_id,title,work_email,work_phone,effective_from::text,effective_to::text,unlinked_at::text \
                     FROM person_affiliations WHERE tenant_id=$1 AND person_id=$2 ORDER BY id",
                    &[&scope.tenant(), &subject.person.get()],
                )
                .await?;
            let records: Vec<Value> = rows
                .iter()
                .map(|row| {
                    json!({
                        "id": row.get::<_, i64>(0), "company_id": row.get::<_, i64>(1), "title": row.get::<_, Option<String>>(2),
                        "work_email": row.get::<_, Option<String>>(3), "work_phone": row.get::<_, Option<String>>(4),
                        "effective_from": row.get::<_, Option<String>>(5), "effective_to": row.get::<_, Option<String>>(6),
                        "unlinked_at": row.get::<_, Option<String>>(7),
                    })
                })
                .collect();
            Ok(Exported {
                records: json!({"affiliations": records}),
                files: Vec::new(),
            })
        })
    }
    fn erase<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            let tenant = scope.tenant();
            let rows = tx
                .sql()
                .query(
                    "SELECT id,title,work_email,work_phone FROM person_affiliations WHERE tenant_id=$1 AND person_id=$2 ORDER BY id FOR UPDATE",
                    &[&tenant, &subject.person.get()],
                )
                .await?;
            let mut erased = Erased::default();
            let mut count = 0;
            for row in rows {
                let id: i64 = row.get(0);
                if holds.record_held(KIND, id) {
                    erased.kept.push(Kept {
                        kind: KIND.into(),
                        id: id.to_string(),
                        reason: KeptReason::LegalHold,
                        until: None,
                    });
                    continue;
                }
                let before = json!({"title": row.get::<_, Option<String>>(1), "work_email": row.get::<_, Option<String>>(2), "work_phone": row.get::<_, Option<String>>(3)});
                if before
                    .as_object()
                    .is_some_and(|o| o.values().any(|v| !v.is_null()))
                {
                    tx.mutate(move |sql| {
                        Box::pin(async move {
                            sql.execute(
                                "UPDATE person_affiliations SET title=NULL,work_email=NULL,work_phone=NULL,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                                &[&tenant, &id],
                            )
                            .await?;
                            let after = json!({"title": null, "work_email": null, "work_phone": null});
                            Ok(((), vec![Change { kind: KIND.into(), id, old: before, new: after }]))
                        })
                    })
                    .await?;
                }
                count += 1;
                erased.records.push(ErasedRecord {
                    kind: KIND.into(),
                    id,
                    fields: FIELDS.map(String::from).to_vec(),
                });
            }
            if count > 0 {
                erased.actions.push(KindAction {
                    kind: KIND.into(),
                    action: Action::Anonymised,
                    count,
                });
            }
            Ok(erased)
        })
    }
}

inventory::submit! { Contribution::new(PERSONAL_DATA, "core/affiliation.affiliations",
Provenance { module: "core/affiliation", source: file!(), line: line!() },
&PersonalData { label: "Affiliations (work contact)", record_kinds: &[KIND], handler: &AFFILIATIONS }) }
