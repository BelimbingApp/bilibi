//! Durable delivery of committed transitions. Listener callbacks perform only
//! PostgreSQL effects in the supplied transaction and return every mutation.
//! Receipt + effects + audit commit together. External delivery must insert an
//! owner outbox here and use `event_key` as its remote idempotency key.
use crate::{history::Fact, Engine, OwnerFuture, Transition};
use bilibi_authz::SystemScope;
use bilibi_database::{Change, Error, Result};
use bilibi_queue::{Execution, Handler, HandlerFuture, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::Transaction;

pub const DELIVERY_WORKER: &str = "base/workflow-transition-delivery";
pub const DELIVER: &str = "workflow.transition.deliver";
#[derive(Clone, Copy)]
pub struct Listener {
    pub key: &'static str,
    pub subjects: &'static [&'static str],
    pub adapter: &'static dyn TransitionListener,
}
pub trait TransitionListener: Send + Sync {
    /// Recorded actors are event data, never authority. Scope is the live
    /// binding tenant's anonymous system identity. Enforce any owner policy
    /// and tenant predicates here; never open/commit a second transaction.
    fn handle<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a SystemScope,
        event: &'a TransitionEvent,
    ) -> OwnerFuture<'a, Vec<Change>>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitionEvent {
    pub event_key: String,
    pub event_type: String,
    pub tenant: i64,
    pub subject_type: String,
    pub subject_id: i64,
    pub flow: String,
    pub transition: Value,
    pub history: Fact,
    pub attempt: i32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    Delivered,
    Deferred,
    Skipped,
}
#[derive(Default, Debug)]
pub struct DeliverySummary {
    pub delivered: usize,
    pub deferred: usize,
    pub skipped: usize,
}

pub(crate) async fn insert(
    sql: &Transaction<'_>,
    fact: &Fact,
    edge: &Transition,
) -> Result<Change> {
    let event_key = format!("workflow.transition.completed:{}", fact.id);
    let snapshot = serde_json::to_value(edge).map_err(|_| Error::Invalid)?;
    let id: i64 = sql.query_one("INSERT INTO base_workflow_transition_outbox(history_id,event_key,transition_snapshot) VALUES($1,$2,$3) RETURNING id", &[&fact.id,&event_key,&snapshot]).await?.get(0);
    Ok(Change {
        kind: "workflow.transition_outbox".into(),
        id,
        old: Value::Null,
        new: json!({"history_id":fact.id,"event_key":event_key}),
    })
}

impl Engine {
    /// Trusted worker/maintenance edge. Never expose this method as a browser
    /// command: it is system delivery, not acting as the transition's author.
    pub async fn deliver_transition(&self, history_id: i64) -> Result<DeliveryOutcome> {
        if history_id <= 0 {
            return Err(Error::Invalid);
        }
        let token = uuid::Uuid::new_v4().to_string();
        let client = self.db.get().await?;
        let row = client.query_opt("UPDATE base_workflow_transition_outbox SET attempts=attempts+1,lease_token=$2,lease_expires_at=clock_timestamp()+interval '5 minutes' WHERE history_id=$1 AND delivered_at IS NULL AND available_at<=clock_timestamp() AND (lease_token IS NULL OR lease_expires_at<=clock_timestamp()) RETURNING id,attempts", &[&history_id,&token]).await?;
        let Some(row) = row else {
            return Ok(DeliveryOutcome::Skipped);
        };
        let (id, attempt): (i64, i32) = (row.get(0), row.get(1));
        drop(client);
        // A task boundary catches a panicking listener without losing the
        // batch. Dropping this future aborts the child and rolls back its SQL.
        let mut tasks = tokio::task::JoinSet::new();
        let engine = self.clone();
        let delivery_token = token.clone();
        tasks.spawn(async move {
            engine
                .dispatch_transition(id, &delivery_token, attempt)
                .await
        });
        let outcome = tasks.join_next().await;
        // All errors are bounded codes, never callback messages or secret SQL.
        // A cancelled parent retains its short lease for recovery.
        let client = self.db.get().await?;
        let (delivered, error) = match outcome {
            Some(Ok(Ok(()))) => (true, None),
            _ => (false, Some("listener_or_binding_unavailable")),
        };
        let delay = 2_i32.pow(attempt.clamp(0, 10) as u32).min(3600);
        let changed = client.execute("UPDATE base_workflow_transition_outbox SET delivered_at=CASE WHEN $3 THEN clock_timestamp() ELSE NULL END,available_at=CASE WHEN $3 THEN available_at ELSE clock_timestamp()+make_interval(secs=>$4::integer) END,last_error=$5,lease_token=NULL,lease_expires_at=NULL WHERE id=$1 AND lease_token=$2 AND lease_expires_at>clock_timestamp()", &[&id,&token,&delivered,&delay,&error]).await?;
        Ok(if changed == 0 {
            DeliveryOutcome::Skipped
        } else if delivered {
            DeliveryOutcome::Delivered
        } else {
            DeliveryOutcome::Deferred
        })
    }
    pub async fn deliver_due_transitions(&self, limit: i64) -> Result<DeliverySummary> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid);
        }
        let client = self.db.get().await?;
        let rows = client.query("SELECT history_id FROM base_workflow_transition_outbox WHERE delivered_at IS NULL AND available_at<=clock_timestamp() AND (lease_token IS NULL OR lease_expires_at<=clock_timestamp()) ORDER BY available_at,id LIMIT $1", &[&limit]).await?;
        drop(client);
        let mut summary = DeliverySummary::default();
        for row in rows {
            match self.deliver_transition(row.get(0)).await? {
                DeliveryOutcome::Delivered => summary.delivered += 1,
                DeliveryOutcome::Deferred => summary.deferred += 1,
                DeliveryOutcome::Skipped => summary.skipped += 1,
            }
        }
        Ok(summary)
    }
    async fn dispatch_transition(&self, id: i64, token: &str, attempt: i32) -> Result<()> {
        let client = self.db.get().await?;
        // Identity is resolved through immutable history and its proven binding,
        // never tenant/subject claims from an event payload or queue arguments.
        let row = client.query_one("SELECT b.tenant_id,b.subject_type,b.subject_id,b.flow,b.owner,o.event_key,o.transition_snapshot,h.id,h.from_status,h.status,h.kind,h.subject_version,h.actor_type,h.actor_id,h.impersonator,h.comment,h.comment_tag,h.assignees,h.attachments,h.metadata,h.tat,h.transitioned_at::text FROM base_workflow_transition_outbox o JOIN base_workflow_status_history h ON h.id=o.history_id JOIN base_workflow_bindings b ON b.id=h.binding_id WHERE o.id=$1", &[&id]).await?;
        let subject_type: String = row.get(1);
        let (owner, flow) = self.registry.subject(&subject_type)?;
        if owner.key != subject_type
            || owner.owner != row.get::<_, String>(4)
            || flow.code != row.get::<_, String>(3)
            || row.get::<_, String>(10) != "transition"
        {
            return Err(Error::Invalid);
        }
        let event = TransitionEvent {
            tenant: row.get(0),
            subject_type,
            subject_id: row.get(2),
            flow: row.get(3),
            event_key: row.get(5),
            transition: row.get(6),
            event_type: "workflow.transition.completed".into(),
            attempt,
            history: Fact {
                id: row.get(7),
                from_status: row.get(8),
                status: row.get(9),
                kind: row.get(10),
                version: row.get(11),
                actor_type: row.get(12),
                actor_id: row.get(13),
                impersonator: row.get(14),
                comment: row.get(15),
                comment_tag: row.get(16),
                assignees: row.get(17),
                attachments: row.get(18),
                metadata: row.get(19),
                tat: row.get(20),
                transitioned_at: row.get(21),
            },
        };
        drop(client);
        for listener in self
            .registry
            .listeners
            .values()
            .filter(|l| l.subjects.contains(&event.subject_type.as_str()))
        {
            let mut client = self.db.get().await?;
            let scope = SystemScope::rehydrate(&**client, event.tenant, None).await?;
            let mut tx = self
                .boundary
                .begin(
                    &mut client,
                    scope.mutation_context("workflow.listener.delivered"),
                )
                .await?;
            // The row lock fences overlapping attempts, including a slow old
            // worker. Do not hold it over network/file IO (forbidden above).
            let alive = tx.sql().query_opt("SELECT id FROM base_workflow_transition_outbox WHERE id=$1 AND lease_token=$2 AND lease_expires_at>clock_timestamp() AND delivered_at IS NULL FOR UPDATE", &[&id,&token]).await?.is_some();
            if !alive {
                return Err(Error::Conflict);
            }
            if tx.sql().query_opt("SELECT outbox_id FROM base_workflow_listener_receipts WHERE outbox_id=$1 AND listener_key=$2", &[&id,&listener.key]).await?.is_some() {
                tx.rollback().await?;
                continue;
            }
            let listener = *listener;
            let event = event.clone();
            tx.mutate(move |sql|Box::pin(async move {
                let mut changes = listener.adapter.handle(sql, &scope, &event).await?;
                sql.execute("INSERT INTO base_workflow_listener_receipts(outbox_id,listener_key) VALUES($1,$2)", &[&id,&listener.key]).await?;
                changes.push(Change {kind:"workflow.listener_receipt".into(),id,old:Value::Null,new:json!({"listener":listener.key,"event_key":event.event_key})});
                Ok(((),changes))
            })).await?;
            tx.commit().await?;
        }
        // Also prove tenant liveness when there are no listeners.
        let client = self.db.get().await?;
        SystemScope::rehydrate(&**client, event.tenant, None).await?;
        Ok(())
    }
}
pub(crate) struct Delivery {
    pub engine: Arc<Engine>,
}
impl Handler for Delivery {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(id) = args.get("history_id").and_then(Value::as_i64) else {
                return Outcome::Cancel("invalid_transition_reference");
            };
            let checked = async {
                let mut client = self.engine.db.get().await?;
                let tx = execution.begin(&mut client, "workflow.delivery.attempted").await?;
                let owns = tx.sql().query_opt("SELECT o.id FROM base_workflow_transition_outbox o JOIN base_workflow_status_history h ON h.id=o.history_id JOIN base_workflow_bindings b ON b.id=h.binding_id WHERE h.id=$1 AND b.tenant_id=$2", &[&id,&execution.tenant()]).await?.is_some();
                tx.rollback().await?;
                if owns {Ok(())} else {Err(Error::NotFound)}
            }.await;
            match checked {
                Ok(()) => {}
                Err(
                    Error::Forbidden
                    | Error::Unauthorized
                    | Error::Conflict
                    | Error::NotFound
                    | Error::Invalid,
                ) => return Outcome::Cancel("transition_scope_unavailable"),
                Err(_) => return Outcome::Retry("transition_store_unavailable"),
            }
            match self.engine.deliver_transition(id).await {
                Ok(DeliveryOutcome::Delivered | DeliveryOutcome::Skipped) => Outcome::Completed,
                Ok(DeliveryOutcome::Deferred) | Err(_) => {
                    Outcome::Retry("transition_delivery_deferred")
                }
            }
        })
    }
}
