//! Approval persistence and transactional continuation. Mirrors Bilimbi's
//! owner-first Workflow lock order and human action replay contract.
use crate::approval::*;
use crate::{
    ActionRequest, ActionResult, ApprovalMode, ApprovalPolicy, ApprovalSeat, ApprovalSelector,
    ApprovalStage, Engine, Load, Operation, StartOptions,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::{Channel, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio_postgres::{Row, Transaction};

#[derive(Default)]
pub(crate) struct Services {
    bindings: Mutex<Bindings>,
    settings: Option<Arc<bilibi_settings::Settings>>,
    notices: Option<Arc<bilibi_notifications::NotificationBoundary>>,
    step_up: Option<Arc<bilibi_step_up::StepUp>>,
}
#[derive(Default)]
struct Bindings {
    closed: bool,
    cases: BTreeMap<&'static str, Arc<dyn ApprovalAdapter>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRequest {
    controls: Option<Controls>,
    pub id: i64,
    pub company: i64,
    pub case: String,
    pub subject_type: String,
    pub subject_id: i64,
    pub revision: String,
    pub policy_id: i64,
    pub stages: Vec<ApprovalStage>,
    pub stage_index: usize,
    pub state: String,
    pub version: i64,
    pub routing_version: i64,
    pub initiator: i64,
    pub prior_request: Option<i64>,
    pub run_id: Option<i64>,
}
// Preserve owner floors independently of future code and policy changes.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Controls {
    distinct_stages: bool,
    approval_reason: bool,
    allow_preparer: bool,
    delegation: bool,
    forbid_impersonation: bool,
    step_up: Option<String>,
    reason_characters: usize,
    reason_bytes: usize,
}
impl Controls {
    fn from_case(case: &ApprovalCase) -> Self {
        Self {
            distinct_stages: case.distinct_stages,
            approval_reason: case.approval_reason,
            allow_preparer: case.allow_preparer,
            delegation: case.delegation,
            forbid_impersonation: case.forbid_impersonation,
            step_up: case.step_up.map(str::to_owned),
            reason_characters: case.reason_characters,
            reason_bytes: case.reason_bytes,
        }
    }
}
impl ApprovalRequest {
    fn enforce(&self, mut case: ApprovalCase) -> Result<ApprovalCase> {
        // A pre-snapshot round must be recalled and resubmitted, never guessed.
        let saved = self.controls.as_ref().ok_or(Error::Conflict)?;
        if saved
            .step_up
            .as_deref()
            .is_some_and(|s| Some(s) != case.step_up)
        {
            return Err(Error::Conflict);
        }
        case.distinct_stages |= saved.distinct_stages;
        case.approval_reason |= saved.approval_reason;
        case.forbid_impersonation |= saved.forbid_impersonation;
        case.allow_preparer &= saved.allow_preparer;
        case.delegation &= saved.delegation;
        case.reason_characters = case.reason_characters.min(saved.reason_characters);
        case.reason_bytes = case.reason_bytes.min(saved.reason_bytes);
        Ok(case)
    }
}
fn request(row: &Row) -> Result<ApprovalRequest> {
    Ok(ApprovalRequest {
        controls: row
            .get::<_, Option<Value>>("controls")
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::Invalid)?,
        id: row.get("id"),
        company: row.get("company_id"),
        case: row.get("case_key"),
        subject_type: row.get("subject_type"),
        subject_id: row.get("subject_id"),
        revision: row.get("revision"),
        policy_id: row.get("policy_id"),
        stages: serde_json::from_value(row.get("stages")).map_err(|_| Error::Invalid)?,
        stage_index: usize::try_from(row.get::<_, i32>("stage_index"))
            .map_err(|_| Error::Invalid)?,
        state: row.get("state"),
        version: row.get("version"),
        routing_version: row.get("routing_version"),
        initiator: row.get("initiator"),
        prior_request: row.get("prior_request"),
        run_id: row.get("run_id"),
    })
}
fn change(kind: &str, id: i64, new: Value) -> Change {
    Change {
        kind: kind.into(),
        id,
        old: json!({}),
        new,
    }
}
fn updated(kind: &str, id: i64, old: Value, new: Value) -> Change {
    Change {
        kind: kind.into(),
        id,
        old,
        new,
    }
}
fn policy_key(case: &str) -> String {
    format!("workflow.approval.policy.{case}")
}
/// A reviewed publication of one company's rules.
pub struct ApprovalPolicyChange<'a> {
    pub case: &'a str,
    pub company: i64,
    pub expected: Option<i64>,
    pub policy: ApprovalPolicy,
    pub reason: &'a str,
}
/// Temporary one-hop cover, with inclusive start and exclusive end in UTC.
pub struct ApprovalCover<'a> {
    pub case: &'a str,
    pub company: i64,
    pub delegate: i64,
    pub starts: &'a str,
    pub ends: &'a str,
    pub reason: &'a str,
}
struct Eligibility<'a> {
    case: &'a ApprovalCase,
    subject: &'a crate::Subject,
    facts: &'a ApprovalSubject,
    request: &'a ApprovalRequest,
    allow_preparer: bool,
}
struct RecipientContext<'a> {
    case: &'a ApprovalCase,
    subject: &'a crate::Subject,
    facts: &'a ApprovalSubject,
    saved: &'a ApprovalRequest,
    seat: &'a ApprovalSeat,
    selector: &'a ApprovalSelector,
    allow_preparer: bool,
    distinct_stages: bool,
}
impl Engine {
    pub(crate) fn with_approval_services(
        mut self,
        settings: Arc<bilibi_settings::Settings>,
        notices: Arc<bilibi_notifications::NotificationBoundary>,
        step_up: Arc<bilibi_step_up::StepUp>,
    ) -> Self {
        self.approval = Arc::new(Services {
            settings: Some(settings),
            notices: Some(notices),
            step_up: Some(step_up),
            ..Services::default()
        });
        self
    }
    /// Only the declared owner may bind its case, before readiness. No service
    /// lookup, Domain dependency or callback replacement is allowed at runtime.
    pub fn bind_approval_case(
        &self,
        owner: &'static str,
        key: &'static str,
        adapter: Arc<dyn ApprovalAdapter>,
    ) -> bilibi_module_registry::Result<()> {
        let mut bindings = self.approval.bindings.lock().map_err(|_| {
            bilibi_module_registry::RegistryError::Service("approval bindings poisoned".into())
        })?;
        if bindings.closed
            || !self
                .registry
                .approval_cases
                .get(key)
                .is_some_and(|(o, _)| *o == owner)
            || bindings.cases.contains_key(key)
        {
            return Err(bilibi_module_registry::RegistryError::Service(
                "foreign, late or duplicate approval binding".into(),
            ));
        }
        bindings.cases.insert(key, adapter);
        Ok(())
    }
    pub(crate) fn approval_ready(&self) -> bilibi_module_registry::Result<()> {
        let mut bindings = self.approval.bindings.lock().map_err(|_| {
            bilibi_module_registry::RegistryError::Service("approval bindings poisoned".into())
        })?;
        if self
            .registry
            .approval_cases
            .keys()
            .any(|k| !bindings.cases.contains_key(k))
        {
            return Err(bilibi_module_registry::RegistryError::Service(
                "missing approval adapter".into(),
            ));
        }
        bindings.closed = true;
        Ok(())
    }
    pub(crate) fn approval_case(&self, key: &str) -> Result<ApprovalCase> {
        self.registry
            .approval_cases
            .get(key)
            .map(|(_, c)| *c)
            .ok_or(Error::NotFound)
    }
    fn adapter(&self, key: &str) -> Result<Arc<dyn ApprovalAdapter>> {
        self.approval
            .bindings
            .lock()
            .map_err(|_| Error::Internal)?
            .cases
            .get(key)
            .cloned()
            .ok_or(Error::NotFound)
    }
    pub(crate) async fn approval_identities(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
    ) -> Result<ApprovalIdentities> {
        if scope.user() <= 0 || scope.principal().is_some() || scope.channel() == Channel::Job {
            return Err(Error::Forbidden);
        }
        let mut ids = ApprovalIdentities::default();
        ids.accounts.insert(scope.user());
        if let Some(person) = scope.actor().person() {
            ids.people.insert(person.get());
        }
        if let Some(operator) = scope.impersonator() {
            ids.accounts.insert(operator.user());
            if let Some(person) = self
                .authentication
                .account_identity(sql, scope, operator.user())
                .await?
                .person
            {
                ids.people.insert(person.get());
            }
        }
        Ok(ids)
    }
    async fn account_ids(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        account: i64,
    ) -> Result<ApprovalIdentities> {
        let account_proof = self
            .authentication
            .account_identity(sql, scope, account)
            .await?;
        let mut ids = ApprovalIdentities::default();
        ids.accounts.insert(account);
        if let Some(p) = account_proof.person {
            ids.people.insert(p.get());
        }
        Ok(ids)
    }
    async fn prove_subject_identities(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        facts: &mut ApprovalSubject,
    ) -> Result<()> {
        for account in facts.preparers.accounts.clone() {
            match self.account_ids(sql, scope, account).await {
                Ok(ids) => facts.preparers.extend(&ids),
                Err(Error::Unauthorized | Error::NotFound | Error::Forbidden) => {}
                Err(e) => return Err(e),
            }
        }
        for account in facts.beneficiaries.accounts.clone() {
            facts
                .beneficiaries
                .extend(&self.account_ids(sql, scope, account).await?);
        }
        Ok(())
    }
    async fn policy_lock(
        sql: &Transaction<'_>,
        scope: &Scope,
        company: i64,
        case: &str,
        exclusive: bool,
    ) -> Result<()> {
        let key = format!("workflow.approval:{}:{company}:{case}", scope.tenant());
        let query = if exclusive {
            "SELECT pg_advisory_xact_lock(hashtextextended($1,0))"
        } else {
            "SELECT pg_advisory_xact_lock_shared(hashtextextended($1,0))"
        };
        sql.query_one(query, &[&key]).await?;
        Ok(())
    }
    async fn current_policy(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        company: i64,
        case: &str,
    ) -> Result<(i64, ApprovalPolicy)> {
        let settings = self.approval.settings.as_ref().ok_or(Error::Internal)?;
        let id = settings
            .get(
                sql,
                scope,
                bilibi_settings::Target::Company(company.try_into().map_err(|_| Error::Invalid)?),
                &policy_key(case),
            )
            .await?
            .value
            .as_i64()
            .ok_or(Error::NotFound)?;
        let row=sql.query_opt("SELECT policy FROM base_workflow_approval_policies WHERE tenant_id=$1 AND id=$2 AND company_id=$3 AND case_key=$4",&[&scope.tenant(),&id,&company,&case]).await?.ok_or(Error::NotFound)?;
        Ok((
            id,
            serde_json::from_value(row.get(0)).map_err(|_| Error::Invalid)?,
        ))
    }
    pub async fn approval_policy(
        &self,
        scope: &Scope,
        case: &str,
        company: i64,
    ) -> Result<(i64, ApprovalPolicy)> {
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let result = self.approval_policy_on(&sql, scope, case, company).await?;
        sql.commit().await?;
        Ok(result)
    }
    pub(super) async fn approval_policy_on(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        case: &str,
        company: i64,
    ) -> Result<(i64, ApprovalPolicy)> {
        let scope = self.live(sql, scope).await?;
        let declaration = self.approval_case(case)?;
        bilibi_authz::authorize_key(sql, &scope, declaration.policy_capability).await?;
        self.adapter(case)?
            .authorize_company(sql, &scope, company, ApprovalAccess::Policy)
            .await?;
        let policy = self.current_policy(sql, &scope, company, case).await?;
        Ok(policy)
    }
    /// Immutable publication, fenced by the reviewed pointer. Settings contains
    /// only the current id; Workflow owns the single authoritative snapshot.
    pub async fn publish_approval_policy_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        publication: ApprovalPolicyChange<'_>,
    ) -> Result<i64> {
        let ApprovalPolicyChange {
            case,
            company,
            expected,
            policy,
            reason,
        } = publication;
        let engine = self.clone();
        let scope = scope.clone();
        let case = case.to_owned();
        let reason = reason.trim().to_owned();
        tx.coordinate(move |tx| Box::pin(async move {
            engine.writer(tx.context(),&scope,company)?; let scope=engine.live(tx.sql(),&scope).await?;
            let declaration=engine.approval_case(&case)?; check_reason(&declaration,&reason,true)?; policy.validate(&declaration)?;
            bilibi_authz::authorize_key(tx.sql(),&scope,declaration.policy_capability).await?;
            let adapter=engine.adapter(&case)?; adapter.authorize_company(tx.sql(),&scope,company,ApprovalAccess::Policy).await?;
            Self::policy_lock(tx.sql(),&scope,company,&case,true).await?;
            for stage in policy.stages.iter().chain(policy.rules.iter().flat_map(|r|r.stages.iter())) {
                for seat in &stage.seats {
                    for selector in std::iter::once(&seat.selector).chain(seat.escalation.iter()) {
                        match selector {
                            ApprovalSelector::Person{account}=>{engine.account_ids(tx.sql(),&scope,*account).await?;},
                            ApprovalSelector::Role{id}=>{
                                let rows=bilibi_authz::role_page(tx.sql(),&scope,id-1,1).await?;
                                if rows.first().is_none_or(|r|r.id!=*id) {return Err(Error::NotFound);}
                            },
                            _=>{},
                        }
                    }
                }
            }
            let current=match engine.current_policy(tx.sql(),&scope,company,&case).await { Ok((id,_))=>Some(id),Err(Error::NotFound)=>None,Err(e)=>return Err(e) };
            if current != expected { return Err(Error::Conflict); }
            let value=serde_json::to_value(&policy).map_err(|_|Error::Invalid)?;
            let mut canonical=String::new(); crate::process::canonical(&value,&mut canonical);
            use sha2::Digest; let digest=sha2::Sha256::digest(canonical.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>();
            let publish_scope=scope.clone(); let publish_case=case.clone();
            let id=tx.mutate(move |sql| Box::pin(async move {
                let version:i64=sql.query_one("SELECT coalesce(max(version),0)+1 FROM base_workflow_approval_policies WHERE tenant_id=$1 AND company_id=$2 AND case_key=$3",&[&publish_scope.tenant(),&company,&publish_case]).await?.get(0);
                let id:i64=sql.query_one("INSERT INTO base_workflow_approval_policies(tenant_id,company_id,case_key,version,policy,digest,reason,publisher,impersonator) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",&[&publish_scope.tenant(),&company,&publish_case,&version,&value,&digest,&reason,&publish_scope.user(),&publish_scope.impersonator().map(|i|i.user())]).await?.get(0);
                Ok((id,vec![change("workflow.approval_policy",id,json!({"case":publish_case,"version":version,"policy":value,"digest":digest,"reason":reason}))]))
            })).await?;
            engine.approval.settings.as_ref().ok_or(Error::Internal)?.put(tx,&scope,bilibi_settings::Target::Company(company.try_into().map_err(|_|Error::Invalid)?),&policy_key(&case),json!(id)).await?;
            Ok(id)
        })).await
    }
    /// Owner submission uses the same transaction as its immutable revision.
    pub async fn submit_approval_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        case: &str,
        id: i64,
        key: &str,
        prior: Option<i64>,
    ) -> Result<i64> {
        let engine = self.clone();
        let scope = scope.clone();
        let case = case.to_owned();
        let key = key.to_owned();
        tx.coordinate(move |tx| Box::pin(async move {
            engine.writer(tx.context(),&scope,id)?; if !crate::process::text(&key) {return Err(Error::Invalid);}
            let scope=engine.live(tx.sql(),&scope).await?; let identities=engine.approval_identities(tx.sql(),&scope).await?;
            let declaration=engine.approval_case(&case)?; let owner=engine.registry.owned_subject(declaration.subject)?;
            let subject=engine.load(tx.sql(),&scope,owner,id,Load::Lock,Operation::ExecuteAction).await?;
            let adapter=engine.adapter(&case)?; let mut facts=adapter.inspect(tx.sql(),&scope,&subject,ApprovalAccess::Submit).await?; check_facts(&declaration,&facts)?;
            adapter.authorize_company(tx.sql(),&scope,facts.company,ApprovalAccess::Submit).await?;
            Self::policy_lock(tx.sql(),&scope,facts.company,&case,false).await?;
            for account in facts.preparers.accounts.clone() {
                match engine.account_ids(tx.sql(),&scope,account).await {
                    Ok(ids)=>facts.preparers.extend(&ids),
                    Err(Error::Unauthorized|Error::NotFound|Error::Forbidden)=>{},
                    Err(e)=>return Err(e),
                }
            }
            for account in facts.beneficiaries.accounts.clone() {facts.beneficiaries.extend(&engine.account_ids(tx.sql(),&scope,account).await?);}
            let (policy_id,policy)=engine.current_policy(tx.sql(),&scope,facts.company,&case).await?;
            policy.validate(&declaration)?; let stages=policy.route(&facts.facts)?;
            if let Some(row)=tx.sql().query_opt("SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND idempotency_key=$2 FOR UPDATE",&[&scope.tenant(),&key]).await? {
                let saved=request(&row)?;
                if saved.case!=case || saved.subject_id!=id || saved.revision!=facts.revision || saved.initiator!=scope.user() || saved.prior_request!=prior {return Err(Error::Conflict);}
                return Ok(saved.id);
            }
            if let Some(prior)=prior {
                let old=tx.sql().query_opt("SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&scope.tenant(),&prior]).await?.ok_or(Error::NotFound)?;
                let old=request(&old)?;
                if old.case!=case || old.subject_id!=id || old.company!=facts.company || old.state=="awaiting" || old.state=="approved" || old.revision==facts.revision {return Err(Error::Conflict);}
            }
            let activation_facts=facts.clone();
            let activation_subject=subject.clone();
            let revision=facts.revision.clone();
            let policy_digest:String=tx.sql().query_one("SELECT digest FROM base_workflow_approval_policies WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&policy_id]).await?.get(0);
            let insert_scope=scope.clone(); let insert_case=case.clone();
            let request_id=tx.mutate(move |sql|Box::pin(async move {
                let id:i64=sql.query_one("INSERT INTO base_workflow_approval_requests(tenant_id,company_id,case_key,subject_type,subject_id,revision,policy_id,facts,stages,initiator,prior_request,idempotency_key,controls) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING id",&[&insert_scope.tenant(),&facts.company,&insert_case,&declaration.subject,&id,&facts.revision,&policy_id,&serde_json::to_value(&facts.facts).map_err(|_|Error::Invalid)?,&serde_json::to_value(&stages).map_err(|_|Error::Invalid)?,&insert_scope.user(),&prior,&key,&json!(Controls::from_case(&declaration))]).await?.get(0);
                let mut participant_changes=Vec::new();
                for (role,ids) in [("preparer",&facts.preparers),("beneficiary",&facts.beneficiaries),("initiator",&identities)] { participants(sql,&insert_scope,id,role,ids,&mut participant_changes).await?; }
                let mut changes=vec![change("workflow.approval_request",id,json!({"case":insert_case,"subject_id":subject.id,"revision":facts.revision,"policy_id":policy_id,"prior_request":prior,"state":"awaiting"}))];
                changes.extend(participant_changes);
                event(sql,&insert_scope,id,"submitted",json!({"policy_id":policy_id,"preparer_count":facts.preparers.accounts.len(),"identity_digest":identity_digest(&facts.preparers)}),&mut changes).await?;
                Ok((id,changes))
            })).await?;
            let mut options=StartOptions::new(format!("approval:{request_id}")); options.input=json!({"approval_request":request_id,"revision":revision,"policy_digest":policy_digest});
            let run=engine.start_run_in(tx,&scope,declaration.process,declaration.subject,id,options).await?;
            let tenant=scope.tenant(); let run_id=run.id;
            tx.mutate(move |sql|Box::pin(async move { sql.execute("UPDATE base_workflow_approval_requests SET run_id=$3 WHERE tenant_id=$1 AND id=$2",&[&tenant,&request_id,&run_id]).await?;
                Ok(((),vec![updated("workflow.approval_request",request_id,json!({"run_id":null}),json!({"run_id":run_id}))])) })).await?;
            engine.notify_approval_stage_in(tx,&scope,&case,&activation_subject,&activation_facts,request_id).await?;
            Ok(request_id)
        })).await
    }
    pub(crate) async fn execute_approval_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        case: ApprovalCase,
        id: i64,
        action_request: ActionRequest,
    ) -> Result<ActionResult> {
        let engine = self.clone();
        let scope = scope.clone();
        tx.coordinate(move |tx| Box::pin(async move {
            engine.writer(tx.context(),&scope,id)?; let scope=engine.live(tx.sql(),&scope).await?;
            let identities=engine.approval_identities(tx.sql(),&scope).await?;
            if case.forbid_impersonation && scope.impersonator().is_some() {return Err(Error::Forbidden);}
            let owner=engine.registry.owned_subject(case.subject)?;
            let subject=engine.load(tx.sql(),&scope,owner,id,Load::Lock,Operation::Read).await?;
            let action=engine.registry.human_actions(owner).find(|a|a.key==action_request.action_key).ok_or(Error::NotFound)?;
            action_request.validate(&action)?;
            bilibi_authz::authorize_key(tx.sql(),&scope,action.capability).await?;
            let adapter=engine.adapter(case.key)?; let admission=adapter.inspect(tx.sql(),&scope,&subject,ApprovalAccess::Read).await?;
            adapter.authorize_company(tx.sql(),&scope,admission.company,ApprovalAccess::Read).await?; check_facts(&case,&admission)?;
            Self::policy_lock(tx.sql(),&scope,admission.company,case.key,false).await?;
            if let Some(saved)=crate::runs::human::lock_request(tx.sql(),scope.tenant(),&action_request.idempotency_key).await? {
                let mut changes=Vec::new(); let result=engine.replay(&scope,owner,id,saved,&action_request,&mut changes)?;
                for change in changes {tx.record(change).await?;} return Ok(result);
            }
            owner.adapter.authorize(tx.sql(),&scope,&subject,Operation::ExecuteAction).await?;
            let mut facts=adapter.inspect(tx.sql(),&scope,&subject,ApprovalAccess::Decide).await?;
            engine.prove_subject_identities(tx.sql(),&scope,&mut facts).await?;
            adapter.authorize_company(tx.sql(),&scope,facts.company,ApprovalAccess::Decide).await?;
            check_facts(&case,&facts)?;
            if facts.company!=admission.company {return Err(Error::Conflict);}
            if subject.version!=action_request.expected_subject_version {return Err(Error::Conflict);}
            let vote:ApprovalVote=serde_json::from_value(action_request.payload.clone()).map_err(|_|Error::Invalid)?;
            let row=tx.sql().query_opt("SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&scope.tenant(),&vote.request]).await?.ok_or(Error::NotFound)?;
            let saved=request(&row)?;
            let case=saved.enforce(case)?;
            if case.forbid_impersonation && scope.impersonator().is_some() {return Err(Error::Forbidden);}
            if saved.case!=case.key || saved.subject_type!=case.subject || saved.subject_id!=id || saved.company!=facts.company || saved.revision!=facts.revision
                || saved.state!="awaiting" || saved.version!=vote.version || saved.routing_version!=vote.routing_version {return Err(Error::Conflict);}
            let frozen_facts:Value=row.get("facts");
            if frozen_facts!=serde_json::to_value(&facts.facts).map_err(|_|Error::Invalid)? {return Err(Error::Conflict);}
            let stage=saved.stages.get(saved.stage_index).ok_or(Error::Invalid)?;
            let seat=stage.seats.iter().find(|s|s.key==vote.seat && stage.key==vote.stage && s.capability==action.capability).ok_or(Error::Forbidden)?;
            // Acquire run/work before reading requirements, and keep these locks
            // through owner effect, reducer and final aggregate completion.
            engine.lock_approval_run(tx.sql(),&scope,&saved).await?;
            let (_,live_policy)=engine.current_policy(tx.sql(),&scope,facts.company,case.key).await?;
            live_policy.validate(&case)?;
            let policy:ApprovalPolicy=serde_json::from_value(tx.sql().query_one("SELECT policy FROM base_workflow_approval_policies WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&saved.policy_id]).await?.get(0)).map_err(|_|Error::Invalid)?;
            policy.validate(&case)?;
            check_reason(&case,&vote.reason,!vote.approve || case.approval_reason || policy.approval_reason || live_policy.approval_reason)?;
            let eligible=engine.eligible(tx.sql(),&scope,Eligibility{case:&case,subject:&subject,facts:&facts,request:&saved,allow_preparer:live_policy.allow_preparer && policy.allow_preparer && case.allow_preparer},seat,&identities).await?.ok_or(Error::Forbidden)?;
            if let Some(operation)=case.step_up {engine.approval.step_up.as_ref().ok_or(Error::Internal)?.require(&scope,operation,&format!("approval:{}",saved.id)).await?;}
            let mut excluded=engine.excluded(tx.sql(),&scope,&saved).await?;
            excluded.extend(&facts.beneficiaries);
            let mut decision_ids=identities.clone();
            if let Some((_,_,grantor))=eligible {decision_ids.extend(&engine.account_ids(tx.sql(),&scope,grantor).await?);}
            let exceptional = decision_ids.overlaps(&excluded);
            let mut exception_participants = excluded.clone();
            exception_participants.extend(&decision_ids);
            let control_reference = if exceptional {
                let (id,_) = engine.current_policy(tx.sql(),&scope,facts.company,case.key).await?;
                format!("approval-policy:{id}")
            } else { String::new() };
            let control_reason = if exceptional {
                tx.sql().query_one("SELECT reason FROM base_workflow_approval_policies WHERE tenant_id=$1 AND id=$2", &[&scope.tenant(), &engine.current_policy(tx.sql(),&scope,facts.company,case.key).await?.0]).await?.get::<_,String>(0)
            } else { String::new() };
            let prior=tx.sql().query("SELECT identities,stage_key,seat_key,reason,impersonator FROM base_workflow_approval_decisions WHERE tenant_id=$1 AND request_id=$2",&[&scope.tenant(),&saved.id]).await?;
            let mut completed=0;
            for row in &prior {
                let previous:ApprovalIdentities=serde_json::from_value(row.get(0)).map_err(|_|Error::Invalid)?;
                let same_stage=row.get::<_,String>(1)==stage.key;
                if same_stage {completed+=1; if row.get::<_,String>(2)==seat.key {return Err(Error::Conflict);}}
                if (same_stage || policy.distinct_stages || live_policy.distinct_stages || case.distinct_stages) && previous.overlaps(&decision_ids) {return Err(Error::Forbidden);}
            }
            let terminal=if !vote.approve {Some(ApprovalOutcome::Rejected)} else if (stage.mode==ApprovalMode::Any || completed+1==stage.seats.len()) && saved.stage_index+1==saved.stages.len() {Some(ApprovalOutcome::Approved)} else {None};
            if terminal==Some(ApprovalOutcome::Approved) {
                let mut excluded=engine.excluded(tx.sql(),&scope,&saved).await?; excluded.extend(&facts.beneficiaries);
                for (i,row) in prior.iter().enumerate() {
                    if case.forbid_impersonation && row.get::<_,Option<i64>>(4).is_some() {return Err(Error::Forbidden);}
                    check_reason(&case,&row.get::<_,String>(3),case.approval_reason || policy.approval_reason || live_policy.approval_reason)?;
                    let identities:ApprovalIdentities=serde_json::from_value(row.get(0)).map_err(|_|Error::Invalid)?;
                    if !(live_policy.allow_preparer && policy.allow_preparer && case.allow_preparer) && excluded.overlaps(&identities) {return Err(Error::Forbidden);}
                    if case.distinct_stages || policy.distinct_stages || live_policy.distinct_stages {
                        for earlier in &prior[..i] {
                            let ids:ApprovalIdentities=serde_json::from_value(earlier.get(0)).map_err(|_|Error::Invalid)?;
                            if ids.overlaps(&identities) {return Err(Error::Forbidden);}
                        }
                    }
                    adapter.validate_saved(tx.sql(),&scope,&subject,&identities).await?;
                }
                adapter.validate_saved(tx.sql(),&scope,&subject,&decision_ids).await?;
            }
            // Reserve through the same human-action idempotency namespace.
            let hash=crate::intent::digest(&crate::intent::Intent{subject_type:owner.key,subject_id:id,actor_type:"user",actor_id:scope.user(),action_key:action.key,process_run_id:None,work_item_id:None,payload:&action_request.payload});
            let reserve_scope=scope.clone(); let reserve_request=action_request.clone();
            let action_id=tx.mutate(move |sql| Box::pin(async move {
                let action_id:i64=sql.query_one("INSERT INTO base_workflow_human_action_requests(tenant_id,idempotency_key,intent_hash,action_key,subject_type,subject_id,actor_type,actor_id) VALUES($1,$2,$3,$4,$5,$6,'user',$7) RETURNING id",&[&reserve_scope.tenant(),&reserve_request.idempotency_key,&hash,&action.key,&owner.key,&id,&reserve_scope.user()]).await?.get(0);
                Ok((action_id,vec![change("workflow.human_action_request",action_id,json!({"action_key":action.key,"subject_id":id,"reserved":true}))]))
            })).await?;
            if let Some(outcome)=terminal { adapter.apply_in(tx,&scope,&subject,outcome,vote.reason.trim(),&vote.input).await?; }
            let next_stage=if vote.approve && (stage.mode==ApprovalMode::Any || completed+1==stage.seats.len()) {saved.stage_index+1} else {saved.stage_index};
            let outcome=terminal.map_or("awaiting",ApprovalOutcome::as_str).to_owned();
            let output=json!({"request":saved.id,"state":outcome,"version":saved.version+1});
            let result=ActionResult{request_id:action_id,action_key:action.key.into(),outcome:outcome.clone(),output:output.clone(),result_ref:Some(format!("approval:{}",saved.id)),process_run_id:None,work_item_id:None,replayed:false};
            let result_json=serde_json::to_value(&result).map_err(|_|Error::Invalid)?;
            let decision_trace=tx.context().trace_id.clone();
            let closed_unused=stage.mode==ApprovalMode::Any && vote.approve;
            let decide_scope=scope.clone(); let saved_clone=saved.clone(); let decision_ids_copy=decision_ids.clone(); let vote_copy=vote.clone();
            tx.mutate(move |sql| Box::pin(async move {
                let (delegation_id,delegation_version,grantor)=eligible.map_or((None,None,None),|(id,v,g)|(Some(id),Some(v),Some(g)));
                let decision:i64=sql.query_one("INSERT INTO base_workflow_approval_decisions(tenant_id,request_id,stage_key,seat_key,outcome,reason,owner_input,actor,impersonator,identities,delegation_id,delegation_version,grantor,agent_connection,trace_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) RETURNING id",&[&decide_scope.tenant(),&saved_clone.id,&vote_copy.stage,&vote_copy.seat,&if vote_copy.approve{"approved"}else{"rejected"},&vote_copy.reason.trim(),&vote_copy.input,&decide_scope.user(),&decide_scope.impersonator().map(|i|i.user()),&json!(decision_ids_copy),&delegation_id,&delegation_version,&grantor,&decide_scope.agent().map(|a|a.connection()),&decision_trace]).await?.get(0);
                sql.execute("UPDATE base_workflow_approval_requests SET state=$3,version=version+1,stage_index=$4,activated_at=CASE WHEN stage_index<>$4 THEN now() ELSE activated_at END WHERE tenant_id=$1 AND id=$2",&[&decide_scope.tenant(),&saved_clone.id,&outcome,&(next_stage as i32)]).await?;
                sql.execute("UPDATE base_workflow_human_action_requests SET result=$3,completed_at=now(),updated_at=now() WHERE tenant_id=$1 AND id=$2",&[&decide_scope.tenant(),&action_id,&result_json]).await?;
                let mut changes=vec![change("workflow.approval_decision",decision,json!({"request":saved_clone.id,"stage":vote_copy.stage,"seat":vote_copy.seat,"outcome":outcome,"reason":vote_copy.reason,"identities":decision_ids_copy,"grantor":grantor})),updated("workflow.approval_request",saved_clone.id,json!({"state":saved_clone.state,"version":saved_clone.version,"stage_index":saved_clone.stage_index}),json!({"state":outcome,"version":saved_clone.version+1,"stage_index":next_stage})),updated("workflow.human_action_request",action_id,json!({"reserved":true}),json!({"completed":true}))];
                if exceptional {
                    let exception:i64=sql.query_one("INSERT INTO base_workflow_control_exceptions(tenant_id,company_id,case_key,subject_type,subject_id,revision,request_id,decision_id,kind,control_reference,reason,participants,actor,impersonator,agent_connection,trace_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'same_person',$9,$10,$11,$12,$13,$14,$15) RETURNING id", &[&decide_scope.tenant(),&saved_clone.company,&saved_clone.case,&saved_clone.subject_type,&saved_clone.subject_id,&saved_clone.revision,&saved_clone.id,&decision,&control_reference,&control_reason,&json!(exception_participants),&decide_scope.user(),&decide_scope.impersonator().map(|p|p.user()),&decide_scope.agent().map(|a|a.connection()),&decision_trace]).await?.get(0);
                    changes.push(change("workflow.control_exception",exception,json!({"request":saved_clone.id,"decision":decision,"kind":"same_person","control_reference":control_reference,"reason":control_reason,"participants":exception_participants})));
                }
                event(sql,&decide_scope,saved_clone.id,"decision",json!({"decision":decision,"state":outcome,"stage":vote_copy.stage,"seat":vote_copy.seat,"reason":vote_copy.reason,"identities":decision_ids_copy,"grantor":grantor,"agent_connection":decide_scope.agent().map(|a|a.connection()),"closed_unused":closed_unused}),&mut changes).await?;
                Ok(((),changes))
            })).await?;
            if let Some(outcome)=terminal {engine.finish_approval_run_in(tx,&scope,&saved,outcome).await?;}
            else if next_stage!=saved.stage_index {engine.notify_approval_stage_in(tx,&scope,case.key,&subject,&facts,saved.id).await?;}
            Ok(result)
        })).await
    }
    async fn excluded(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        request: &ApprovalRequest,
    ) -> Result<ApprovalIdentities> {
        let mut ids = ApprovalIdentities::default();
        for row in sql.query("SELECT kind,identity_id FROM base_workflow_approval_participants WHERE tenant_id=$1 AND request_id=$2",&[&scope.tenant(),&request.id]).await? {
            if row.get::<_,String>(0)=="account" {ids.accounts.insert(row.get(1));} else {ids.people.insert(row.get(1));}
        }
        Ok(ids)
    }
    async fn matches_selector(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        case: &ApprovalCase,
        subject: &crate::Subject,
        selector: &ApprovalSelector,
        account: i64,
    ) -> Result<bool> {
        match selector {
            ApprovalSelector::Capability => Ok(true),
            ApprovalSelector::Person { account: expected } => Ok(account == *expected),
            ApprovalSelector::Role { id } => {
                bilibi_authz::has_effective_role(sql, scope, *id, account).await
            }
            ApprovalSelector::Relationship { key } => {
                self.adapter(case.key)?
                    .relationship(sql, scope, subject, key, account)
                    .await
            }
        }
    }
    // Outer Option is eligibility; inner Option carries one-hop cover evidence.
    async fn eligible(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        context: Eligibility<'_>,
        seat: &ApprovalSeat,
        identities: &ApprovalIdentities,
    ) -> Result<Option<Option<(i64, i64, i64)>>> {
        let Eligibility {
            case,
            subject,
            facts,
            request,
            allow_preparer,
        } = context;
        if !bilibi_authz::decide(sql, scope, &seat.capability)
            .await?
            .allowed
        {
            return Ok(None);
        }
        let mut excluded = self.excluded(sql, scope, request).await?;
        excluded.extend(&facts.beneficiaries);
        // Beneficiaries are a hard floor even where preparer overlap is allowed.
        if identities.overlaps(&facts.beneficiaries)
            || (!allow_preparer && identities.overlaps(&excluded))
        {
            return Ok(None);
        }
        let escalated:bool=sql.query_one("SELECT EXISTS(SELECT 1 FROM base_workflow_approval_notices WHERE tenant_id=$1 AND request_id=$2 AND stage_key=$3 AND seat_key=$4 AND notice_key='escalated')",&[&scope.tenant(),&request.id,&request.stages[request.stage_index].key,&seat.key]).await?.get(0);
        let selector = if escalated {
            seat.escalation.as_ref().ok_or(Error::Invalid)?
        } else {
            &seat.selector
        };
        if self
            .matches_selector(sql, scope, case, subject, selector, scope.user())
            .await?
        {
            return Ok(Some(None));
        }
        if !seat.delegation || !case.delegation {
            return Ok(None);
        }
        let candidates=sql.query("SELECT id,version,grantor FROM base_workflow_approval_delegations WHERE tenant_id=$1 AND company_id=$2 AND case_key=$3 AND delegate=$4 AND revoked_at IS NULL AND starts_at<=now() AND ends_at>now() ORDER BY id LIMIT 100",&[&scope.tenant(),&facts.company,&case.key,&scope.user()]).await?;
        for row in candidates {
            let grantor: i64 = row.get(2);
            let identity = self
                .authentication
                .account_identity(sql, scope, grantor)
                .await?;
            if !self
                .adapter(case.key)?
                .candidate(sql, scope, subject, facts.company, grantor, &identity)
                .await?
            {
                continue;
            }
            let ids = self.account_ids(sql, scope, grantor).await?;
            if ids.overlaps(&facts.beneficiaries) || (!allow_preparer && ids.overlaps(&excluded)) {
                continue;
            }
            if !bilibi_authz::effective_capabilities_of(sql, scope, grantor)
                .await?
                .allowed
                .contains(seat.capability.as_str())
            {
                continue;
            }
            if self
                .matches_selector(sql, scope, case, subject, selector, grantor)
                .await?
            {
                return Ok(Some(Some((row.get(0), row.get(1), grantor))));
            }
        }
        Ok(None)
    }
}
fn identity_digest(ids: &ApprovalIdentities) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(serde_json::to_vec(ids).expect("identity sets serialize"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
async fn participants(
    sql: &Transaction<'_>,
    scope: &Scope,
    request: i64,
    role: &str,
    ids: &ApprovalIdentities,
    changes: &mut Vec<Change>,
) -> Result<()> {
    for (kind, values) in [("account", &ids.accounts), ("person", &ids.people)] {
        for id in values {
            sql.execute("INSERT INTO base_workflow_approval_participants(tenant_id,request_id,kind,identity_id,role) VALUES($1,$2,$3,$4,$5)",&[&scope.tenant(),&request,&kind,id,&role]).await?;
            changes.push(change(
                "workflow.approval_participant",
                request,
                json!({"role":role,"kind":kind,"identity":id}),
            ));
        }
    }
    Ok(())
}
async fn event(
    sql: &Transaction<'_>,
    scope: &Scope,
    request: i64,
    event: &str,
    detail: Value,
    changes: &mut Vec<Change>,
) -> Result<()> {
    let id:i64=sql.query_one("INSERT INTO base_workflow_approval_events(tenant_id,request_id,event,actor,impersonator,detail) VALUES($1,$2,$3,$4,$5,$6) RETURNING id",&[&scope.tenant(),&request,&event,&scope.user(),&scope.impersonator().map(|i|i.user()),&detail]).await?.get(0);
    changes.push(change(
        "workflow.approval_event",
        id,
        json!({"request":request,"event":event,"detail":detail}),
    ));
    Ok(())
}

inventory::submit! {bilibi_module_registry::Ready{module:"base/workflow",check:|services|services.get::<Engine>("base/workflow.engine")?.approval_ready()}}
inventory::submit! {bilibi_module_registry::Contribution::new(bilibi_settings::DEFINITIONS,"workflow.approval.policy.*",bilibi_module_registry::Provenance{module:"base/workflow",source:file!(),line:line!()},&bilibi_settings::Setting::RuntimeClaim)}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalItem {
    pub request: ApprovalRequest,
    pub label: String,
    pub summary: Vec<crate::ApprovalSummary>,
    pub approval_reason_required: bool,
    pub owner_url: String,
    pub subject_version: i64,
    pub submitted_at: String,
    pub due_at: Option<String>,
    pub seats: Vec<String>,
    pub covering_for: Option<i64>,
}
#[derive(Clone, Debug, Default)]
pub struct ApprovalQuery {
    pub limit: i64,
    pub after: Option<crate::PendingCursor>,
    pub company: Option<i64>,
    pub case: Option<String>,
    pub overdue: bool,
}
#[derive(Clone, Debug)]
pub struct ApprovalPage {
    pub entries: Vec<ApprovalItem>,
    pub next_cursor: Option<crate::PendingCursor>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalHistory {
    pub policy: ApprovalPolicy,
    pub policy_version: i64,
    pub policy_digest: String,
    pub policy_reason: String,
    pub owner_url: String,
    pub label: String,
    pub summary: Vec<crate::ApprovalSummary>,
    pub request: ApprovalRequest,
    pub events: Vec<Value>,
    pub next_cursor: Option<i64>,
}
impl Engine {
    /// Scan bounded aggregate candidates, then apply the same live eligibility
    /// as a vote. Returned counts describe this page, never hidden candidates.
    pub async fn my_approvals(&self, scope: &Scope, query: &ApprovalQuery) -> Result<ApprovalPage> {
        if !(1..=100).contains(&query.limit) {
            return Err(Error::Invalid);
        }
        let mut next = query.after;
        let mut entries = Vec::new();
        // At most 500 candidates per call; empty filtered pages still advance.
        for _ in 0..5 {
            let page = self
                .pending_work(
                    scope,
                    &crate::PendingQuery {
                        limit: 100,
                        after: next,
                        definition_key: query.case.clone(),
                        executor_keys: Some(vec!["approval.wait".into()]),
                        ..crate::PendingQuery::default()
                    },
                )
                .await?;
            let mut client = self.db.get().await?;
            let sql = client.transaction().await?;
            let scope = self.live(&sql, scope).await?;
            let ids = self.approval_identities(&sql, &scope).await?;
            for candidate in &page.entries {
                let cursor = crate::PendingCursor {
                    priority: candidate.item.priority,
                    available_at_micros: candidate
                        .item
                        .available_at
                        .as_ref()
                        .and_then(|v| v.parse::<jiff::Timestamp>().ok())
                        .map_or(0, |v| v.as_microsecond()),
                    id: candidate.item.id,
                };
                next = Some(cursor);
                let Some(row)=sql.query_opt("SELECT *,to_char(submitted_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS submitted FROM base_workflow_approval_requests WHERE tenant_id=$1 AND run_id=$2 AND state='awaiting'",&[&scope.tenant(),&candidate.item.run_id]).await? else {continue};
                let saved = request(&row)?;
                if query.company.is_some_and(|c| c != saved.company) {
                    continue;
                }
                match self.approval_item(&sql, &scope, saved, &ids).await {
                    Ok(mut item) => {
                        item.submitted_at = row.get("submitted");
                        if query.overdue
                            && !item.due_at.as_ref().is_some_and(|due| {
                                due.parse::<jiff::Timestamp>()
                                    .is_ok_and(|d| d <= jiff::Timestamp::now())
                            })
                        {
                            continue;
                        }
                        entries.push(item);
                    }
                    Err(
                        Error::Forbidden
                        | Error::NotFound
                        | Error::Invalid
                        | Error::Conflict
                        | Error::Unauthorized,
                    ) => continue,
                    Err(e) => return Err(e),
                }
                if entries.len() >= query.limit as usize {
                    sql.commit().await?;
                    return Ok(ApprovalPage {
                        entries,
                        next_cursor: next,
                    });
                }
            }
            sql.commit().await?;
            next = page.next_cursor;
            if next.is_none() {
                break;
            }
        }
        Ok(ApprovalPage {
            entries,
            next_cursor: next,
        })
    }
    async fn approval_item(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        saved: ApprovalRequest,
        ids: &ApprovalIdentities,
    ) -> Result<ApprovalItem> {
        let case = saved.enforce(self.approval_case(&saved.case)?)?;
        if case.forbid_impersonation && scope.impersonator().is_some() {
            return Err(Error::Forbidden);
        }
        let subject = self
            .load(
                sql,
                scope,
                self.registry.owned_subject(case.subject)?,
                saved.subject_id,
                Load::Read,
                Operation::Read,
            )
            .await?;
        let adapter = self.adapter(case.key)?;
        let mut facts = adapter
            .inspect(sql, scope, &subject, ApprovalAccess::Read)
            .await?;
        self.prove_subject_identities(sql, scope, &mut facts)
            .await?;
        check_facts(&case, &facts)?;
        adapter
            .authorize_company(sql, scope, facts.company, ApprovalAccess::Read)
            .await?;
        if facts.company != saved.company || facts.revision != saved.revision {
            return Err(Error::Conflict);
        }
        let (_, policy) = self
            .current_policy(sql, scope, facts.company, case.key)
            .await?;
        let snapshot: ApprovalPolicy = serde_json::from_value(
            sql.query_one(
                "SELECT policy FROM base_workflow_approval_policies WHERE tenant_id=$1 AND id=$2",
                &[&scope.tenant(), &saved.policy_id],
            )
            .await?
            .get(0),
        )
        .map_err(|_| Error::Invalid)?;
        policy.validate(&case)?;
        snapshot.validate(&case)?;
        let stage = saved.stages.get(saved.stage_index).ok_or(Error::Invalid)?;
        let prior=sql.query("SELECT seat_key,identities,stage_key FROM base_workflow_approval_decisions WHERE tenant_id=$1 AND request_id=$2",&[&scope.tenant(),&saved.id]).await?;
        let mut seats = Vec::new();
        let mut covering_for = None;
        for seat in &stage.seats {
            if prior
                .iter()
                .any(|r| r.get::<_, String>(0) == seat.key && r.get::<_, String>(2) == stage.key)
            {
                continue;
            }
            if let Some(delegation) = self
                .eligible(
                    sql,
                    scope,
                    Eligibility {
                        case: &case,
                        subject: &subject,
                        facts: &facts,
                        request: &saved,
                        allow_preparer: policy.allow_preparer
                            && snapshot.allow_preparer
                            && case.allow_preparer,
                    },
                    seat,
                    ids,
                )
                .await?
            {
                let mut accountable = ids.clone();
                if let Some((_, _, grantor)) = delegation {
                    accountable.extend(&self.account_ids(sql, scope, grantor).await?);
                }
                let mut distinct = true;
                for p in &prior {
                    let previous: ApprovalIdentities =
                        serde_json::from_value(p.get(1)).map_err(|_| Error::Invalid)?;
                    if (p.get::<_, String>(2) == stage.key
                        || policy.distinct_stages
                        || snapshot.distinct_stages
                        || case.distinct_stages)
                        && previous.overlaps(&accountable)
                    {
                        distinct = false;
                    }
                }
                if distinct {
                    seats.push(seat.key.clone());
                    covering_for = delegation.map(|(_, _, g)| g);
                }
            }
        }
        if seats.is_empty() {
            return Err(Error::Forbidden);
        }
        let hours = stage
            .seats
            .iter()
            .filter_map(|s| s.reminder_hours.or(s.escalation_hours))
            .min()
            .map(i64::from);
        let due_at = if let Some(hours) = hours {
            Some(sql.query_one("SELECT to_char((activated_at + make_interval(hours=>$3::integer)) AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&saved.id,&(hours as i32)]).await?.get(0))
        } else {
            None
        };
        Ok(ApprovalItem {
            request: saved,
            label: facts.label,
            summary: facts.summary,
            approval_reason_required: case.approval_reason
                || policy.approval_reason
                || snapshot.approval_reason,
            owner_url: facts.url,
            subject_version: subject.version,
            submitted_at: String::new(),
            due_at,
            seats,
            covering_for,
        })
    }
    pub async fn approval_detail(&self, scope: &Scope, id: i64) -> Result<ApprovalItem> {
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let item = self.approval_detail_on(&sql, scope, id).await?;
        sql.commit().await?;
        Ok(item)
    }
    pub(super) async fn approval_detail_on(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        id: i64,
    ) -> Result<ApprovalItem> {
        let scope = self.live(sql, scope).await?;
        let ids = self.approval_identities(sql, &scope).await?;
        let row = sql
            .query_opt(
                "SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2",
                &[&scope.tenant(), &id],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let item = self
            .approval_item(sql, &scope, request(&row)?, &ids)
            .await?;
        Ok(item)
    }
    pub async fn approval_history(
        &self,
        scope: &Scope,
        id: i64,
        after: i64,
        limit: i64,
    ) -> Result<ApprovalHistory> {
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let history = self
            .approval_history_on(&sql, scope, id, after, limit)
            .await?;
        sql.commit().await?;
        Ok(history)
    }
    pub(super) async fn approval_history_on(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        id: i64,
        after: i64,
        limit: i64,
    ) -> Result<ApprovalHistory> {
        if !(1..=100).contains(&limit) || after < 0 {
            return Err(Error::Invalid);
        }
        let scope = self.live(sql, scope).await?;
        let row = sql
            .query_opt(
                "SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2",
                &[&scope.tenant(), &id],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let saved = request(&row)?;
        let case = self.approval_case(&saved.case)?;
        let subject = self
            .load(
                sql,
                &scope,
                self.registry.owned_subject(case.subject)?,
                saved.subject_id,
                Load::Read,
                Operation::Read,
            )
            .await?;
        let adapter = self.adapter(case.key)?;
        let facts = adapter
            .inspect(sql, &scope, &subject, ApprovalAccess::Read)
            .await?;
        adapter
            .authorize_company(sql, &scope, saved.company, ApprovalAccess::Read)
            .await?;
        check_facts(&case, &facts)?;
        if facts.company != saved.company {
            return Err(Error::Forbidden);
        }
        let summary = if facts.revision == saved.revision {
            facts.summary
        } else {
            Vec::new()
        };
        let rows=sql.query("SELECT id,event,actor,impersonator,detail,to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM base_workflow_approval_events WHERE tenant_id=$1 AND request_id=$2 AND id>$3 ORDER BY id LIMIT $4",&[&scope.tenant(),&id,&after,&(limit+1)]).await?;
        let events=rows.iter().take(limit as usize).map(|r|json!({"id":r.get::<_,i64>(0),"event":r.get::<_,String>(1),"actor":r.get::<_,i64>(2),"impersonator":r.get::<_,Option<i64>>(3),"detail":r.get::<_,Value>(4),"at":r.get::<_,String>(5)})).collect::<Vec<_>>();
        let next_cursor = if rows.len() > limit as usize {
            events.last().and_then(|e| e["id"].as_i64())
        } else {
            None
        };
        let policy=sql.query_one("SELECT policy,version,digest,reason FROM base_workflow_approval_policies WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&saved.policy_id]).await?;
        Ok(ApprovalHistory {
            policy: serde_json::from_value(policy.get(0)).map_err(|_| Error::Invalid)?,
            policy_version: policy.get(1),
            policy_digest: policy.get(2),
            policy_reason: policy.get(3),
            owner_url: facts.url,
            label: facts.label,
            summary,
            request: saved,
            events,
            next_cursor,
        })
    }
    pub async fn recall_approval_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
        reason: &str,
    ) -> Result<()> {
        let engine = self.clone();
        let scope = scope.clone();
        let reason = reason.trim().to_owned();
        tx.coordinate(move |tx|Box::pin(async move {
            engine.writer(tx.context(),&scope,id)?;let scope=engine.live(tx.sql(),&scope).await?;engine.approval_identities(tx.sql(),&scope).await?;
            let candidate=tx.sql().query_opt("SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&id]).await?.ok_or(Error::NotFound)?;
            let candidate=request(&candidate)?;let case=engine.approval_case(&candidate.case)?;check_reason(&case,&reason,true)?;
            bilibi_authz::authorize_key(tx.sql(),&scope,case.recall_capability).await?;
            let subject=engine.load(tx.sql(),&scope,engine.registry.owned_subject(case.subject)?,candidate.subject_id,Load::Lock,Operation::ExecuteAction).await?;
            let adapter=engine.adapter(case.key)?;adapter.inspect(tx.sql(),&scope,&subject,ApprovalAccess::Recall).await?;
            Self::policy_lock(tx.sql(),&scope,candidate.company,case.key,false).await?;
            let row=tx.sql().query_one("SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&scope.tenant(),&id]).await?;
            let saved=request(&row)?;
            if saved.state!="awaiting" || saved.version!=version || saved.initiator!=scope.user() {return Err(Error::Conflict);}
            engine.lock_approval_run(tx.sql(),&scope,&saved).await?;
            adapter.apply_in(tx,&scope,&subject,ApprovalOutcome::Recalled,&reason,&json!({})).await?;
            let recall_scope=scope.clone();
            tx.mutate(move |sql|Box::pin(async move {
                sql.execute("UPDATE base_workflow_approval_requests SET state='recalled',version=version+1 WHERE tenant_id=$1 AND id=$2",&[&recall_scope.tenant(),&id]).await?;
                let mut changes=vec![updated("workflow.approval_request",id,json!({"state":"awaiting","version":version}),json!({"state":"recalled","version":version+1}))];event(sql,&recall_scope,id,"recalled",json!({"reason":reason}),&mut changes).await?;Ok(((),changes))
            })).await?;
            engine.finish_approval_run_in(tx,&scope,&saved,ApprovalOutcome::Recalled).await
        })).await
    }
    /// One-hop cover. Both people must remain independently eligible at vote
    /// time; this never grants a capability or company reach.
    pub async fn delegate_approvals_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        cover: ApprovalCover<'_>,
    ) -> Result<i64> {
        let ApprovalCover {
            case,
            company,
            delegate,
            starts,
            ends,
            reason,
        } = cover;
        let engine = self.clone();
        let scope = scope.clone();
        let case = case.to_owned();
        let starts = starts.to_owned();
        let ends = ends.to_owned();
        let reason = reason.trim().to_owned();
        tx.coordinate(move |tx|Box::pin(async move {
            engine.writer(tx.context(),&scope,company)?;let scope=engine.live(tx.sql(),&scope).await?;
            let ids=engine.approval_identities(tx.sql(),&scope).await?;let declaration=engine.approval_case(&case)?;
            bilibi_authz::authorize_key(tx.sql(),&scope,declaration.delegation_capability).await?;
            if !declaration.delegation || scope.impersonator().is_some() {return Err(Error::Forbidden);}
            check_reason(&declaration,&reason,true)?;
            let target=engine.account_ids(tx.sql(),&scope,delegate).await?;if target.overlaps(&ids) {return Err(Error::Forbidden);}
            let identity=engine.authentication.account_identity(tx.sql(),&scope,delegate).await?;
            if !engine.adapter(&case)?.cover_candidate(tx.sql(),&scope,company,delegate,&identity).await? {return Err(Error::Forbidden);}
            engine.adapter(&case)?.authorize_company(tx.sql(),&scope,company,ApprovalAccess::Delegate).await?;
            let start:jiff::Timestamp=starts.parse().map_err(|_|Error::Invalid)?;let end:jiff::Timestamp=ends.parse().map_err(|_|Error::Invalid)?;
            if end<=start || end<=jiff::Timestamp::now() {return Err(Error::Invalid);}
            let tenant=scope.tenant();let grantor=scope.user();
            // Serialize all case/company cover changes, including first insert.
            Self::policy_lock(tx.sql(),&scope,company,&case,true).await?;
            if tx.sql().query_one("SELECT EXISTS(SELECT 1 FROM base_workflow_approval_delegations WHERE tenant_id=$1 AND company_id=$2 AND case_key=$3 AND revoked_at IS NULL AND ends_at>now() AND (delegate=$4 OR grantor=$5))",&[&tenant,&company,&case,&grantor,&delegate]).await?.get::<_,bool>(0) {return Err(Error::Conflict);}
            let effective=bilibi_authz::effective_capabilities_of(tx.sql(),&scope,delegate).await?;
            let own=bilibi_authz::effective_capabilities(tx.sql(),&scope).await?;
            if !declaration.decisions.iter().any(|a|own.contains(a.capability) && effective.allowed.contains(a.capability)) {return Err(Error::Forbidden);}
            tx.mutate(move |sql|Box::pin(async move {
                let id:i64=sql.query_one("INSERT INTO base_workflow_approval_delegations(tenant_id,company_id,case_key,grantor,delegate,issuer,starts_at,ends_at,reason) VALUES($1,$2,$3,$4,$5,$4,$6::text::timestamptz,$7::text::timestamptz,$8) RETURNING id",&[&tenant,&company,&case,&grantor,&delegate,&starts,&ends,&reason]).await?.get(0);
                let event:i64=sql.query_one("INSERT INTO base_workflow_approval_delegation_events(tenant_id,delegation_id,event,actor,detail) VALUES($1,$2,'created',$3,$4) RETURNING id",&[&tenant,&id,&grantor,&json!({"grantor":grantor,"delegate":delegate,"starts":starts,"ends":ends,"reason":reason})]).await?.get(0);
                Ok((id,vec![change("workflow.approval_delegation",id,json!({"grantor":grantor,"delegate":delegate,"reason":reason})),change("workflow.approval_delegation_event",event,json!({"delegation":id,"event":"created"}))]))
            })).await
        })).await
    }
    pub async fn revoke_approval_delegation_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
        reason: &str,
    ) -> Result<()> {
        let engine = self.clone();
        let scope = scope.clone();
        let reason = reason.trim().to_owned();
        tx.coordinate(move |tx|Box::pin(async move {
            engine.writer(tx.context(),&scope,id)?;let scope=engine.live(tx.sql(),&scope).await?;engine.approval_identities(tx.sql(),&scope).await?;
            if scope.impersonator().is_some() {return Err(Error::Forbidden);}
            let row=tx.sql().query_opt("SELECT case_key,company_id,grantor FROM base_workflow_approval_delegations WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&id]).await?.ok_or(Error::NotFound)?;
            let case:String=row.get(0);let company:i64=row.get(1);if row.get::<_,i64>(2)!=scope.user() {return Err(Error::Forbidden);}
            let declaration=engine.approval_case(&case)?;
            bilibi_authz::authorize_key(tx.sql(),&scope,declaration.delegation_capability).await?;
            check_reason(&declaration,&reason,true)?;
            engine.adapter(&case)?.authorize_company(tx.sql(),&scope,company,ApprovalAccess::Delegate).await?;
            Self::policy_lock(tx.sql(),&scope,company,&case,true).await?;
            tx.mutate(move |sql|Box::pin(async move {
                if sql.execute("UPDATE base_workflow_approval_delegations SET revoked_at=now(),version=version+1 WHERE tenant_id=$1 AND id=$2 AND version=$3 AND revoked_at IS NULL",&[&scope.tenant(),&id,&version]).await?!=1 {return Err(Error::Conflict);}
                let event:i64=sql.query_one("INSERT INTO base_workflow_approval_delegation_events(tenant_id,delegation_id,event,actor,detail) VALUES($1,$2,'revoked',$3,$4) RETURNING id",&[&scope.tenant(),&id,&scope.user(),&json!({"reason":reason})]).await?.get(0);
                Ok(((),vec![updated("workflow.approval_delegation",id,json!({"revoked":false,"version":version}),json!({"revoked":true,"version":version+1,"reason":reason})),change("workflow.approval_delegation_event",event,json!({"delegation":id,"event":"revoked"}))]))
            })).await
        })).await
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlException {
    pub case: String,
    pub subject_id: i64,
    pub revision: String,
    pub kind: String,
    pub control_reference: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalException {
    pub id: i64,
    pub company: i64,
    pub case: String,
    pub subject_id: i64,
    pub revision: String,
    pub request: Option<i64>,
    pub kind: String,
    pub reason: String,
    pub control_reference: String,
    pub reviewed: bool,
    pub reviewable: bool,
    pub label: String,
    pub owner_url: String,
    pub actor: i64,
    pub impersonator: Option<i64>,
    pub agent_connection: Option<i64>,
    pub trace_id: Option<String>,
    pub created_at: String,
    pub reviewer: Option<i64>,
    pub review_note: Option<String>,
    pub reviewed_at: Option<String>,
}
impl Engine {
    /// Evidence of an already-authorized owner override. This does not grant
    /// permission or manufacture a second person's prior approval.
    pub async fn record_control_exception_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: ControlException,
    ) -> Result<i64> {
        let engine = self.clone();
        let scope = scope.clone();
        tx.coordinate(move |tx|Box::pin(async move {
            engine.writer(tx.context(),&scope,input.subject_id)?;let scope=engine.live(tx.sql(),&scope).await?;
            let ids=engine.approval_identities(tx.sql(),&scope).await?;let case=engine.approval_case(&input.case)?;
            check_reason(&case,&input.reason,true)?;
            if !crate::approval_policy::label(&input.kind) || !crate::approval_policy::label(&input.control_reference) {return Err(Error::Invalid);}
            let subject=engine.load(tx.sql(),&scope,engine.registry.owned_subject(case.subject)?,input.subject_id,Load::Lock,Operation::ExecuteAction).await?;
            let adapter=engine.adapter(case.key)?;
            adapter.authorize_control(tx.sql(),&scope,&subject,&input.kind,&input.control_reference).await?;
            let mut facts=adapter.inspect(tx.sql(),&scope,&subject,ApprovalAccess::Read).await?;
            engine.prove_subject_identities(tx.sql(),&scope,&mut facts).await?;
            adapter.authorize_company(tx.sql(),&scope,facts.company,ApprovalAccess::Read).await?;
            check_facts(&case,&facts)?;if facts.revision!=input.revision {return Err(Error::Conflict);}
            let mut participants=facts.preparers;participants.extend(&facts.beneficiaries);participants.extend(&ids);
            let trace=tx.context().trace_id.clone();
            tx.mutate(move |sql|Box::pin(async move {
                let id:i64=sql.query_one("INSERT INTO base_workflow_control_exceptions(tenant_id,company_id,case_key,subject_type,subject_id,revision,kind,control_reference,reason,participants,actor,impersonator,agent_connection,trace_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING id",&[&scope.tenant(),&facts.company,&case.key,&case.subject,&input.subject_id,&input.revision,&input.kind,&input.control_reference,&input.reason.trim(),&json!(participants),&scope.user(),&scope.impersonator().map(|p|p.user()),&scope.agent().map(|a|a.connection()),&trace]).await?.get(0);
                Ok((id,vec![change("workflow.control_exception",id,json!({"case":case.key,"subject_id":input.subject_id,"kind":input.kind,"reason":input.reason,"participants":participants}))]))
            })).await
        })).await
    }
    pub async fn approval_exceptions(
        &self,
        scope: &Scope,
        after: i64,
        limit: i64,
        company: Option<i64>,
        case_filter: Option<&str>,
        unreviewed: bool,
    ) -> Result<(Vec<ApprovalException>, Option<i64>)> {
        if !(1..=100).contains(&limit) || after < 0 {
            return Err(Error::Invalid);
        }
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let scope = self.live(&sql, scope).await?;
        let reader_ids = self.approval_identities(&sql, &scope).await?;
        let mut cursor = after;
        let mut entries = Vec::new();
        let mut more = false;
        for _ in 0..5 {
            let rows=sql.query("SELECT e.*,r.id IS NOT NULL AS reviewed,r.reviewer,r.note AS review_note,to_char(r.created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS reviewed_at,to_char(e.created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS created FROM base_workflow_control_exceptions e LEFT JOIN base_workflow_control_reviews r ON r.tenant_id=e.tenant_id AND r.exception_id=e.id WHERE e.tenant_id=$1 AND e.id>$2 AND ($3::bigint IS NULL OR e.company_id=$3) AND ($4::text IS NULL OR e.case_key=$4) AND (NOT $5 OR r.id IS NULL) ORDER BY e.id LIMIT 100",&[&scope.tenant(),&cursor,&company,&case_filter,&unreviewed]).await?;
            more = rows.len() == 100;
            for row in rows {
                cursor = row.get("id");
                let key: String = row.get("case_key");
                let case = match self.approval_case(&key) {
                    Ok(c) => c,
                    Err(Error::NotFound) => continue,
                    Err(e) => return Err(e),
                };
                if !bilibi_authz::decide(&*sql, &scope, case.review_capability)
                    .await?
                    .allowed
                {
                    continue;
                }
                let subject = match self
                    .load(
                        &sql,
                        &scope,
                        self.registry.owned_subject(case.subject)?,
                        row.get("subject_id"),
                        Load::Read,
                        Operation::Read,
                    )
                    .await
                {
                    Ok(s) => s,
                    Err(Error::Forbidden | Error::NotFound | Error::Unauthorized) => continue,
                    Err(e) => return Err(e),
                };
                let adapter = self.adapter(case.key)?;
                let mut facts = match adapter
                    .inspect(&sql, &scope, &subject, ApprovalAccess::Review)
                    .await
                {
                    Ok(facts) => facts,
                    Err(Error::Forbidden | Error::NotFound | Error::Unauthorized) => continue,
                    Err(e) => return Err(e),
                };
                match adapter
                    .authorize_company(&sql, &scope, row.get("company_id"), ApprovalAccess::Review)
                    .await
                {
                    Ok(()) => {}
                    Err(Error::Forbidden | Error::NotFound | Error::Unauthorized) => continue,
                    Err(e) => return Err(e),
                }
                match check_facts(&case, &facts) {
                    Ok(()) => {}
                    Err(Error::Invalid) => continue,
                    Err(e) => return Err(e),
                }
                if facts.company != row.get::<_, i64>("company_id") {
                    continue;
                }
                match self
                    .prove_subject_identities(&sql, &scope, &mut facts)
                    .await
                {
                    Ok(()) => {}
                    Err(Error::Forbidden | Error::NotFound | Error::Unauthorized) => continue,
                    Err(e) => return Err(e),
                }
                let mut participants: ApprovalIdentities =
                    serde_json::from_value(row.get("participants")).map_err(|_| Error::Invalid)?;
                participants.extend(&facts.preparers);
                participants.extend(&facts.beneficiaries);
                let mut terminal = true;
                if let Some(request_id) = row.get::<_, Option<i64>>("request_id") {
                    let approval=request(&sql.query_one("SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&request_id]).await?)?;
                    terminal = approval.state != "awaiting";
                    participants.extend(&self.excluded(&sql, &scope, &approval).await?);
                    for vote in sql.query("SELECT identities FROM base_workflow_approval_decisions WHERE tenant_id=$1 AND request_id=$2",&[&scope.tenant(),&request_id]).await? {participants.extend(&serde_json::from_value(vote.get(0)).map_err(|_|Error::Invalid)?);}
                }
                entries.push(ApprovalException {
                    reviewable: terminal
                        && !row.get::<_, bool>("reviewed")
                        && !reader_ids.overlaps(&participants),
                    label: facts.label,
                    owner_url: facts.url,
                    actor: row.get("actor"),
                    impersonator: row.get("impersonator"),
                    agent_connection: row.get("agent_connection"),
                    trace_id: row.get("trace_id"),
                    created_at: row.get("created"),
                    reviewer: row.get("reviewer"),
                    review_note: row.get("review_note"),
                    reviewed_at: row.get("reviewed_at"),
                    id: cursor,
                    company: row.get("company_id"),
                    case: key,
                    subject_id: row.get("subject_id"),
                    revision: row.get("revision"),
                    request: row.get("request_id"),
                    kind: row.get("kind"),
                    reason: row.get("reason"),
                    control_reference: row.get("control_reference"),
                    reviewed: row.get("reviewed"),
                });
                if entries.len() == limit as usize {
                    sql.commit().await?;
                    return Ok((entries, Some(cursor)));
                }
            }
            if !more {
                break;
            }
        }
        sql.commit().await?;
        Ok((entries, more.then_some(cursor)))
    }
    pub async fn review_approval_exception_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        note: &str,
    ) -> Result<()> {
        let engine = self.clone();
        let scope = scope.clone();
        let note = note.trim().to_owned();
        tx.coordinate(move |tx|Box::pin(async move {
            engine.writer(tx.context(),&scope,id)?;let scope=engine.live(tx.sql(),&scope).await?;
            let ids=engine.approval_identities(tx.sql(),&scope).await?;
            let row=tx.sql().query_opt("SELECT * FROM base_workflow_control_exceptions WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&id]).await?.ok_or(Error::NotFound)?;
            let key:String=row.get("case_key");let case=engine.approval_case(&key)?;check_reason(&case,&note,false)?;
            bilibi_authz::authorize_key(tx.sql(),&scope,case.review_capability).await?;
            let subject=engine.load(tx.sql(),&scope,engine.registry.owned_subject(case.subject)?,row.get("subject_id"),Load::Lock,Operation::ExecuteAction).await?;
            let adapter=engine.adapter(case.key)?;let mut facts=adapter.inspect(tx.sql(),&scope,&subject,ApprovalAccess::Review).await?;
            engine.prove_subject_identities(tx.sql(),&scope,&mut facts).await?;
            adapter.authorize_company(tx.sql(),&scope,row.get("company_id"),ApprovalAccess::Review).await?;
            let mut participants:ApprovalIdentities=serde_json::from_value(row.get("participants")).map_err(|_|Error::Invalid)?;
            participants.extend(&facts.preparers);participants.extend(&facts.beneficiaries);
            if let Some(request_id)=row.get::<_,Option<i64>>("request_id") {
                let row=tx.sql().query_one("SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&scope.tenant(),&request_id]).await?;
                let saved=request(&row)?;if saved.state=="awaiting" {return Err(Error::Conflict);}
                participants.extend(&engine.excluded(tx.sql(),&scope,&saved).await?);
                for row in tx.sql().query("SELECT identities FROM base_workflow_approval_decisions WHERE tenant_id=$1 AND request_id=$2",&[&scope.tenant(),&request_id]).await? {
                    participants.extend(&serde_json::from_value(row.get(0)).map_err(|_|Error::Invalid)?);
                }
            }
            if ids.overlaps(&participants) {return Err(Error::Forbidden);}
            let request_id:Option<i64>=row.get("request_id");let trace=tx.context().trace_id.clone();
            tx.mutate(move |sql|Box::pin(async move {
                let review:i64=sql.query_opt("INSERT INTO base_workflow_control_reviews(tenant_id,exception_id,reviewer,impersonator,identities,note,agent_connection,trace_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(tenant_id,exception_id) DO NOTHING RETURNING id",&[&scope.tenant(),&id,&scope.user(),&scope.impersonator().map(|i|i.user()),&json!(ids),&note,&scope.agent().map(|a|a.connection()),&trace]).await?.ok_or(Error::Conflict)?.get(0);
                let mut changes=vec![change("workflow.control_review",review,json!({"exception":id,"reviewer":scope.user(),"identities":ids,"note":note}))];
                if let Some(request_id)=request_id {event(sql,&scope,request_id,"exception_reviewed",json!({"exception":id,"review":review,"reason":note,"agent_connection":scope.agent().map(|a|a.connection())}),&mut changes).await?;}
                Ok(((),changes))
            })).await
        })).await
    }
}

const MAINTAIN: &str = "workflow.approvals.maintain";
const PRINCIPAL: &str = "workflow-approval-maintenance";
impl Engine {
    /// Existing scheduled Workflow maintenance invokes this sweep. It uses a
    /// named, explicitly granted principal, and can only remind or reassign.
    pub(crate) async fn sweep_approvals(&self, limit: i64) -> Result<()> {
        // The reviewed Queue job owns the durable execution lease. A saved
        // per-tenant keyset cursor bounds each pass and advances past missing
        // owners. A crash resumes later rows; wrapping retries earlier work.
        let client = self.db.get().await?;
        let tenants=client.query("SELECT DISTINCT tenant_id FROM base_workflow_approval_requests WHERE state='awaiting' ORDER BY tenant_id",&[]).await?;
        drop(client);
        for tenant in tenants {
            let tenant: i64 = tenant.get(0);
            let candidates = match self.approval_maintenance_page(tenant, limit).await {
                Ok(ids) => ids,
                Err(Error::Forbidden | Error::Unauthorized | Error::NotFound) => {
                    eprintln!("workflow approval maintenance unavailable in tenant {tenant}: grant or tenant unavailable");
                    continue;
                }
                Err(e) => return Err(e),
            };
            for id in candidates {
                match self.maintain_approval(tenant,id).await {
                    Ok(())=>{},
                    Err(Error::Forbidden|Error::NotFound|Error::Invalid|Error::Unauthorized|Error::Conflict)=>eprintln!("workflow approval maintenance deferred request {id}: permission, owner or routing unavailable"),
                    Err(e)=>return Err(e),
                }
            }
        }
        Ok(())
    }
    async fn approval_maintenance_page(&self, tenant: i64, limit: i64) -> Result<Vec<i64>> {
        use bilibi_authz::SystemAuthentication;
        let mut client = self.db.get().await?;
        let system =
            bilibi_authz::SystemScope::rehydrate(&**client, tenant, Some(PRINCIPAL)).await?;
        let scope = self.authentication.for_system(&**client, &system).await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("workflow.approval.scan"),
            )
            .await?;
        bilibi_authz::authorize_key(tx.sql(), &scope, MAINTAIN).await?;
        let ids=tx.mutate(move |sql|Box::pin(async move {
            sql.execute("INSERT INTO base_workflow_approval_maintenance(tenant_id) VALUES($1) ON CONFLICT DO NOTHING",&[&tenant]).await?;
            let Some(row)=sql.query_opt("SELECT last_request FROM base_workflow_approval_maintenance WHERE tenant_id=$1 FOR UPDATE SKIP LOCKED",&[&tenant]).await? else {return Ok((Vec::new(),Vec::new()));};
            let previous:i64=row.get(0);
            let mut rows=sql.query("SELECT id FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id>$2 AND state='awaiting' ORDER BY id LIMIT $3",&[&tenant,&previous,&limit]).await?;
            if rows.is_empty() && previous>0 {
                rows=sql.query("SELECT id FROM base_workflow_approval_requests WHERE tenant_id=$1 AND state='awaiting' ORDER BY id LIMIT $2",&[&tenant,&limit]).await?;
            }
            let ids=rows.iter().map(|r|r.get::<_,i64>(0)).collect::<Vec<_>>();
            let next=ids.last().copied().unwrap_or(0);
            sql.execute("UPDATE base_workflow_approval_maintenance SET last_request=$2 WHERE tenant_id=$1",&[&tenant,&next]).await?;
            Ok((ids,vec![Change{kind:"workflow.approval_maintenance_cursor".into(),id:tenant,old:json!({"last_request":previous}),new:json!({"last_request":next})}]))
        })).await?;
        tx.commit().await?;
        Ok(ids)
    }
    async fn maintain_approval(&self, tenant: i64, id: i64) -> Result<()> {
        use bilibi_authz::SystemAuthentication;
        let mut client = self.db.get().await?;
        let system =
            bilibi_authz::SystemScope::rehydrate(&**client, tenant, Some(PRINCIPAL)).await?;
        let scope = self.authentication.for_system(&**client, &system).await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("workflow.approval.maintained"),
            )
            .await?;
        bilibi_authz::authorize_key(tx.sql(), &scope, MAINTAIN).await?;
        let row = tx
            .sql()
            .query_opt(
                "SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2",
                &[&tenant, &id],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let candidate = request(&row)?;
        let case = candidate.enforce(self.approval_case(&candidate.case)?)?;
        let owner = self.registry.owned_subject(case.subject)?;
        let subject = owner
            .adapter
            .maintenance_load(tx.sql(), &system, candidate.subject_id)
            .await?;
        if subject.tenant != tenant || subject.id != candidate.subject_id {
            return Err(Error::Invalid);
        }
        let adapter = self.adapter(case.key)?;
        let mut facts = adapter.maintenance(tx.sql(), &system, &subject).await?;
        self.prove_subject_identities(tx.sql(), &scope, &mut facts)
            .await?;
        check_facts(&case, &facts)?;
        Self::policy_lock(tx.sql(), &scope, facts.company, case.key, false).await?;
        let row=tx.sql().query_one("SELECT *,floor(extract(epoch FROM (now()-activated_at))/3600)::bigint AS elapsed FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&tenant,&id]).await?;
        let saved = request(&row)?;
        if saved.state != "awaiting"
            || saved.revision != facts.revision
            || saved.company != facts.company
        {
            return Ok(());
        }
        let elapsed: i64 = row.get("elapsed");
        let stage = saved.stages.get(saved.stage_index).ok_or(Error::Invalid)?;
        // Match the owner -> policy -> request -> run/work lock order used by
        // votes. Maintenance holds no approval authority and invokes no effect.
        tx.sql()
            .query_one(
                "SELECT id FROM base_workflow_process_runs WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                &[&tenant, &saved.run_id.ok_or(Error::Invalid)?],
            )
            .await?;
        tx.sql().query("SELECT id FROM base_workflow_process_work_items WHERE tenant_id=$1 AND run_id=$2 ORDER BY id FOR UPDATE",&[&tenant,&saved.run_id]).await?;
        let policy:ApprovalPolicy=serde_json::from_value(tx.sql().query_one("SELECT policy FROM base_workflow_approval_policies WHERE tenant_id=$1 AND id=$2",&[&tenant,&saved.policy_id]).await?.get(0)).map_err(|_|Error::Invalid)?;
        let (_, live_policy) = self
            .current_policy(tx.sql(), &scope, facts.company, case.key)
            .await?;
        live_policy.validate(&case)?;
        policy.validate(&case)?;
        let mut routed = Vec::new();
        let mut seat_routes = serde_json::Map::new();
        for seat in &stage.seats {
            if tx.sql().query_one("SELECT EXISTS(SELECT 1 FROM base_workflow_approval_decisions WHERE tenant_id=$1 AND request_id=$2 AND stage_key=$3 AND seat_key=$4)",&[&tenant,&id,&stage.key,&seat.key]).await?.get::<_,bool>(0) {continue;}
            let escalated:bool=tx.sql().query_one("SELECT EXISTS(SELECT 1 FROM base_workflow_approval_notices WHERE tenant_id=$1 AND request_id=$2 AND stage_key=$3 AND seat_key=$4 AND notice_key='escalated')",&[&tenant,&id,&stage.key,&seat.key]).await?.get(0);
            let selector = if escalated {
                seat.escalation.as_ref().ok_or(Error::Invalid)?
            } else {
                &seat.selector
            };
            let mut holders = self
                .approval_recipients(
                    tx.sql(),
                    &scope,
                    RecipientContext {
                        case: &case,
                        subject: &subject,
                        facts: &facts,
                        saved: &saved,
                        seat,
                        selector,
                        allow_preparer: policy.allow_preparer
                            && live_policy.allow_preparer
                            && case.allow_preparer,
                        distinct_stages: policy.distinct_stages
                            || live_policy.distinct_stages
                            || case.distinct_stages,
                    },
                )
                .await?;
            holders.sort();
            holders.dedup();
            routed.extend(&holders);
            seat_routes.insert(seat.key.clone(), json!(holders));
        }
        routed.sort();
        routed.dedup();
        routed.truncate(bilibi_notifications::MAX_RECIPIENTS);
        let previous=tx.sql().query_opt("SELECT detail->'seats' FROM base_workflow_approval_events WHERE tenant_id=$1 AND request_id=$2 AND event IN ('routed','reassigned') AND detail->>'stage'=$3 ORDER BY id DESC LIMIT 1",&[&tenant,&id,&stage.key]).await?.and_then(|r|r.get::<_,Option<Value>>(0));
        let mut seats = previous
            .clone()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        // Someone who already decided leaves the other seats' holders without any change in responsibility.
        let distinct_stages = policy.distinct_stages || live_policy.distinct_stages || case.distinct_stages;
        let mut decided = std::collections::BTreeSet::new();
        for row in tx.sql().query("SELECT identities FROM base_workflow_approval_decisions WHERE tenant_id=$1 AND request_id=$2 AND ($3 OR stage_key=$4)",&[&tenant,&id,&distinct_stages,&stage.key]).await? {
            decided.extend(serde_json::from_value::<ApprovalIdentities>(row.get(0)).map_err(|_|Error::Invalid)?.accounts);
        }
        if previous.is_none()
            || seat_routes.iter().any(|(key, now)| {
                let before = seats.get(key).and_then(Value::as_array).map(|holders| {
                    json!(holders
                        .iter()
                        .filter(|a| a.as_i64().is_none_or(|a| !decided.contains(&a)))
                        .collect::<Vec<_>>())
                });
                before.as_ref() != Some(now)
            })
        {
            seats.extend(seat_routes);
            let route_scope = scope.clone();
            let stage_key = stage.key.clone();
            let old_route = saved.routing_version;
            let evidence = routed.clone();
            tx.mutate(move |sql|Box::pin(async move {
                sql.execute("UPDATE base_workflow_approval_requests SET routing_version=routing_version+1,version=version+1 WHERE tenant_id=$1 AND id=$2",&[&tenant,&id]).await?;
                let mut changes=vec![updated("workflow.approval_request",id,json!({"routing_version":old_route}),json!({"routing_version":old_route+1}))];
                event(sql,&route_scope,id,"reassigned",json!({"stage":stage_key,"recipients":evidence,"seats":seats,"reason":"Live approval responsibility changed."}),&mut changes).await?;
                Ok(((),changes))
            })).await?;
            if !routed.is_empty() {
                self.approval
                    .notices
                    .as_ref()
                    .ok_or(Error::Internal)?
                    .notify(
                        &mut tx,
                        &scope,
                        &routed,
                        &bilibi_notifications::Notice::new("Waiting for your approval")
                            .body("Approval responsibility changed. Review your waiting approvals.")
                            .url(format!("/workflow/approvals/{id}")),
                    )
                    .await?;
            }
        }
        let saved = request(
            &tx.sql()
                .query_one(
                    "SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id],
                )
                .await?,
        )?;
        for seat in &stage.seats {
            if tx.sql().query_one("SELECT EXISTS(SELECT 1 FROM base_workflow_approval_decisions WHERE tenant_id=$1 AND request_id=$2 AND stage_key=$3 AND seat_key=$4)",&[&tenant,&id,&stage.key,&seat.key]).await?.get::<_,bool>(0){continue;}
            let escalated:bool=tx.sql().query_one("SELECT EXISTS(SELECT 1 FROM base_workflow_approval_notices WHERE tenant_id=$1 AND request_id=$2 AND stage_key=$3 AND seat_key=$4 AND notice_key='escalated')",&[&tenant,&id,&stage.key,&seat.key]).await?.get(0);
            let escalate = !escalated
                && seat
                    .escalation_hours
                    .is_some_and(|hours| elapsed >= i64::from(hours));
            let reminder = seat
                .reminder_hours
                .filter(|hours| elapsed >= i64::from(*hours))
                .map(|hours| elapsed / i64::from(hours));
            if !escalate && reminder.is_none() {
                continue;
            }
            let notice_key = if escalate {
                "escalated".into()
            } else {
                format!("reminder:{}", reminder.unwrap_or(0))
            };
            let selector = if escalate || escalated {
                seat.escalation.as_ref().ok_or(Error::Invalid)?
            } else {
                &seat.selector
            };
            let (_, live_policy) = self
                .current_policy(tx.sql(), &scope, facts.company, case.key)
                .await?;
            let recipients = self
                .approval_recipients(
                    tx.sql(),
                    &scope,
                    RecipientContext {
                        case: &case,
                        subject: &subject,
                        facts: &facts,
                        saved: &saved,
                        seat,
                        selector,
                        allow_preparer: policy.allow_preparer
                            && live_policy.allow_preparer
                            && case.allow_preparer,
                        distinct_stages: policy.distinct_stages
                            || live_policy.distinct_stages
                            || case.distinct_stages,
                    },
                )
                .await?;
            let saved_id = saved.id;
            let stage_key = stage.key.clone();
            let seat_key = seat.key.clone();
            let route = saved.routing_version;
            let notice_scope = scope.clone();
            let key = notice_key.clone();
            let notice=tx.mutate(move |sql|Box::pin(async move {
                let receipt=sql.query_opt("INSERT INTO base_workflow_approval_notices(tenant_id,request_id,stage_key,seat_key,routing_version,notice_key) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING RETURNING id",&[&tenant,&saved_id,&stage_key,&seat_key,&route,&key]).await?;
                let mut changes=Vec::new();
                let inserted=if let Some(row)=receipt {let receipt:i64=row.get(0);changes.push(change("workflow.approval_notice",receipt,json!({"request":saved_id,"notice":key,"stage":stage_key,"seat":seat_key})));true}else{changes.push(change("workflow.approval_maintenance",saved_id,json!({"notice_already_recorded":true})));false};
                if inserted&&escalate {
                    sql.execute("UPDATE base_workflow_approval_requests SET routing_version=routing_version+1,version=version+1 WHERE tenant_id=$1 AND id=$2",&[&tenant,&saved_id]).await?;
                    changes.push(updated("workflow.approval_request",saved_id,json!({"routing_version":route}),json!({"routing_version":route+1})));
                    event(sql,&notice_scope,saved_id,"escalated",json!({"stage":stage_key,"seat":seat_key,"previous_routing_version":route}),&mut changes).await?;
                }
                Ok((inserted,changes))
            })).await?;
            if notice && !recipients.is_empty() {
                self.approval
                    .notices
                    .as_ref()
                    .ok_or(Error::Internal)?
                    .notify(
                        &mut tx,
                        &scope,
                        &recipients,
                        &bilibi_notifications::Notice::new("Waiting for your approval")
                            .body("Review your waiting approvals.")
                            .url(format!("/workflow/approvals/{id}"))
                            .icon("check"),
                    )
                    .await?;
            }
        }
        tx.commit().await
    }
}
inventory::submit! {bilibi_module_registry::Contribution::new(bilibi_authz::VOCABULARY,"workflow.approval.maintenance.vocabulary",bilibi_module_registry::Provenance{module:"base/workflow",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution{domains:&[],verbs:&["maintain"],capabilities:&[bilibi_authz::CapabilityDefinition::tenant(MAINTAIN)],roles:&[],system_principals:&[bilibi_authz::SystemPrincipalDefinition{name:PRINCIPAL,capabilities:&[MAINTAIN]}]})}

impl Engine {
    pub async fn approval_delegations(
        &self,
        scope: &Scope,
        case: &str,
        company: i64,
        after: i64,
        limit: i64,
    ) -> Result<Vec<Value>> {
        if !(1..=100).contains(&limit) || after < 0 {
            return Err(Error::Invalid);
        }
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let scope = self.live(&sql, scope).await?;
        self.approval_identities(&sql, &scope).await?;
        let declaration = self.approval_case(case)?;
        bilibi_authz::authorize_key(&*sql, &scope, declaration.delegation_capability).await?;
        self.adapter(case)?
            .authorize_company(&sql, &scope, company, ApprovalAccess::Delegate)
            .await?;
        let rows=sql.query("SELECT id,version,delegate,to_char(starts_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'),to_char(ends_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'),revoked_at IS NOT NULL FROM base_workflow_approval_delegations WHERE tenant_id=$1 AND company_id=$2 AND case_key=$3 AND grantor=$4 AND id>$5 ORDER BY id LIMIT $6",&[&scope.tenant(),&company,&case,&scope.user(),&after,&limit]).await?;
        let values=rows.iter().map(|r|json!({"id":r.get::<_,i64>(0),"version":r.get::<_,i64>(1),"delegate":r.get::<_,i64>(2),"starts":r.get::<_,String>(3),"ends":r.get::<_,String>(4),"revoked":r.get::<_,bool>(5)})).collect();
        sql.commit().await?;
        Ok(values)
    }
}

impl Engine {
    async fn approval_recipients(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        c: RecipientContext<'_>,
    ) -> Result<Vec<i64>> {
        let adapter = self.adapter(c.case.key)?;
        let mut excluded = self.excluded(sql, scope, c.saved).await?;
        excluded.extend(&c.facts.beneficiaries);
        let prior=sql.query("SELECT identities,stage_key FROM base_workflow_approval_decisions WHERE tenant_id=$1 AND request_id=$2",&[&scope.tenant(),&c.saved.id]).await?.into_iter().map(|r|Ok((serde_json::from_value::<ApprovalIdentities>(r.get(0)).map_err(|_|Error::Invalid)?,r.get::<_,String>(1)))).collect::<Result<Vec<_>>>()?;
        let mut recipients = Vec::new();
        let mut after = 0_i64;
        loop {
            let candidates =
                bilibi_authz::holder_page(sql, scope, after, &c.seat.capability, 100).await?;
            for account in &candidates {
                after = *account;
                let identity = match self
                    .authentication
                    .account_identity(sql, scope, *account)
                    .await
                {
                    Ok(i) => i,
                    Err(Error::Unauthorized | Error::NotFound | Error::Forbidden) => continue,
                    Err(e) => return Err(e),
                };
                if !adapter
                    .candidate(sql, scope, c.subject, c.facts.company, *account, &identity)
                    .await?
                {
                    continue;
                }
                let mut own = ApprovalIdentities::default();
                own.accounts.insert(*account);
                if let Some(person) = identity.person {
                    own.people.insert(person.get());
                }
                let mut responsibility = Vec::new();
                if self
                    .matches_selector(sql, scope, c.case, c.subject, c.selector, *account)
                    .await?
                {
                    responsibility.push(own.clone());
                }
                if c.case.delegation && c.seat.delegation {
                    let covers=sql.query("SELECT grantor FROM base_workflow_approval_delegations WHERE tenant_id=$1 AND company_id=$2 AND case_key=$3 AND delegate=$4 AND revoked_at IS NULL AND starts_at<=now() AND ends_at>now() ORDER BY id LIMIT 100",&[&scope.tenant(),&c.facts.company,&c.case.key,account]).await?;
                    for cover in covers {
                        let grantor: i64 = cover.get(0);
                        let identity = match self
                            .authentication
                            .account_identity(sql, scope, grantor)
                            .await
                        {
                            Ok(i) => i,
                            Err(Error::Unauthorized | Error::NotFound | Error::Forbidden) => {
                                continue
                            }
                            Err(e) => return Err(e),
                        };
                        if !bilibi_authz::effective_capabilities_of(sql, scope, grantor)
                            .await?
                            .allowed
                            .contains(c.seat.capability.as_str())
                            || !adapter
                                .candidate(
                                    sql,
                                    scope,
                                    c.subject,
                                    c.facts.company,
                                    grantor,
                                    &identity,
                                )
                                .await?
                            || !self
                                .matches_selector(
                                    sql, scope, c.case, c.subject, c.selector, grantor,
                                )
                                .await?
                        {
                            continue;
                        }
                        let mut ids = own.clone();
                        ids.accounts.insert(grantor);
                        if let Some(person) = identity.person {
                            ids.people.insert(person.get());
                        }
                        responsibility.push(ids);
                    }
                }
                if responsibility.iter().any(|ids| {
                    !ids.overlaps(&c.facts.beneficiaries)
                        && (c.allow_preparer || !ids.overlaps(&excluded))
                        && !prior.iter().any(|(previous, stage)| {
                            (stage == &c.saved.stages[c.saved.stage_index].key || c.distinct_stages)
                                && previous.overlaps(ids)
                        })
                }) {
                    recipients.push(*account);
                }
                if recipients.len() >= bilibi_notifications::MAX_RECIPIENTS {
                    break;
                }
            }
            if candidates.len() < 100 || recipients.len() >= bilibi_notifications::MAX_RECIPIENTS {
                break;
            }
        }
        Ok(recipients)
    }
    async fn notify_approval_stage_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        key: &str,
        subject: &crate::Subject,
        facts: &ApprovalSubject,
        id: i64,
    ) -> Result<()> {
        let case = self.approval_case(key)?;
        let row = tx
            .sql()
            .query_one(
                "SELECT * FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2",
                &[&scope.tenant(), &id],
            )
            .await?;
        let saved = request(&row)?;
        if saved.state != "awaiting" {
            return Ok(());
        }
        let stage = saved.stages.get(saved.stage_index).ok_or(Error::Invalid)?;
        let (_, policy) = self
            .current_policy(tx.sql(), scope, facts.company, key)
            .await?;
        let snapshot:ApprovalPolicy=serde_json::from_value(tx.sql().query_one("SELECT policy FROM base_workflow_approval_policies WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&saved.policy_id]).await?.get(0)).map_err(|_|Error::Invalid)?;
        let case = saved.enforce(case)?;
        let mut recipients = Vec::new();
        let mut seat_routes = serde_json::Map::new();
        for seat in &stage.seats {
            let mut holders = self
                .approval_recipients(
                    tx.sql(),
                    scope,
                    RecipientContext {
                        case: &case,
                        subject,
                        facts,
                        saved: &saved,
                        seat,
                        selector: &seat.selector,
                        allow_preparer: policy.allow_preparer
                            && snapshot.allow_preparer
                            && case.allow_preparer,
                        distinct_stages: policy.distinct_stages
                            || snapshot.distinct_stages
                            || case.distinct_stages,
                    },
                )
                .await?;
            holders.sort();
            holders.dedup();
            recipients.extend(&holders);
            seat_routes.insert(seat.key.clone(), json!(holders));
        }
        recipients.sort();
        recipients.dedup();
        recipients.truncate(bilibi_notifications::MAX_RECIPIENTS);
        let tenant = scope.tenant();
        let stage = stage.key.clone();
        let routing = saved.routing_version;
        let route_scope = scope.clone();
        let route_recipients = recipients.clone();
        let seat_routes = Value::Object(seat_routes);
        let receipt=tx.mutate(move |sql|Box::pin(async move{
            let row=sql.query_opt("INSERT INTO base_workflow_approval_notices(tenant_id,request_id,stage_key,seat_key,routing_version,notice_key) VALUES($1,$2,$3,'aggregate',$4,'activated') ON CONFLICT DO NOTHING RETURNING id",&[&tenant,&id,&stage,&routing]).await?;
            if let Some(row)=row {let receipt:i64=row.get(0);let mut changes=vec![change("workflow.approval_notice",receipt,json!({"request":id,"stage":stage,"notice":"activated"}))];event(sql,&route_scope,id,"routed",json!({"stage":stage,"recipients":route_recipients,"seats":seat_routes}),&mut changes).await?;Ok((true,changes))}
            else{Ok((false,vec![change("workflow.approval_notice_replay",id,json!({"request":id,"stage":stage}))]))}
        })).await?;
        if receipt && !recipients.is_empty() {
            self.approval
                .notices
                .as_ref()
                .ok_or(Error::Internal)?
                .notify(
                    tx,
                    scope,
                    &recipients,
                    &bilibi_notifications::Notice::new("Waiting for your approval")
                        .body("Review your waiting approvals.")
                        .url(format!("/workflow/approvals/{id}"))
                        .icon("check"),
                )
                .await?;
        }
        Ok(())
    }
}
