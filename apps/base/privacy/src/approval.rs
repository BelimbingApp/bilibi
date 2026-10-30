//! Erasure approval (ADR 05, ADR 15; owner decision of 2026-10-29): an erasure
//! is approved through Workflow's shared approval case `privacy.erasure`, not
//! by Privacy itself. Workflow routes the request, keeps the policy, waiting
//! list and history, and decides who may approve: a person with
//! `privacy.request.approve`, directly or through their AI agent, with a
//! fresh Step-up `base.privacy.erasure.approve` on every decision
//! (`.always()`); never a system principal or a queued job, and never anyone the case excludes (the data subject and every
//! account linked to them, whoever received or verified the request, and an
//! operator who impersonated them). Privacy keeps the execution: on the final
//! approval it starts every store's job in the same transaction. No Bilimbi
//! counterpart.
use crate::{
    requests::{self, RequestKind, RequestView, SUBJECT},
    Privacy, APPROVAL_MANAGE, REQUEST_APPROVE, REQUEST_RECEIVE, REQUEST_VIEW, STEP_UP_ERASURE,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{AccountIdentity, Authentication, PartyProofs, Scope};
use bilibi_workflow::{
    ApprovalAccess, ApprovalAdapter, ApprovalCase, ApprovalDecisionAction, ApprovalIdentities,
    ApprovalMode, ApprovalOutcome, ApprovalPolicy, ApprovalSeat, ApprovalSelector, ApprovalStage,
    ApprovalSubject, ApprovalSummary, OwnerFuture, Subject,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio_postgres::Transaction;

/// The case key, also its fixed wait process.
pub const ERASURE_CASE: &str = "privacy.erasure";
/// The decision action and its operation contract.
pub const DECIDE: &str = "privacy.erasure.decide";
pub const DECIDE_CONTRACT: &str = "base.privacy.erasure.decide";

/// Protected like Workflow's strict reference case: one independent decision
/// with a fresh Step-up check every time, no cover and no impersonation
/// (Queue refuses an impersonated write, so the store jobs could not start). A reason is asked
/// only to reject: approval records are not yet a personal-data store, so
/// they should hold as little free text as possible.
pub(crate) const ERASURE: ApprovalCase = ApprovalCase {
    key: ERASURE_CASE,
    label: "Personal-data erasure",
    subject: SUBJECT,
    process: ERASURE_CASE,
    decisions: &[ApprovalDecisionAction {
        label: "Erasure approval",
        key: DECIDE,
        capability: REQUEST_APPROVE,
        contract: DECIDE_CONTRACT,
    }],
    default_policy,
    recall_capability: REQUEST_RECEIVE,
    delegation_capability: REQUEST_APPROVE,
    policy_capability: APPROVAL_MANAGE,
    review_capability: APPROVAL_MANAGE,
    policy_contract: "base.privacy.erasure.rules",
    recall_contract: "base.privacy.erasure.recall",
    delegation_contract: "base.privacy.erasure.delegate",
    revoke_contract: "base.privacy.erasure.revoke",
    review_contract: "base.privacy.erasure.review",
    facts: &[],
    relationships: &[],
    required_capabilities: &[REQUEST_APPROVE],
    distinct_stages: true,
    approval_reason: false,
    reason_characters: 1000,
    reason_bytes: 4000,
    allow_preparer: false,
    delegation: false,
    forbid_impersonation: true,
    step_up: Some(STEP_UP_ERASURE),
};
bilibi_workflow::declare_approval_case!(contracts, "base/privacy", ERASURE,
    policy: "base.privacy.erasure.rules", recall: "base.privacy.erasure.recall",
    delegation: "base.privacy.erasure.delegate", revoke: "base.privacy.erasure.revoke",
    review: "base.privacy.erasure.review", step_up: &[STEP_UP_ERASURE],
    decision_input: || serde_json::json!({"type": "object", "properties": {}}),
    decisions: [(DECISION, "base.privacy.erasure.decide", "privacy.erasure.decide", "privacy.request.approve")]);

/// The erasure case's default rules, for an operator or installation step
/// that publishes them (`Engine::publish_approval_policy_in`).
pub fn erasure_rules() -> ApprovalPolicy {
    default_policy(&ERASURE)
}
/// One step, one approver holding `privacy.request.approve`. Installed only
/// when an operator publishes it on Approval rules.
fn default_policy(case: &ApprovalCase) -> ApprovalPolicy {
    ApprovalPolicy {
        stages: vec![ApprovalStage {
            key: "approval".into(),
            label: "Approval".into(),
            mode: ApprovalMode::All,
            seats: vec![ApprovalSeat {
                key: "approver".into(),
                label: "Approver".into(),
                capability: case.decisions[0].capability.into(),
                selector: ApprovalSelector::Capability,
                delegation: case.delegation,
                reminder_hours: Some(24),
                escalation_hours: None,
                escalation: None,
            }],
        }],
        rules: vec![],
        distinct_stages: case.distinct_stages,
        approval_reason: case.approval_reason,
        allow_preparer: false,
    }
}

tokio::task_local! {
    /// The erasure request whose approval is being applied. Only the owner's
    /// `apply_in` sets it, so a Workflow transition from any other caller can
    /// never move an erasure to `approved`.
    static APPLYING: (i64, i64);
}
/// Whether this task is applying the final approval of `tenant`'s request `id`.
pub(crate) fn applying(tenant: i64, id: i64) -> bool {
    APPLYING
        .try_with(|current| *current == (tenant, id))
        .unwrap_or(false)
}

/// Binds the case's adapter and contracts; called once by the module.
pub(crate) fn bind(
    engine: Arc<bilibi_workflow::Engine>,
    privacy: Arc<Privacy>,
    authentication: Arc<Authentication>,
) -> bilibi_module_registry::Result<()> {
    let parties = privacy.parties.clone();
    engine.bind_approval_case(
        "base/privacy",
        ERASURE_CASE,
        Arc::new(Erasures {
            privacy,
            parties,
            authentication,
        }),
    )?;
    contracts::bind(engine)
}

struct Erasures {
    privacy: Arc<Privacy>,
    parties: Arc<PartyProofs>,
    authentication: Arc<Authentication>,
}
/// The capability each approval access needs at the request's company.
fn capability(access: ApprovalAccess) -> &'static str {
    match access {
        ApprovalAccess::Read => REQUEST_VIEW,
        ApprovalAccess::Submit | ApprovalAccess::Recall => REQUEST_RECEIVE,
        ApprovalAccess::Decide | ApprovalAccess::Delegate => REQUEST_APPROVE,
        ApprovalAccess::Policy | ApprovalAccess::Review => APPROVAL_MANAGE,
    }
}
/// What Workflow routes on: the request's company and version, whoever
/// received or verified it (and impersonated them), and the data subject,
/// whose every linked account is excluded. No personal value is summarised.
fn routed(request: &RequestView) -> ApprovalSubject {
    let mut preparers = ApprovalIdentities::default();
    preparers.accounts.extend(
        [
            Some(request.received_by),
            request.verified_by,
            request.received_impersonator,
            request.verified_impersonator,
        ]
        .into_iter()
        .flatten(),
    );
    let mut beneficiaries = ApprovalIdentities::default();
    beneficiaries.people.insert(request.person.get());
    ApprovalSubject {
        company: request.company.get(),
        revision: format!("v{}", request.version),
        facts: BTreeMap::new(),
        preparers,
        beneficiaries,
        label: format!("Erasure request #{}", request.id),
        url: format!("/privacy/requests/{}", request.id),
        summary: vec![
            ApprovalSummary {
                label: "Person".into(),
                value: request.person.get().to_string(),
            },
            ApprovalSummary {
                label: "Due".into(),
                // `YYYY-MM-DDTHH:MM:SSZ`, shown to the minute.
                value: format!(
                    "{} UTC",
                    request
                        .due_at
                        .get(..16)
                        .unwrap_or(&request.due_at)
                        .replacen('T', " ", 1)
                ),
            },
        ],
    }
}
/// An erasure that can still be decided: verified, with its round open.
fn open(request: &RequestView, awaiting: bool) -> Result<()> {
    if request.kind != RequestKind::Erasure
        || request.status != "verified"
        || request.approval_awaiting != awaiting
    {
        return Err(Error::Conflict);
    }
    Ok(())
}
impl Erasures {
    /// Whether `account` may decide at `company`, by the rule its own live
    /// scope meets: the same eligibility the approver faces.
    async fn reaches(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        company: i64,
        account: i64,
    ) -> Result<bool> {
        let other = match self
            .authentication
            .authenticate(sql, scope.tenant(), account, String::new())
            .await
        {
            Ok(other) => other,
            Err(Error::Unauthorized | Error::Forbidden | Error::NotFound) => return Ok(false),
            Err(error) => return Err(error),
        };
        let company = OrganisationId::new(company).map_err(|_| Error::Invalid)?;
        match requests::eligible_with(&self.parties, sql, &other, company, REQUEST_APPROVE).await {
            Ok(()) => Ok(true),
            Err(Error::Forbidden | Error::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
}
impl ApprovalAdapter for Erasures {
    fn authorize_company<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        company: i64,
        access: ApprovalAccess,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            let company = OrganisationId::new(company).map_err(|_| Error::Invalid)?;
            requests::eligible_with(&self.parties, sql, scope, company, capability(access)).await
        })
    }
    fn inspect<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        access: ApprovalAccess,
    ) -> OwnerFuture<'a, ApprovalSubject> {
        Box::pin(async move {
            let request = requests::load(sql, scope.tenant(), subject.id, false).await?;
            if request.kind != RequestKind::Erasure {
                return Err(Error::NotFound);
            }
            self.authorize_company(sql, scope, request.company.get(), access)
                .await?;
            match access {
                ApprovalAccess::Submit => open(&request, false)?,
                ApprovalAccess::Decide | ApprovalAccess::Recall => open(&request, true)?,
                _ => {}
            }
            Ok(routed(&request))
        })
    }
    fn cover_candidate<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        company: i64,
        account: i64,
        _identity: &'a AccountIdentity,
    ) -> OwnerFuture<'a, bool> {
        Box::pin(self.reaches(sql, scope, company, account))
    }
    fn candidate<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        _subject: &'a Subject,
        company: i64,
        account: i64,
        _identity: &'a AccountIdentity,
    ) -> OwnerFuture<'a, bool> {
        Box::pin(self.reaches(sql, scope, company, account))
    }
    fn maintenance<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a bilibi_authz::SystemScope,
        subject: &'a Subject,
    ) -> OwnerFuture<'a, ApprovalSubject> {
        Box::pin(async move {
            let request = requests::load(sql, scope.tenant(), subject.id, false).await?;
            if request.kind != RequestKind::Erasure {
                return Err(Error::NotFound);
            }
            Ok(routed(&request))
        })
    }
    fn relationship<'a>(
        &'a self,
        _sql: &'a Transaction<'_>,
        _scope: &'a Scope,
        _subject: &'a Subject,
        _key: &'a str,
        _account: i64,
    ) -> OwnerFuture<'a, bool> {
        // The case declares no reporting relationship.
        Box::pin(async { Err(Error::Invalid) })
    }
    fn authorize_control<'a>(
        &'a self,
        _sql: &'a Transaction<'_>,
        _scope: &'a Scope,
        _subject: &'a Subject,
        _kind: &'a str,
        _control_reference: &'a str,
    ) -> OwnerFuture<'a, ()> {
        // No business control of an erasure may be overridden.
        Box::pin(async { Err(Error::Forbidden) })
    }
    fn validate_saved<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        _identities: &'a ApprovalIdentities,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            let request = requests::load(sql, scope.tenant(), subject.id, true).await?;
            open(&request, true)
        })
    }
    fn apply_in<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        outcome: ApprovalOutcome,
        _reason: &'a str,
        _input: &'a serde_json::Value,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            let (tenant, id) = (scope.tenant(), subject.id);
            let before = requests::load(tx.sql(), tenant, id, true).await?;
            open(&before, true)?;
            // The round is over whatever its outcome. A rejected or recalled
            // erasure goes back to whoever received it with a new version, so
            // a fresh round can follow; refusing the data subject stays their
            // decision, with their own reason.
            let next = i64::from(outcome != ApprovalOutcome::Approved);
            tx.mutate(move |sql| {
                Box::pin(async move {
                    sql.execute(
                        "UPDATE base_privacy_requests SET approval_awaiting=false,version=version+$3 WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &id, &next],
                    )
                    .await?;
                    let after = requests::load(sql, tenant, id, false).await?;
                    let change = Change {
                        kind: requests::REQUEST_KIND.into(),
                        id,
                        old: requests::evidence(&before),
                        new: requests::evidence(&after),
                    };
                    Ok(((), vec![change]))
                })
            })
            .await?;
            if outcome == ApprovalOutcome::Approved {
                APPLYING
                    .scope((tenant, id), self.privacy.run_erasure_in(tx, scope, id))
                    .await?;
            }
            Ok(())
        })
    }
}
