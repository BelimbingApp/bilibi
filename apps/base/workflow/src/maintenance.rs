//! Trusted structural reconciliation, outbox retry and delivered-event retention.
//! This schedule is unreviewed/disabled until an operator enables it.
use crate::{outbox, Engine};
use bilibi_database::{Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, HANDLERS, WORKERS,
};
use bilibi_schedule::{
    worker, Definition, Misfire, Overlap, Reach, Schedule, DEFINITIONS, EXECUTE,
};
use bilibi_settings::{Definition as SettingDefinition, ScopeKind, Setting, Settings, ValueType};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::Arc;

pub const WORKER: &str = "base/workflow-maintenance";
pub const RETENTION_DAYS: &str = "workflow.outbox.keep_days";
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/workflow",
            source: file!(),
            line: line!(),
        }
    };
}
fn no_arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(o) if o.is_empty() => Ok(args),
        _ => Err(Error::Invalid),
    }
}
fn delivery_arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(o)
            if o.len() == 1
                && o.get("history_id")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| id > 0) =>
        {
            Ok(args)
        }
        _ => Err(Error::Invalid),
    }
}
inventory::submit! {Contribution::new(WORKERS,outbox::DELIVERY_WORKER,provenance!(),&Worker {max_attempts:5,validate:delivery_arguments})}
inventory::submit! {Contribution::new(HANDLERS,outbox::DELIVERY_WORKER,provenance!(),&HandlerBinding {service:"base/workflow.delivery",capability:outbox::DELIVER})}
inventory::submit! {Contribution::new(bilibi_authz::VOCABULARY,"workflow.delivery.vocabulary",provenance!(),&bilibi_authz::VocabularyContribution {
    domains:&[],verbs:&["deliver"],capabilities:&[bilibi_authz::CapabilityDefinition::tenant(outbox::DELIVER)],
    roles:&[],system_principals:&[],
})}
inventory::submit! {Contribution::new(WORKERS,WORKER,provenance!(),&Worker {max_attempts:3,validate:|args|worker::validate(args,no_arguments)})}
inventory::submit! {Contribution::new(HANDLERS,WORKER,provenance!(),&HandlerBinding {service:"base/workflow.maintenance",capability:EXECUTE})}
inventory::submit! {Contribution::new(DEFINITIONS,"base/workflow-maintenance",provenance!(),&Definition {
    name:"Reconcile workflow runs and deliver transition events",expression:"* * * * *",timezone:"Etc/UTC",task_name:"Base Workflow maintenance",worker:WORKER,args:||json!({}),overlap:Overlap::Forbid,misfire:Misfire::Coalesce,reach:Reach::Installation,owner_route:None,
})}
inventory::submit! {Contribution::new(bilibi_settings::DEFINITIONS,RETENTION_DAYS,provenance!(),&Setting::Definition(SettingDefinition {
    value_type:ValueType::Integer,scopes:&[ScopeKind::Global],default:||json!(30),nullable:false,encrypted:false,minimum:Some(1.0),maximum:Some(3650.0),validator:None,editor:None,
}))}
#[derive(Debug)]
pub struct MaintenanceSummary {
    pub reconciled: usize,
    pub skipped: usize,
    pub delivery: outbox::DeliverySummary,
    pub pruned: u64,
}
impl Engine {
    /// Operator command for the same bounded maintenance used by the scheduler.
    /// It never impersonates a history actor or changes owner business state.
    pub async fn maintain_workflows(
        &self,
        scope: &Scope,
        limit: i64,
        keep_days: i64,
    ) -> Result<MaintenanceSummary> {
        let mut client = self.db.get().await?;
        let tx = client.transaction().await?;
        let scope = self.live(&tx, scope).await?;
        bilibi_authz::authorize_operator(&*tx, &scope, EXECUTE).await?;
        tx.commit().await?;
        drop(client);
        self.maintenance_batch(limit, keep_days).await
    }
    pub(crate) async fn maintenance_batch(
        &self,
        limit: i64,
        keep_days: i64,
    ) -> Result<MaintenanceSummary> {
        if !(1..=1000).contains(&limit) || !(1..=3650).contains(&keep_days) {
            return Err(Error::Invalid);
        }
        // Delivery still runs if one run's owner or definition is unavailable.
        let runs = self.sweep_processes(limit).await;
        let approvals = self.sweep_approvals(limit).await;
        let delivery = self.deliver_due_transitions(limit).await?;
        let (reconciled, skipped) = runs?;
        approvals?;
        let client = self.db.get().await?;
        // Pending/leased/failed deliveries and append-only business facts are
        // never deleted. SKIP LOCKED avoids a concurrent dispatcher.
        let pruned = client.execute("DELETE FROM base_workflow_transition_outbox WHERE id IN (SELECT id FROM base_workflow_transition_outbox WHERE delivered_at<clock_timestamp()-make_interval(days=>$1::integer) AND lease_token IS NULL ORDER BY delivered_at,id LIMIT $2 FOR UPDATE SKIP LOCKED)", &[&(keep_days as i32),&limit]).await?;
        Ok(MaintenanceSummary {
            reconciled,
            skipped,
            delivery,
            pruned,
        })
    }
}
pub(crate) struct Maintenance {
    pub engine: Arc<Engine>,
    pub schedule: Arc<Schedule>,
    pub settings: Arc<Settings>,
}
impl Handler for Maintenance {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            let result = async {
                let client = self.engine.db.get().await?;
                let days = self
                    .settings
                    .get_global(&**client, RETENTION_DAYS)
                    .await?
                    .as_i64()
                    .ok_or(Error::Invalid)?;
                drop(client);
                self.engine.maintenance_batch(100, days).await
            }
            .await;
            match result {
                Ok(_) => Outcome::Completed,
                Err(_) => Outcome::Retry("workflow_maintenance_unavailable"),
            }
        }))
    }
}
