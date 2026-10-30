//! Affiliation's personal-data store (ADR 05, `base/personal_data`): the person's
//! affiliations with companies. Erasure keeps each affiliation, its company
//! link and its dates, so the company's records stay whole, and clears the
//! job title, work email and work phone. No Bilimbi counterpart.
use bilibi_database::{Change, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_personal_data::{
    Action, Disclosure, Erased, ErasedRecord, Erasure, Exported, Handler, HandlerFuture, Holds,
    Identifier, Kept, KindCount, Located, PersonalData, RecordKind, Subject, ERASED, PERSONAL_DATA,
    RECORD_KINDS,
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
            // Digits only, compared with the stored phone's digits, so
            // `+60 12-345 6789` and `60123456789` match.
            Identifier::Phone(value) => {
                let digits: String = value.chars().filter(char::is_ascii_digit).collect();
                if !digits.is_empty() {
                    phones.push(digits);
                }
            }
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
                     count(*) FILTER (WHERE person_id<>$2 AND (lower(work_email)=ANY($3) OR regexp_replace(work_phone,'[^0-9]','','g')=ANY($4))) \
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
                ..Located::default()
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
        grant: &'a Erasure,
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            grant.check_subject(scope, subject)?;
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
            let ids: Vec<i64> = rows.iter().map(|row| row.get(0)).collect();
            let held = holds.records(grant, KIND, &ids).await?;
            for row in rows {
                let id: i64 = row.get(0);
                if let Some(hold) = held.get(&id) {
                    erased.kept.push(Kept::held(KIND, id, *hold));
                    continue;
                }
                let before = json!({"title": row.get::<_, Option<String>>(1), "work_email": row.get::<_, Option<String>>(2), "work_phone": row.get::<_, Option<String>>(3)});
                // An empty affiliation is still written while its history
                // holds values, since the database clears evidence only of a
                // record this transaction changed; otherwise it is not a write.
                let live = before
                    .as_object()
                    .is_some_and(|o| o.values().any(|v| !v.is_null()));
                if !live && !bilibi_audit::holds_values(tx, KIND, id).await? {
                    count += 1;
                    continue;
                }
                tx.mutate(move |sql| {
                    Box::pin(async move {
                        sql.execute(
                            "UPDATE person_affiliations SET title=NULL,work_email=NULL,work_phone=NULL,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                            &[&tenant, &id],
                        )
                        .await?;
                        // `[erased]` where a value was, so the trail tells erasure from absence.
                        let after = Value::Object(before.as_object().into_iter().flatten().map(|(f, v)| (f.clone(), if v.is_null() { Value::Null } else { json!(ERASED) })).collect());
                        Ok(((), vec![Change { kind: KIND.into(), id, old: before, new: after }]))
                    })
                })
                .await?;
                erased.records.push(ErasedRecord {
                    kind: KIND.into(),
                    id,
                    person: None,
                });
                count += 1;
            }
            erased.count(KIND, Action::Anonymised, count);
            Ok(erased)
        })
    }
}

inventory::submit! { Contribution::new(PERSONAL_DATA, "core/affiliation.affiliations",
Provenance { module: "core/affiliation", source: file!(), line: line!() },
&PersonalData { label: "Affiliations (work contact)", kinds: &[KIND], person_subjects: &[], handler: &AFFILIATIONS }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/affiliation.affiliation",
Provenance { module: "core/affiliation", source: file!(), line: line!() },
&RecordKind { kind: KIND, fields: &FIELDS, subject_identifier: false, person: false }) }
