//! Events' personal-data store (ADR 05, `base/privacy`): recorded event
//! payloads. An event names no person, so it is found only by an identifier
//! (email or phone) the operator confirmed when verifying the request, and
//! erasure replaces that identifier inside the payload's strings with
//! `[erased]`. Subscribers keep their own copies and answer for them through
//! their own handlers. No Bilimbi counterpart.
use bilibi_database::{Change, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_privacy::{
    Action, Erased, Exported, Handler, HandlerFuture, Holds, Kept, KeptReason, KindAction,
    KindCount, Located, PersonalData, Subject, PERSONAL_DATA,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::Client;

const KIND: &str = "base/events.event";
const MATCH: &str = "tenant_id=$1 AND payload::text ILIKE ANY($2)";

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
            let patterns = subject.like_patterns();
            let count: i64 = if patterns.is_empty() {
                0
            } else {
                client
                    .query_one(
                        &format!("SELECT count(*) FROM base_events WHERE {MATCH}"),
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
                    &format!("SELECT name,subject,recorded_at::text,payload FROM base_events WHERE {MATCH} ORDER BY id LIMIT 5000"),
                    &[&scope.tenant(), &patterns],
                )
                .await?;
            let events: Vec<Value> = rows
                .iter()
                .map(|row| json!({"name": row.get::<_, String>(0), "subject": row.get::<_, String>(1), "recorded_at": row.get::<_, String>(2), "payload": row.get::<_, Value>(3)}))
                .collect();
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
                    &format!(
                        "SELECT id,payload FROM base_events WHERE {MATCH} ORDER BY id FOR UPDATE"
                    ),
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
                let mut payload: Value = row.get(1);
                if !subject.redact(&mut payload) {
                    continue;
                }
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
                                old: json!({"payload": "held an identifier"}),
                                new: json!({"payload": "erased"}),
                            }],
                        ))
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

inventory::submit! { Contribution::new(PERSONAL_DATA, "base/events.payloads",
Provenance { module: "base/events", source: file!(), line: line!() },
&PersonalData { label: "Event payloads", record_kinds: &[KIND], handler: &PAYLOADS }) }
