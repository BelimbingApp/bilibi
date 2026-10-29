//! Statically authorized approval contracts, shared by all calling doors.
//! Extends Bilimbi Workflow actions using Bilibi ADR 01's reviewed writes.
use crate::{ApprovalPolicy, Engine};
use bilibi_agent_api::{OperationClient, OperationFuture, WriteHandler};
use bilibi_database::{Error, MutationTransaction};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy)]
pub enum ApprovalContractKind {
    Decision { action: &'static str },
    Policy,
    Recall,
    Delegate,
    RevokeDelegation,
    Review,
}
/// One instance per statically declared owner operation. Bind during service
/// installation; the owner and case cannot change while requests are running.
pub struct ApprovalContract {
    owner: &'static str,
    case: &'static str,
    kind: ApprovalContractKind,
    engine: OnceLock<Arc<Engine>>,
}
impl ApprovalContract {
    pub const fn new(owner: &'static str, case: &'static str, kind: ApprovalContractKind) -> Self {
        Self {
            owner,
            case,
            kind,
            engine: OnceLock::new(),
        }
    }
    pub fn bind(&self, engine: Arc<Engine>) -> bilibi_module_registry::Result<()> {
        let declaration = engine
            .registry
            .approval_cases
            .get(self.case)
            .ok_or_else(|| {
                bilibi_module_registry::RegistryError::Service(
                    "missing contract approval case".into(),
                )
            })?;
        if declaration.0 != self.owner {
            return Err(bilibi_module_registry::RegistryError::Service(
                "foreign approval contract".into(),
            ));
        }
        self.engine.set(engine).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate approval contract binding".into(),
            )
        })
    }
    async fn check_target(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        input: &Value,
    ) -> bilibi_database::Result<()> {
        let query=match self.kind {
            ApprovalContractKind::RevokeDelegation=>Some(("SELECT case_key FROM base_workflow_approval_delegations WHERE tenant_id=$1 AND id=$2",number(input,"delegation")?)),
            ApprovalContractKind::Review=>Some(("SELECT case_key FROM base_workflow_control_exceptions WHERE tenant_id=$1 AND id=$2",number(input,"exception")?)),
            _=>None,
        };
        if let Some((sql, id)) = query {
            let row = client
                .query_opt(sql, &[&scope.tenant(), &id])
                .await?
                .ok_or(Error::NotFound)?;
            if row.get::<_, String>(0) != self.case {
                return Err(Error::Forbidden);
            }
        }
        Ok(())
    }
    fn engine(&self) -> bilibi_database::Result<&Engine> {
        self.engine.get().map(Arc::as_ref).ok_or(Error::Internal)
    }
}
fn number(input: &Value, key: &str) -> bilibi_database::Result<i64> {
    input[key].as_i64().ok_or(Error::Invalid)
}
fn text<'a>(input: &'a Value, key: &str) -> bilibi_database::Result<&'a str> {
    input[key].as_str().ok_or(Error::Invalid)
}
impl WriteHandler for ApprovalContract {
    fn owner(&self) -> &'static str {
        self.owner
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let engine = self.engine()?;
            if scope.channel() == bilibi_tenancy::Channel::Job || scope.principal().is_some() {
                return Err(Error::Forbidden.into());
            }
            let sql = client.transaction().ok_or(Error::Internal)?;
            let case = engine.approval_case(self.case)?;
            let reason = text(input, "reason")?;
            crate::approval::check_reason(
                &case,
                reason,
                !matches!(
                    self.kind,
                    ApprovalContractKind::Review | ApprovalContractKind::Decision { .. }
                ),
            )?;
            self.check_target(client, scope, input).await?;
            let review = match self.kind {
                ApprovalContractKind::Decision { .. } => {
                    let item = engine
                        .approval_detail_on(sql, scope, number(input, "request")?)
                        .await?;
                    if item.request.case != self.case
                        || item.request.version != number(input, "version")?
                        || item.subject_version != number(input, "subject_version")?
                        || item.request.routing_version != number(input, "routing_version")?
                        || item
                            .request
                            .stages
                            .get(item.request.stage_index)
                            .is_none_or(|s| Some(s.key.as_str()) != input["stage"].as_str())
                        || !item
                            .seats
                            .iter()
                            .any(|s| Some(s.as_str()) == input["seat"].as_str())
                    {
                        return Err(Error::Conflict.into());
                    }
                    crate::approval::check_reason(
                        &case,
                        reason,
                        input["approve"] != true || item.approval_reason_required,
                    )?;
                    json!({"label":item.label,"summary":item.summary,"owner_url":item.owner_url,"company":item.request.company,"request":item.request.id,"revision":item.request.revision,"version":item.request.version,"subject_version":item.subject_version,"decision":input["approve"],"reason":input["reason"],"stage":input["stage"],"seat":input["seat"],"routing_version":item.request.routing_version,"owner_input":input["input"]})
                }
                ApprovalContractKind::Policy => {
                    let policy: ApprovalPolicy =
                        serde_json::from_str(text(input, "policy")?).map_err(|_| Error::Invalid)?;
                    policy.validate(&engine.approval_case(self.case)?)?;
                    let current = match engine
                        .approval_policy_on(sql, scope, self.case, number(input, "company")?)
                        .await
                    {
                        Ok((id, _)) => Some(id),
                        Err(Error::NotFound) => None,
                        Err(e) => return Err(e.into()),
                    };
                    if current.unwrap_or(0) != number(input, "expected")? {
                        return Err(Error::Conflict.into());
                    }
                    json!({"case":self.case,"company":input["company"],"before":current,"policy":policy,"explanation":policy.explanation(),"reason":input["reason"]})
                }
                ApprovalContractKind::Recall => {
                    let history = engine
                        .approval_history_on(sql, scope, number(input, "request")?, 0, 1)
                        .await?;
                    if history.request.case != self.case
                        || history.request.state != "awaiting"
                        || history.request.initiator != scope.user()
                        || history.request.version != number(input, "version")?
                    {
                        return Err(Error::Conflict.into());
                    }
                    json!({"request":history.request,"reason":input["reason"]})
                }
                // These inputs are exact, bounded operation schemas. Commit
                // performs live company/identity/independence checks.
                _ => input.clone(),
            };
            Ok(review.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let engine = self.engine()?;
            self.check_target(tx.sql(), scope, input).await?;
            let output = match self.kind {
                ApprovalContractKind::Decision { action } => {
                    let row=tx.sql().query_opt("SELECT subject_type,subject_id FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2 AND case_key=$3",&[&scope.tenant(),&number(input,"request")?,&self.case]).await.map_err(Error::from)?.ok_or(Error::NotFound)?;
                    let subject_type: String = row.get(0);
                    let subject_id: i64 = row.get(1);
                    let mut request = crate::ActionRequest::new(
                        action,
                        text(input, "key")?,
                        number(input, "subject_version")?,
                    );
                    request.payload = json!({"request":input["request"],"version":input["version"],"stage":input["stage"],"seat":input["seat"],"routing_version":input["routing_version"],"approve":input["approve"],"reason":input["reason"],"input":input["input"]});
                    json!(
                        engine
                            .execute_action_in(tx, scope, &subject_type, subject_id, request)
                            .await?
                    )
                }
                ApprovalContractKind::Policy => {
                    json!({"policy_id":engine.publish_approval_policy_in(tx,scope,crate::ApprovalPolicyChange{case:self.case,company:number(input,"company")?,expected:match number(input,"expected")?{0=>None,id=>Some(id)},policy:serde_json::from_str(text(input,"policy")?).map_err(|_|Error::Invalid)?,reason:text(input,"reason")?}).await?})
                }
                ApprovalContractKind::Recall => {
                    engine
                        .recall_approval_in(
                            tx,
                            scope,
                            number(input, "request")?,
                            number(input, "version")?,
                            text(input, "reason")?,
                        )
                        .await?;
                    json!({"recalled":true})
                }
                ApprovalContractKind::Delegate => {
                    json!({"delegation":engine.delegate_approvals_in(tx,scope,crate::ApprovalCover{case:self.case,company:number(input,"company")?,delegate:number(input,"delegate")?,starts:text(input,"starts")?,ends:text(input,"ends")?,reason:text(input,"reason")?}).await?})
                }
                ApprovalContractKind::RevokeDelegation => {
                    engine
                        .revoke_approval_delegation_in(
                            tx,
                            scope,
                            number(input, "delegation")?,
                            number(input, "version")?,
                            text(input, "reason")?,
                        )
                        .await?;
                    json!({"revoked":true})
                }
                ApprovalContractKind::Review => {
                    engine
                        .review_approval_exception_in(
                            tx,
                            scope,
                            number(input, "exception")?,
                            text(input, "reason")?,
                        )
                        .await?;
                    json!({"reviewed":true})
                }
            };
            Ok(output.into())
        })
    }
}
/// The declaration uses this schema with the owner's exact input fields. A
/// case that accepts decision data supplies its own object schema for `input`.
pub fn approval_decision_schema(owner_input: Value) -> Value {
    let integer = json!({"type":"integer","minimum":1});
    json!({"type":"object","required":["request","version","subject_version","stage","seat","routing_version","approve","reason","key","input"],"properties":{
        "request":integer,"version":integer,"subject_version":{"type":"integer","minimum":0},"routing_version":integer,
        "stage":{"type":"string","maxLength":255},"seat":{"type":"string","maxLength":255},"approve":{"type":"boolean"},
        "reason":{"type":"string","maxLength":4000},"key":{"type":"string","maxLength":255},"input":owner_input}})
}
pub fn approval_policy_schema() -> Value {
    json!({"type":"object","required":["company","expected","policy","reason"],"properties":{"company":{"type":"integer","minimum":1},"expected":{"type":"integer","minimum":0},"policy":{"type":"string","maxLength":65536},"reason":{"type":"string","maxLength":4000}}})
}
pub fn approval_recall_schema() -> Value {
    json!({"type":"object","required":["request","version","reason"],"properties":{"request":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":1},"reason":{"type":"string","maxLength":4000}}})
}
pub fn approval_delegation_schema() -> Value {
    json!({"type":"object","required":["company","delegate","starts","ends","reason"],"properties":{"company":{"type":"integer","minimum":1},"delegate":{"type":"integer","minimum":1},"starts":{"type":"string","format":"date-time"},"ends":{"type":"string","format":"date-time"},"reason":{"type":"string","maxLength":4000}}})
}
pub fn approval_revoke_schema() -> Value {
    json!({"type":"object","required":["delegation","version","reason"],"properties":{"delegation":{"type":"integer","minimum":1},"version":{"type":"integer","minimum":1},"reason":{"type":"string","maxLength":4000}}})
}
pub fn approval_review_schema() -> Value {
    json!({"type":"object","required":["exception","reason"],"properties":{"exception":{"type":"integer","minimum":1},"reason":{"type":"string","maxLength":4000}}})
}

/// Declare the case and its statically authorized operations together. Owners
/// call the generated module's `bind` from their service factory, then bind
/// their business adapter on Engine. Every binding carries owner provenance.
#[macro_export]
macro_rules! declare_approval_case {
    ($module:ident, $owner:literal, $case:expr,
     policy:$policy:literal, recall:$recall:literal, delegation:$delegation:literal,
     revoke:$revoke:literal, review:$review:literal, step_up:$step_up:expr, decision_input:$input_schema:expr,
     decisions:[$(($handler:ident,$contract:literal,$action:literal,$capability:literal)),+ $(,)?]) => {
        mod $module {
            use super::*;
            const CASE:$crate::ApprovalCase=$case;
            static POLICY:$crate::ApprovalContract=$crate::ApprovalContract::new($owner,CASE.key,$crate::ApprovalContractKind::Policy);
            static RECALL:$crate::ApprovalContract=$crate::ApprovalContract::new($owner,CASE.key,$crate::ApprovalContractKind::Recall);
            static DELEGATE:$crate::ApprovalContract=$crate::ApprovalContract::new($owner,CASE.key,$crate::ApprovalContractKind::Delegate);
            static REVOKE:$crate::ApprovalContract=$crate::ApprovalContract::new($owner,CASE.key,$crate::ApprovalContractKind::RevokeDelegation);
            static REVIEW:$crate::ApprovalContract=$crate::ApprovalContract::new($owner,CASE.key,$crate::ApprovalContractKind::Review);
            $(static $handler:$crate::ApprovalContract=$crate::ApprovalContract::new($owner,CASE.key,$crate::ApprovalContractKind::Decision{action:$action});)+
            pub(super) fn bind(engine:std::sync::Arc<$crate::Engine>)->$crate::__registry::Result<()> {
                let decisions:&[(&str,&str,&str)]=&[$(($contract,$action,$capability)),+];
                let step_up:&[&str]=$step_up;
                if CASE.policy_contract!=$policy || CASE.recall_contract!=$recall || CASE.delegation_contract!=$delegation || CASE.revoke_contract!=$revoke || CASE.review_contract!=$review
                    || CASE.decisions.len()!=decisions.len()
                    || decisions.iter().any(|(contract,action,capability)|!CASE.decisions.iter().any(|d|d.contract==*contract && d.key==*action && d.capability==*capability))
                    || CASE.step_up.is_some_and(|key|!step_up.contains(&key)) {
                    return Err($crate::__registry::RegistryError::Service("approval operation declaration disagrees with case".into()));
                }
                for contract in [&POLICY,&RECALL,&DELEGATE,&REVOKE,&REVIEW,$(&$handler),+] {contract.bind(engine.clone())?;} Ok(())
            }
            fn decision_schema()->serde_json::Value {$crate::approval_decision_schema(($input_schema)())}
            static CASE_CONTRIBUTION:$crate::Contribution=$crate::Contribution::ApprovalCase(&CASE);
            $crate::__inventory::submit! {$crate::__registry::Contribution::new($crate::CONTRIBUTIONS,CASE.key,$crate::__registry::Provenance{module:$owner,source:file!(),line:line!()},&CASE_CONTRIBUTION)}
            static OPERATIONS:&[$crate::__api::Operation]=&[
                $($crate::__api::Operation{key:$contract,title:"Approval decision",summary:"Review and decide one eligible approval requirement.",keywords:&["approval","decision"],capability:$capability,input:decision_schema,output:||serde_json::json!({"type":"object"}),handler:$crate::__api::Handler::SafeWrite(&$handler),channels:$crate::__api::Channels::Every,step_up:$step_up},)+
                $crate::__api::Operation{key:$policy,title:"Approval rules",summary:"Review and publish approval rules with a reason.",keywords:&["approval","rules"],capability:CASE.policy_capability,input:$crate::approval_policy_schema,output:||serde_json::json!({"type":"object"}),handler:$crate::__api::Handler::SafeWrite(&POLICY),channels:$crate::__api::Channels::Every,step_up:&[]},
                $crate::__api::Operation{key:$recall,title:"Approval recall",summary:"Take back a waiting request with a reason.",keywords:&["approval","recall"],capability:CASE.recall_capability,input:$crate::approval_recall_schema,output:||serde_json::json!({"type":"object"}),handler:$crate::__api::Handler::SafeWrite(&RECALL),channels:$crate::__api::Channels::Every,step_up:&[]},
                $crate::__api::Operation{key:$delegation,title:"Approval cover",summary:"Arrange temporary cover by another permitted person.",keywords:&["approval","cover"],capability:CASE.delegation_capability,input:$crate::approval_delegation_schema,output:||serde_json::json!({"type":"object"}),handler:$crate::__api::Handler::SafeWrite(&DELEGATE),channels:$crate::__api::Channels::Every,step_up:&[]},
                $crate::__api::Operation{key:$revoke,title:"Approval cover cancellation",summary:"End a cover arrangement with a reason.",keywords:&["approval","cover"],capability:CASE.delegation_capability,input:$crate::approval_revoke_schema,output:||serde_json::json!({"type":"object"}),handler:$crate::__api::Handler::SafeWrite(&REVOKE),channels:$crate::__api::Channels::Every,step_up:&[]},
                $crate::__api::Operation{key:$review,title:"Approval exception review",summary:"Record an independent review of a control exception.",keywords:&["approval","exception"],capability:CASE.review_capability,input:$crate::approval_review_schema,output:||serde_json::json!({"type":"object"}),handler:$crate::__api::Handler::SafeWrite(&REVIEW),channels:$crate::__api::Channels::Every,step_up:&[]},
            ];
            static DEFINITIONS:$crate::__api::Definitions=$crate::__api::Definitions{operations:OPERATIONS,guides:&[]};
            $crate::__inventory::submit! {$crate::__registry::Contribution::new($crate::__api::DEFINITIONS,concat!($policy,".operations"),$crate::__registry::Provenance{module:$owner,source:file!(),line:line!()},&DEFINITIONS)}
            $( $crate::__inventory::submit! {$crate::__registry::Contribution::new($crate::__api::page::PAGE_BINDINGS,$contract,$crate::__registry::Provenance{module:$owner,source:file!(),line:line!()},&$crate::__api::page::PageBinding{service:"base/workflow.approval-pages",event:concat!("approval.",$contract),contract:$contract})} )+
            $crate::__inventory::submit! {$crate::__registry::Contribution::new($crate::__api::page::PAGE_BINDINGS,$policy,$crate::__registry::Provenance{module:$owner,source:file!(),line:line!()},&$crate::__api::page::PageBinding{service:"base/workflow.approval-pages",event:concat!("approval.",$policy),contract:$policy})}
            $crate::__inventory::submit! {$crate::__registry::Contribution::new($crate::__api::page::PAGE_BINDINGS,$recall,$crate::__registry::Provenance{module:$owner,source:file!(),line:line!()},&$crate::__api::page::PageBinding{service:"base/workflow.approval-pages",event:concat!("approval.",$recall),contract:$recall})}
            $crate::__inventory::submit! {$crate::__registry::Contribution::new($crate::__api::page::PAGE_BINDINGS,$delegation,$crate::__registry::Provenance{module:$owner,source:file!(),line:line!()},&$crate::__api::page::PageBinding{service:"base/workflow.approval-pages",event:concat!("approval.",$delegation),contract:$delegation})}
            $crate::__inventory::submit! {$crate::__registry::Contribution::new($crate::__api::page::PAGE_BINDINGS,$revoke,$crate::__registry::Provenance{module:$owner,source:file!(),line:line!()},&$crate::__api::page::PageBinding{service:"base/workflow.approval-pages",event:concat!("approval.",$revoke),contract:$revoke})}
            $crate::__inventory::submit! {$crate::__registry::Contribution::new($crate::__api::page::PAGE_BINDINGS,$review,$crate::__registry::Provenance{module:$owner,source:file!(),line:line!()},&$crate::__api::page::PageBinding{service:"base/workflow.approval-pages",event:concat!("approval.",$review),contract:$review})}
        }
    }
}
