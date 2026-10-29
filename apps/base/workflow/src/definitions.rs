//! Immutable compiled adapter catalog. Status configuration is explicitly
//! inserted, never overwritten at boot or on an owner definition change.
use crate::{
    process::{self, Process, ProcessDefinition},
    ActionRequest, OwnerFuture, Request, Subject, SubjectAdapter,
};
use bilibi_database::{Change, Error, Result};
use bilibi_module_registry::{Entry, RegistryError};
use bilibi_tenancy::Scope;
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::Transaction;

pub const CONTRIBUTIONS: &str = "base/workflow.definitions";
pub trait Guard: Send + Sync {
    fn check<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        edge: &'a Transition,
        request: &'a Request,
    ) -> OwnerFuture<'a, ()>;
}
pub trait Action: Send + Sync {
    fn execute<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        edge: &'a Transition,
        request: &'a Request,
    ) -> OwnerFuture<'a, Vec<Change>>;
}
/// The owner's business effect of one human action, run by the gate inside
/// its transaction once the subject is locked and proven, the capability and
/// expected subject version checked, idempotency settled (a repeated request
/// never reaches the handler) and, for a work-bound action, the work item's
/// run, availability, executor and version verified. Take who acts from the
/// scope, never from the payload; perform database effects only. Any error
/// rolls back the owner writes, the request row and the work completion.
pub trait HumanActionHandler: Send + Sync {
    fn handle<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        action: &'a HumanAction,
        request: &'a ActionRequest,
    ) -> OwnerFuture<'a, HumanOutcome>;
}
/// Saved on the request and, for a work-bound action, as the completed work
/// item's outcome, output and result reference. `changes` are the handler's
/// owner writes for audit capture.
#[derive(Debug)]
pub struct HumanOutcome {
    pub outcome: String,
    pub output: serde_json::Value,
    pub result_ref: Option<String>,
    pub changes: Vec<Change>,
}
impl HumanOutcome {
    /// `completed` with the given output, no result reference.
    pub fn completed(output: serde_json::Value, changes: Vec<Change>) -> Self {
        Self {
            outcome: "completed".into(),
            output,
            result_ref: None,
            changes,
        }
    }
}
/// An action a signed-in person may perform on a registered subject. `key` is
/// unique per subject. With `executor`, the action completes due available
/// work of that executor key in a running run of the same subject.
#[derive(Clone, Copy)]
pub struct HumanAction {
    pub key: &'static str,
    pub label: &'static str,
    pub subject: &'static str,
    pub capability: &'static str,
    pub executor: Option<&'static str>,
    pub handler: &'static dyn HumanActionHandler,
}
impl std::fmt::Debug for HumanAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HumanAction")
            .field("key", &self.key)
            .field("subject", &self.subject)
            .field("capability", &self.capability)
            .field("executor", &self.executor)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub code: &'static str,
    pub label: &'static str,
    pub position: i32,
    pub active: bool,
    /// JSON object holding PIC, notifications, comment_tags and prompt data.
    pub settings: &'static str,
}
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub from: &'static str,
    pub to: &'static str,
    pub label: &'static str,
    pub capability: Option<&'static str>,
    pub guard: Option<&'static str>,
    pub action: Option<&'static str>,
    pub position: i32,
    pub active: bool,
    pub sla_seconds: Option<i64>,
    pub metadata: &'static str,
}
/// Resolved, operator-configured edge passed to owner callbacks.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Transition {
    pub from: String,
    pub to: String,
    pub label: String,
    pub capability: Option<String>,
    pub guard: Option<String>,
    pub action: Option<String>,
    pub position: i32,
    pub sla_seconds: Option<i64>,
    pub metadata: serde_json::Value,
}
#[derive(Clone, Copy, Debug)]
pub struct Flow {
    pub code: &'static str,
    pub label: &'static str,
    pub subject: &'static str,
    pub active: bool,
    pub statuses: &'static [Status],
    pub edges: &'static [Edge],
}
pub enum Contribution {
    Subject {
        key: &'static str,
        aliases: &'static [&'static str],
        adapter: &'static dyn SubjectAdapter,
    },
    Guard {
        key: &'static str,
        aliases: &'static [&'static str],
        adapter: &'static dyn Guard,
    },
    Action {
        key: &'static str,
        aliases: &'static [&'static str],
        adapter: &'static dyn Action,
    },
    Flow(Flow),
    ApprovalCase(&'static crate::ApprovalCase),
    /// Versioned process graph; see `crate::process`.
    Process(Process),
    /// Human action on a subject this module owns; see `crate::runs::human`.
    HumanAction(HumanAction),
    Listener(crate::outbox::Listener),
}
#[derive(Clone, Copy)]
pub struct OwnedSubject {
    pub owner: &'static str,
    pub key: &'static str,
    pub adapter: &'static dyn SubjectAdapter,
}
#[derive(Default)]
pub struct Registry {
    pub(crate) approval_cases: BTreeMap<&'static str, (&'static str, crate::ApprovalCase)>,
    pub(crate) subjects: BTreeMap<&'static str, OwnedSubject>,
    pub(crate) flows: BTreeMap<&'static str, (&'static str, Flow)>,
    pub(crate) processes: BTreeMap<(&'static str, i32), ProcessDefinition>,
    /// Keyed by (subject key, action key), with the contributing owner.
    pub(crate) human_actions: BTreeMap<(&'static str, &'static str), (&'static str, HumanAction)>,
    pub(crate) listeners: BTreeMap<&'static str, crate::outbox::Listener>,
    guards: BTreeMap<&'static str, (&'static str, &'static dyn Guard)>,
    actions: BTreeMap<&'static str, (&'static str, &'static dyn Action)>,
    aliases: BTreeMap<(&'static str, &'static str), &'static str>,
}
fn invalid(message: impl Into<String>) -> RegistryError {
    RegistryError::Contribution(message.into())
}
pub(crate) fn key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('.').all(|part| {
            part.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && part
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        })
}
fn object(value: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(value).is_ok_and(|v| v.is_object())
}
impl Registry {
    pub fn from_inventory() -> bilibi_module_registry::Result<Self> {
        let entries = inventory::iter::<bilibi_module_registry::Contribution>
            .into_iter()
            .filter(|entry| entry.consumer == CONTRIBUTIONS)
            .map(|entry| Ok((entry.provenance.module, entry.get::<Contribution>()?)))
            .collect::<bilibi_module_registry::Result<Vec<_>>>()?;
        let vocabulary = bilibi_authz::vocabulary()
            .map_err(|_| invalid("authz vocabulary unavailable for workflow edges"))?;
        Self::build(&entries, Some(vocabulary))
    }
    /// `vocabulary` is `None` only inside the boot-time consumer validator:
    /// discovery runs validators while `bilibi_authz::vocabulary()` initializes,
    /// so calling it there deadlocks. The engine factory proves capabilities.
    pub fn build(
        entries: &[(&'static str, &Contribution)],
        vocabulary: Option<&bilibi_authz::Vocabulary>,
    ) -> bilibi_module_registry::Result<Self> {
        let mut registry = Self::default();
        for &(owner, entry) in entries {
            if owner != "base/workflow"
                && !inventory::iter::<bilibi_module_registry::Module>
                    .into_iter()
                    .any(|m| m.id == owner && m.dependencies.contains(&"base/workflow"))
            {
                return Err(invalid(format!("{owner} must depend on base/workflow")));
            }
            if let Contribution::ApprovalCase(case) = entry {
                crate::approval::validate_case(case)
                    .map_err(|_| invalid("invalid approval case"))?;
                if registry
                    .approval_cases
                    .insert(case.key, (owner, **case))
                    .is_some()
                {
                    return Err(invalid("duplicate approval case"));
                }
                let process = Process {
                    key: case.process,
                    version: 1,
                    subject: case.subject,
                    adapter: &crate::approval::UNREACHABLE,
                    steps: crate::approval::WAIT_STEPS,
                };
                if registry
                    .processes
                    .insert((case.process, 1), process::compile(owner, &process)?)
                    .is_some()
                {
                    return Err(invalid("duplicate approval process"));
                }
                for decision in case.decisions {
                    let action = HumanAction {
                        key: decision.key,
                        label: "Approval decision",
                        subject: case.subject,
                        capability: decision.capability,
                        executor: None,
                        handler: &crate::approval::UNREACHABLE,
                    };
                    if registry
                        .human_actions
                        .insert((case.subject, decision.key), (owner, action))
                        .is_some()
                    {
                        return Err(invalid("duplicate approval action"));
                    }
                }
                continue;
            }
            if let Contribution::Listener(listener) = entry {
                if !key(listener.key)
                    || listener.subjects.is_empty()
                    || listener.subjects.len() > 500
                    || registry.listeners.insert(listener.key, *listener).is_some()
                {
                    return Err(invalid(format!(
                        "invalid/duplicate listener {}",
                        listener.key
                    )));
                }
                continue;
            }
            if let Contribution::Process(process) = entry {
                let compiled = process::compile(owner, process)?;
                if registry
                    .processes
                    .insert((process.key, process.version), compiled)
                    .is_some()
                {
                    return Err(invalid(format!(
                        "duplicate process {} version {}",
                        process.key, process.version
                    )));
                }
                continue;
            }
            if let Contribution::HumanAction(action) = entry {
                if registry
                    .human_actions
                    .insert((action.subject, action.key), (owner, *action))
                    .is_some()
                {
                    return Err(invalid(format!(
                        "duplicate human action {} on {}",
                        action.key, action.subject
                    )));
                }
                continue;
            }
            let (kind, k, aliases, duplicate) = match entry {
                Contribution::Subject {
                    key,
                    aliases,
                    adapter,
                } => (
                    "subject",
                    *key,
                    *aliases,
                    registry
                        .subjects
                        .insert(
                            key,
                            OwnedSubject {
                                owner,
                                key,
                                adapter: *adapter,
                            },
                        )
                        .is_some(),
                ),
                Contribution::Guard {
                    key,
                    aliases,
                    adapter,
                } => (
                    "guard",
                    *key,
                    *aliases,
                    registry.guards.insert(key, (owner, *adapter)).is_some(),
                ),
                Contribution::Action {
                    key,
                    aliases,
                    adapter,
                } => (
                    "action",
                    *key,
                    *aliases,
                    registry.actions.insert(key, (owner, *adapter)).is_some(),
                ),
                Contribution::Flow(flow) => (
                    "flow",
                    flow.code,
                    &[][..],
                    registry.flows.insert(flow.code, (owner, *flow)).is_some(),
                ),
                Contribution::ApprovalCase(_)
                | Contribution::Process(_)
                | Contribution::HumanAction(_)
                | Contribution::Listener(_) => {
                    unreachable!("handled above")
                }
            };
            if !key(k) || duplicate {
                return Err(invalid(format!("invalid/duplicate {kind} {k}")));
            }
            for alias in std::iter::once(&k).chain(aliases.iter()) {
                if alias.is_empty()
                    || alias.len() > 255
                    || registry.aliases.insert((kind, alias), k).is_some()
                {
                    return Err(invalid(format!("duplicate/invalid {kind} alias {alias}")));
                }
            }
        }
        for listener in registry.listeners.values() {
            let mut seen = BTreeSet::new();
            for subject in listener.subjects {
                if !registry.subjects.contains_key(subject) || !seen.insert(subject) {
                    return Err(invalid(format!(
                        "listener {} has unknown/duplicate subject",
                        listener.key
                    )));
                }
            }
        }
        // Every version of a key keeps one owner and one subject, owned by
        // the contributor; a version's fingerprint is fixed here.
        for definition in registry.processes.values() {
            let same_key = registry
                .processes
                .values()
                .filter(|d| d.key == definition.key);
            if !registry
                .subjects
                .get(definition.subject)
                .is_some_and(|s| s.owner == definition.owner)
                || same_key
                    .into_iter()
                    .any(|d| d.owner != definition.owner || d.subject != definition.subject)
            {
                return Err(invalid(format!(
                    "process {} crosses ownership or subject boundaries",
                    definition.key
                )));
            }
        }
        // A human action names a subject its contributor owns and a capability
        // in the Authz vocabulary; its handler belongs to the same module.
        for &(owner, action) in registry.human_actions.values() {
            if !key(action.key)
                || action.label.trim().is_empty()
                || !key(action.capability)
                || vocabulary.is_some_and(|v| !v.capabilities.contains_key(action.capability))
                || action.executor.is_some_and(|e| !process::text(e))
                || !registry
                    .subjects
                    .get(action.subject)
                    .is_some_and(|s| s.owner == owner)
            {
                return Err(invalid(format!(
                    "invalid human action {} on {}",
                    action.key, action.subject
                )));
            }
        }
        for &(owner, case) in registry.approval_cases.values() {
            if !registry
                .subjects
                .get(case.subject)
                .is_some_and(|s| s.owner == owner)
                || [
                    case.policy_capability,
                    case.review_capability,
                    case.recall_capability,
                    case.delegation_capability,
                ]
                .iter()
                .any(|key| {
                    !self::key(key) || vocabulary.is_some_and(|v| !v.capabilities.contains_key(key))
                })
            {
                return Err(invalid(
                    "foreign approval subject or unknown control capability",
                ));
            }
        }
        let mut bound = BTreeSet::new();
        for &(owner, flow) in registry.flows.values() {
            if flow.label.trim().is_empty()
                || !registry
                    .subjects
                    .get(flow.subject)
                    .is_some_and(|s| s.owner == owner)
                || !bound.insert(flow.subject)
                || flow.statuses.is_empty()
            {
                return Err(invalid("invalid flow ownership or repeated subject"));
            }
            let mut statuses = BTreeSet::new();
            for status in flow.statuses {
                if !key(status.code)
                    || status.label.trim().is_empty()
                    || !object(status.settings)
                    || !statuses.insert(status.code)
                {
                    return Err(invalid("invalid/duplicate status"));
                }
            }
            let mut edges = BTreeSet::new();
            for edge in flow.edges {
                if !statuses.contains(edge.from)
                    || !statuses.contains(edge.to)
                    || !edges.insert((edge.from, edge.to))
                    || edge.label.trim().is_empty()
                    || !object(edge.metadata)
                    || edge.sla_seconds.is_some_and(|s| s < 0)
                    || edge.capability.is_some_and(|k| {
                        !key(k) || vocabulary.is_some_and(|v| !v.capabilities.contains_key(k))
                    })
                {
                    return Err(invalid("invalid edge/status/capability"));
                }
                registry
                    .guard(owner, edge.guard)
                    .map_err(|_| invalid("missing or foreign guard"))?;
                registry
                    .action(owner, edge.action)
                    .map_err(|_| invalid("missing or foreign action"))?;
            }
        }
        Ok(registry)
    }
    pub fn subject(&self, alias: &str) -> Result<(OwnedSubject, Flow)> {
        let subject = self.owned_subject(alias)?;
        let flow = self
            .flows
            .values()
            .find(|(_, f)| f.subject == subject.key)
            .map(|(_, f)| *f)
            .ok_or(Error::Invalid)?;
        Ok((subject, flow))
    }
    /// A process subject needs no status flow.
    pub fn owned_subject(&self, alias: &str) -> Result<OwnedSubject> {
        let key = self
            .aliases
            .get(&("subject", alias))
            .ok_or(Error::Invalid)?;
        self.subjects.get(key).copied().ok_or(Error::Invalid)
    }
    /// The subject's human actions, ordered by key, with their owner.
    pub(crate) fn human_actions(
        &self,
        subject: OwnedSubject,
    ) -> impl Iterator<Item = HumanAction> + '_ {
        self.human_actions
            .range((subject.key, "")..)
            .take_while(move |((s, _), _)| *s == subject.key)
            .filter(move |(_, (owner, _))| *owner == subject.owner)
            .map(|(_, (_, action))| *action)
    }
    /// `None` selects the highest installed version of `key`.
    pub fn process(&self, key: &str, version: Option<i32>) -> Result<&ProcessDefinition> {
        self.processes
            .iter()
            .filter(|((k, v), _)| *k == key && version.is_none_or(|wanted| wanted == *v))
            .max_by_key(|((_, v), _)| *v)
            .map(|(_, d)| d)
            .ok_or(Error::Invalid)
    }
    pub(crate) fn guard(
        &self,
        owner: &str,
        alias: Option<&str>,
    ) -> Result<Option<&'static dyn Guard>> {
        alias
            .map(|alias| {
                let key = self.aliases.get(&("guard", alias)).ok_or(Error::Invalid)?;
                self.guards
                    .get(key)
                    .filter(|(o, _)| *o == owner)
                    .map(|(_, a)| *a)
                    .ok_or(Error::Invalid)
            })
            .transpose()
    }
    pub(crate) fn action(
        &self,
        owner: &str,
        alias: Option<&str>,
    ) -> Result<Option<&'static dyn Action>> {
        alias
            .map(|alias| {
                let key = self.aliases.get(&("action", alias)).ok_or(Error::Invalid)?;
                self.actions
                    .get(key)
                    .filter(|(o, _)| *o == owner)
                    .map(|(_, a)| *a)
                    .ok_or(Error::Invalid)
            })
            .transpose()
    }
}
fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    let entries = entries
        .iter()
        .map(|e| Ok((e.provenance.module, e.get::<Contribution>()?)))
        .collect::<bilibi_module_registry::Result<Vec<_>>>()?;
    Registry::build(&entries, None).map(|_| ())
}
inventory::submit! { bilibi_module_registry::Consumer::new::<Contribution>(CONTRIBUTIONS,"base/workflow",validate) }
