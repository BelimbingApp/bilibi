//! Agent operations over Authz's grant writes: the same `Grants` facade the
//! account and role pages use, reached through Agent API's reviewed plans.
//! Authz sits below Agent API, so these adapters live one module above both,
//! as Audit Views sits above Audit. No Bilimbi counterpart.
//!
//! An agent never exceeds its person: the plan service authorizes each
//! operation's capability, `Grants` re-proves it inside the write, and the
//! escalation guard the account page applies holds here too (a role or a
//! capability is grantable only by an actor who holds everything it grants; a
//! full-access role only by a full-access actor). Every write is a Base
//! Step-up operation declared by Authz, so an agent gets `step_up_required`
//! and succeeds with its person's code.
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_authz::{
    effective_capabilities_of,
    grants::Grants,
    roles::{self, RoleSummary},
};
use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

const LIST: &str = "admin.authz.role.list";
const UPDATE: &str = "admin.authz.role.update";
/// Authz's step-up operation keys these writes ask for (`bilibi_authz` declares them).
pub const PRINCIPAL_ROLE_SET: &str = "base.authz_agent.principal_role.set";
const STEP_UP_ROLE: &str = "base.authz.principal_role.set";
const STEP_UP_GRANT_ALL: &str = "base.authz.grant_all_role.assign";
const STEP_UP_CAPABILITY: &str = "base.authz.principal_capability.set";
const STEP_UP_ROLE_CAPABILITY: &str = "base.authz.role_capability.set";

struct Bound {
    db: Db,
    grants: Arc<Grants>,
}
static BOUND: OnceLock<Bound> = OnceLock::new();
fn bound() -> bilibi_agent_api::Result<&'static Bound> {
    BOUND.get().ok_or(Error::Internal.into())
}

fn field_error(field: &str, message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([(field.into(), vec![message.into()])]))
}
fn positive(input: &Value, field: &str) -> bilibi_agent_api::Result<i64> {
    input[field]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| field_error(field, "must be a positive id"))
}
fn capability<'a>(grants: &Grants, input: &'a Value) -> bilibi_agent_api::Result<&'a str> {
    let key = input["capability"].as_str().unwrap_or_default();
    if grants.vocabulary().capabilities.contains_key(key) {
        Ok(key)
    } else {
        Err(field_error("capability", "is not a registered capability"))
    }
}
fn role_json(role: &RoleSummary) -> Value {
    json!({"id":role.id,"code":role.code,"name":role.name,"grant_all":role.grant_all,"is_system":role.is_system})
}

/// One role of the tenant the actor may see, with what it grants.
async fn role(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    id: i64,
) -> bilibi_agent_api::Result<roles::RoleDetails> {
    let bound = bound()?;
    let companies = bound.grants.visible_companies(client, scope).await?;
    roles::get(client, scope, &companies, id)
        .await?
        .ok_or_else(|| field_error("role_id", "is not a role of this tenant"))
}

/// The escalation guard (`core/user` account page): the actor must hold
/// everything granted, and a full-access role needs a full-access actor.
async fn guard(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    grant_all: bool,
    capabilities: &[String],
) -> bilibi_agent_api::Result<()> {
    let actor = effective_capabilities_of(client, scope, scope.user()).await?;
    let holds = actor.grant_all
        || (!grant_all
            && capabilities
                .iter()
                .all(|key| actor.allowed.contains(key.as_str())));
    if holds {
        Ok(())
    } else {
        Err(Error::Forbidden.into())
    }
}

enum Read {
    Roles,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/authz_agent"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let Self::Roles = self;
            let bound = bound()?;
            let client = bound.db.get().await.map_err(Error::from)?;
            let companies = bound.grants.visible_companies(&**client, scope).await?;
            let page = roles::list(
                &**client,
                scope,
                &companies,
                &roles::Query {
                    search: input["search"].as_str().unwrap_or_default().to_owned(),
                    sort: roles::Sort::Name,
                    descending: false,
                    page: input["page"].as_u64().unwrap_or(1),
                    page_size: input["page_size"].as_u64().unwrap_or(100),
                },
            )
            .await?;
            Ok(Output::Data(Data::Json(json!({
                "roles": page.entries.iter().map(role_json).collect::<Vec<_>>(),
                "total": page.total_entries,
            }))))
        })
    }
}

enum Write {
    PrincipalRole,
    GrantAllRole,
    PrincipalCapability,
    RoleCapability,
}

/// What one write will do, read without writing: the review the plan shows
/// the person and compares again at commit.
async fn review(
    write: &Write,
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let bound = bound()?;
    match write {
        Write::PrincipalRole | Write::GrantAllRole => {
            let user = positive(input, "user_id")?;
            let assigned = matches!(write, Write::GrantAllRole)
                || input["assigned"].as_bool().unwrap_or(false);
            bound.grants.verify_principal(client, scope, user).await?;
            let details = role(client, scope, positive(input, "role_id")?).await?;
            if matches!(write, Write::GrantAllRole) && !details.role.grant_all {
                return Err(field_error("role_id", "is not a full-access role"));
            }
            if assigned {
                guard(client, scope, details.role.grant_all, &details.capabilities).await?;
            }
            let holds = details.principals.contains(&user);
            let mut step_up = vec![STEP_UP_ROLE];
            if assigned && details.role.grant_all {
                step_up.push(STEP_UP_GRANT_ALL);
            }
            Ok(json!({
                "summary": format!("{} the {} role {} user {user}", if assigned {"Give"} else {"Remove"}, details.role.name, if assigned {"to"} else {"from"}),
                "user_id": user, "role": role_json(&details.role), "assigned": assigned,
                "currently_assigned": holds, "step_up": step_up,
            }))
        }
        Write::PrincipalCapability => {
            let user = positive(input, "user_id")?;
            let key = capability(&bound.grants, input)?;
            let rule = input["rule"].as_str().unwrap_or_default();
            if !["allow", "deny", "clear"].contains(&rule) {
                return Err(field_error("rule", "must be allow, deny or clear"));
            }
            bound.grants.verify_principal(client, scope, user).await?;
            if rule == "allow" {
                guard(client, scope, false, &[key.to_owned()]).await?;
            }
            let target = effective_capabilities_of(client, scope, user).await?;
            let current = if target.direct_allowed.contains(key) {
                "allow"
            } else if target.direct_denied.contains(key) {
                "deny"
            } else {
                "clear"
            };
            Ok(json!({
                "summary": format!("Set the direct rule for {key} on user {user} to {rule}"),
                "user_id": user, "capability": key, "rule": rule, "current_rule": current,
                "step_up": [STEP_UP_CAPABILITY],
            }))
        }
        Write::RoleCapability => {
            let key = capability(&bound.grants, input)?;
            let granted = input["granted"].as_bool().unwrap_or(false);
            let details = role(client, scope, positive(input, "role_id")?).await?;
            if details.role.is_system {
                return Err(field_error(
                    "role_id",
                    "is a system role; reconciliation owns its capabilities",
                ));
            }
            if granted {
                guard(client, scope, false, &[key.to_owned()]).await?;
            }
            Ok(json!({
                "summary": format!("{} {key} {} the {} role", if granted {"Grant"} else {"Remove"}, if granted {"to"} else {"from"}, details.role.name),
                "role": role_json(&details.role), "capability": key, "granted": granted,
                "currently_granted": details.capabilities.iter().any(|held| held == key),
                "step_up": [STEP_UP_ROLE_CAPABILITY],
            }))
        }
    }
}

impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "base/authz_agent"
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let facts = if let Some(tx) = client.transaction() {
                review(self, tx, scope, input).await?
            } else {
                let client = bound()?.db.get().await.map_err(Error::from)?;
                review(self, &**client, scope, input).await?
            };
            Ok(facts.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            // The review again on this transaction: the guard and the facts
            // hold at commit, not only when the person looked.
            let facts = review(self, tx.sql(), scope, input).await?;
            let grants = bound()?.grants.clone();
            match self {
                Self::PrincipalRole | Self::GrantAllRole => {
                    let role = facts["role"]["id"].as_i64().ok_or(Error::Internal)?;
                    let user = facts["user_id"].as_i64().ok_or(Error::Internal)?;
                    let assigned = facts["assigned"].as_bool().ok_or(Error::Internal)?;
                    grants
                        .assign_role_in(tx, scope, user, role, assigned)
                        .await?;
                }
                Self::PrincipalCapability => {
                    let user = facts["user_id"].as_i64().ok_or(Error::Internal)?;
                    let key = facts["capability"].as_str().ok_or(Error::Internal)?;
                    let allowed = match facts["rule"].as_str() {
                        Some("allow") => Some(true),
                        Some("deny") => Some(false),
                        _ => None,
                    };
                    grants.set_direct_in(tx, scope, user, key, allowed).await?;
                }
                Self::RoleCapability => {
                    let role = facts["role"]["id"].as_i64().ok_or(Error::Internal)?;
                    let key = facts["capability"].as_str().ok_or(Error::Internal)?;
                    let granted = facts["granted"].as_bool().ok_or(Error::Internal)?;
                    grants
                        .set_role_capability_in(tx, scope, role, key, granted)
                        .await?;
                }
            }
            Ok(facts.into())
        })
    }
}

fn id_schema() -> Value {
    json!({"type":"integer","minimum":1})
}
fn capability_schema() -> Value {
    json!({"type":"string","maxLength":255})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{"search":{"type":"string","maxLength":255},"page":{"type":"integer","minimum":1,"maximum":1000000},"page_size":{"type":"integer","minimum":1,"maximum":100}}})
}
fn principal_role_schema() -> Value {
    json!({"type":"object","required":["user_id","role_id","assigned"],"properties":{"user_id":id_schema(),"role_id":id_schema(),"assigned":{"type":"boolean"}}})
}
fn grant_all_schema() -> Value {
    json!({"type":"object","required":["user_id","role_id"],"properties":{"user_id":id_schema(),"role_id":id_schema()}})
}
fn principal_capability_schema() -> Value {
    json!({"type":"object","required":["user_id","capability","rule"],"properties":{"user_id":id_schema(),"capability":capability_schema(),"rule":{"type":"string","enum":["allow","deny","clear"]}}})
}
fn role_capability_schema() -> Value {
    json!({"type":"object","required":["role_id","capability","granted"],"properties":{"role_id":id_schema(),"capability":capability_schema(),"granted":{"type":"boolean"}}})
}
fn review_output() -> Value {
    json!({"type":"object","description":"The change, the state before it, and the step-up operation keys it asks for."})
}
fn roles_output() -> Value {
    json!({"type":"object","description":"The tenant's roles: id, code, name, grant_all, is_system."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "base/authz_agent", Provenance { module: "base/authz_agent", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key:"base.authz_agent.role.list", title:"List roles", summary:"The tenant's authorization roles with their ids, codes and whether they grant full access.", keywords:&["role","authorization","access"], capability:LIST, input:list_schema, output:roles_output, handler:Handler::Read(&Read::Roles),channels:Channels::Every,step_up:&[] },
        Operation { key:PRINCIPAL_ROLE_SET, title:"Give or remove a user's role", summary:"Assign or remove one authorization role for one account. Step-up: base.authz.principal_role.set, and base.authz.grant_all_role.assign for a full-access role.", keywords:&["role","assign","access","user"], capability:UPDATE, input:principal_role_schema, output:review_output, handler:Handler::SafeWrite(&Write::PrincipalRole),channels:Channels::Every,step_up:&[STEP_UP_ROLE, STEP_UP_GRANT_ALL] },
        Operation { key:"base.authz_agent.grant_all_role.assign", title:"Give a user a full-access role", summary:"Assign a full-access role such as Core Administrator; refuses any other role. Step-up: base.authz.principal_role.set and base.authz.grant_all_role.assign.", keywords:&["core_admin","administrator","full access","role"], capability:UPDATE, input:grant_all_schema, output:review_output, handler:Handler::SafeWrite(&Write::GrantAllRole),channels:Channels::Every,step_up:&[STEP_UP_ROLE, STEP_UP_GRANT_ALL] },
        Operation { key:"base.authz_agent.principal_capability.set", title:"Set a user's direct capability rule", summary:"Allow, deny or clear one capability directly on one account; a deny wins over every grant. Step-up: base.authz.principal_capability.set.", keywords:&["capability","grant","deny","user"], capability:UPDATE, input:principal_capability_schema, output:review_output, handler:Handler::SafeWrite(&Write::PrincipalCapability),channels:Channels::Every,step_up:&[STEP_UP_CAPABILITY] },
        Operation { key:"base.authz_agent.role_capability.set", title:"Change a role's capabilities", summary:"Grant or remove one capability on a custom role. System roles are refused. Step-up: base.authz.role_capability.set.", keywords:&["role","capability","grant"], capability:UPDATE, input:role_capability_schema, output:review_output, handler:Handler::SafeWrite(&Write::RoleCapability),channels:Channels::Every,step_up:&[STEP_UP_ROLE_CAPABILITY] },
    ],
    guides: &[Guide { key:"base.authz_agent", title:"Roles and capability grants", summary:"Who may do what in this tenant, and how to change it on the person's behalf.", keywords:&["role","capability","grant","access","core_admin"], capability:LIST,
        body:"List roles with base.authz_agent.role.list to find a role id. Changes are reviewed plans: plan, show the review to the person, then commit. You can only grant what your person holds; a full-access role such as Core Administrator only if your person has full access. Every change is a two-factor (step-up) operation: a commit may answer step_up_required naming the operation; then ask the person for the current code from their authenticator app and commit the same plan again with step_up_code. A code is used once and opens a window for this connection, so further changes soon after may pass without one, unless the tenant asks every time. Impersonation is never available to agents." }],
}) }

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/authz_agent", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/agent_api"],
        migrations: &[],
        services: &[bilibi_module_registry::Service { id: "base/authz_agent.binding", factory: |services| {
            let bound = Bound {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                grants: services.get("base/authz.grants")?,
            };
            BOUND.set(bound).map_err(|_| bilibi_module_registry::RegistryError::Service("duplicate authz agent binding".into()))?;
            Ok(Arc::new(()))
        } }],
    }
}
