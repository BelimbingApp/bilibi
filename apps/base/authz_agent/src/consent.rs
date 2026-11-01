//! One reviewed door over Authz's existing system-principal grants for the
//! two engines that ask every tenant before acting there.
use crate::{field_error, Bound};
use bilibi_authz::{authorize_key, effective_capabilities, effective_system, in_operator_tenant};
use bilibi_database::{Error, Result};
use bilibi_events::{Subscriber, DELIVER, PRINCIPAL as EVENTS_PRINCIPAL, SUBSCRIBERS};
use bilibi_module_registry::Catalog;
use bilibi_schedule::{Reach, Schedule, EXECUTE, PRINCIPAL as SCHEDULE_PRINCIPAL, TENANT_WORK};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

pub const LIST: &str = "base.authz_agent.automation_consent.list";
pub const SET: &str = "base.authz_agent.automation_consent.set";
const GRANT: &str = "admin.authz.system-principal.grant";
const REVOKE: &str = "admin.authz.system-principal.revoke";

#[derive(Clone)]
pub(crate) struct ScheduleItem {
    pub key: &'static str,
    pub name: &'static str,
    pub owner: &'static str,
    pub task: &'static str,
}

#[derive(Clone)]
pub(crate) struct SubscriberItem {
    pub key: &'static str,
    pub owner: &'static str,
    pub events: Vec<&'static str>,
}

pub(crate) fn schedules(schedule: std::sync::Arc<Schedule>) -> Vec<ScheduleItem> {
    schedule
        .definitions()
        .filter(|item| item.definition().reach == Reach::EachTenant)
        .map(|item| ScheduleItem {
            key: item.key(),
            name: item.definition().name,
            owner: item.owner(),
            task: item.definition().task_name,
        })
        .collect()
}

pub(crate) fn subscribers(
    catalog: &Catalog,
) -> bilibi_module_registry::Result<Vec<SubscriberItem>> {
    catalog
        .entries::<Subscriber>(SUBSCRIBERS)?
        .iter()
        .map(|entry| {
            let subscriber = entry.get::<Subscriber>()?;
            Ok(SubscriberItem {
                key: entry.id,
                owner: entry.provenance.module,
                events: subscriber.events.iter().map(|on| on.event).collect(),
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Schedule,
    Events,
}

impl Kind {
    pub(crate) fn from_input(input: &Value) -> std::result::Result<Self, bilibi_agent_api::Error> {
        match input["kind"].as_str() {
            Some("schedule") => Ok(Self::Schedule),
            Some("events") => Ok(Self::Events),
            _ => Err(field_error("kind", "must be schedule or events")),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Events => "events",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Schedule => "installation-wide scheduled work",
            Self::Events => "event subscriptions",
        }
    }
}

pub(crate) async fn target(
    client: &impl GenericClient,
    scope: &Scope,
    kind: Kind,
) -> Result<(&'static str, &'static str)> {
    Ok(match kind {
        Kind::Schedule if in_operator_tenant(client, scope.tenant()).await? => {
            (SCHEDULE_PRINCIPAL, EXECUTE)
        }
        Kind::Schedule => (SCHEDULE_PRINCIPAL, TENANT_WORK),
        Kind::Events => (EVENTS_PRINCIPAL, DELIVER),
    })
}

async fn facts(client: &(impl GenericClient + Sync), scope: &Scope, kind: Kind) -> Result<Value> {
    let (principal, capability) = target(client, scope, kind).await?;
    let current = effective_system(client, scope.tenant(), principal).await?;
    let actor = effective_capabilities(client, scope).await?;
    Ok(json!({
        "kind": kind.id(),
        "label": kind.label(),
        "principal": principal,
        "capability": capability,
        "granted": current.allowed.contains(capability),
        "can_grant": actor.contains(GRANT) && actor.contains(capability),
        "can_withdraw": actor.contains(REVOKE),
    }))
}

pub(crate) async fn list(
    bound: &Bound,
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<Value> {
    let mut schedule = facts(client, scope, Kind::Schedule).await?;
    schedule["items"] = Value::Array(
        bound
            .schedules
            .iter()
            .map(
                |item| json!({"key":item.key,"name":item.name,"owner":item.owner,"task":item.task}),
            )
            .collect(),
    );
    let mut events = facts(client, scope, Kind::Events).await?;
    events["items"] = Value::Array(
        bound
            .subscribers
            .iter()
            .map(|item| json!({"key":item.key,"owner":item.owner,"events":item.events}))
            .collect(),
    );
    Ok(json!({"schedule":schedule,"events":events}))
}

pub(crate) async fn review(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let kind = Kind::from_input(input)?;
    let granted = input["granted"]
        .as_bool()
        .ok_or_else(|| field_error("granted", "must be true or false"))?;
    let gate = if granted { GRANT } else { REVOKE };
    authorize_key(client, scope, gate).await?;
    let (principal, capability) = target(client, scope, kind).await?;
    if granted
        && !effective_capabilities(client, scope)
            .await?
            .contains(capability)
    {
        return Err(Error::Forbidden.into());
    }
    let current = effective_system(client, scope.tenant(), principal)
        .await?
        .allowed
        .contains(capability);
    Ok(json!({
        "summary": format!("{} consent for {} in this tenant", if granted {"Grant"} else {"Withdraw"}, kind.label()),
        "kind": kind.id(),
        "principal": principal,
        "capability": capability,
        "granted": granted,
        "currently_granted": current,
        "step_up": [crate::STEP_UP_SYSTEM],
    }))
}
