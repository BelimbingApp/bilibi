//! Address's personal-data store (ADR 05, `base/personal_data`): addresses attached
//! to the person. Erasure clears the lines, phone, label, postcode, locality
//! and raw input of an address only this person uses, keeping its country so
//! the record still says where in the world it was; an address another party
//! also uses (a company's office) is kept and reported. The person's
//! attachments stay as links to the anonymised address.
use bilibi_database::{Change, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_personal_data::{
    Action, Disclosure, Erased, ErasedRecord, Erasure, Exported, Handler, HandlerFuture, Holds,
    Kept, KeptReason, KindCount, Located, PersonalData, RecordKind, Subject, ERASED, PERSONAL_DATA,
    RECORD_KINDS,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::Client;

const KIND: &str = "address";
const FIELDS: [&str; 9] = [
    "label",
    "phone",
    "line1",
    "line2",
    "line3",
    "locality",
    "postcode",
    "raw_input",
    "source_ref",
];

struct Addresses;
static ADDRESSES: Addresses = Addresses;
impl Handler for Addresses {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located> {
        Box::pin(async move {
            let count: i64 = client
                .query_one(
                    "SELECT count(*) FROM address_person_subjects WHERE tenant_id=$1 AND person_id=$2",
                    &[&scope.tenant(), &subject.person.get()],
                )
                .await?
                .get(0);
            Ok(Located {
                kinds: vec![KindCount {
                    kind: KIND.into(),
                    by_person: count,
                    by_identifier: 0,
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
                    "SELECT a.id,a.label,a.phone,a.line1,a.line2,a.line3,a.locality,a.postcode,a.country_iso,t.kinds,t.is_primary,a.admin1_code,a.raw_input,a.source_ref \
                     FROM address_person_subjects s JOIN addresses a ON a.tenant_id=s.tenant_id AND a.id=s.address_id \
                     LEFT JOIN address_attachments t ON t.tenant_id=s.tenant_id AND t.address_id=s.address_id AND t.person_id=s.person_id \
                     WHERE s.tenant_id=$1 AND s.person_id=$2 ORDER BY a.id",
                    &[&scope.tenant(), &subject.person.get()],
                )
                .await?;
            let records: Vec<Value> = rows
                .iter()
                .map(|row| {
                    json!({
                        "id": row.get::<_, i64>(0), "label": row.get::<_, Option<String>>(1), "phone": row.get::<_, Option<String>>(2),
                        "line1": row.get::<_, Option<String>>(3), "line2": row.get::<_, Option<String>>(4), "line3": row.get::<_, Option<String>>(5),
                        "locality": row.get::<_, Option<String>>(6), "postcode": row.get::<_, Option<String>>(7),
                        "country": row.get::<_, Option<String>>(8), "kinds": row.get::<_, Option<Vec<String>>>(9), "primary": row.get::<_, Option<bool>>(10),
                        // Every field erasure clears is also disclosed.
                        "admin1": row.get::<_, Option<String>>(11), "raw_input": row.get::<_, Option<String>>(12),
                        "source_ref": row.get::<_, Option<String>>(13),
                    })
                })
                .collect();
            Ok(Exported {
                records: json!({"addresses": records}),
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
            // Take address locks before reading sharing: an attach waiting on
            // the same lock must not be missed by an older statement snapshot.
            tx.sql().query(
                "SELECT id FROM addresses WHERE tenant_id=$1 AND id IN \
                 (SELECT address_id FROM address_person_subjects WHERE tenant_id=$1 AND person_id=$2) ORDER BY id FOR UPDATE",
                &[&tenant, &subject.person.get()],
            ).await?;
            // Shared: another party (a company or another person) also uses it.
            let rows = tx
                .sql()
                .query(
                    "SELECT a.id,a.label,a.phone,a.line1,a.line2,a.line3,a.locality,a.postcode,a.raw_input,a.source_ref,\
                     EXISTS (SELECT 1 FROM address_attachments o WHERE o.tenant_id=a.tenant_id AND o.address_id=a.id \
                       AND (o.organisation_id IS NOT NULL OR o.person_id<>$2)) AS shared \
                     FROM addresses a WHERE a.tenant_id=$1 AND a.id IN \
                       (SELECT address_id FROM address_person_subjects WHERE tenant_id=$1 AND person_id=$2) ORDER BY a.id FOR UPDATE OF a",
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
                if row.get::<_, bool>("shared") {
                    erased.kept.push(Kept {
                        kind: KIND.into(),
                        id: id.to_string(),
                        reason: KeptReason::Shared,
                        until: None,
                        hold: None,
                    });
                    continue;
                }
                let mut before = serde_json::Map::new();
                for (index, field) in FIELDS.iter().enumerate() {
                    before.insert(
                        (*field).into(),
                        json!(row.get::<_, Option<String>>(index + 1)),
                    );
                }
                // An empty address is still written while its history holds
                // values, since the database clears evidence only of a record
                // this transaction changed; otherwise it is not a write.
                let live = before.values().any(|v| !v.is_null());
                if !live && !bilibi_audit::holds_values(tx, KIND, id).await? {
                    count += 1;
                    continue;
                }
                let before = Value::Object(before);
                tx.mutate(move |sql| {
                    Box::pin(async move {
                        sql.execute(
                            "UPDATE addresses SET label=NULL,phone=NULL,line1=NULL,line2=NULL,line3=NULL,locality=NULL,postcode=NULL,\
                             raw_input=NULL,source_ref=NULL,verification_status='unverified',version=version+1,updated_at=now() \
                             WHERE tenant_id=$1 AND id=$2",
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

inventory::submit! { Contribution::new(PERSONAL_DATA, "core/address.addresses",
Provenance { module: "core/address", source: file!(), line: line!() },
&PersonalData { label: "Addresses", kinds: &[KIND], person_subjects: &[], handler: &ADDRESSES }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/address.address",
Provenance { module: "core/address", source: file!(), line: line!() },
&RecordKind { kind: KIND, fields: &FIELDS, subject_identifier: false, person: false }) }
