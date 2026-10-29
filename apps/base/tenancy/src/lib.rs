//! Sealed identities for a single unit of work. No Base query of Core tables.
//! Mirrors Bilimbi `Base.Tenancy`; Core Tenant Administration composes its pages.
mod admin;
pub use admin::{
    platform_operator, provision_platform_operator, provision_tenant, CreateOutcome, CreatedTenant,
    NewTenant, TenantErrors, TenantIdentity, TenantStatus, Tenants, CREATE, LIST, NAME_MAX,
};
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

/// Object-safe reads for owners that need lists as well as party proofs.
/// A borrowed transaction stays the transaction used for every query.
pub trait ReadClient: ProofClient {
    fn query<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> ProofFuture<'a, Vec<Row>>;
}
impl<T: GenericClient + Sync> ReadClient for T {
    fn query<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> ProofFuture<'a, Vec<Row>> {
        Box::pin(async move { Ok(GenericClient::query(self, sql, params).await?) })
    }
}

/// Owner output, never deserialize from a browser/job. Core User proves account
/// eligibility and each party facet through the owners' live reads
/// (`bilibi_user::standing`), on every authentication.
pub struct AccountIdentity {
    pub name: String,
    pub email: String,
    pub person: Option<PersonId>,
    pub organisation: Option<OrganisationId>,
    pub affiliation: Option<AffiliationId>,
    /// Staff or a customer of the tenant (`users.kind`, ADR 16).
    pub kind: AccountKind,
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
    /// Whether the account `scope` was just sealed for may work in
    /// `company` right now: a live, unarchived company of its tenant that it
    /// reaches (its own affiliation, or any company with Company's
    /// tenant-wide reach). Asked on every authentication for the company the
    /// session chose (`Scope::current_company`), so a choice outlives neither its
    /// reach nor the company. A verifier that cannot tell keeps this default,
    /// which never confirms, and the scope falls back to the affiliation.
    fn reaches_company<'a>(
        &'a self,
        _client: &'a dyn ProofClient,
        _scope: &'a Scope,
        _company: OrganisationId,
    ) -> ProofFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
}

/// Which audience an account belongs to (ADR 16). A customer signs in only to
/// its tenant's portal (`/portal/...`) and holds only portal capabilities; a
/// staff account never reaches the portal. Sealed from the account at every
/// authentication, never read from a request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountKind {
    Staff,
    Customer,
}
impl AccountKind {
    /// The stored value (`users.kind`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staff => "staff",
            Self::Customer => "customer",
        }
    }
    /// `None` for a value the schema does not allow.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "staff" => Some(Self::Staff),
            "customer" => Some(Self::Customer),
            _ => None,
        }
    }
}

/// Who a sealed scope acts as: an account with its live party facets, or a
/// named system principal (SP1) with none of them.
#[derive(Clone, Debug)]
pub struct Actor {
    user: i64,
    kind: AccountKind,
    person: Option<PersonId>,
    organisation: Option<OrganisationId>,
    affiliation: Option<AffiliationId>,
    principal: Option<String>,
}
impl Actor {
    /// The account acted as. `0` for a named system principal: no account has
    /// that id, so every user-shaped grant or ownership read matches nothing
    /// and the principal holds only what `Authz` grants it by name.
    pub fn user(&self) -> i64 {
        self.user
    }
    /// The named system principal this scope runs as (`sbg.ax_import`,
    /// `scheduler`), sealed by `Authentication::for_principal`; `None` for an
    /// account. Audit shows it as `System · <principal>`.
    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
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
    /// Staff or customer. A named system principal is staff: it is the
    /// tenant's own unattended work, never a customer.
    pub fn kind(&self) -> AccountKind {
        self.kind
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
    /// A queued job calling a write contract for the person who started it
    /// (`bilibi_agent_api::job`, ADR 10).
    Job,
}
impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Api => "api",
            Self::Mcp => "mcp",
            Self::Job => "job",
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
    self_report: Option<SelfReport>,
    client_identity: Option<ClientIdentity>,
}
impl AgentFacet {
    pub fn connection(&self) -> i64 {
        self.connection
    }
    /// One request's correlation ID: audit trace and exchange log share it.
    pub fn trace(&self) -> &str {
        &self.trace
    }
    /// Client transport identity, distinct from the model's self-report.
    pub fn client_identity(&self) -> Option<&ClientIdentity> {
        self.client_identity.as_ref()
    }
    /// What the agent said about itself on this request. Evidence only.
    pub fn self_report(&self) -> Option<&SelfReport> {
        self.self_report.as_ref()
    }
}
/// An agent's self-reported `harness/model-version-effort` (ADR 01, "Agent
/// identity at runtime"). It is what the agent says about itself: recorded
/// and shown labelled "Model (as reported)", never used for authorization.
/// Only the shape is checked: one `/` between the harness and the
/// `model-version-effort` part, either side may be empty but not both, and
/// each side holds only ASCII letters, digits, `.`, `_` and `-`, so the
/// value is safe to show anywhere. Hyphens inside a model name make the
/// parts after the `/` ambiguous; they are kept as the agent sent them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelfReport(String);
impl SelfReport {
    pub const MAX_LEN: usize = 120;
    /// The one label every surface shows the value under.
    pub const LABEL: &'static str = "Model (as reported)";
    /// The request header the agent edge (`/api/v1`, `/mcp`) reads it from.
    pub const HEADER: &'static str = "x-bilibi-agent-self-report";
    /// Model-accessible argument on the six generic MCP tools.
    pub const MCP_ARGUMENT: &'static str = "agent_self_report";
    /// The MCP `params._meta` key, for clients that cannot set headers.
    pub const MCP_META: &'static str = "bilibi/agent-self-report";
    /// `None` for a malformed value; the caller drops it and says so.
    pub fn parse(value: &str) -> Option<Self> {
        let (harness, rest) = value.split_once('/')?;
        let part = |part: &str| {
            part.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        (value.len() <= Self::MAX_LEN
            && !(harness.is_empty() && rest.is_empty())
            && part(harness)
            && part(rest))
        .then(|| Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Transport-observed client label for the MCP edge. This says
/// what the client sent, not which model ran; never authentication or authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientIdentity(String);
impl ClientIdentity {
    pub const LABEL: &'static str = "Client identity";
    pub const MAX_LEN: usize = 255;
    pub fn parse(value: &str) -> Option<Self> {
        (!value.is_empty()
            && value.len() <= Self::MAX_LEN
            && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)))
        .then(|| Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
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
    /// Re-read identity and grant standing on the caller's connection.
    authentication: Authentication,
    /// Set by `in_job`: the request is a queued job, not the browser.
    job: bool,
    /// A step-up code presented with this request, if any.
    step_up: Option<Box<PresentedCode>>,
    /// The company the session chose to work in, kept only while the
    /// verifier confirms the account reaches it (`Scope::current_company`).
    company: Option<OrganisationId>,
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
    /// The company this request works in: the one the browser session chose
    /// (the shell's company switcher), when the account still reaches it,
    /// else the account's own company through its affiliation
    /// (`Actor::organisation`). A page or report that operates "on the
    /// current company" reads this; an owner that decides reach or binds a
    /// record to the person's employer keeps reading the affiliation. The
    /// choice grants nothing: every read and write still proves the company
    /// as its target with its own capability (`bilibi_company::authorize_target`,
    /// `OrganisationDirectory::target`). An agent (bearer) scope and a system
    /// principal carry no choice, so this is their affiliation, or `None`;
    /// agent operations name their company explicitly.
    pub fn current_company(&self) -> Option<OrganisationId> {
        self.company.or(self.actor.organisation)
    }
    /// The session's explicit company choice, when it holds one the account
    /// reaches now; `None` means the affiliation's company (`current_company`).
    pub fn chosen_company(&self) -> Option<OrganisationId> {
        self.company
    }
    /// The named system principal this scope runs as, `None` for an account
    /// (`Actor::principal`). A principal scope has no user, person,
    /// organisation, affiliation, agent facet, impersonator or step-up code.
    pub fn principal(&self) -> Option<&str> {
        self.actor.principal()
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
    pub fn for_agent(
        self,
        connection: i64,
        trace: String,
        self_report: Option<SelfReport>,
    ) -> Self {
        Self {
            agent: Some(AgentFacet {
                connection,
                trace,
                channel: Channel::Api,
                self_report,
                client_identity: None,
            }),
            csrf: uuid::Uuid::new_v4().simple().to_string(),
            ..self
        }
    }
    /// Adds transport evidence without changing the sealed actor or grant.
    pub fn with_client_identity(mut self, client: Option<ClientIdentity>) -> Self {
        if let Some(agent) = &mut self.agent {
            agent.client_identity = client;
        }
        self
    }
    /// Web for a browser session; the agent edge it came through otherwise,
    /// and `Job` for a queued job's call (`in_job`).
    pub fn channel(&self) -> Channel {
        if self.job {
            return Channel::Job;
        }
        self.agent
            .as_ref()
            .map_or(Channel::Web, |agent| agent.channel)
    }
    /// Marks the scope a queued job re-sealed for its person as the job
    /// door, so a contract that names its channels sees the job, not a
    /// browser. Descriptive only; it adds and removes no authority.
    pub fn in_job(mut self) -> Self {
        self.job = true;
        self
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
        self.step_up = (!code.is_empty() && code.len() <= 256).then(|| {
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
        // A principal's writes are system-attributed by name (`System ·
        // <principal>`); it never carries an impersonator or agent facet.
        if let Some(principal) = self.principal() {
            return Context::system(self.tenant, principal, action);
        }
        let mut context = Context::user(self.tenant, self.user(), action);
        context.impersonator = self
            .impersonator
            .as_ref()
            .map(|impersonator| bilibi_database::Principal::user(impersonator.user));
        if let Some(agent) = &self.agent {
            context.trace_id.clone_from(&agent.trace);
            context.client_identity = agent
                .client_identity
                .as_ref()
                .map(|client| client.0.clone());
            context.agent_connection = Some(agent.connection);
            context.agent_self_report = agent.self_report.as_ref().map(|report| report.0.clone());
        }
        context
    }
}

/// A customer's proof on a portal request (ADR 16): the sealed `Scope` of a
/// customer account, with the Person it signs in as and, for a business
/// customer, the affiliation and company. Portal facades take this and filter
/// "my records" by `person()`, or by `organisation()` when it is present, so
/// the ids come from the proof, never from the request. A private individual
/// has no company: `organisation()` and `affiliation()` are `None`, and code
/// must then show only the Person's own records. It cannot be built from a
/// staff scope, a system principal, an agent or an impersonated session.
#[derive(Clone, Debug)]
pub struct PortalScope {
    scope: Scope,
    person: PersonId,
}
impl PortalScope {
    /// Seals a portal scope from a scope the host authenticated from a portal
    /// session. Refuses anything that is not a customer acting in person.
    pub fn seal(scope: Scope) -> Result<Self> {
        if scope.actor.kind != AccountKind::Customer
            || scope.principal().is_some()
            || scope.agent().is_some()
            || scope.impersonator().is_some()
        {
            return Err(Error::Unauthorized);
        }
        let person = scope.actor.person.ok_or(Error::Unauthorized)?;
        Ok(Self { scope, person })
    }
    /// The Person the customer signs in as; always present.
    pub fn person(&self) -> PersonId {
        self.person
    }
    /// The customer's company, through its affiliation; `None` for a private
    /// individual.
    pub fn organisation(&self) -> Option<OrganisationId> {
        self.scope.actor.organisation
    }
    /// The affiliation the customer signs in through; `None` for a private
    /// individual.
    pub fn affiliation(&self) -> Option<AffiliationId> {
        self.scope.actor.affiliation
    }
    /// The underlying scope, for audit (`mutation_context`) and for portal
    /// capability checks. A staff facade given it refuses: Authz never grants
    /// a staff capability to a customer.
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
}

/// The credential owner rechecks an agent's live grant, bound by Core Agent
/// Access. Missing bindings refuse agent reproofs, never confer standing.
pub trait AgentVerifier: Send + Sync {
    fn verify<'a>(&'a self, client: &'a dyn ProofClient, scope: &'a Scope) -> ProofFuture<'a, ()>;
}
/// The longest principal name a sealed scope carries: Audit's
/// `system_principal` column width.
const PRINCIPAL_MAX: usize = 100;
#[derive(Clone, Default)]
pub struct Authentication {
    verifier: Arc<OnceLock<Arc<dyn ActorVerifier>>>,
    agents: Arc<OnceLock<Arc<dyn AgentVerifier>>>,
}
impl std::fmt::Debug for Authentication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authentication").finish_non_exhaustive()
    }
}
impl Authentication {
    pub fn bind_agents(
        &self,
        verifier: Arc<dyn AgentVerifier>,
    ) -> bilibi_module_registry::Result<()> {
        self.agents.set(verifier).map_err(|_| {
            bilibi_module_registry::RegistryError::Service("duplicate agent verifier".into())
        })
    }
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
        // A system principal has no credential to confirm.
        if scope.impersonator().is_some() || scope.principal().is_some() {
            return Err(Error::Forbidden);
        }
        self.verifier
            .get()
            .ok_or(Error::Unauthorized)?
            .confirm_password(client, scope.tenant(), scope.user(), password)
            .await
    }
    /// Scoped live account evidence for routing and independent controls. This
    /// proves identity only; it seals no credential and grants no authority.
    pub async fn account_identity(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        account: i64,
    ) -> Result<AccountIdentity> {
        if account <= 0 {
            return Err(Error::Invalid);
        }
        self.verifier
            .get()
            .ok_or(Error::Unauthorized)?
            .verify(client, scope.tenant(), account)
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
        self.authenticate_session(client, tenant, user, None, None, csrf)
            .await
    }
    /// The durable session edge: `impersonator` is the operator the session
    /// row names, never a value from the browser. Both accounts are re-proved
    /// live in the same tenant, so a removed or frozen operator ends the
    /// impersonated session as surely as a removed target does. `company` is
    /// the company the session row says the person chose to work in; it is
    /// kept on the scope only when the verifier confirms the account reaches
    /// it now (`ActorVerifier::reaches_company`), and otherwise dropped, so
    /// the scope falls back to the affiliation (`Scope::current_company`).
    pub async fn authenticate_session(
        &self,
        client: &(impl GenericClient + Sync),
        tenant: i64,
        user: i64,
        impersonator: Option<i64>,
        company: Option<i64>,
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
        // A customer always stands on a Person (`users_customer_needs_person`).
        if identity.kind == AccountKind::Customer && identity.person.is_none() {
            return Err(Error::Unauthorized);
        }
        let impersonator = match impersonator {
            Some(operator) => {
                // Only staff impersonate, and nobody impersonates a customer.
                let operator_identity = verifier.verify(client, tenant, operator).await?;
                if operator_identity.kind != AccountKind::Staff
                    || identity.kind != AccountKind::Staff
                {
                    return Err(Error::Unauthorized);
                }
                Some(Impersonator {
                    user: operator,
                    name: operator_identity.name,
                })
            }
            None => None,
        };
        let mut scope = Scope {
            tenant,
            actor: Actor {
                user,
                kind: identity.kind,
                person: identity.person,
                organisation: identity.organisation,
                affiliation: identity.affiliation,
                principal: None,
            },
            agent: None,
            impersonator,
            authentication: self.clone(),
            job: false,
            step_up: None,
            company: None,
            tenant_name: row.get(0),
            user_name: identity.name,
            email: identity.email,
            csrf,
        };
        // The chosen company is a request until its owner confirms the reach
        // on this connection; an id that is not even a company id is nothing.
        // A customer has no company switcher: its company is its affiliation's.
        let company = company.filter(|_| identity.kind == AccountKind::Staff);
        if let Some(company) = company.and_then(|id| OrganisationId::new(id).ok()) {
            if verifier.reaches_company(client, &scope, company).await? {
                scope.company = Some(company);
            }
        }
        Ok(scope)
    }
    /// The trusted worker edge for a named system principal (SP1): a sealed
    /// scope whose actor is the principal, with no account, party facet,
    /// agent, impersonator or step-up code, in a live tenant. Its CSRF value
    /// is fresh so no form check can match. Never a browser constructor: the
    /// principal comes from a worker's rehydrated job row or a schedule
    /// definition, and `Authz` (`SystemAuthentication::for_system`) proves
    /// the declaration first. Identity only: the principal holds exactly the
    /// capabilities an administrator granted it by name in this tenant, which
    /// every decision re-reads.
    pub async fn for_principal(
        &self,
        client: &(impl GenericClient + Sync),
        tenant: i64,
        principal: &str,
    ) -> Result<Scope> {
        if tenant <= 0 || principal.is_empty() || principal.chars().count() > PRINCIPAL_MAX {
            return Err(Error::Unauthorized);
        }
        let row = client
            .query_opt(
                "SELECT name FROM tenants WHERE id=$1 AND deleted_at IS NULL AND status='active'",
                &[&tenant],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
        Ok(Scope {
            tenant,
            actor: Actor {
                user: 0,
                kind: AccountKind::Staff,
                person: None,
                organisation: None,
                affiliation: None,
                principal: Some(principal.to_owned()),
            },
            agent: None,
            impersonator: None,
            authentication: self.clone(),
            job: false,
            step_up: None,
            company: None,
            tenant_name: row.get(0),
            user_name: format!("System · {principal}"),
            email: String::new(),
            csrf: uuid::Uuid::new_v4().simple().to_string(),
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
        // A principal scope is re-sealed against the live tenant; it carries
        // no agent facet or step-up code to keep.
        if let Some(principal) = scope.principal() {
            return self.for_principal(client, scope.tenant(), principal).await;
        }
        let mut fresh = self
            .authenticate_session(
                client,
                scope.tenant(),
                scope.user(),
                scope.impersonator().map(Impersonator::user),
                scope.chosen_company().map(OrganisationId::get),
                scope.csrf.clone(),
            )
            .await?;
        // The request's own facts stay: its agent grant and channel, and a
        // step-up code it presented. The CSRF value is the scope's already;
        // the chosen company was re-proved with the identity above.
        fresh.agent.clone_from(&scope.agent);
        if fresh.agent().is_some() {
            self.agents
                .get()
                .ok_or(Error::Unauthorized)?
                .verify(client, &fresh)
                .await?;
        }
        fresh.step_up.clone_from(&scope.step_up);
        fresh.job = scope.job;
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
    fn authorize_operator_in<'a>(
        &'a self,
        tx: &'a tokio_postgres::Transaction<'_>,
        scope: &'a Scope,
        key: &'a str,
    ) -> ProofFuture<'a, OperatorProof>;

    fn authorize_operator<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
    ) -> ProofFuture<'a, OperatorProof>;

    /// One capability decision for `scope` on the caller's connection (Authz
    /// `authorize_proof`): the way a module below Authz (Settings) enforces a
    /// capability on a sealed scope, including a system principal's named
    /// grant. `Err(Forbidden)` is a refusal, logged by the implementation.
    fn authorize_scope_in<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        key: &'a str,
    ) -> ProofFuture<'a, ()>;
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
    pub async fn authorize_operator_in(
        &self,
        tx: &tokio_postgres::Transaction<'_>,
        scope: &Scope,
        key: &str,
    ) -> Result<OperatorProof> {
        self.authorizer
            .get()
            .ok_or(Error::Forbidden)?
            .authorize_operator_in(tx, scope, key)
            .await
    }
    pub async fn authorize_operator(&self, scope: &Scope, key: &str) -> Result<OperatorProof> {
        self.authorizer
            .get()
            .ok_or(Error::Forbidden)?
            .authorize_operator(scope, key)
            .await
    }
    /// Enforces `key` for `scope` on the caller's connection through the
    /// bound Authz decision; without a binding every decision refuses.
    pub async fn authorize_scope(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
        key: &str,
    ) -> Result<()> {
        self.authorizer
            .get()
            .ok_or(Error::Forbidden)?
            .authorize_scope_in(client, scope, key)
            .await
    }
}
async fn require_operator_marker(client: &(impl GenericClient + Sync), tenant: i64) -> Result<()> {
    client
        .query_opt("SELECT t.id FROM tenants t JOIN platform_operator o ON o.tenant_id=t.id WHERE o.singleton=true AND t.id=$1 AND t.status='active' AND t.deleted_at IS NULL", &[&tenant])
        .await?
        .map(|_| ())
        .ok_or(Error::Forbidden)
}
pub async fn prove_operator(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<OperatorProof> {
    // Operator standing is an account's: a principal decides a platform
    // capability through Authz's own operator gate and never holds a proof
    // that administers tenants or writes global settings.
    if scope.principal().is_some() {
        return Err(Error::Forbidden);
    }
    require_operator_marker(client, scope.tenant()).await?;
    let fresh = scope.authentication.reprove(client, scope).await?;
    Ok(OperatorProof { scope: fresh })
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
/// Owner standing belongs to the person named by the live bootstrap receipt,
/// including an agent acting for that person. Impersonation never holds it.
/// Reproof checks account eligibility and agent revocation on this connection;
/// no cached flag, role or capability confers ownership.
pub async fn prove_owner(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<OwnerProof> {
    // Owner standing belongs to a person; neither an operator behind an
    // impersonated session nor a system principal holds it.
    if scope.impersonator().is_some() || scope.principal().is_some() {
        return Err(Error::Forbidden);
    }
    require_operator_marker(client, scope.tenant()).await?;
    let fresh = &scope.authentication.reprove(client, scope).await?;
    let identity = fresh
        .authentication
        .verifier
        .get()
        .ok_or(Error::Unauthorized)?
        .verify(client, fresh.tenant(), fresh.user())
        .await?;
    if !identity.installation_owner {
        return Err(Error::Forbidden);
    }
    Ok(OwnerProof {
        scope: fresh.clone(),
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
/// Mirrors Bilimbi `Authz.CompanyDirectory`'s company summary.
/// `status` carries the full company lifecycle state; `archived` is retained
/// for callers that only need the read-only flag. An id here is still a
/// request: every write reproves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedOrganisation {
    pub id: OrganisationId,
    pub name: String,
    /// Core Company's stored status: active, pending, suspended or archived.
    pub status: String,
    /// Equivalent to `status == "archived"` for summaries from Core Company.
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
    /// The unarchived companies this actor may work in, whatever the
    /// operation: its own company, or every company of the tenant with
    /// Company's tenant-wide reach (the shell's company switcher offers
    /// these). No capability is asked, because the list is the reach alone;
    /// each operation still proves its target with its own capability.
    fn reachable<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Vec<NamedOrganisation>>;
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
        dependencies: &["base/database", "base/party_ids", "base/module_registry"],
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
                services.get("base/tenancy.authorization")?,
            ))) },
        ],
    }
}
inventory::submit! {
    bilibi_module_registry::Ready { module: "base/tenancy", check: |services| {
        services.get::<Authentication>("base/tenancy.authentication")?.ready()?;
        services.get::<Authorization>("base/tenancy.authorization")?.ready()
    } }
}
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
    /// The companies the actor may work in (`OrganisationDirectory::reachable`);
    /// none when no Company owner is installed.
    pub async fn reachable_organisations(
        &self,
        client: &dyn ProofClient,
        scope: &Scope,
    ) -> Result<Vec<NamedOrganisation>> {
        match self.directory.get() {
            Some(directory) => directory.reachable(client, scope).await,
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
