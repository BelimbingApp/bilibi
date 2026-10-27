//! Live authorization, immutable contributed vocabulary, and field withholding.
pub mod capabilities;
mod capabilities_web;
pub mod decision_logs;
pub mod effective;
pub mod field_catalog;
pub mod fields;
pub mod grants;
pub mod restrictions;
mod restrictions_web;
pub mod roles;
mod roles_web;
mod step_up;
mod step_up_web;
pub mod vocabulary;
mod web;
use bilibi_database::{Error, Result};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
pub use effective::{
    effective_capabilities, effective_capabilities_of, effective_system, holders, Effective,
};
use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};
use tokio_postgres::{types::ToSql, GenericClient, Row};
pub use vocabulary::{
    CapabilityDefinition, RoleDefinition, StandingGrant, SystemPrincipalDefinition, Vocabulary,
    VocabularyContribution, STANDING_GRANTS, VOCABULARY,
};

/// Immutable deployment snapshot. Database grants/restrictions are never cached.
/// Never call this from a contribution validator: catalog discovery runs every
/// validator while this snapshot initializes, so the call deadlocks. Prove
/// capability references in a service factory instead.
pub fn vocabulary() -> Result<&'static Vocabulary> {
    static SNAPSHOT: OnceLock<bilibi_module_registry::Result<Vocabulary>> = OnceLock::new();
    SNAPSHOT
        .get_or_init(Vocabulary::from_inventory)
        .as_ref()
        .map_err(|_| Error::Internal)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Decision {
    pub allowed: bool,
    pub reason: &'static str,
}
impl Decision {
    fn allow(reason: &'static str) -> Self {
        Self {
            allowed: true,
            reason,
        }
    }
    fn deny(reason: &'static str) -> Self {
        Self {
            allowed: false,
            reason,
        }
    }
    pub fn require(self) -> Result<()> {
        if self.allowed {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
}
pub async fn authorize_key(client: &impl GenericClient, scope: &Scope, key: &str) -> Result<()> {
    decide(client, scope, key).await?.require()
}
/// Opens for any one of `keys` (a route or menu entry that serves a combined
/// page, `Access::AnyOf`). Candidates come from the live effective set, which
/// already drops direct denies, so a key the account does not hold writes no
/// denial row while another key opens the page; each candidate is then
/// decided (and logged) like [`authorize_key`]. When none is held the first
/// key is decided once, so a refusal leaves one denial row. An empty list or
/// an empty or oversized key is `Invalid`.
pub async fn authorize_any(
    client: &impl GenericClient,
    scope: &Scope,
    keys: &[&str],
) -> Result<()> {
    if keys.is_empty() || keys.iter().any(|key| key.is_empty() || key.len() > 200) {
        return Err(Error::Invalid);
    }
    let held = effective_capabilities(client, scope).await?;
    for key in keys.iter().filter(|key| held.contains(**key)) {
        if decide(client, scope, key).await?.allowed {
            return Ok(());
        }
    }
    authorize_key(client, scope, keys[0]).await?;
    Err(Error::Forbidden)
}
/// Unknown keys, invalid live tenants, and non-operator platform requests deny
/// before any grants (including grant_all) can take effect. Denials are logged.
pub async fn decide(client: &impl GenericClient, scope: &Scope, key: &str) -> Result<Decision> {
    if key.is_empty() || key.len() > 200 {
        return Err(Error::Invalid);
    }
    // A named system principal's scope (SP1) is decided by its name: the
    // declaration and the durable tenant grant, never roles or grant-all.
    if let Some(principal) = scope.principal() {
        return system_decision(client, scope.tenant(), Some(principal), key).await;
    }
    let decision = match vocabulary()?.capabilities.get(key) {
        None => Decision::deny("denied_unknown_capability"),
        Some(cap) => {
            let live = client
                .query_opt(LIVE_TENANT_SQL, &[&scope.tenant()])
                .await?
                .is_some();
            let operator = !cap.definition.platform || is_operator(client, scope.tenant()).await?;
            if !live {
                Decision::deny("denied_invalid_actor_context")
            } else if !operator {
                Decision::deny("denied_platform_scope")
            } else {
                let row = client
                    .query_one(GRANTS_SQL, &[&scope.tenant(), &scope.user(), &key])
                    .await?;
                judge(&row)
            }
        }
    };
    log(client, scope.tenant(), scope.user(), None, key, decision).await?;
    Ok(decision)
}
/// `decide` for an owner adapter that holds only Tenancy's sealed proof
/// connection (`OrganisationDirectory`, Media `SubjectOwner`, Artifacts
/// `Owner`): the same vocabulary, tenant, operator and deny-wins rules, the
/// same decision log, on the caller's connection or transaction.
pub async fn decide_proof(client: &dyn ProofClient, scope: &Scope, key: &str) -> Result<Decision> {
    if key.is_empty() || key.len() > 200 {
        return Err(Error::Invalid);
    }
    if let Some(principal) = scope.principal() {
        return system_decision(&Proof(client), scope.tenant(), Some(principal), key).await;
    }
    let decision = match vocabulary()?.capabilities.get(key) {
        None => Decision::deny("denied_unknown_capability"),
        Some(cap) => {
            let live = client
                .query_opt(LIVE_TENANT_SQL, &[&scope.tenant()])
                .await?
                .is_some();
            let operator = !cap.definition.platform
                || client
                    .query_opt(OPERATOR_SQL, &[&scope.tenant()])
                    .await?
                    .is_some();
            if !live {
                Decision::deny("denied_invalid_actor_context")
            } else if !operator {
                Decision::deny("denied_platform_scope")
            } else {
                let row = client
                    .query_opt(GRANTS_SQL, &[&scope.tenant(), &scope.user(), &key])
                    .await?
                    .ok_or(Error::Internal)?;
                judge(&row)
            }
        }
    };
    let kind = "user";
    let principal: Option<&str> = None;
    client
        .query_opt(
            LOG_RETURNING_SQL,
            &[
                &scope.tenant(),
                &scope.user(),
                &key,
                &decision.allowed,
                &decision.reason,
                &kind,
                &principal,
            ],
        )
        .await?;
    Ok(decision)
}
/// `authorize_key` on a sealed proof connection.
pub async fn authorize_proof(client: &dyn ProofClient, scope: &Scope, key: &str) -> Result<()> {
    decide_proof(client, scope, key).await?.require()
}
const LIVE_TENANT_SQL: &str =
    "SELECT id FROM tenants WHERE id=$1 AND status='active' AND deleted_at IS NULL";
const OPERATOR_SQL: &str = "SELECT o.tenant_id FROM platform_operator o JOIN tenants t ON t.id=o.tenant_id WHERE o.singleton AND o.tenant_id=$1 AND t.status='active' AND t.deleted_at IS NULL";
const GRANTS_SQL: &str = "SELECT EXISTS(SELECT 1 FROM principal_capabilities WHERE tenant_id=$1 AND user_id=$2 AND capability=$3 AND NOT is_allowed), EXISTS(SELECT 1 FROM principal_capabilities WHERE tenant_id=$1 AND user_id=$2 AND capability=$3 AND is_allowed), EXISTS(SELECT 1 FROM effective_principal_roles p JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2 AND r.grant_all), EXISTS(SELECT 1 FROM effective_principal_roles p JOIN role_capabilities r ON r.tenant_id=p.tenant_id AND r.role_id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2 AND r.capability=$3)";
const SYSTEM_GRANT_SQL: &str = "SELECT is_allowed FROM authz_system_grants WHERE tenant_id=$1 AND principal=$2 AND capability=$3";
const LOG_SQL: &str = "INSERT INTO authz_decision_log(tenant_id,user_id,capability,allowed,reason,principal_type,system_principal) VALUES($1,$2,$3,$4,$5,$6,$7)";
const LOG_RETURNING_SQL: &str = "INSERT INTO authz_decision_log(tenant_id,user_id,capability,allowed,reason,principal_type,system_principal) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id";
/// Deny wins over every allow; an allow names the grant that carried it.
fn judge(row: &tokio_postgres::Row) -> Decision {
    if row.get::<_, bool>(0) {
        Decision::deny("denied_explicitly")
    } else if row.get::<_, bool>(1) {
        Decision::allow("direct_capability")
    } else if row.get::<_, bool>(2) {
        Decision::allow("grant_all")
    } else if row.get::<_, bool>(3) {
        Decision::allow("role_capability")
    } else {
        Decision::deny("denied_missing_capability")
    }
}
pub(crate) async fn is_operator(client: &impl GenericClient, tenant: i64) -> Result<bool> {
    Ok(client.query_opt(OPERATOR_SQL, &[&tenant]).await?.is_some())
}
async fn log(
    client: &impl GenericClient,
    tenant: i64,
    user: i64,
    principal: Option<&str>,
    key: &str,
    decision: Decision,
) -> Result<()> {
    let kind = if user == 0 { "system" } else { "user" };
    client
        .execute(
            LOG_SQL,
            &[
                &tenant,
                &user,
                &key,
                &decision.allowed,
                &decision.reason,
                &kind,
                &principal,
            ],
        )
        .await?;
    Ok(())
}
/// Trusted worker identity; not deserializable or usable as a user scope. It
/// holds no roles, cannot administer grants, and gains nothing by declaration.
#[derive(Clone, Debug)]
pub struct SystemScope {
    tenant: i64,
    principal: Option<String>,
}
impl SystemScope {
    pub fn tenant(&self) -> i64 {
        self.tenant
    }
    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }
    /// Attribution for an already authorized worker write. Context is evidence,
    /// never a capability grant; the worker must authorize before beginning it.
    pub fn mutation_context(&self, action: impl Into<String>) -> bilibi_database::Context {
        let mut context = bilibi_database::Context::user(self.tenant, 0, action);
        context.actor = bilibi_database::Principal {
            kind: "system".into(),
            id: self.principal.clone().unwrap_or_else(|| "0".into()),
        };
        context
    }
    /// Rehydrate persisted worker metadata at the trusted execution edge. Every
    /// decision still checks the live tenant, declaration and durable grant.
    pub async fn rehydrate(
        client: &impl GenericClient,
        tenant: i64,
        principal: Option<&str>,
    ) -> Result<Self> {
        if client
            .query_opt(
                "SELECT id FROM tenants WHERE id=$1 AND status='active' AND deleted_at IS NULL",
                &[&tenant],
            )
            .await?
            .is_none()
        {
            return Err(Error::Unauthorized);
        }
        if principal
            .is_some_and(|name| !vocabulary().is_ok_and(|v| v.system_principals.contains_key(name)))
        {
            return Err(Error::Forbidden);
        }
        Ok(Self {
            tenant,
            principal: principal.map(str::to_owned),
        })
    }
}
pub async fn decide_system(
    client: &impl GenericClient,
    scope: &SystemScope,
    key: &str,
) -> Result<Decision> {
    system_decision(client, scope.tenant, scope.principal(), key).await
}
/// The one read the named-principal rule needs, so the same decision runs on
/// a pooled client, a transaction or Tenancy's sealed proof connection.
trait Reader {
    async fn one(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>>;
}
impl<T: GenericClient> Reader for T {
    async fn one(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>> {
        Ok(self.query_opt(sql, params).await?)
    }
}
struct Proof<'a>(&'a dyn ProofClient);
impl Reader for Proof<'_> {
    async fn one(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>> {
        self.0.query_opt(sql, params).await
    }
}
/// The named-principal rule, shared by `decide_system` and by `decide` /
/// `decide_proof` on a principal's sealed scope: a live tenant, a known key, a
/// named principal, the platform gate, the installed declaration, then the
/// durable tenant grant, where a denying grant wins. Logged as `system` with
/// the principal; roles and grant-all never apply.
async fn system_decision(
    client: &impl Reader,
    tenant: i64,
    principal: Option<&str>,
    key: &str,
) -> Result<Decision> {
    if key.is_empty() || key.len() > 200 {
        return Err(Error::Invalid);
    }
    let vocab = vocabulary()?;
    let live = client.one(LIVE_TENANT_SQL, &[&tenant]).await?.is_some();
    let decision = match principal {
        _ if !live => Decision::deny("denied_invalid_actor_context"),
        _ if !vocab.capabilities.contains_key(key) => Decision::deny("denied_unknown_capability"),
        None => Decision::deny("denied_no_principal"),
        Some(name) => {
            let platform = vocab.capabilities[key].definition.platform;
            if platform && client.one(OPERATOR_SQL, &[&tenant]).await?.is_none() {
                Decision::deny("denied_platform_scope")
            } else if !vocab
                .system_principals
                .get(name)
                .is_some_and(|declared| declared.capabilities.contains(&key))
            {
                Decision::deny("denied_undeclared_capability")
            } else {
                match client
                    .one(SYSTEM_GRANT_SQL, &[&tenant, &name, &key])
                    .await?
                    .map(|row| row.get::<_, bool>(0))
                {
                    Some(true) => Decision::allow("system_principal_grant"),
                    Some(false) => Decision::deny("denied_explicitly"),
                    None if vocab.standing(name, key) => {
                        Decision::allow("system_principal_standing")
                    }
                    None => Decision::deny("denied_missing_capability"),
                }
            }
        }
    };
    let kind = "system";
    client
        .one(
            LOG_RETURNING_SQL,
            &[
                &tenant,
                &0_i64,
                &key,
                &decision.allowed,
                &decision.reason,
                &kind,
                &principal,
            ],
        )
        .await?;
    Ok(decision)
}
/// `Authentication::for_system`: the sealed `Scope` a worker runs a named
/// system principal's job under (SP1). Authz proves the declaration, then
/// Tenancy seals the principal (`Authentication::for_principal`); every
/// decision on that scope comes back here as the named-principal rule. An
/// anonymous `SystemScope` gets no scope: it can hold no capability.
pub trait SystemAuthentication {
    fn for_system<'a, C: GenericClient + Sync>(
        &'a self,
        client: &'a C,
        scope: &'a SystemScope,
    ) -> ProofFuture<'a, Scope>;
}
impl SystemAuthentication for Authentication {
    fn for_system<'a, C: GenericClient + Sync>(
        &'a self,
        client: &'a C,
        scope: &'a SystemScope,
    ) -> ProofFuture<'a, Scope> {
        Box::pin(async move {
            let principal = scope.principal().ok_or(Error::Forbidden)?;
            if !vocabulary()?.system_principals.contains_key(principal) {
                return Err(Error::Forbidden);
            }
            self.for_principal(client, scope.tenant(), principal).await
        })
    }
}
pub async fn authorize_system(
    client: &impl GenericClient,
    scope: &SystemScope,
    key: &str,
) -> Result<()> {
    decide_system(client, scope, key).await?.require()
}
/// Operator identity and capability are independent, logged requirements.
pub async fn authorize_operator(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    key: &str,
) -> Result<bilibi_tenancy::OperatorProof> {
    if !is_operator(client, scope.tenant()).await? {
        if key.is_empty() || key.len() > 200 {
            return Err(Error::Invalid);
        }
        log(
            client,
            scope.tenant(),
            scope.user(),
            scope.principal(),
            key,
            Decision::deny("denied_platform_scope"),
        )
        .await?;
        return Err(Error::Forbidden);
    }
    authorize_key(client, scope, key).await?;
    bilibi_tenancy::prove_operator(client, scope).await
}
/// Whether `key` is granted by name, directly or through a role listing it,
/// with no direct denial. A `grant_all` role does not count: some capabilities
/// (stored-secret reveal) must be handed out deliberately. Pair it with
/// `decide`, which still applies tenant, platform and logging rules.
pub async fn explicitly_granted(
    client: &impl GenericClient,
    scope: &Scope,
    key: &str,
) -> Result<bool> {
    // A principal's only grants are by name: declared, allowed and not denied.
    if let Some(principal) = scope.principal() {
        let declared = vocabulary()?
            .system_principals
            .get(principal)
            .is_some_and(|declared| declared.capabilities.contains(&key));
        return Ok(declared
            && client
                .query_opt(SYSTEM_GRANT_SQL, &[&scope.tenant(), &principal, &key])
                .await?
                .is_some_and(|row| row.get::<_, bool>(0)));
    }
    Ok(client.query_one("SELECT NOT EXISTS(SELECT 1 FROM principal_capabilities WHERE tenant_id=$1 AND user_id=$2 AND capability=$3 AND NOT is_allowed) AND (EXISTS(SELECT 1 FROM principal_capabilities WHERE tenant_id=$1 AND user_id=$2 AND capability=$3 AND is_allowed) OR EXISTS(SELECT 1 FROM effective_principal_roles p JOIN role_capabilities r ON r.tenant_id=p.tenant_id AND r.role_id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2 AND r.capability=$3))",&[&scope.tenant(),&scope.user(),&key]).await?.get(0))
}
/// The `base/tenancy.authorization` binding: `authorize_operator` on a pooled
/// connection, so Tenancy's own screens decide `CREATE` without importing Authz.
pub struct OperatorAuthorizer {
    db: bilibi_database::Db,
}
impl bilibi_tenancy::OperatorAuthorizer for OperatorAuthorizer {
    fn authorize_operator_in<'a>(
        &'a self,
        tx: &'a tokio_postgres::Transaction<'_>,
        scope: &'a Scope,
        key: &'a str,
    ) -> bilibi_tenancy::ProofFuture<'a, bilibi_tenancy::OperatorProof> {
        Box::pin(async move { authorize_operator(tx, scope, key).await })
    }
    fn authorize_operator<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
    ) -> bilibi_tenancy::ProofFuture<'a, bilibi_tenancy::OperatorProof> {
        Box::pin(async move {
            let client = self.db.get().await?;
            authorize_operator(&**client, scope, key).await
        })
    }
    fn authorize_scope_in<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        key: &'a str,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move { authorize_proof(client, scope, key).await })
    }
}
pub async fn restrictions(client: &impl GenericClient, scope: &Scope) -> Result<HashSet<String>> {
    restrictions_for(client, scope, "companies").await
}
pub async fn restrictions_for(
    client: &impl GenericClient,
    scope: &Scope,
    table: &str,
) -> Result<HashSet<String>> {
    if table.is_empty() || table.len() > 200 {
        return Err(Error::Invalid);
    }
    // A principal holds no roles, so no role's restriction can be shown to
    // hold for it: every field any role in the tenant is restricted from stays
    // withheld.
    if scope.principal().is_some() {
        return Ok(client.query("SELECT DISTINCT field_id FROM field_restrictions WHERE tenant_id=$1 AND table_id=$2",&[&scope.tenant(),&table]).await?.iter().map(|row|row.get(0)).collect());
    }
    Ok(client.query("SELECT DISTINCT f.field_id FROM field_restrictions f JOIN effective_principal_roles p ON p.tenant_id=f.tenant_id AND p.role_id=f.role_id WHERE f.tenant_id=$1 AND p.user_id=$2 AND f.table_id=$3",&[&scope.tenant(),&scope.user(),&table]).await?.iter().map(|row|row.get(0)).collect())
}
inventory::submit! { bilibi_module_registry::Module {
    id:"base/authz",layer:bilibi_module_registry::Layer::Base,
    dependencies:&["base/module_registry","base/database","base/tenancy","base/audit","base/menu","base/settings","base/ui","base/locale","base/datetime","base/step_up"],
    migrations:&[("base-authz-012-external-roles",include_str!("../migrations/012-external-roles.sql")),("007-authz-policy",include_str!("../migrations/007-policy.sql")),("base-authz-008-decision-log-restrictions",include_str!("../migrations/008-decision-log-restrictions.sql")),("base-authz-009-roles-admin",include_str!("../migrations/009-roles-admin.sql")),("base-authz-010-decision-log-retention",include_str!("../migrations/010-decision-log-retention.sql")),("base-authz-011-role-company",include_str!("../migrations/011-role-company.sql"))],
    services:&[bilibi_module_registry::Service {id:"base/authz.grants",factory:|services| Ok(Arc::new(grants::Grants::new(
        services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
        services.get("base/database.mutations")?, vocabulary::Vocabulary::from_inventory()?, services.get("base/tenancy.authentication")?,
        services.get("base/tenancy.parties")?, services.get(bilibi_step_up::SERVICE)?,
    ))) }, bilibi_module_registry::Service { id:"base/authz.seeds", factory:|services| Ok(Arc::new(bilibi_database::seeds::SeedService(services.get::<grants::Grants>("base/authz.grants")?))) },
    bilibi_module_registry::Service {id:"base/authz.field_restrictions",factory:|services| Ok(Arc::new(restrictions::FieldRestrictions::new(
        services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
        services.get("base/database.mutations")?, services.get("base/tenancy.authentication")?,
    ))) },
    // Tenancy sits below Authz, so Authz binds the decision its screens need.
    bilibi_module_registry::Service {id:"base/authz.operator",factory:|services| {
        let authorizer = Arc::new(OperatorAuthorizer { db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone() });
        services.get::<bilibi_tenancy::Authorization>("base/tenancy.authorization")?.bind(authorizer.clone())?;
        Ok(authorizer)
    }},
    bilibi_module_registry::Service {id:"base/authz.pages",factory:|services| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
        db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
        display: services.get("base/datetime.display")?, locales: services.get("base/locale.locales")?,
        grants: services.get("base/authz.grants")?,
        restrictions: services.get("base/authz.field_restrictions")?,
        parties: services.get("base/tenancy.parties")?,
        step_up: services.get(bilibi_step_up::SERVICE)?,
        mutations: services.get("base/database.mutations")?,
    })))) }],
} }

// Tenancy's capability keys (Bilimbi `Tenancy.Contributions`): declared here
// because the vocabulary type is Authz's and Tenancy sits below it; the keys
// themselves are `bilibi_tenancy::{LIST, CREATE}`.
inventory::submit! { bilibi_module_registry::Contribution::new(VOCABULARY,"authz.tenancy",bilibi_module_registry::Provenance {module:"base/authz",source:file!(),line:line!()}, &VocabularyContribution {
    domains:&["admin"], verbs:&["list","create"],
    capabilities:&[CapabilityDefinition::platform(bilibi_tenancy::LIST), CapabilityDefinition::platform(bilibi_tenancy::CREATE)],
    roles:&[], system_principals:&[],
}) }
inventory::submit! {
    bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "base/authz.decision-logs",
        bilibi_module_registry::Provenance {module:"base/authz",source:file!(),line:line!()},
        &bilibi_module_registry::contributions::Route { method:bilibi_module_registry::contributions::Method::Get, path:"/authz/decision-logs",
            service:"base/authz.pages", operation:"decision-logs", access:bilibi_module_registry::contributions::Access::Capability(decision_logs::LIST), framed:true })
}
macro_rules! page_route {
    ($id:literal, $path:literal, $operation:literal, $capability:expr) => {
        inventory::submit! {
            bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, $id,
                bilibi_module_registry::Provenance {module:"base/authz",source:file!(),line:line!()},
                &bilibi_module_registry::contributions::Route { method:bilibi_module_registry::contributions::Method::Get, path:$path,
                    service:"base/authz.pages", operation:$operation, access:bilibi_module_registry::contributions::Access::Capability($capability), framed:true })
        }
    };
}
page_route!(
    "base/authz.capabilities",
    "/authz/capabilities",
    "capabilities",
    capabilities::LIST
);
page_route!("base/authz.roles", "/authz/roles", "roles", roles::LIST);
// A literal segment outranks the parameter, so "create" never reaches the
// detail page as an id.
page_route!(
    "base/authz.role-create",
    "/authz/roles/create",
    "role-create",
    roles::CREATE
);
page_route!("base/authz.role", "/authz/roles/{id}", "role", roles::VIEW);
page_route!(
    "base/authz.field-restrictions",
    "/authz/field-restrictions",
    "field-restrictions",
    restrictions::MANAGE
);
// Bilimbi's Authorization menu (`contributions.ex`): items join it as their
// screens land; the container stays hidden until one is visible.
bilibi_menu::contribute!(
    "base/authz",
    bilibi_menu::MenuItem::container("admin.authz", "Authorization")
        .icon("shield-check")
        .under("admin")
        .order(20)
);
bilibi_menu::contribute!(
    "base/authz",
    bilibi_menu::MenuItem::page(
        "admin.authz.capability",
        "Capabilities",
        "/authz/capabilities"
    )
    .icon("puzzle-piece")
    .under("admin.authz")
    .requires(bilibi_menu::Requirement::Capability(capabilities::LIST))
    .order(10)
);
bilibi_menu::contribute!(
    "base/authz",
    bilibi_menu::MenuItem::page("admin.authz.role", "Roles", "/authz/roles")
        .icon("shield-check")
        .under("admin.authz")
        .requires(bilibi_menu::Requirement::Capability(roles::LIST))
        .order(20)
);
bilibi_menu::contribute!(
    "base/authz",
    bilibi_menu::MenuItem::page(
        "admin.authz.decision-log",
        "Decision Logs",
        "/authz/decision-logs"
    )
    .icon("clipboard-document-list")
    .under("admin.authz")
    .requires(bilibi_menu::Requirement::Capability(decision_logs::LIST))
    .order(50)
);
page_route!(
    "base/authz.step-up",
    "/authz/step-up",
    "step-up",
    step_up_web::MANAGE
);
bilibi_menu::contribute!(
    "base/authz",
    bilibi_menu::MenuItem::page("admin.authz.step-up", "Step-up 2FA", "/authz/step-up")
        .icon("lock-closed")
        .under("admin.authz")
        .requires(bilibi_menu::Requirement::Capability(step_up_web::MANAGE))
        .order(70)
);
bilibi_menu::contribute!(
    "base/authz",
    bilibi_menu::MenuItem::page(
        "admin.authz.field-restrictions",
        "Field Restrictions",
        "/authz/field-restrictions"
    )
    .icon("lock-closed")
    .under("admin.authz")
    .requires(bilibi_menu::Requirement::Capability(restrictions::MANAGE))
    .order(60)
);

inventory::submit! {
    bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::SEEDS,
        "authz.roles.v1", bilibi_module_registry::Provenance {module:"base/authz",source:file!(),line:line!()},
        &bilibi_module_registry::contributions::Seed { version:"authz-system-roles-v1", mode:bilibi_module_registry::contributions::SeedMode::Production,
            action:bilibi_module_registry::contributions::SeedAction::Service {service:"base/authz.seeds",operation:"reconcile"} })
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "base/authz.pages", bilibi_module_registry::Provenance { module: "base/authz", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "base/authz.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Roles, field restrictions and two-factor policy write through the Authz facades; base/authz_agent's grant contracts serve agents only." },
}) }
