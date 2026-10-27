//! Audit's personal-data store (ADR 05, `base/personal_data`): the evidence
//! history of the subject's person record, for an access request. Erasure of
//! evidence happens with each store's own erasure, through `erase_record`, so
//! this store's erase has nothing left to do. No Bilimbi counterpart.
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_personal_data::{
    Disclosure, Erased, Erasure, Exported, Handler, HandlerFuture, Holds, KindCount, Located,
    PersonalData, Stores, Subject, PERSONAL_DATA,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::Client;

const KIND: &str = "base/audit.mutation";
const PAGE: i64 = 1000;
/// More rows than this is an export too large to bundle, never a shorter one.
const MAX_ROWS: usize = 100_000;

fn person_kinds() -> Result<Vec<String>, Error> {
    Ok(Stores::installed()
        .map_err(|_| Error::Internal)?
        .person_kinds()
        .into_iter()
        .map(String::from)
        .collect())
}

struct History;
static HISTORY: History = History;
impl Handler for History {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located> {
        Box::pin(async move {
            let count: i64 = client
                .query_one(
                    "SELECT count(*) FROM base_audit_mutations WHERE tenant_id=$1 AND auditable_type=ANY($2) AND auditable_id=$3",
                    &[&scope.tenant(), &person_kinds()?, &subject.person.get().to_string()],
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
            let kinds = person_kinds()?;
            let id = subject.person.get().to_string();
            let (mut rows, mut after) = (Vec::new(), 0_i64);
            loop {
                let page = client
                    .query(
                        "SELECT id,event,to_char(occurred_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),old_values,new_values \
                         FROM base_audit_mutations WHERE tenant_id=$1 AND auditable_type=ANY($2) AND auditable_id=$3 AND id>$4 \
                         ORDER BY id LIMIT $5",
                        &[&scope.tenant(), &kinds, &id, &after, &PAGE],
                    )
                    .await?;
                let done = (page.len() as i64) < PAGE;
                for row in page {
                    after = row.get(0);
                    rows.push(json!({
                        "event": row.get::<_, String>(1), "occurred_at": row.get::<_, String>(2),
                        "old": row.get::<_, Option<Value>>(3), "new": row.get::<_, Option<Value>>(4),
                    }));
                }
                if rows.len() > MAX_ROWS {
                    return Err(Error::Invalid);
                }
                if done {
                    break;
                }
            }
            Ok(Exported {
                records: json!({"history": rows}),
                files: Vec::new(),
            })
        })
    }
    fn erase<'a>(
        &'a self,
        _tx: &'a mut MutationTransaction<'_>,
        _scope: &'a Scope,
        _subject: &'a Subject,
        _grant: &'a Erasure,
        _holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async { Ok(Erased::default()) })
    }
}

inventory::submit! { Contribution::new(PERSONAL_DATA, "base/audit.history",
Provenance { module: "base/audit", source: file!(), line: line!() },
&PersonalData { label: "Audit history", kinds: &[], handler: &HISTORY }) }
