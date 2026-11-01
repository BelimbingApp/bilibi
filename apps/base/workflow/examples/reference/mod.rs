//! The disposable reference owner: a bounded, non-business `reference.record`
//! subject with its guard, action, listeners, flows, processes and human
//! actions. Production never contributes it. It is shared by the `owner`
//! command specimen beside this file and by the Web fixture executable
//! `apps/web/examples/w7-workflow-reference.rs`, which serves
//! `/workflow/reference/{id}` over the same disposable database.
use bilibi_database::{Change, Error, Result};
use bilibi_module_registry::{Contribution as Registration, Provenance};
use bilibi_tenancy::Scope;
use bilibi_workflow::{
    Action, ActionRequest, Contribution, Dependency, DependencyMode, Edge, Flow, Guard,
    HumanAction, HumanActionHandler, HumanOutcome, Load, Operation, OwnerFuture, Process,
    ProcessAdapter, Request, RunFacts, Status, Step, Subject, SubjectAdapter, Transition,
    CONTRIBUTIONS,
};
use serde_json::json;
use tokio_postgres::Transaction;
mod followup;
struct Owner;
impl SubjectAdapter for Owner {
    fn maintenance_load<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a bilibi_authz::SystemScope,
        id: i64,
    ) -> OwnerFuture<'a, Subject> {
        Box::pin(async move {
            let row=tx.query_opt("SELECT status,version FROM workflow_reference_records WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&scope.tenant(),&id]).await?.ok_or(Error::NotFound)?;
            Ok(Subject {
                id,
                tenant: scope.tenant(),
                status: row.get(0),
                version: row.get(1),
                facts: json!({}),
            })
        })
    }
    fn load<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        id: i64,
        mode: Load,
    ) -> OwnerFuture<'a, Subject> {
        Box::pin(async move {
            let sql=match mode { Load::Read=>"SELECT status,version FROM workflow_reference_records WHERE tenant_id=$1 AND id=$2",Load::Lock=>"SELECT status,version FROM workflow_reference_records WHERE tenant_id=$1 AND id=$2 FOR UPDATE"};
            let row = tx
                .query_opt(sql, &[&scope.tenant(), &id])
                .await?
                .ok_or(Error::NotFound)?;
            Ok(Subject {
                id,
                tenant: scope.tenant(),
                status: row.get(0),
                version: row.get(1),
                facts: json!({}),
            })
        })
    }
    fn authorize<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        _: &'a Subject,
        operation: Operation,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            bilibi_authz::authorize_key(
                tx,
                scope,
                if matches!(operation, Operation::Read | Operation::ReadRun) {
                    "companies.view"
                } else {
                    "companies.update"
                },
            )
            .await
        })
    }
    fn persist<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        to: &'a str,
        request: &'a Request,
    ) -> OwnerFuture<'a, Vec<Change>> {
        Box::pin(async move {
            let status = if request.input["bad_persist"] == true {
                "pending"
            } else {
                to
            };
            let row=tx.query_opt("UPDATE workflow_reference_records SET status=$1,version=version+1 WHERE tenant_id=$2 AND id=$3 AND status=$4 AND version=$5 RETURNING version",&[&status,&scope.tenant(),&subject.id,&subject.status,&subject.version]).await?.ok_or(Error::Conflict)?;
            Ok(vec![Change {
                kind: "reference.record".into(),
                id: subject.id,
                old: json!({"status":subject.status,"version":subject.version}),
                new: json!({"status":status,"version":row.get::<_,i64>(0)}),
            }])
        })
    }
}
// Two durable listeners: the second can refuse after its INSERT, so operators
// can see the first receipt survive and the second effect roll back.
struct SecondListener;
static SECOND_LISTENER: SecondListener = SecondListener;
impl bilibi_workflow::TransitionListener for Owner {
    fn handle<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a bilibi_authz::SystemScope,
        event: &'a bilibi_workflow::TransitionEvent,
    ) -> OwnerFuture<'a, Vec<Change>> {
        Box::pin(async move { listener_effect(tx, scope, event, "first").await })
    }
}
impl bilibi_workflow::TransitionListener for SecondListener {
    fn handle<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a bilibi_authz::SystemScope,
        event: &'a bilibi_workflow::TransitionEvent,
    ) -> OwnerFuture<'a, Vec<Change>> {
        Box::pin(async move {
            let changes = listener_effect(tx, scope, event, "second").await?;
            let row=tx.query_one("SELECT fail_second,wait_seconds FROM workflow_reference_listener_gate WHERE singleton=true",&[]).await?;
            let wait: i32 = row.get(1);
            tx.query_one("SELECT pg_sleep($1::integer)", &[&wait])
                .await?;
            if row.get::<_, bool>(0) {
                return Err(Error::Invalid);
            }
            Ok(changes)
        })
    }
}
async fn listener_effect(
    tx: &Transaction<'_>,
    scope: &bilibi_authz::SystemScope,
    event: &bilibi_workflow::TransitionEvent,
    listener: &str,
) -> Result<Vec<Change>> {
    let id:i64=tx.query_one("INSERT INTO workflow_reference_listener_effects(tenant_id,event_key,listener) VALUES($1,$2,$3) RETURNING id",&[&scope.tenant(),&event.event_key,&listener]).await?.get(0);
    Ok(vec![Change {
        kind: "reference.listener_effect".into(),
        id,
        old: json!(null),
        new: json!({"event_key":event.event_key,"listener":listener}),
    }])
}
impl Guard for Owner {
    fn check<'a>(
        &'a self,
        _: &'a Transaction<'_>,
        _: &'a Scope,
        _: &'a Subject,
        _: &'a Transition,
        request: &'a Request,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            if request.input["guard_deny"] == true {
                Err(Error::Forbidden)
            } else {
                Ok(())
            }
        })
    }
}
impl Action for Owner {
    fn execute<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        _: &'a Transition,
        request: &'a Request,
    ) -> OwnerFuture<'a, Vec<Change>> {
        Box::pin(async move {
            tx.execute("UPDATE workflow_reference_records SET action_count=action_count+1 WHERE tenant_id=$1 AND id=$2",&[&scope.tenant(),&subject.id]).await?;
            if request.input["action_fail"] == true {
                return Err(Error::Invalid);
            }
            if request.input["action_wait"] == true {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
            Ok(vec![Change {
                kind: "reference.action".into(),
                id: subject.id,
                old: json!({"increment":0}),
                new: json!({"increment":1}),
            }])
        })
    }
}
impl ProcessAdapter for Owner {
    fn authorize<'a>(
        &'a self,
        _: &'a Transaction<'_>,
        _: &'a Scope,
        subject: &'a Subject,
        run: &'a RunFacts,
        _: Operation,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            // Owner policy: a run belongs to the locked subject; an archived
            // reference record would be refused here.
            if run.subject_id != subject.id || run.input.get("deny") == Some(&json!(true)) {
                return Err(Error::Forbidden);
            }
            Ok(())
        })
    }
}
impl HumanActionHandler for Owner {
    fn handle<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        action: &'a HumanAction,
        request: &'a ActionRequest,
    ) -> OwnerFuture<'a, HumanOutcome> {
        Box::pin(async move {
            // The owner write: every row change increments the version, so a
            // page holding the old version cannot act again.
            let row = tx.query_opt("UPDATE workflow_reference_records SET action_count=action_count+1,version=version+1 WHERE tenant_id=$1 AND id=$2 AND version=$3 RETURNING version,action_count", &[&scope.tenant(), &subject.id, &subject.version]).await?.ok_or(Error::Conflict)?;
            let (version, count): (i64, i32) = (row.get(0), row.get(1));
            if request.payload.get("handler_wait") == Some(&json!(true)) {
                // Widens the window so a concurrent duplicate really overlaps.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            if request.payload.get("handler_fail") == Some(&json!(true)) {
                return Err(Error::Invalid);
            }
            Ok(HumanOutcome {
                outcome: "completed".into(),
                output: json!({"action":action.key,"action_count":count,"payload":request.payload}),
                result_ref: Some(format!("reference:{}:{}", subject.id, action.key)),
                changes: vec![Change {
                    kind: "reference.human_action".into(),
                    id: subject.id,
                    old: json!({"version":subject.version}),
                    new: json!({"version":version,"action_count":count}),
                }],
            })
        })
    }
}
static OWNER: Owner = Owner;
const REVIEW_V1: [Step; 3] = [
    Step::new("first", "First review"),
    Step::new("second", "Second review"),
    Step {
        dependencies: &[
            Dependency {
                step: "first",
                acceptable_outcomes: &[],
            },
            Dependency {
                step: "second",
                acceptable_outcomes: &["completed", "waived"],
            },
        ],
        ..Step::new("finish", "Finish")
    },
];
const REVIEW_V2: [Step; 4] = [
    REVIEW_V1[0],
    REVIEW_V1[1],
    REVIEW_V1[2],
    Step {
        dependencies: &[Dependency {
            step: "finish",
            acceptable_outcomes: &[],
        }],
        delay_seconds: 3600,
        ..Step::new("archive", "Archive")
    },
];
// A bounded graph for operating the lifecycle API against real durable facts.
const LIFECYCLE: [Step; 6] = [
    Step::new("first", "First review"),
    Step::new("second", "Second review"),
    Step {
        dependencies: &[
            Dependency {
                step: "first",
                acceptable_outcomes: &["accepted"],
            },
            Dependency {
                step: "second",
                acceptable_outcomes: &[],
            },
        ],
        ..Step::new("all", "Both reviews")
    },
    Step {
        dependencies: &[
            Dependency {
                step: "first",
                acceptable_outcomes: &["accepted"],
            },
            Dependency {
                step: "second",
                acceptable_outcomes: &[],
            },
        ],
        mode: DependencyMode::Any,
        ..Step::new("any", "Either review")
    },
    Step {
        dependencies: &[Dependency {
            step: "all",
            acceptable_outcomes: &[],
        }],
        required_signal: Some("review.approved"),
        ..Step::new("signal", "Approval signal")
    },
    Step {
        dependencies: &[
            Dependency {
                step: "any",
                acceptable_outcomes: &[],
            },
            Dependency {
                step: "signal",
                acceptable_outcomes: &[],
            },
        ],
        delay_seconds: 2,
        ..Step::new("delayed", "Delayed release")
    },
];
// Worker-leased steps: `fetch` allows two attempts, the others one.
const WORKER: [Step; 4] = [
    Step {
        max_attempts: 2,
        executor: Some("reference.fetch"),
        ..Step::new("fetch", "Fetch source")
    },
    Step {
        dependencies: &[Dependency {
            step: "fetch",
            acceptable_outcomes: &[],
        }],
        executor: Some("reference.render"),
        ..Step::new("render", "Render")
    },
    Step {
        dependencies: &[Dependency {
            step: "render",
            acceptable_outcomes: &["completed", "waived"],
        }],
        executor: Some("reference.publish"),
        ..Step::new("publish", "Publish")
    },
    Step {
        executor: Some("reference.notify"),
        ..Step::new("notify", "Notify")
    },
];
// A person submits `review` through a work-bound human action.
const HUMAN: [Step; 2] = [
    Step {
        executor: Some("reference.human.review"),
        ..Step::new("review", "Human review")
    },
    Step {
        dependencies: &[Dependency {
            step: "review",
            acceptable_outcomes: &[],
        }],
        executor: Some("reference.human.publish"),
        ..Step::new("publish", "Publish")
    },
];
/// Same key and version as the installed v1 with one changed step contract:
/// the running proof builds a second registry from it to show the refusal.
pub static DRIFTED_V1: Contribution = Contribution::Process(Process {
    key: "reference.review",
    version: 1,
    subject: "reference.record",
    adapter: &OWNER,
    steps: &[
        Step::new("first", "First review (renamed)"),
        REVIEW_V1[1],
        REVIEW_V1[2],
    ],
});
inventory::submit! { bilibi_module_registry::Module {
    id:"extensions/workflow_reference",layer:bilibi_module_registry::Layer::Extensions,
    dependencies:&["base/workflow","base/tenancy","base/step_up","base/notifications","base/queue","base/database"],services:&[bilibi_module_registry::Service{id:"extensions/workflow_reference.approvals",factory:|services|{
        let engine=services.get::<bilibi_workflow::Engine>("base/workflow.engine")?;
        let adapter=std::sync::Arc::new(ApprovalOwner{parties:services.get("base/tenancy.parties")?,notices:services.get("base/notifications.sink")?,queue:services.get("base/queue.store")?});
        engine.bind_approval_case("extensions/workflow_reference",APPROVAL_CASE.key,adapter.clone())?;
        engine.bind_approval_case("extensions/workflow_reference",STRICT_CASE.key,adapter)?;
        approval_contracts::bind(engine.clone())?;
        strict_contracts::bind(engine)?;Ok(std::sync::Arc::new(()))
    }},bilibi_module_registry::Service{id:"extensions/workflow_reference.followup",factory:|services|Ok(std::sync::Arc::new(bilibi_queue::HandlerService(std::sync::Arc::new(followup::Followup{db:services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),parties:services.get("base/tenancy.parties")?,engine:services.get(bilibi_workflow::SERVICE)?}))))}],
    migrations:&[("extensions-workflow-reference-001-records","CREATE TABLE workflow_reference_records(id bigint PRIMARY KEY,tenant_id bigint NOT NULL REFERENCES tenants(id),status text NOT NULL DEFAULT 'pending',version bigint NOT NULL DEFAULT 0,action_count integer NOT NULL DEFAULT 0,facade_value integer NOT NULL DEFAULT 0)"),
    ("extensions-workflow-reference-002-listeners","CREATE TABLE workflow_reference_listener_effects(id bigserial PRIMARY KEY,tenant_id bigint NOT NULL,event_key text NOT NULL,listener text NOT NULL); CREATE TABLE workflow_reference_listener_gate(singleton boolean PRIMARY KEY CHECK(singleton),fail_second boolean NOT NULL DEFAULT true,wait_seconds integer NOT NULL DEFAULT 0); INSERT INTO workflow_reference_listener_gate(singleton) VALUES(true)"),
    ("extensions-workflow-reference-003-approval-facts","CREATE TABLE workflow_reference_approval_facts(record_id bigint PRIMARY KEY,tenant_id bigint NOT NULL,company_id bigint NOT NULL,prepared_by bigint NOT NULL,manager_id bigint NOT NULL DEFAULT 1,amount text NOT NULL DEFAULT '12500.00',currency text NOT NULL DEFAULT 'MYR',resource_type text NOT NULL DEFAULT 'controlled',revision text NOT NULL DEFAULT '1',closed boolean NOT NULL DEFAULT false,fail_effect boolean NOT NULL DEFAULT false)"),
    ("extensions-workflow-reference-004-followups","CREATE TABLE workflow_reference_followups(id bigserial PRIMARY KEY,tenant_id bigint NOT NULL,record_id bigint NOT NULL,revision text NOT NULL,UNIQUE(tenant_id,record_id,revision))")],
} }
// The reference flow's production seed: `seed` inserts it without the
// operator's `initialize`, once per version.
inventory::submit! { Registration::new(bilibi_module_registry::contributions::SEEDS, "reference.flow.seed",
Provenance {module:"extensions/workflow_reference",source:file!(),line:line!()},
&bilibi_module_registry::contributions::Seed { version: "workflow-reference-approval-flow-v1",
    mode: bilibi_module_registry::contributions::SeedMode::Production,
    action: bilibi_module_registry::contributions::SeedAction::Service { service: bilibi_workflow::FLOW_SEEDS, operation: "reference.approval" } }) }
macro_rules! register {($id:literal,$value:expr)=>{inventory::submit! { Registration::new(CONTRIBUTIONS,$id,Provenance {module:"extensions/workflow_reference",source:file!(),line:line!()},&$value) }};}
register!(
    "reference.listener.first",
    Contribution::Listener(bilibi_workflow::Listener {
        key: "reference.first",
        subjects: &["reference.record"],
        adapter: &OWNER
    })
);
register!(
    "reference.listener.second",
    Contribution::Listener(bilibi_workflow::Listener {
        key: "reference.second",
        subjects: &["reference.record"],
        adapter: &SECOND_LISTENER
    })
);
register!(
    "reference.subject",
    Contribution::Subject {
        key: "reference.record",
        aliases: &["ReferenceRecord"],
        adapter: &OWNER
    }
);
register!(
    "reference.guard",
    Contribution::Guard {
        key: "reference.ready",
        aliases: &[],
        adapter: &OWNER
    }
);
register!(
    "reference.action",
    Contribution::Action {
        key: "reference.approved",
        aliases: &[],
        adapter: &OWNER
    }
);
register!(
    "reference.process.v1",
    Contribution::Process(Process {
        key: "reference.review",
        version: 1,
        subject: "reference.record",
        adapter: &OWNER,
        steps: &REVIEW_V1,
    })
);
register!(
    "reference.process.v2",
    Contribution::Process(Process {
        key: "reference.review",
        version: 2,
        subject: "reference.record",
        adapter: &OWNER,
        steps: &REVIEW_V2,
    })
);
register!(
    "reference.process.lifecycle",
    Contribution::Process(Process {
        key: "reference.lifecycle",
        version: 1,
        subject: "reference.record",
        adapter: &OWNER,
        steps: &LIFECYCLE,
    })
);
register!(
    "reference.process.worker",
    Contribution::Process(Process {
        key: "reference.worker",
        version: 1,
        subject: "reference.record",
        adapter: &OWNER,
        steps: &WORKER,
    })
);
register!(
    "reference.process.human",
    Contribution::Process(Process {
        key: "reference.human",
        version: 1,
        subject: "reference.record",
        adapter: &OWNER,
        steps: &HUMAN,
    })
);
register!(
    "reference.human.acknowledge",
    Contribution::HumanAction(HumanAction {
        key: "reference.acknowledge",
        label: "Acknowledge",
        subject: "reference.record",
        capability: "companies.update",
        executor: None,
        handler: &OWNER,
    })
);
register!(
    "reference.human.review",
    Contribution::HumanAction(HumanAction {
        key: "reference.review.submit",
        label: "Submit review",
        subject: "reference.record",
        capability: "companies.update",
        executor: Some("reference.human.review"),
        handler: &OWNER,
    })
);
register!(
    "reference.flow",
    Contribution::Flow(Flow {
        code: "reference.approval",
        label: "Reference approval",
        subject: "reference.record",
        active: true,
        statuses: &[
            Status {
                code: "pending",
                label: "Pending",
                position: 0,
                active: true,
                settings: "{}"
            },
            Status {
                code: "approved",
                label: "Approved",
                position: 1,
                active: true,
                settings: "{}"
            }
        ],
        edges: &[Edge {
            from: "pending",
            to: "approved",
            label: "Approve",
            capability: Some("companies.update"),
            guard: Some("reference.ready"),
            action: Some("reference.approved"),
            position: 0,
            active: true,
            sla_seconds: None,
            metadata: "{}"
        }],
    })
);

// A private approval case. It is
// linked only by disposable examples; no business Domain is converted.
pub(super) const APPROVAL_CASE: bilibi_workflow::ApprovalCase = bilibi_workflow::ApprovalCase {
    key: "reference.approval",
    label: "Reference request",
    subject: "reference.record",
    process: "reference.approval",
    default_policy: default_approval_policy,
    decisions: &[bilibi_workflow::ApprovalDecisionAction {
        label: "Reference approval",
        key: "reference.approval.decide",
        capability: "companies.update",
        contract: "extensions.workflow_reference.approval.decide",
    }],
    recall_capability: "companies.update",
    delegation_capability: "companies.update",
    policy_capability: "admin.workflow.definition.manage",
    review_capability: "companies.update",
    policy_contract: "extensions.workflow_reference.approval.policy",
    recall_contract: "extensions.workflow_reference.approval.recall",
    delegation_contract: "extensions.workflow_reference.approval.delegate",
    revoke_contract: "extensions.workflow_reference.approval.revoke",
    review_contract: "extensions.workflow_reference.approval.review",
    facts: &[
        ("amount", bilibi_workflow::ApprovalFactKind::Money),
        ("resource_type", bilibi_workflow::ApprovalFactKind::Code),
    ],
    relationships: &["manager"],
    required_capabilities: &["companies.update"],
    distinct_stages: false,
    approval_reason: false,
    reason_characters: 500,
    reason_bytes: 4000,
    allow_preparer: true,
    delegation: true,
    forbid_impersonation: false,
    step_up: None,
};
bilibi_workflow::declare_approval_case!(approval_contracts,"extensions/workflow_reference",APPROVAL_CASE,
    policy:"extensions.workflow_reference.approval.policy",recall:"extensions.workflow_reference.approval.recall",
    delegation:"extensions.workflow_reference.approval.delegate",revoke:"extensions.workflow_reference.approval.revoke",review:"extensions.workflow_reference.approval.review",step_up:&[],decision_input:||serde_json::json!({"type":"object","properties":{}}),
    decisions:[(DECIDE,"extensions.workflow_reference.approval.decide","reference.approval.decide","companies.update")]);
struct ApprovalOwner {
    notices: std::sync::Arc<bilibi_notifications::NotificationBoundary>,
    queue: std::sync::Arc<bilibi_queue::Queue>,
    parties: std::sync::Arc<bilibi_tenancy::PartyProofs>,
}
impl bilibi_workflow::ApprovalAdapter for ApprovalOwner {
    fn authorize_company<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        company: i64,
        access: bilibi_workflow::ApprovalAccess,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            // Disposable owner has one company. Prove it via the company
            // boundary rather than treating its numeric id as reach.
            let capability = match access {
                bilibi_workflow::ApprovalAccess::Policy => "admin.workflow.definition.manage",
                bilibi_workflow::ApprovalAccess::Read => "companies.view",
                _ => "companies.update",
            };
            self.parties
                .organisation_target(
                    sql,
                    scope,
                    company.try_into().map_err(|_| Error::Invalid)?,
                    capability,
                )
                .await?;
            Ok(())
        })
    }
    fn inspect<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        access: bilibi_workflow::ApprovalAccess,
    ) -> OwnerFuture<'a, bilibi_workflow::ApprovalSubject> {
        Box::pin(async move {
            let row=sql.query_opt("SELECT company_id,prepared_by,amount,currency,resource_type,revision,closed FROM workflow_reference_approval_facts WHERE tenant_id=$1 AND record_id=$2",&[&scope.tenant(),&subject.id]).await?.ok_or(Error::NotFound)?;
            let company: i64 = row.get(0);
            <Self as bilibi_workflow::ApprovalAdapter>::authorize_company(
                self, sql, scope, company, access,
            )
            .await?;
            if matches!(
                access,
                bilibi_workflow::ApprovalAccess::Submit
                    | bilibi_workflow::ApprovalAccess::Decide
                    | bilibi_workflow::ApprovalAccess::Recall
            ) && (row.get::<_, bool>(6) || subject.status != "pending")
            {
                return Err(Error::Conflict);
            }
            let preparer: i64 = row.get(1);
            let mut preparers = bilibi_workflow::ApprovalIdentities::default();
            preparers.accounts.insert(preparer);
            Ok(bilibi_workflow::ApprovalSubject {
                company,
                revision: row.get(5),
                preparers,
                beneficiaries: Default::default(),
                facts: std::collections::BTreeMap::from([
                    (
                        "amount".into(),
                        bilibi_workflow::ApprovalFact::Money {
                            currency: row.get(3),
                            value: row.get(2),
                        },
                    ),
                    (
                        "resource_type".into(),
                        bilibi_workflow::ApprovalFact::Code { value: row.get(4) },
                    ),
                ]),
                label: format!("Reference request {}", subject.id),
                url: format!("/workflow/reference/{}", subject.id),
                summary: vec![
                    bilibi_workflow::ApprovalSummary {
                        label: "Amount".into(),
                        value: format!("{} {}", row.get::<_, String>(3), row.get::<_, String>(2)),
                    },
                    bilibi_workflow::ApprovalSummary {
                        label: "Resource type".into(),
                        value: row.get(4),
                    },
                ],
            })
        })
    }
    fn cover_candidate<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        company: i64,
        account: i64,
        identity: &'a bilibi_tenancy::AccountIdentity,
    ) -> OwnerFuture<'a, bool> {
        Box::pin(async move {
            Ok(identity.organisation.is_some_and(|c| c.get() == company)
                || bilibi_authz::effective_capabilities_of(sql, scope, account)
                    .await?
                    .allowed
                    .contains("admin.company.tenant-wide.manage"))
        })
    }
    fn candidate<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        _subject: &'a Subject,
        company: i64,
        account: i64,
        identity: &'a bilibi_tenancy::AccountIdentity,
    ) -> OwnerFuture<'a, bool> {
        <Self as bilibi_workflow::ApprovalAdapter>::cover_candidate(
            self, sql, scope, company, account, identity,
        )
    }
    fn maintenance<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a bilibi_authz::SystemScope,
        subject: &'a Subject,
    ) -> OwnerFuture<'a, bilibi_workflow::ApprovalSubject> {
        Box::pin(async move {
            let row=sql.query_opt("SELECT company_id,prepared_by,amount,currency,resource_type,revision FROM workflow_reference_approval_facts WHERE tenant_id=$1 AND record_id=$2",&[&scope.tenant(),&subject.id]).await?.ok_or(Error::NotFound)?;
            let mut preparers = bilibi_workflow::ApprovalIdentities::default();
            preparers.accounts.insert(row.get(1));
            Ok(bilibi_workflow::ApprovalSubject {
                company: row.get(0),
                revision: row.get(5),
                preparers,
                beneficiaries: Default::default(),
                facts: std::collections::BTreeMap::from([
                    (
                        "amount".into(),
                        bilibi_workflow::ApprovalFact::Money {
                            currency: row.get(3),
                            value: row.get(2),
                        },
                    ),
                    (
                        "resource_type".into(),
                        bilibi_workflow::ApprovalFact::Code { value: row.get(4) },
                    ),
                ]),
                label: format!("Reference request {}", subject.id),
                url: format!("/workflow/reference/{}", subject.id),
                summary: vec![
                    bilibi_workflow::ApprovalSummary {
                        label: "Amount".into(),
                        value: format!("{} {}", row.get::<_, String>(3), row.get::<_, String>(2)),
                    },
                    bilibi_workflow::ApprovalSummary {
                        label: "Resource type".into(),
                        value: row.get(4),
                    },
                ],
            })
        })
    }
    fn relationship<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        key: &'a str,
        account: i64,
    ) -> OwnerFuture<'a, bool> {
        Box::pin(async move {
            if key != "manager" {
                return Err(Error::Invalid);
            }
            Ok(sql.query_one("SELECT manager_id=$3 FROM workflow_reference_approval_facts WHERE tenant_id=$1 AND record_id=$2",&[&scope.tenant(),&subject.id,&account]).await?.get(0))
        })
    }
    fn authorize_control<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        _: &'a Subject,
        kind: &'a str,
        reference: &'a str,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            if kind != "reference_override" || reference != "reference.control" {
                return Err(Error::Forbidden);
            }
            bilibi_authz::authorize_key(sql, scope, "companies.update").await
        })
    }
    fn validate_saved<'a>(
        &'a self,
        sql: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        _: &'a bilibi_workflow::ApprovalIdentities,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            let closed:bool=sql.query_one("SELECT closed FROM workflow_reference_approval_facts WHERE tenant_id=$1 AND record_id=$2",&[&scope.tenant(),&subject.id]).await?.get(0);
            if closed {
                Err(Error::Conflict)
            } else {
                Ok(())
            }
        })
    }
    fn apply_in<'a>(
        &'a self,
        tx: &'a mut bilibi_database::MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        outcome: bilibi_workflow::ApprovalOutcome,
        reason: &'a str,
        _: &'a serde_json::Value,
    ) -> OwnerFuture<'a, ()> {
        let scope = scope.clone();
        let subject = subject.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            let reference_id = subject.id;
            let reference_revision:String=tx.sql().query_one("SELECT revision FROM workflow_reference_approval_facts WHERE tenant_id=$1 AND record_id=$2",&[&scope.tenant(),&reference_id]).await?.get(0);
            let effect_scope = scope.clone();
            tx.mutate(move |sql|Box::pin(async move {
                let status=match outcome {bilibi_workflow::ApprovalOutcome::Approved=>"approved",bilibi_workflow::ApprovalOutcome::Rejected=>"rejected",bilibi_workflow::ApprovalOutcome::Recalled=>"recalled"};
                let row=sql.query_one("UPDATE workflow_reference_records SET status=$3,version=version+1,action_count=action_count+CASE WHEN $3='approved' THEN 1 ELSE 0 END WHERE tenant_id=$1 AND id=$2 RETURNING version,action_count",&[&effect_scope.tenant(),&subject.id,&status]).await?;
                if sql.query_one("SELECT fail_effect FROM workflow_reference_approval_facts WHERE tenant_id=$1 AND record_id=$2",&[&effect_scope.tenant(),&subject.id]).await?.get::<_,bool>(0) {return Err(Error::Invalid);}
                Ok(((),vec![Change{kind:"reference.approval_effect".into(),id:subject.id,old:json!({"version":subject.version,"status":subject.status}),new:json!({"version":row.get::<_,i64>(0),"status":status,"count":row.get::<_,i32>(1),"reason":reason})}]))
            })).await?;
            self.notices
                .record(tx, &scope, "Reference approval updated")
                .await?;
            if scope.impersonator().is_none()
                && outcome == bilibi_workflow::ApprovalOutcome::Approved
            {
                self.queue
                    .enqueue_for(
                        tx,
                        &scope,
                        "extensions/workflow_reference-followup",
                        json!({"record":reference_id,"revision":reference_revision}),
                    )
                    .await?;
            }
            Ok(())
        })
    }
}

/// Owner defaults are ordinary immutable data; explicit publication remains an
/// operator action. This example uses no company-specific people or amounts.
pub(super) fn default_approval_policy(
    case: &bilibi_workflow::ApprovalCase,
) -> bilibi_workflow::ApprovalPolicy {
    use bilibi_workflow::{
        ApprovalMode, ApprovalPolicy, ApprovalSeat, ApprovalSelector, ApprovalStage,
    };
    ApprovalPolicy {
        stages: vec![ApprovalStage {
            key: "review".into(),
            label: "Review".into(),
            mode: ApprovalMode::All,
            seats: vec![ApprovalSeat {
                key: "reviewer".into(),
                label: "Reviewer".into(),
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
const STRICT_STEP_UP: &str = "extensions.workflow_reference.strict.approve";
pub(super) const STRICT_CASE: bilibi_workflow::ApprovalCase = bilibi_workflow::ApprovalCase {
    key: "reference.strict_approval",
    label: "Protected reference request",
    process: "reference.strict_approval",
    decisions: &[bilibi_workflow::ApprovalDecisionAction {
        label: "Protected reference approval",
        key: "reference.strict.decide",
        capability: "companies.update",
        contract: "extensions.workflow_reference.strict.decide",
    }],
    policy_contract: "extensions.workflow_reference.strict.policy",
    recall_contract: "extensions.workflow_reference.strict.recall",
    delegation_contract: "extensions.workflow_reference.strict.delegate",
    revoke_contract: "extensions.workflow_reference.strict.revoke",
    review_contract: "extensions.workflow_reference.strict.review",
    distinct_stages: true,
    approval_reason: true,
    allow_preparer: false,
    delegation: false,
    forbid_impersonation: true,
    step_up: Some(STRICT_STEP_UP),
    ..APPROVAL_CASE
};
bilibi_step_up::declare!(
    "extensions/workflow_reference",
    bilibi_step_up::Operation::new(STRICT_STEP_UP, "Approve a protected reference request").floor()
);
bilibi_workflow::declare_approval_case!(strict_contracts,"extensions/workflow_reference",STRICT_CASE,
    policy:"extensions.workflow_reference.strict.policy",recall:"extensions.workflow_reference.strict.recall",delegation:"extensions.workflow_reference.strict.delegate",revoke:"extensions.workflow_reference.strict.revoke",review:"extensions.workflow_reference.strict.review",step_up:&[STRICT_STEP_UP],decision_input:||serde_json::json!({"type":"object","properties":{}}),
    decisions:[(DECIDE,"extensions.workflow_reference.strict.decide","reference.strict.decide","companies.update")]);
