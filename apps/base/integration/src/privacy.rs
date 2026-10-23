//! Integration's personal-data store (ADR 05, `base/privacy`): the saved,
//! already redacted previews of API exchanges. An exchange names no person,
//! so it is found only by an identifier (email or phone) the operator
//! confirmed when verifying the request; erasure replaces that identifier in
//! the endpoint, headers and bodies with `[erased]`. Previews also expire
//! after `integration.payload_retention_days`. No Bilimbi counterpart.
use bilibi_database::{Change, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_privacy::{
    Action, Erased, Exported, Handler, HandlerFuture, Holds, Kept, KeptReason, KindAction,
    KindCount, Located, PersonalData, Subject, PERSONAL_DATA,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::Client;

const KIND: &str = "base/integration.exchange";
const MATCH: &str = "tenant_id=$1 AND concat_ws(' ',endpoint,path,request_headers::text,request_body::text,response_headers::text,response_body::text) ILIKE ANY($2)";

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
            let patterns = subject.like_patterns();
            let count: i64 = if patterns.is_empty() {
                0
            } else {
                client
                    .query_one(
                        &format!("SELECT count(*) FROM base_integration_exchanges WHERE {MATCH}"),
                        &[&scope.tenant(), &patterns],
                    )
                    .await?
                    .get(0)
            };
            Ok(Located {
                kinds: vec![KindCount {
                    kind: KIND.into(),
                    by_person: 0,
                    by_identifier: count,
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
            let patterns = subject.like_patterns();
            if patterns.is_empty() {
                return Ok(Exported::default());
            }
            let rows = client
                .query(
                    &format!("SELECT public_id,direction,system,operation,occurred_at::text,request_body,response_body FROM base_integration_exchanges WHERE {MATCH} ORDER BY id LIMIT 5000"),
                    &[&scope.tenant(), &patterns],
                )
                .await?;
            let exchanges: Vec<Value> = rows
                .iter()
                .map(|row| json!({
                    "id": row.get::<_, String>(0), "direction": row.get::<_, String>(1), "system": row.get::<_, String>(2),
                    "operation": row.get::<_, String>(3), "occurred_at": row.get::<_, String>(4),
                    "request_body": row.get::<_, Option<Value>>(5), "response_body": row.get::<_, Option<Value>>(6),
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
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            let patterns = subject.like_patterns();
            let mut erased = Erased::default();
            if patterns.is_empty() {
                return Ok(erased);
            }
            let tenant = scope.tenant();
            let rows = tx
                .sql()
                .query(
                    &format!("SELECT id,endpoint,path,request_headers,request_body,response_headers,response_body FROM base_integration_exchanges WHERE {MATCH} ORDER BY id FOR UPDATE"),
                    &[&tenant, &patterns],
                )
                .await?;
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
                let mut endpoint = Value::String(row.get(1));
                let mut path = Value::String(row.get(2));
                let mut bodies: Vec<Option<Value>> =
                    (3..7).map(|i| row.get::<_, Option<Value>>(i)).collect();
                let mut changed = subject.redact(&mut endpoint) | subject.redact(&mut path);
                for body in bodies.iter_mut().flatten() {
                    changed |= subject.redact(body);
                }
                if !changed {
                    continue;
                }
                let (endpoint, path) = (
                    endpoint.as_str().unwrap_or_default().to_owned(),
                    path.as_str().unwrap_or_default().to_owned(),
                );
                tx.mutate(move |sql| {
                    Box::pin(async move {
                        sql.execute(
                            "UPDATE base_integration_exchanges SET endpoint=$3,path=$4,request_headers=$5,request_body=$6,response_headers=$7,response_body=$8 WHERE tenant_id=$1 AND id=$2",
                            &[&tenant, &id, &endpoint, &path, &bodies[0], &bodies[1], &bodies[2], &bodies[3]],
                        )
                        .await?;
                        Ok(((), vec![Change { kind: KIND.into(), id, old: json!({"preview": "held an identifier"}), new: json!({"preview": "erased"}) }]))
                    })
                })
                .await?;
                count += 1;
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

inventory::submit! { Contribution::new(PERSONAL_DATA, "base/integration.previews",
Provenance { module: "base/integration", source: file!(), line: line!() },
&PersonalData { label: "API exchange previews", record_kinds: &[KIND], handler: &PREVIEWS }) }
