//! Sealed identities for a single unit of work. No Base query of Core tables.
//! `admin` is tenant administration; `web` the module-owned `/tenancy/tenants`.
pub mod admin;
mod web;
use bilibi_database::{Context, Error, Result};
use bilibi_party_ids::{AffiliationId, OrganisationId, PersonId};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};
use tokio_postgres::{types::ToSql, GenericClient, Row};

pub type ProofFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
/// Object-safe read adapter preserves the caller's transaction for owner proofs.
pub trait ProofClient: Sync {
    fn query_opt<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> ProofFuture<'a, Option<Row>>;
}
impl<T: GenericClient + Sync> ProofClient for T {
    fn query_opt<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> ProofFuture<'a, Option<Row>> {
        Box::pin(async move { Ok(GenericClient::query_opt(self, sql, params).await?) })
    }
}

/// Owner output, never deserialize from a browser/job. Core User proves account
/// eligibility and each party facet through the owners' live reads
/// (`bilibi_user::identity::standing`), on every authentication.
pub struct AccountIdentity {
    pub name: String,
    pub email: String,
    pub person: Option<PersonId>,
    pub organisation: Option<OrganisationId>,
    pub affiliation: Option<AffiliationId>,
    /// Whether this account is the installation owner: the one account the
    /// installation's bootstrap receipt names (Core User `user_bootstrap`).
    /// Owner standing; never a grant, and read afresh on every proof.
    pub installation_owner: bool,
}
pub trait ActorVerifier: Send + Sync {
    fn verify<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        tenant: i64,
        user: i64,
    ) -> ProofFuture<'a, AccountIdentity>;
    /// Re-proves the holder of an authenticated session before a sensitive
    /// read (stored-secret reveal). A verifier that cannot check credentials
    /// keeps this default, which never confirms.
    fn confirm_password<'a>(
        &'a self,
        _client: &'a dyn ProofClient,
        _tenant: i64,
        _user: i64,
        _password: &'a str,
    ) -> ProofFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
}

#[derive(Clone, Debug)]
pub struct Actor {
    user: i64,
    person: Option<PersonId>,
    organisation: Option<OrganisationId>,
    affiliation: Option<AffiliationId>,
}
impl Actor {
    pub fn user(&self) -> i64 {
        self.user
    }
    pub fn person(&self) -> Option<PersonId> {
        self.person
    }
    pub fn organisation(&self) -> Option<OrganisationId> {
        self.organisation
    }
    pub fn affiliation(&self) -> Option<AffiliationId> {
        self.affiliation
    }
}
/// The surface a request arrived through. Step-up (`base/step_up`) picks
/// its proof by it and audit records it; it grants nothing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Channel {
    /// A browser session.
    #[default]
    Web,
    /// The agent edge `/api/v1`.
    Api,
    /// The agent edge `/mcp`.
    Mcp,
}
impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Api => "api",
            Self::Mcp => "mcp",
        }
    }
}

/// A step-up code the human account holder gave for this one request (an
/// agent's commit carries it). A claim, not a proof: only the step-up
/// provider can verify and spend it. Its `Debug` never shows the value.
///
/// The code covers the one request it came with: once the provider accepted
/// it, every further step-up check of this request (clones of the scope share
/// the mark) passes on that acceptance instead of spending it again.
#[derive(Clone)]
pub struct PresentedCode {
    value: String,
    accepted: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl PresentedCode {
    pub fn expose(&self) -> &str {
        &self.value
    }
    /// Whether the provider already accepted this code for this request.
    pub fn accepted(&self) -> bool {
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// Recorded by Base Step-up after the provider accepted the code.
    pub fn mark_accepted(&self) {
        self.accepted
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
impl std::fmt::Debug for PresentedCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PresentedCode([redacted])")
    }
}

/// The OAuth grant a request authenticated with, sealed by the host's bearer
/// edge after live actor proof. It narrows the account; it never widens it.
#[derive(Clone, Debug)]
pub struct AgentFacet {
    connection: i64,
    trace: String,
    channel: Channel,
}
impl AgentFacet {
    pub fn connection(&self) -> i64 {
        self.connection
    }
    /// One request's correlation ID: audit trace and exchange log share it.
    pub fn trace(&self) -> &str {
        &self.trace
    }
}
/// The operator behind an impersonated session (Bilimbi `impersonator_id`),
/// sealed from the durable session row and re-proved live with the actor on
/// every authentication. The actor is the account acted as; this is who acts.
#[derive(Clone, Debug)]
pub struct Impersonator {
    user: i64,
    name: String,
}
impl Impersonator {
    pub fn user(&self) -> i64 {
        self.user
    }
    /// Display only, from the operator's own live account.
    pub fn name(&self) -> &str {
        &self.name
    }
}
/// Private tenant + actor cannot be spliced, constructed, or deserialized by callers.
/// Display fields are compatibility data and never authority.
#[derive(Clone, Debug)]
pub struct Scope {
    tenant: i64,
    actor: Actor,
    agent: Option<AgentFacet>,
    impersonator: Option<Impersonator>,
    /// The actor's live installation-owner standing, sealed by the verifier.
    /// Read only through [`prove_owner`], which adds the operator proof.
    owner: bool,
    /// A step-up code presented with this request, if any.
    step_up: Option<Box<PresentedCode>>,
    pub tenant_name: String,
    pub user_name: String,
    pub email: String,
    pub csrf: String,
}
impl Scope {
    pub fn tenant(&self) -> i64 {
        self.tenant
    }
    pub fn user(&self) -> i64 {
        self.actor.user
    }
    pub fn actor(&self) -> &Actor {
        &self.actor
    }
    /// Present only on a bearer request; self-service account and grant
    /// management refuse a scope that carries it.
    pub fn agent(&self) -> Option<&AgentFacet> {
        self.agent.as_ref()
    }
    /// Present only on a browser session an operator entered through
    /// impersonation. Self-service writes of the account's own preferences,
    /// credentials and grants refuse a scope that carries it.
    pub fn impersonator(&self) -> Option<&Impersonator> {
        self.impersonator.as_ref()
    }
    /// Seals the grant onto a scope its owner just proved live for that grant's
    /// tenant and account. Only adds a restriction, so no new authority follows;
    /// the browser CSRF value is replaced so no form check can match.
    pub fn for_agent(self, connection: i64, trace: String) -> Self {
        Self {
            agent: Some(AgentFacet {
                connection,
                trace,
                channel: Channel::Api,
            }),
            csrf: uuid::Uuid::new_v4().simple().to_string(),
            ..self
        }
    }
    /// Web for a browser session; the agent edge it came through otherwise.
    pub fn channel(&self) -> Channel {
        self.agent
            .as_ref()
            .map_or(Channel::Web, |agent| agent.channel)
    }
    /// Marks an agent scope as arriving over `/mcp` rather than `/api/v1`.
    /// Descriptive only; a browser scope is left as it is.
    pub fn over_mcp(mut self) -> Self {
        if let Some(agent) = &mut self.agent {
            agent.channel = Channel::Mcp;
        }
        self
    }
    /// Attaches a step-up code the request carried. Bounded and trimmed; an
    /// empty or oversized value attaches nothing.
    pub fn presenting_step_up(mut self, code: &str) -> Self {
        let code = code.trim();
        self.step_up = (!code.is_empty() && code.len() <= 64).then(|| {
            Box::new(PresentedCode {
                value: code.to_owned(),
                accepted: std::sync::Arc::default(),
            })
        });
        self
    }
    /// The code this request presented for step-up.
    pub fn step_up_code(&self) -> Option<&PresentedCode> {
        self.step_up.as_deref()
    }
    pub fn mutation_context(&self, action: impl Into<String>) -> Context {
        let mut context = Context::user(self.tenant, self.user(), action);
        context.impersonator = self
            .impersonator
            .as_ref()
            .map(|impersonator| bilibi_database::Principal::user(impersonator.user));
        if let Some(agent) = &self.agent {
            context.trace_id.clone_from(&agent.trace);
        }
        context
    }
}

#[derive(Default)]
pub struct Authentication {
    verifier: OnceLock<Arc<dyn ActorVerifier>>,
}
impl Authentication {
    /// Trusted boot seam. Duplicate and missing providers fail closed.
    pub fn bind(&self, verifier: Arc<dyn ActorVerifier>) -> bilibi_module_registry::Result<()> {
        self.verifier.set(verifier).map_err(|_| {
            bilibi_module_registry::RegistryError::Service("duplicate actor verifier".into())
        })
    }
    pub fn ready(&self) -> bilibi_module_registry::Result<()> {
        self.verifier.get().map(|_| ()).ok_or_else(|| {
            bilibi_module_registry::RegistryError::Service("missing actor verifier".into())
        })
    }
    /// Whether `password` is the scope's own current password. Callers throttle
    /// and audit attempts; this grants nothing and a missing verifier refuses.
    pub async fn confirm_password(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        password: &str,
    ) -> Result<bool> {
        // The operator behind an impersonated session does not hold the
        // account's credential, and a re-proof must never be lent to them.
        if scope.impersonator().is_some() {
            return Err(Error::Forbidden);
        }
        self.verifier
            .get()
            .ok_or(Error::Unauthorized)?
            .confirm_password(client, scope.tenant(), scope.user(), password)
            .await
    }
    /// Authentication edge only: IDs must come from a durable credential/session.
    /// This method establishes identity; it grants no capabilities.
    pub async fn authenticate(
        &self,
        client: &(impl GenericClient + Sync),
        tenant: i64,
        user: i64,
        csrf: String,
    ) -> Result<Scope> {
        self.authenticate_session(client, tenant, user, None, csrf)
            .await
    }
    /// The durable session edge: `impersonator` is the operator the session
    /// row names, never a value from the browser. Both accounts are re-proved
    /// live in the same tenant, so a removed or frozen operator ends the
    /// impersonated session as surely as a removed target does.
    pub async fn authenticate_session(
        &self,
        client: &(impl GenericClient + Sync),
        tenant: i64,
        user: i64,
        impersonator: Option<i64>,
        csrf: String,
    ) -> Result<Scope> {
        if tenant <= 0 || user <= 0 || impersonator.is_some_and(|id| id <= 0 || id == user) {
            return Err(Error::Unauthorized);
        }
        let row = client
            .query_opt(
                "SELECT name FROM tenants WHERE id=$1 AND deleted_at IS NULL AND status='active'",
                &[&tenant],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
        let verifier = self.verifier.get().ok_or(Error::Unauthorized)?;
        let identity = verifier.verify(client, tenant, user).await?;
        if identity.affiliation.is_some()
            && (identity.person.is_none() || identity.organisation.is_none())
        {
            return Err(Error::Unauthorized);
        }
        let impersonator = match impersonator {
            Some(operator) => Some(Impersonator {
                user: operator,
                name: verifier.verify(client, tenant, operator).await?.name,
            }),
            None => None,
        };
        Ok(Scope {
            tenant,
            actor: Actor {
                user,
                person: identity.person,
                organisation: identity.organisation,
                affiliation: identity.affiliation,
            },
            agent: None,
            impersonator,
            owner: identity.installation_owner,
            step_up: None,
            tenant_name: row.get(0),
            user_name: identity.name,
            email: identity.email,
            csrf,
        })
    }
    /// Re-proves a scope an edge already sealed, keeping its impersonator,
    /// agent facet and presented step-up code: owners refresh eligibility with this rather than
    /// `authenticate`, which would drop the operator from the audit pair.
    pub async fn reprove(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Scope> {
        let mut fresh = self
            .authenticate_session(
                client,
                scope.tenant(),
                scope.user(),
                scope.impersonator().map(Impersonator::user),
                scope.csrf.clone(),
            )
            .await?;
        // The request's own facts stay: its agent grant and channel, and a
        // step-up code it presented. The CSRF value is the scope's already.
        fresh.agent.clone_from(&scope.agent);
        fresh.step_up.clone_from(&scope.step_up);
        Ok(fresh)
    }
}

/// Explicit persistent operator identity; tenant IDs and grant_all do not imply it.
#[derive(Clone, Debug)]
pub struct OperatorProof {
    scope: Scope,
}
impl OperatorProof {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
}

/// A capability decision for Tenancy's own screens, on top of operator
/// identity. Authz sits above Tenancy, so it binds the implementation at boot
/// (`authorize_operator`: both requirements, each independently logged) and
/// Tenancy never imports it. `Err(Forbidden)` is a refusal to show, not a fault.
pub trait OperatorAuthorizer: Send + Sync {
    fn authorize_operator<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
    ) -> ProofFuture<'a, OperatorProof>;
}
/// `base/tenancy.authorization`: once-bound; without a binding every decision
/// refuses and boot is refused by the readiness check.
#[derive(Default)]
pub struct Authorization {
    authorizer: OnceLock<Arc<dyn OperatorAuthorizer>>,
}
impl Authorization {
    pub fn bind(
        &self,
        authorizer: Arc<dyn OperatorAuthorizer>,
    ) -> bilibi_module_registry::Result<()> {
        self.authorizer.set(authorizer).map_err(|_| {
            bilibi_module_registry::RegistryError::Service("duplicate operator authorizer".into())
        })
    }
    pub fn ready(&self) -> bilibi_module_registry::Result<()> {
        self.authorizer.get().map(|_| ()).ok_or_else(|| {
            bilibi_module_registry::RegistryError::Service("missing operator authorizer".into())
        })
    }
    pub async fn authorize_operator(&self, scope: &Scope, key: &str) -> Result<OperatorProof> {
        self.authorizer
            .get()
            .ok_or(Error::Forbidden)?
            .authorize_operator(scope, key)
            .await
    }
}
pub async fn prove_operator(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<OperatorProof> {
    let row = client.query_opt("SELECT t.id FROM tenants t JOIN platform_operator o ON o.tenant_id=t.id WHERE o.singleton=true AND t.id=$1 AND t.status='active' AND t.deleted_at IS NULL", &[&scope.tenant()]).await?;
    if row.is_none() {
        return Err(Error::Forbidden);
    }
    Ok(OperatorProof {
        scope: scope.clone(),
    })
}

/// The installation owner, proved for one unit of work: see [`prove_owner`].
#[derive(Clone, Debug)]
pub struct OwnerProof {
    scope: Scope,
}
impl OwnerProof {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
}
/// Owner standing: the account the installation's one-time bootstrap receipt
/// names, still live in the platform-operator tenant, acting for itself in a
/// browser session. An impersonated session or an agent grant never carries
/// it, and no role or capability confers it. The verifier re-reads the
/// receipt on every authentication; this re-proves the operator marker.
pub async fn prove_owner(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<OwnerProof> {
    if !scope.owner || scope.impersonator.is_some() || scope.agent.is_some() {
        return Err(Error::Forbidden);
    }
    prove_operator(client, scope).await?;
    Ok(OwnerProof {
        scope: scope.clone(),
    })
}

/// Core providers prove live, eligible targets in this exact actor scope. They
/// must reject deleted, archived, cross-tenant and inaccessible identities.
pub trait OrganisationProvider: Send + Sync {
    fn prove<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: OrganisationId,
    ) -> ProofFuture<'a, ()>;
}
pub trait PersonProvider: Send + Sync {
    fn prove<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: PersonId,
    ) -> ProofFuture<'a, ()>;
}
/// One organisation of the actor's tenant, named as the actor may see it.
/// `archived` companies are listed so a picker can show them read-only, and
/// an id here is still a request: every write reproves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedOrganisation {
    pub id: OrganisationId,
    pub name: String,
    pub archived: bool,
}
/// Core Company's naming half of the organisation seam (Bilimbi
/// `Authz.CompanyDirectory` + `Company.list_selectable_companies/2` +
/// `authorize_company_target/3`), bound beside `OrganisationProvider` so Base
/// pickers and binders never query the `companies` table. Every method keeps
/// to the actor's own tenant; a missing or cross-tenant id is `NotFound` and
/// never confirms that a company exists elsewhere.
pub trait OrganisationDirectory: Send + Sync {
    /// Every live company of the tenant, sorted case-insensitively by the
    /// displayed name, archived ones flagged.
    fn in_scope<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Vec<NamedOrganisation>>;
    /// The unarchived companies this actor may target with `capability`:
    /// none without the capability, the actor's own company, or every company
    /// of the tenant with Company's tenant-wide reach.
    fn selectable<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        capability: &'a str,
    ) -> ProofFuture<'a, Vec<NamedOrganisation>>;
    /// One company as the target of `capability`: `Forbidden` without the
    /// capability or the reach, `NotFound` when missing or in another tenant.
    /// An archived company is returned flagged; the caller decides whether
    /// the operation is a read.
    fn target<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: OrganisationId,
        capability: &'a str,
    ) -> ProofFuture<'a, NamedOrganisation>;
    /// A live company's name for display, `None` when missing or elsewhere.
    fn lookup<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: OrganisationId,
    ) -> ProofFuture<'a, Option<NamedOrganisation>>;
}
#[derive(Clone, Debug)]
pub struct OrganisationProof {
    scope: Scope,
    id: OrganisationId,
}
impl OrganisationProof {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn id(&self) -> OrganisationId {
        self.id
    }
}
#[derive(Clone, Debug)]
pub struct PersonProof {
    scope: Scope,
    id: PersonId,
}
impl PersonProof {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn id(&self) -> PersonId {
        self.id
    }
}
pub async fn prove_organisation(
    provider: &dyn OrganisationProvider,
    client: &dyn ProofClient,
    scope: &Scope,
    id: OrganisationId,
) -> Result<OrganisationProof> {
    provider.prove(client, scope, id).await?;
    Ok(OrganisationProof {
        scope: scope.clone(),
        id,
    })
}
pub async fn prove_person(
    provider: &dyn PersonProvider,
    client: &dyn ProofClient,
    scope: &Scope,
    id: PersonId,
) -> Result<PersonProof> {
    provider.prove(client, scope, id).await?;
    Ok(PersonProof {
        scope: scope.clone(),
        id,
    })
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/tenancy", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/database", "base/party_ids", "base/menu", "base/ui"],
        migrations: &[
            ("005-tenancy-operator", include_str!("../migrations/005-operator.sql")),
            ("009-tenancy-hierarchy", include_str!("../migrations/009-hierarchy.sql")),
        ],
        services: &[
            bilibi_module_registry::Service { id: "base/tenancy.authentication", factory: |_| Ok(Arc::new(Authentication::default())) },
            bilibi_module_registry::Service { id: "base/tenancy.parties", factory: |_| Ok(Arc::new(PartyProofs::default())) },
            bilibi_module_registry::Service { id: "base/tenancy.authorization", factory: |_| Ok(Arc::new(Authorization::default())) },
            bilibi_module_registry::Service { id: "base/tenancy.tenants", factory: |services| Ok(Arc::new(admin::Tenants::new(
                services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                services.get("base/database.mutations")?,
            ))) },
            bilibi_module_registry::Service { id: "base/tenancy.pages", factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                tenants: services.get("base/tenancy.tenants")?,
                authorization: services.get("base/tenancy.authorization")?,
            })))) },
        ],
    }
}
inventory::submit! {
    bilibi_module_registry::Ready { module: "base/tenancy", check: |services| {
        services.get::<Authentication>("base/tenancy.authentication")?.ready()?;
        services.get::<Authorization>("base/tenancy.authorization")?.ready()
    } }
}
inventory::submit! {
    bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "base/tenancy.tenants",
        bilibi_module_registry::Provenance { module: "base/tenancy", source: file!(), line: line!() },
        &bilibi_module_registry::contributions::Route { method: bilibi_module_registry::contributions::Method::Get, path: "/tenancy/tenants",
            service: "base/tenancy.pages", operation: "tenants",
            access: bilibi_module_registry::contributions::Access::OperatorCapability(admin::LIST), framed: true })
}
// Bilimbi's Tenancy menu (`contributions.ex`): the branch stays hidden until
// the actor may list tenants, and the route enforces the same gate itself.
bilibi_menu::contribute!(
    "base/tenancy",
    bilibi_menu::MenuItem::container("admin.tenancy", "Tenancy")
        .icon("building-library")
        .under("admin")
        .order(80)
);
bilibi_menu::contribute!(
    "base/tenancy",
    bilibi_menu::MenuItem::page("admin.tenancy.tenant", "Tenants", "/tenancy/tenants")
        .under("admin.tenancy")
        .requires(bilibi_menu::Requirement::Capability(admin::LIST))
        .order(10)
);

/// Optional owner bindings. Missing Company/Person adapters always refuse proof
/// and name nothing; installing unrelated Core modules never implicitly makes
/// targets trusted.
#[derive(Default)]
pub struct PartyProofs {
    organisations: OnceLock<Arc<dyn OrganisationProvider>>,
    persons: OnceLock<Arc<dyn PersonProvider>>,
    directory: OnceLock<Arc<dyn OrganisationDirectory>>,
}
impl PartyProofs {
    pub fn bind_directory(
        &self,
        directory: Arc<dyn OrganisationDirectory>,
    ) -> bilibi_module_registry::Result<()> {
        self.directory.set(directory).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate organisation directory".into(),
            )
        })
    }
    /// Listings are empty, not refused, when no Company owner is installed:
    /// a picker then says no company is available, and nothing is trusted.
    pub async fn organisations_in_scope(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
    ) -> Result<Vec<NamedOrganisation>> {
        match self.directory.get() {
            Some(directory) => directory.in_scope(client, scope).await,
            None => Ok(Vec::new()),
        }
    }
    pub async fn selectable_organisations(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        capability: &str,
    ) -> Result<Vec<NamedOrganisation>> {
        match self.directory.get() {
            Some(directory) => directory.selectable(client, scope, capability).await,
            None => Ok(Vec::new()),
        }
    }
    /// A target with no directory bound is refused, like a missing provider.
    pub async fn organisation_target(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        id: OrganisationId,
        capability: &str,
    ) -> Result<NamedOrganisation> {
        self.directory
            .get()
            .ok_or(Error::Forbidden)?
            .target(client, scope, id, capability)
            .await
    }
    pub async fn organisation_name(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        id: OrganisationId,
    ) -> Result<Option<NamedOrganisation>> {
        match self.directory.get() {
            Some(directory) => directory.lookup(client, scope, id).await,
            None => Ok(None),
        }
    }
    pub fn bind_organisations(
        &self,
        provider: Arc<dyn OrganisationProvider>,
    ) -> bilibi_module_registry::Result<()> {
        self.organisations.set(provider).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate organisation proof provider".into(),
            )
        })
    }
    pub fn bind_persons(
        &self,
        provider: Arc<dyn PersonProvider>,
    ) -> bilibi_module_registry::Result<()> {
        self.persons.set(provider).map_err(|_| {
            bilibi_module_registry::RegistryError::Service("duplicate person proof provider".into())
        })
    }
    pub async fn organisation(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        id: OrganisationId,
    ) -> Result<OrganisationProof> {
        prove_organisation(
            self.organisations.get().ok_or(Error::Forbidden)?.as_ref(),
            client,
            scope,
            id,
        )
        .await
    }
    pub async fn person(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        id: PersonId,
    ) -> Result<PersonProof> {
        prove_person(
            self.persons.get().ok_or(Error::Forbidden)?.as_ref(),
            client,
            scope,
            id,
        )
        .await
    }
}
