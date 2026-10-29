//! Shared approval cases mirror Bilimbi Workflow's owner-proven human gate.
//! Business meaning remains in the bound owner; Base never queries its tables.
use crate::{ApprovalFact, ApprovalFactKind, OwnerFuture, Subject};
use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::Transaction;

/// Static action keys belong to the contributing owner, as do the operation
/// contracts emitted beside the case. No universal approval capability exists.
#[derive(Clone, Copy, Debug)]
pub struct ApprovalDecisionAction {
    pub label: &'static str,
    pub key: &'static str,
    pub capability: &'static str,
    pub contract: &'static str,
}
#[derive(Clone, Copy, Debug)]
pub struct ApprovalCase {
    pub key: &'static str,
    pub label: &'static str,
    pub subject: &'static str,
    pub process: &'static str,
    pub decisions: &'static [ApprovalDecisionAction],
    /// Pure owner defaults; validated at boot and installed only explicitly.
    pub default_policy: fn(&ApprovalCase) -> crate::ApprovalPolicy,
    pub recall_capability: &'static str,
    pub delegation_capability: &'static str,
    pub policy_capability: &'static str,
    pub review_capability: &'static str,
    pub policy_contract: &'static str,
    pub recall_contract: &'static str,
    pub delegation_contract: &'static str,
    pub revoke_contract: &'static str,
    pub review_contract: &'static str,
    pub facts: &'static [(&'static str, ApprovalFactKind)],
    pub relationships: &'static [&'static str],
    pub required_capabilities: &'static [&'static str],
    pub distinct_stages: bool,
    pub approval_reason: bool,
    pub reason_characters: usize,
    pub reason_bytes: usize,
    pub allow_preparer: bool,
    pub delegation: bool,
    pub forbid_impersonation: bool,
    pub step_up: Option<&'static str>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalIdentities {
    pub accounts: BTreeSet<i64>,
    pub people: BTreeSet<i64>,
}
impl ApprovalIdentities {
    pub fn overlaps(&self, other: &Self) -> bool {
        !self.accounts.is_disjoint(&other.accounts) || !self.people.is_disjoint(&other.people)
    }
    pub(crate) fn extend(&mut self, other: &Self) {
        self.accounts.extend(&other.accounts);
        self.people.extend(&other.people);
    }
    pub(crate) fn valid(&self) -> bool {
        self.accounts.iter().chain(&self.people).all(|id| *id > 0)
    }
}
/// Supplied only by the locked owner, never from a decision payload. Complete
/// preparer sets are stored as participant rows rather than bounded JSON input.
#[derive(Clone, Debug)]
pub struct ApprovalSubject {
    pub company: i64,
    pub revision: String,
    pub facts: BTreeMap<String, ApprovalFact>,
    pub preparers: ApprovalIdentities,
    pub beneficiaries: ApprovalIdentities,
    pub label: String,
    pub url: String,
    /// Only owner-authorized, field-masked values for the decision summary.
    pub summary: Vec<ApprovalSummary>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalSummary {
    pub label: String,
    pub value: String,
}
#[derive(Clone, Copy, Debug)]
pub enum ApprovalAccess {
    Read,
    Submit,
    Decide,
    Recall,
    Policy,
    Delegate,
    Review,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    Rejected,
    Recalled,
}
impl ApprovalOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Recalled => "recalled",
        }
    }
}
/// Bind one adapter during the owner's service installation. Every call uses
/// the handed connection. `apply_in` calls the owner's existing `_in` methods,
/// opens no transaction, performs no external I/O and captures each change once.
pub trait ApprovalAdapter: Send + Sync {
    fn authorize_company<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        company: i64,
        access: ApprovalAccess,
    ) -> OwnerFuture<'a, ()>;
    fn inspect<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        access: ApprovalAccess,
    ) -> OwnerFuture<'a, ApprovalSubject>;
    /// Eligibility of another live account in this company. This is a routing
    /// read, never a replacement for the acting person's owner authorization.
    fn cover_candidate<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        company: i64,
        account: i64,
        identity: &'a bilibi_tenancy::AccountIdentity,
    ) -> OwnerFuture<'a, bool>;
    fn candidate<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        company: i64,
        account: i64,
        identity: &'a bilibi_tenancy::AccountIdentity,
    ) -> OwnerFuture<'a, bool>;
    /// Safe routing facts for named maintenance only; no business effects.
    fn maintenance<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a bilibi_authz::SystemScope,
        subject: &'a Subject,
    ) -> OwnerFuture<'a, ApprovalSubject>;
    fn relationship<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        key: &'a str,
        account: i64,
    ) -> OwnerFuture<'a, bool>;
    /// Recheck hard business/security floors for all saved decision identities
    /// immediately before final application. Ordinary role removal does not
    /// retrospectively erase a valid historical decision.
    fn authorize_control<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        kind: &'a str,
        control_reference: &'a str,
    ) -> OwnerFuture<'a, ()>;
    fn validate_saved<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        identities: &'a ApprovalIdentities,
    ) -> OwnerFuture<'a, ()>;
    fn apply_in<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        outcome: ApprovalOutcome,
        reason: &'a str,
        input: &'a serde_json::Value,
    ) -> OwnerFuture<'a, ()>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalVote {
    pub request: i64,
    pub version: i64,
    pub stage: String,
    pub seat: String,
    pub routing_version: i64,
    pub approve: bool,
    pub reason: String,
    pub input: serde_json::Value,
}
pub(crate) struct Unreachable;
pub(crate) const WAIT_STEPS: &[crate::Step] = &[crate::Step::new("approval.wait", "Approval")];
pub(crate) static UNREACHABLE: Unreachable = Unreachable;
impl crate::HumanActionHandler for Unreachable {
    fn handle<'a>(
        &'a self,
        _: &'a Transaction<'_>,
        _: &'a Scope,
        _: &'a Subject,
        _: &'a crate::HumanAction,
        _: &'a crate::ActionRequest,
    ) -> OwnerFuture<'a, crate::HumanOutcome> {
        Box::pin(async { Err(Error::Forbidden) })
    }
}
impl crate::ProcessAdapter for Unreachable {
    fn authorize<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        run: &'a crate::RunFacts,
        operation: crate::Operation,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            // No signal, waiver, lease, generic completion or administrator
            // command can manufacture approval proof or terminate the wait.
            if !matches!(
                operation,
                crate::Operation::StartRun
                    | crate::Operation::ReadRun
                    | crate::Operation::ReconcileRun
            ) {
                return Err(Error::Forbidden);
            }
            let request = run
                .input
                .get("approval_request")
                .and_then(serde_json::Value::as_i64)
                .ok_or(Error::Invalid)?;
            if sql.query_opt("SELECT id FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2 AND subject_id=$3 AND subject_type=$4 AND case_key=$5 AND (NOT $6::bool OR (run_id IS NULL AND state='awaiting'))", &[&scope.tenant(), &request, &subject.id, &run.subject_type, &run.definition_key,&matches!(operation,crate::Operation::StartRun)]).await?.is_none() {
                return Err(Error::Forbidden);
            }
            Ok(())
        })
    }
}

pub(crate) fn check_reason(case: &ApprovalCase, reason: &str, required: bool) -> Result<()> {
    if (required && reason.trim().is_empty())
        || reason.contains('\0')
        || reason.chars().count() > case.reason_characters
        || reason.len() > case.reason_bytes
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
pub(crate) fn check_facts(case: &ApprovalCase, subject: &ApprovalSubject) -> Result<()> {
    if subject.company <= 0
        || !crate::approval_policy::label(&subject.revision)
        || !subject.preparers.valid()
        || !subject.beneficiaries.valid()
        || !crate::approval_policy::label(&subject.label)
        || !bilibi_notifications::local_path(&subject.url)
        || subject.summary.len() > 32
        || subject.summary.iter().any(|s| {
            !crate::approval_policy::label(&s.label)
                || s.value.len() > 4096
                || s.value.contains('\0')
        })
        || subject.facts.values().any(|v| !v.valid())
        || subject.facts.len() != case.facts.len()
        || case
            .facts
            .iter()
            .any(|(key, kind)| subject.facts.get(*key).is_none_or(|v| v.kind() != *kind))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
pub(crate) fn validate_case(case: &ApprovalCase) -> Result<()> {
    if !crate::definitions::key(case.key)
        || case.process != case.key
        || !crate::definitions::key(case.subject)
        || !crate::approval_policy::label(case.label)
        || case.decisions.is_empty()
        || case.decisions.len() > 32
        || case.reason_characters > 4000
        || case.reason_characters == 0
        || case.reason_bytes == 0
    {
        return Err(Error::Invalid);
    }
    let mut keys = BTreeSet::new();
    for action in case.decisions {
        if !crate::definitions::key(action.key)
            || !crate::definitions::key(action.contract)
            || !crate::approval_policy::label(action.label)
            || !keys.insert(action.key)
            || !crate::definitions::key(action.capability)
        {
            return Err(Error::Invalid);
        }
    }
    let mut facts = BTreeSet::new();
    for (key, _) in case.facts {
        if !crate::definitions::key(key) || !facts.insert(*key) {
            return Err(Error::Invalid);
        }
    }
    let mut relationships = BTreeSet::new();
    for key in case.relationships {
        if !crate::definitions::key(key) || !relationships.insert(*key) {
            return Err(Error::Invalid);
        }
    }
    for key in case.required_capabilities {
        if !case.decisions.iter().any(|a| a.capability == *key) {
            return Err(Error::Invalid);
        }
    }
    (case.default_policy)(case).validate(case)?;
    Ok(())
}
