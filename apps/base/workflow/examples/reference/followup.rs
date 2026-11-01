//! Disposable follow-up for durable owner jobs. It
//! proves an approval can commit an owner intent without executing it inline.
use bilibi_database::{Change, Db, Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{Execution, Handler, HandlerBinding, HandlerFuture, Outcome, HANDLERS};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
const WORKER: &str = "extensions/workflow_reference-followup";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    record: i64,
    revision: String,
}
fn validate(value: Value) -> Result<Value> {
    let input: Input = serde_json::from_value(value.clone()).map_err(|_| Error::Invalid)?;
    if input.record <= 0 || input.revision.is_empty() || input.revision.len() > 255 {
        return Err(Error::Invalid);
    }
    Ok(value)
}
const AT: Provenance = Provenance {
    module: "extensions/workflow_reference",
    source: file!(),
    line: line!(),
};
inventory::submit! {Contribution::new(bilibi_queue::WORKERS,WORKER,AT,&bilibi_queue::Worker{max_attempts:3,validate})}
inventory::submit! {Contribution::new(HANDLERS,WORKER,AT,&HandlerBinding{service:"extensions/workflow_reference.followup",capability:"companies.update"})}
pub(super) struct Followup {
    pub db: Db,
    pub parties: Arc<bilibi_tenancy::PartyProofs>,
    pub engine: Arc<bilibi_workflow::Engine>,
}
impl Handler for Followup {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let result:Result<()>=async {
                let input:Input=serde_json::from_value(args).map_err(|_|Error::Invalid)?;
                let scope=execution.scope().ok_or(Error::Forbidden)?;
                if scope.channel()!=bilibi_tenancy::Channel::Job || !matches!(self.engine.my_approvals(scope,&bilibi_workflow::ApprovalQuery{limit:1,..Default::default()}).await,Err(Error::Forbidden)) {return Err(Error::Internal);}
                println!("reference followup: job channel preserved; approval access refused");
                let mut client=self.db.get().await?;
                let mut tx=execution.begin(&mut client,"reference.approval.followup").await?;
                let row=tx.sql().query_opt("SELECT f.company_id FROM workflow_reference_records r JOIN workflow_reference_approval_facts f ON f.tenant_id=r.tenant_id AND f.record_id=r.id WHERE r.tenant_id=$1 AND r.id=$2 AND r.status='approved' AND f.revision=$3 FOR UPDATE OF r",&[&scope.tenant(),&input.record,&input.revision]).await?.ok_or(Error::Conflict)?;
                self.parties.organisation_target(tx.sql(),scope,row.get::<_,i64>(0).try_into().map_err(|_|Error::Invalid)?,"companies.update").await?;
                let tenant=scope.tenant();
                tx.mutate(move |sql|Box::pin(async move {
                    let inserted=sql.query_opt("INSERT INTO workflow_reference_followups(tenant_id,record_id,revision) VALUES($1,$2,$3) ON CONFLICT DO NOTHING RETURNING id",&[&tenant,&input.record,&input.revision]).await?;
                    let mut changes=Vec::new();
                    if let Some(row)=inserted {
                        let id:i64=row.get(0);
                        let old:i32=sql.query_one("SELECT facade_value FROM workflow_reference_records WHERE tenant_id=$1 AND id=$2",&[&tenant,&input.record]).await?.get(0);
                        sql.execute("UPDATE workflow_reference_records SET facade_value=facade_value+1 WHERE tenant_id=$1 AND id=$2",&[&tenant,&input.record]).await?;
                        changes.push(Change{kind:"reference.followup".into(),id,old:Value::Null,new:json!({"record":input.record,"revision":input.revision})});
                        changes.push(Change{kind:"reference.record".into(),id:input.record,old:json!({"facade_value":old}),new:json!({"facade_value":old+1})});
                    }
                    Ok(((),changes))
                })).await?;
                tx.commit().await
            }.await;
            match result {
                Ok(()) => Outcome::Completed,
                Err(Error::Forbidden | Error::Unauthorized | Error::Conflict) => {
                    Outcome::Cancel("reference_no_longer_eligible")
                }
                Err(_) => Outcome::Retry("reference_unavailable"),
            }
        })
    }
}
