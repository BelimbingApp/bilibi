//! The step-up facade (`base/step_up.gate`): an owner calls
//! [`StepUp::require`] with its declared operation key and the target it is
//! about to change, and gets `Ok` or `Error::StepUpRequired`. Which proof
//! counts is decided here per channel; how a person is checked belongs to the
//! bound [`Provider`], so this contract knows nothing of codes or apps.
use crate::{
    declaration::{Operation, OPERATIONS},
    policy::{self, Mode},
};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Change, Db, Error, MutationTransaction, Result};
use bilibi_module_registry::Catalog;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Channel, ProofClient, ProofFuture, Scope};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock, RwLock,
    },
};

/// What a presented code is being spent on; the provider may record it.
pub struct Purpose<'a> {
    pub operation: &'a str,
    pub target: &'a str,
    pub channel: Channel,
}

/// What a provider says about a presented code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Spent {
    /// Valid, the account holder's own, and now used: it proves nothing else.
    Accepted,
    /// Wrong, expired or already used.
    Rejected,
    /// The holder's checks are locked after repeated failures.
    Locked,
    /// The holder has no confirmed second factor.
    NotEnrolled,
}

/// The browser session's own proof (the session row is the provider's to
/// read: Base Session sits above this module).
#[derive(Clone, Copy, Debug)]
pub enum SessionProof {
    /// A check within the cap whose last covered action is within the
    /// window; `extend` restarts the window from now.
    Window {
        window_seconds: i64,
        cap_seconds: i64,
        extend: bool,
    },
    /// A check within `seconds` that no "ask every time" action used yet;
    /// using it spends it.
    Fresh { seconds: i64, consume: bool },
}

/// What a window looked like when asked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowState {
    Holds,
    /// No check yet (or none left for an "ask every time" action).
    None,
    /// The sliding window ran out.
    Expired,
    /// The absolute cap from the check was reached.
    Capped,
}
impl WindowState {
    pub fn from_flags(checked: bool, within_cap: bool, within_window: bool) -> Self {
        match (checked, within_cap, within_window) {
            (false, _, _) => Self::None,
            (true, false, _) => Self::Capped,
            (true, true, false) => Self::Expired,
            (true, true, true) => Self::Holds,
        }
    }
}

/// A trusted account within the caller's tenant. Constructed by StepUp from
/// a sealed scope or the owning mutation's context, never from request tenant IDs.
#[derive(Clone, Copy)]
pub struct FactorAccount {
    tenant: i64,
    user: i64,
}
impl FactorAccount {
    pub fn tenant(self) -> i64 {
        self.tenant
    }
    pub fn user(self) -> i64 {
        self.user
    }
}

/// Account-wide factor cleanup. Revocation clears pending challenges/proofs,
/// while reset and deletion also remove enrollment (including pending setup).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    Revoke,
    Reset,
    Delete,
}

/// A method's stable identity and human-facing label. IDs must be namespaced
/// (`core.user.authenticator`, for example); registration rejects duplicates.
#[derive(Clone, Copy)]
pub struct Method {
    pub id: &'static str,
    pub label: &'static str,
    pub code_length: usize,
    pub numeric: bool,
}
impl Method {
    pub const fn new(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            code_length: 6,
            numeric: true,
        }
    }
    pub const fn code(mut self, length: usize, numeric: bool) -> Self {
        self.code_length = length;
        self.numeric = numeric;
        self
    }
}

/// Live, read-only presentation of one method. Neither destination starts a
/// challenge: sending/delivering must require an explicit user action.
pub struct MethodChoice {
    pub method: Method,
    pub enrolled: bool,
    pub prompt: String,
    pub setup: String,
}

/// A challenge start result. A provider owns delivery, rate limits and durable
/// challenge state. Destinations must already be masked; never return a secret.
pub struct Challenge {
    pub availability: Availability,
    pub masked_destination: Option<String>,
    pub retry_after_seconds: Option<u64>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Ready,
    Unavailable,
    RetryLater,
}

/// No Bilimbi counterpart. Each second-factor owner registers independently.
/// Reads must have no side effects. All lifecycle writes use the supplied SQL
/// transaction and return every affected record, without committing or fan-out.
pub trait Provider: Send + Sync {
    fn enrolled<'a>(
        &'a self,
        sql: &'a dyn ProofClient,
        account: FactorAccount,
    ) -> ProofFuture<'a, bool>;
    fn start<'a>(&'a self, scope: &'a Scope, purpose: Purpose<'a>) -> ProofFuture<'a, Challenge>;
    fn spend<'a>(
        &'a self,
        scope: &'a Scope,
        code: &'a str,
        purpose: Purpose<'a>,
    ) -> ProofFuture<'a, Spent>;
    fn prompt<'a>(&'a self, scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String>;
    fn setup<'a>(&'a self, scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String>;
    fn lifecycle<'a>(
        &'a self,
        sql: &'a tokio_postgres::Transaction<'_>,
        account: FactorAccount,
        event: Lifecycle,
    ) -> ProofFuture<'a, Vec<Change>>;
}

/// The identity/session owner binds shared browser proof separately from
/// factors. A method need not reimplement the installation's session storage.
pub trait SessionProvider: Send + Sync {
    /// Neutral method choice destination supplied by the browser identity owner.
    fn prompt<'a>(&'a self, scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String>;
    fn session<'a>(&'a self, scope: &'a Scope, proof: SessionProof)
        -> ProofFuture<'a, WindowState>;
}

/// One declared operation with its owner module.
pub struct Declared {
    pub operation: &'static Operation,
    pub module: &'static str,
}

/// One operation as the policy page shows it.
pub struct Rule<'a> {
    pub declared: &'a Declared,
    /// What the tenant chose, if anything.
    pub choice: Option<Mode>,
    /// Installation floor: the tenant cannot turn it off.
    pub floor: bool,
    pub effective: Mode,
}

/// The web window as it applies to a tenant, and the installation limits.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    pub window_minutes: i64,
    pub cap_hours: i64,
    pub window_limit_minutes: i64,
    pub cap_limit_hours: i64,
    /// What the tenant stored (before the installation limits apply).
    pub tenant_window_minutes: i64,
    pub tenant_cap_hours: i64,
}

/// The installation owner supplies current enforcement policy. Missing binding fails closed.
pub trait Enforcement: Send + Sync {
    fn enforced(&self) -> ProofFuture<'_, bool>;
}

type Registered = (Method, Arc<dyn Provider>);

pub struct StepUp {
    db: Db,
    settings: Arc<Settings>,
    operations: BTreeMap<&'static str, Declared>,
    providers: RwLock<BTreeMap<&'static str, Registered>>,
    session_provider: OnceLock<Arc<dyn SessionProvider>>,
    enforcement: OnceLock<Arc<dyn Enforcement>>,
    /// Turned off only by a fixture binary that has no authenticator.
    enforced: AtomicBool,
}

enum Outcome {
    Passed(&'static str),
    Refused(&'static str),
    Failed(&'static str),
}

impl StepUp {
    pub fn new(
        db: Db,
        settings: Arc<Settings>,
        catalog: &Catalog,
    ) -> bilibi_module_registry::Result<Self> {
        let mut operations = BTreeMap::new();
        for entry in catalog.entries::<Operation>(OPERATIONS)? {
            let operation = entry.get::<Operation>()?;
            operations.insert(
                operation.key,
                Declared {
                    operation,
                    module: entry.provenance.module,
                },
            );
        }
        Ok(Self {
            db,
            settings,
            operations,
            providers: RwLock::new(BTreeMap::new()),
            session_provider: OnceLock::new(),
            enforcement: OnceLock::new(),
            enforced: AtomicBool::new(true),
        })
    }
    /// Register during service installation. Ordering is by stable method ID;
    /// duplicate IDs refuse boot rather than silently choosing a winner.
    pub fn register(
        &self,
        method: Method,
        provider: Arc<dyn Provider>,
    ) -> bilibi_module_registry::Result<()> {
        let mut providers = self.providers.write().map_err(|_| {
            bilibi_module_registry::RegistryError::Service("step-up registry poisoned".into())
        })?;
        if !(1..=64).contains(&method.code_length)
            || method.id.is_empty()
            || method.id.len() > 120
            || !method.id.contains('.')
            || !method
                .id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
            || method.label.trim().is_empty()
            || method.label.len() > 120
            || method.label.chars().any(char::is_control)
            || providers.contains_key(method.id)
        {
            return Err(bilibi_module_registry::RegistryError::Service(format!(
                "invalid or duplicate step-up method: {}",
                method.id
            )));
        }
        providers.insert(method.id, (method, provider));
        Ok(())
    }
    pub fn bind_session(
        &self,
        provider: Arc<dyn SessionProvider>,
    ) -> bilibi_module_registry::Result<()> {
        self.session_provider.set(provider).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate step-up session provider".into(),
            )
        })
    }
    fn providers(&self) -> Result<Vec<Registered>> {
        Ok(self
            .providers
            .read()
            .map_err(|_| Error::Internal)?
            .values()
            .cloned()
            .collect())
    }
    /// Enrollment is read in the owner's transaction (including another
    /// account's reset). The tenant comes only from that transaction's context.
    pub async fn account_enrolled(&self, tx: &MutationTransaction<'_>, user: i64) -> Result<bool> {
        self.enrolled_in(
            tx.sql(),
            FactorAccount {
                tenant: tx.context().tenant,
                user,
            },
        )
        .await
    }
    async fn enrolled_in(&self, sql: &dyn ProofClient, account: FactorAccount) -> Result<bool> {
        if account.user <= 0 {
            return Err(Error::Invalid);
        }
        for (_, provider) in self.providers()? {
            if provider.enrolled(sql, account).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Read another account in the scope's tenant, using the caller's read
    /// connection (including an operation preview's read-only transaction).
    pub async fn enrolled_for(
        &self,
        sql: &dyn ProofClient,
        scope: &Scope,
        user: i64,
    ) -> Result<bool> {
        self.enrolled_in(
            sql,
            FactorAccount {
                tenant: scope.tenant(),
                user,
            },
        )
        .await
    }
    /// Fan out in deterministic order inside the owner's mutation boundary.
    /// A marker lets mutate poison failures/cancellation even for a no-op hook.
    pub async fn lifecycle(
        &self,
        tx: &mut MutationTransaction<'_>,
        user: i64,
        event: Lifecycle,
    ) -> Result<()> {
        if user <= 0 {
            return Err(Error::Invalid);
        }
        let account = FactorAccount {
            tenant: tx.context().tenant,
            user,
        };
        let providers = self.providers()?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                for (_, provider) in providers {
                    changes.extend(provider.lifecycle(sql, account, event).await?);
                }
                changes.push(Change {
                    kind: "step_up_lifecycle".into(),
                    id: user,
                    old: json!({}),
                    new: json!({"event": format!("{event:?}").to_lowercase()}),
                });
                Ok(((), changes))
            })
        })
        .await
    }
    /// Whether one provider holds an enrollment for the scope's account holder.
    /// The connection is released on return, so a caller may then ask the
    /// provider for destinations that read through its owner's facade.
    async fn provider_enrolled(&self, provider: &dyn Provider, scope: &Scope) -> Result<bool> {
        let client = self.db.get().await?;
        provider
            .enrolled(
                &**client,
                FactorAccount {
                    tenant: scope.tenant(),
                    user: scope.user(),
                },
            )
            .await
    }
    /// Read-only destinations and enrollment, refreshed on every call.
    pub async fn methods(&self, scope: &Scope, back: &str) -> Result<Vec<MethodChoice>> {
        let mut choices = Vec::new();
        for (method, provider) in self.providers()? {
            let enrolled = self.provider_enrolled(&*provider, scope).await?;
            let prompt = provider.prompt(scope, back).await?;
            let setup = provider.setup(scope, back).await?;
            if !local_destination(&prompt) || !local_destination(&setup) {
                return Err(Error::Invalid);
            }
            choices.push(MethodChoice {
                method,
                enrolled,
                prompt,
                setup,
            });
        }
        Ok(choices)
    }
    /// Explicit delivery/start, never called by presentation checks. The
    /// provider must bind any delivered code to this exact purpose and holder.
    pub async fn start(
        &self,
        scope: &Scope,
        method: &str,
        purpose: Purpose<'_>,
    ) -> Result<Challenge> {
        if scope.impersonator().is_some()
            || purpose.channel != scope.channel()
            || self.declared(purpose.operation).is_none()
        {
            return Err(Error::Forbidden);
        }
        let provider = self
            .providers()?
            .into_iter()
            .find(|(m, _)| m.id == method)
            .ok_or(Error::Invalid)?
            .1;
        if !self.provider_enrolled(&*provider, scope).await? {
            return Ok(Challenge {
                availability: Availability::Unavailable,
                masked_destination: None,
                retry_after_seconds: None,
            });
        }
        provider.start(scope, purpose).await
    }
    /// Bind once to the installation's live policy, never a process-local role cache.
    pub fn bind_enforcement(
        &self,
        policy: Arc<dyn Enforcement>,
    ) -> bilibi_module_registry::Result<()> {
        self.enforcement.set(policy).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate step-up enforcement policy".into(),
            )
        })
    }
    /// For a fixture binary that installs its own services and enrolls no
    /// authenticator; the host never calls it, the stored role decides there.
    pub fn set_enforced(&self, enforced: bool) {
        self.enforced.store(enforced, Ordering::SeqCst);
    }
    async fn enforced(&self) -> Result<bool> {
        if !self.enforced.load(Ordering::SeqCst) {
            return Ok(false);
        }
        match self.enforcement.get() {
            Some(policy) => policy.enforced().await,
            None => Ok(true),
        }
    }
    pub fn declared(&self, key: &str) -> Option<&Declared> {
        self.operations.get(key)
    }
    /// The bound provider's live enrollment state for this account holder.
    /// Presentation only: mutations still call `require` or `require_code`.
    pub async fn enrolled(&self, scope: &Scope) -> Result<bool> {
        Ok(!self.enrolled_registered(scope).await?.is_empty())
    }
    /// The methods the account holder is enrolled in, with no presentation
    /// reads: what a protected form needs to take a code, whatever a method's
    /// destinations do.
    pub async fn enrolled_methods(&self, scope: &Scope) -> Result<Vec<Method>> {
        Ok(self
            .enrolled_registered(scope)
            .await?
            .into_iter()
            .map(|(method, _)| method)
            .collect())
    }
    async fn enrolled_registered(&self, scope: &Scope) -> Result<Vec<Registered>> {
        let mut enrolled = Vec::new();
        for registered in self.providers()? {
            if self.provider_enrolled(&*registered.1, scope).await? {
                enrolled.push(registered);
            }
        }
        Ok(enrolled)
    }
    /// Whether policy currently protects an operation, without spending proof.
    /// Used to explain protected fields before a person changes them.
    pub async fn required(&self, scope: &Scope, key: &str) -> Result<bool> {
        let declared = self.operations.get(key).ok_or(Error::Invalid)?;
        Ok(declared.operation.always
            || (self.enforced().await? && self.mode(scope, declared).await? != Mode::Off))
    }
    /// The operation declared to guard changes to setting `key`, if any.
    pub fn guarding_setting(&self, key: &str) -> Option<&Declared> {
        self.operations
            .values()
            .find(|declared| declared.operation.setting == Some(key))
    }
    /// Every declared operation, ordered by module then label.
    pub fn catalog(&self) -> Vec<&Declared> {
        let mut all: Vec<_> = self.operations.values().collect();
        all.sort_by(|a, b| (a.module, a.operation.label).cmp(&(b.module, b.operation.label)));
        all
    }

    /// The step-up check of one operation on one target. `Ok` when the
    /// tenant's policy leaves it off, enforcement is off (development), or a
    /// proof for this channel holds; otherwise `Error::StepUpRequired`. An
    /// operation declared `always` skips the first two: it needs a fresh
    /// check on every action, on every deployment. Every
    /// check that applies is audited (`step_up.passed`, `.refused`, `.failed`)
    /// with the operation, target, channel and actor, never a code.
    pub async fn require(&self, scope: &Scope, key: &str, target: &str) -> Result<()> {
        let declared = self.declared(key).ok_or(Error::Internal)?;
        let mode = if declared.operation.always {
            // Declared `always`: a fresh code on every action, even where
            // enforcement is off (development) and whatever the tenant chose.
            Mode::EveryTime
        } else {
            if !self.enforced().await? {
                return Ok(());
            }
            let mode = self.mode(scope, declared).await?;
            if mode == Mode::Off {
                return Ok(());
            }
            mode
        };
        let outcome = self.check(scope, declared, target, mode, true).await?;
        self.audit(scope, declared, target, mode, &outcome).await?;
        match outcome {
            Outcome::Passed(_) => Ok(()),
            Outcome::Refused(reason) | Outcome::Failed(reason) => Err(Error::StepUpRequired {
                operation: declared.operation.key,
                reason,
            }),
        }
    }

    /// For an `always` operation whose confirmation form takes a code itself.
    /// A recent browser check (including enrollment) cannot replace that code.
    /// Missing codes are audited refusals; presented codes use the normal gate.
    pub async fn require_code(&self, scope: &Scope, key: &str, target: &str) -> Result<()> {
        let declared = self.declared(key).ok_or(Error::Internal)?;
        if !declared.operation.always {
            return Err(Error::Invalid);
        }
        if scope.step_up_code().is_none() {
            let reason = if scope.impersonator().is_some() {
                "impersonating"
            } else if !self.providers()?.is_empty() {
                if self.enrolled(scope).await? {
                    "code_required"
                } else {
                    "not_enrolled"
                }
            } else {
                "unavailable"
            };
            self.audit(
                scope,
                declared,
                target,
                Mode::EveryTime,
                &Outcome::Refused(reason),
            )
            .await?;
            return Err(Error::StepUpRequired {
                operation: declared.operation.key,
                reason,
            });
        }
        self.require(scope, key, target).await
    }

    /// Whether the web session's proof still covers `key`, without using it
    /// or recording anything: for a page that rechecks while open.
    pub async fn holds(&self, scope: &Scope, key: &str) -> Result<bool> {
        let declared = self.declared(key).ok_or(Error::Internal)?;
        let mode = if declared.operation.always {
            Mode::EveryTime
        } else {
            if !self.enforced().await? || self.mode(scope, declared).await? == Mode::Off {
                return Ok(true);
            }
            Mode::On
        };
        Ok(matches!(
            self.check(scope, declared, "", mode, false).await?,
            Outcome::Passed(_)
        ))
    }

    /// Where a browser that owes a step-up goes, coming back to `back`.
    pub async fn prompt(&self, scope: &Scope, back: &str) -> Result<String> {
        // Core owns the neutral choice page. With one method preserve its
        // existing destination; multiple methods never select by boot order.
        if self.providers()?.len() == 1 {
            let choice = self.methods(scope, back).await?.remove(0);
            return Ok(if choice.enrolled {
                choice.prompt
            } else {
                choice.setup
            });
        }
        self.session_provider
            .get()
            .ok_or(Error::NotFound)?
            .prompt(scope, back)
            .await
    }

    async fn check(
        &self,
        scope: &Scope,
        declared: &Declared,
        target: &str,
        mode: Mode,
        extend: bool,
    ) -> Result<Outcome> {
        if scope.impersonator().is_some() {
            return Ok(Outcome::Refused("impersonating"));
        }
        if self.providers()?.is_empty() {
            return Ok(Outcome::Refused("unavailable"));
        }
        let enrolled = self.enrolled_registered(scope).await?;
        if enrolled.is_empty() {
            return Ok(Outcome::Refused("not_enrolled"));
        }
        if let Some(code) = scope.step_up_code().filter(|_| extend) {
            // One action may need several checks (a role change and a
            // full-access role): the holder's code covers this whole request.
            if code.accepted() {
                return Ok(Outcome::Passed("request_code"));
            }
            // A qualified proof routes to exactly one method; never try a
            // wrong code against every factor (which multiplies lockouts).
            let (method, presented) = match code.expose().split_once(':') {
                Some((method, code)) => (method, code),
                None if enrolled.len() == 1 => (enrolled[0].0.id, code.expose()),
                None => return Ok(Outcome::Refused("method_required")),
            };
            let Some((_, provider)) = enrolled.iter().find(|(m, _)| m.id == method) else {
                return Ok(Outcome::Refused("not_enrolled"));
            };
            let purpose = Purpose {
                operation: declared.operation.key,
                target,
                channel: scope.channel(),
            };
            let spent = provider.spend(scope, presented, purpose).await?;
            if spent == Spent::Accepted {
                code.mark_accepted();
                if let Some(agent) = scope.agent() {
                    self.open_agent_window(scope, agent.connection()).await?;
                }
            }
            return Ok(match spent {
                Spent::Accepted => Outcome::Passed("code"),
                Spent::Rejected => Outcome::Failed("invalid_code"),
                Spent::Locked => Outcome::Failed("locked"),
                Spent::NotEnrolled => Outcome::Refused("not_enrolled"),
            });
        }
        let Some(provider) = self.session_provider.get() else {
            return Ok(Outcome::Refused("unavailable"));
        };
        let window = self.window(scope).await?;
        let (window_seconds, cap_seconds) = (window.window_minutes * 60, window.cap_hours * 3600);
        let state = match (scope.agent(), mode) {
            // An agent's "ask every time" action needs the holder's code.
            (Some(_), Mode::EveryTime) => WindowState::None,
            (Some(agent), _) => {
                self.agent_window(
                    scope,
                    agent.connection(),
                    window_seconds,
                    cap_seconds,
                    extend,
                )
                .await?
            }
            (None, Mode::EveryTime) => {
                provider
                    .session(
                        scope,
                        SessionProof::Fresh {
                            seconds: policy::FRESH_SECONDS,
                            consume: extend,
                        },
                    )
                    .await?
            }
            (None, _) => {
                provider
                    .session(
                        scope,
                        SessionProof::Window {
                            window_seconds,
                            cap_seconds,
                            extend,
                        },
                    )
                    .await?
            }
        };
        Ok(match state {
            WindowState::Holds => Outcome::Passed(if mode == Mode::EveryTime {
                "fresh_check"
            } else {
                "window"
            }),
            WindowState::Capped => Outcome::Refused("window_capped"),
            WindowState::Expired => Outcome::Refused("window_expired"),
            WindowState::None if mode == Mode::EveryTime => Outcome::Refused("every_time"),
            WindowState::None => Outcome::Refused(if scope.agent().is_some() {
                "code_required"
            } else {
                "required"
            }),
        })
    }

    /// Opens (or restarts) the window of this agent connection for its
    /// holder, after the holder's code was accepted.
    async fn open_agent_window(&self, scope: &Scope, connection: i64) -> Result<()> {
        let client = self.db.get().await?;
        client
            .execute(
                "DELETE FROM base_step_up_agent_windows WHERE tenant_id=$1 AND checked_at<now()-interval '1 day'",
                &[&scope.tenant()],
            )
            .await?;
        client
            .execute(
                "INSERT INTO base_step_up_agent_windows(tenant_id,user_id,connection_id,checked_at,used_at) VALUES($1,$2,$3,now(),now()) ON CONFLICT ON CONSTRAINT base_step_up_agent_windows_pkey DO UPDATE SET checked_at=now(),used_at=now()",
                &[&scope.tenant(), &scope.user(), &connection],
            )
            .await?;
        Ok(())
    }

    async fn agent_window(
        &self,
        scope: &Scope,
        connection: i64,
        window_seconds: i64,
        cap_seconds: i64,
        extend: bool,
    ) -> Result<WindowState> {
        let client = self.db.get().await?;
        let row = client
            .query_opt(
                "SELECT checked_at>now()-make_interval(secs => $5::double precision), used_at>now()-make_interval(secs => $4::double precision) FROM base_step_up_agent_windows WHERE tenant_id=$1 AND user_id=$2 AND connection_id=$3",
                &[&scope.tenant(), &scope.user(), &connection, &(window_seconds as f64), &(cap_seconds as f64)],
            )
            .await?;
        let state = row.map_or(WindowState::None, |row| {
            WindowState::from_flags(true, row.get(0), row.get(1))
        });
        if extend && state == WindowState::Holds {
            client
                .execute(
                    "UPDATE base_step_up_agent_windows SET used_at=now() WHERE tenant_id=$1 AND user_id=$2 AND connection_id=$3",
                    &[&scope.tenant(), &scope.user(), &connection],
                )
                .await?;
        }
        Ok(state)
    }

    async fn audit(
        &self,
        scope: &Scope,
        declared: &Declared,
        target: &str,
        mode: Mode,
        outcome: &Outcome,
    ) -> Result<()> {
        let (event, result, detail) = match outcome {
            // A code accepted opens (or restarts) the holder's window.
            Outcome::Passed("code") => (
                "step_up.window_opened",
                "succeeded",
                json!({"proof": "code"}),
            ),
            Outcome::Passed(proof) => ("step_up.passed", "succeeded", json!({"proof": proof})),
            Outcome::Refused(reason) => ("step_up.refused", "refused", json!({"reason": reason})),
            Outcome::Failed(reason) => ("step_up.failed", "failed", json!({"reason": reason})),
        };
        let mut payload = json!({
            "semantic": true,
            "source": "StepUp",
            "summary": declared.operation.label,
            "operation": declared.operation.key,
            "module": declared.module,
            "target": bilibi_audit::bounded_text(target),
            "channel": scope.channel().as_str(),
            "mode": mode.as_str(),
            "result": result,
        });
        if let (Some(fields), Some(extra)) = (payload.as_object_mut(), detail.as_object()) {
            fields.extend(extra.clone());
            if let Some(agent) = scope.agent() {
                // The agent acts for the account holder, who is the actor.
                fields.insert("agent_connection".into(), json!(agent.connection()));
                fields.insert("on_behalf_of".into(), json!(scope.user()));
            }
        }
        let client = self.db.get().await?;
        record_action(
            &**client,
            &Attribution::from_scope(scope),
            None,
            event,
            payload,
            true,
        )
        .await?;
        Ok(())
    }

    async fn mode(&self, scope: &Scope, declared: &Declared) -> Result<Mode> {
        let (choices, floors) = self.stored(scope).await?;
        Ok(effective(declared, &choices, &floors).1)
    }

    async fn stored(&self, scope: &Scope) -> Result<(Map<String, Value>, Map<String, Value>)> {
        let client = self.db.get().await?;
        let choices = self
            .settings
            .get(&**client, scope, Target::Tenant, policy::POLICY_KEY)
            .await?
            .value;
        let floors = self
            .settings
            .get_global(&**client, policy::FLOOR_KEY)
            .await?;
        let object = |value: Value| match value {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        Ok((object(choices), object(floors)))
    }

    /// Every declared operation under this tenant's policy.
    pub async fn rules(&self, scope: &Scope) -> Result<Vec<Rule<'_>>> {
        let (choices, floors) = self.stored(scope).await?;
        Ok(self
            .catalog()
            .into_iter()
            .map(|declared| {
                let (floor, effective) = effective(declared, &choices, &floors);
                Rule {
                    declared,
                    choice: choices
                        .get(declared.operation.key)
                        .and_then(Value::as_str)
                        .and_then(Mode::parse),
                    floor,
                    effective,
                }
            })
            .collect())
    }

    /// The tenant's web window after the installation limits.
    pub async fn window(&self, scope: &Scope) -> Result<Window> {
        let client = self.db.get().await?;
        let int = |value: Value| value.as_i64().ok_or(Error::Internal);
        let tenant_window = int(self
            .settings
            .get(&**client, scope, Target::Tenant, policy::WINDOW_KEY)
            .await?
            .value)?;
        let tenant_cap = int(self
            .settings
            .get(&**client, scope, Target::Tenant, policy::CAP_KEY)
            .await?
            .value)?;
        let window_limit = int(self
            .settings
            .get_global(&**client, policy::WINDOW_LIMIT_KEY)
            .await?)?;
        let cap_limit = int(self
            .settings
            .get_global(&**client, policy::CAP_LIMIT_KEY)
            .await?)?;
        let cap_hours = tenant_cap.min(cap_limit);
        Ok(Window {
            window_minutes: tenant_window.min(window_limit).min(cap_hours * 60),
            cap_hours,
            window_limit_minutes: window_limit,
            cap_limit_hours: cap_limit,
            tenant_window_minutes: tenant_window,
            tenant_cap_hours: tenant_cap,
        })
    }

    /// Stores the tenant's choice for one operation (`None` follows the
    /// declared default) inside the caller's audited transaction. A floor
    /// operation cannot be turned off. The caller authorizes and steps up.
    pub async fn set_mode(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        key: &str,
        mode: Option<Mode>,
    ) -> Result<()> {
        let declared = self.declared(key).ok_or(Error::NotFound)?;
        let (mut choices, floors) = self.stored(scope).await?;
        let (floor, _) = effective(declared, &choices, &floors);
        if declared.operation.always || (floor && mode == Some(Mode::Off)) {
            return Err(Error::Conflict);
        }
        match mode {
            Some(mode) => choices.insert(key.to_owned(), json!(mode.as_str())),
            None => choices.remove(key),
        };
        self.settings
            .put(
                tx,
                scope,
                Target::Tenant,
                policy::POLICY_KEY,
                Value::Object(choices),
            )
            .await
    }

    /// The installation floor for one operation (`None` follows the
    /// declaration). Global: Settings reproves the platform operator; the
    /// caller proves the installation owner and steps up.
    pub async fn set_floor(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        key: &str,
        floor: Option<bool>,
    ) -> Result<()> {
        if self.declared(key).ok_or(Error::NotFound)?.operation.always {
            return Err(Error::Conflict);
        }
        let (_, mut floors) = self.stored(scope).await?;
        match floor {
            Some(floor) => floors.insert(key.to_owned(), json!(floor)),
            None => floors.remove(key),
        };
        self.settings
            .put(
                tx,
                scope,
                Target::Global,
                policy::FLOOR_KEY,
                Value::Object(floors),
            )
            .await
    }

    /// The tenant's window and cap: within the bounds, the cap at least the
    /// window, both within the installation limits (`Error::Invalid`).
    pub async fn set_window(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        window_minutes: i64,
        cap_hours: i64,
    ) -> Result<()> {
        let limits = self.window(scope).await?;
        if !in_range(window_minutes, policy::WINDOW_RANGE)
            || !in_range(cap_hours, policy::CAP_RANGE)
            || cap_hours * 60 < window_minutes
            || window_minutes > limits.window_limit_minutes
            || cap_hours > limits.cap_limit_hours
        {
            return Err(Error::Invalid);
        }
        self.settings
            .put(
                tx,
                scope,
                Target::Tenant,
                policy::WINDOW_KEY,
                json!(window_minutes),
            )
            .await?;
        self.settings
            .put(tx, scope, Target::Tenant, policy::CAP_KEY, json!(cap_hours))
            .await
    }

    /// The installation-wide maxima (the installation owner's stricter limit).
    pub async fn set_limits(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        window_limit_minutes: i64,
        cap_limit_hours: i64,
    ) -> Result<()> {
        if !in_range(window_limit_minutes, policy::WINDOW_RANGE)
            || !in_range(cap_limit_hours, policy::CAP_RANGE)
            || cap_limit_hours * 60 < window_limit_minutes
        {
            return Err(Error::Invalid);
        }
        self.settings
            .put(
                tx,
                scope,
                Target::Global,
                policy::WINDOW_LIMIT_KEY,
                json!(window_limit_minutes),
            )
            .await?;
        self.settings
            .put(
                tx,
                scope,
                Target::Global,
                policy::CAP_LIMIT_KEY,
                json!(cap_limit_hours),
            )
            .await
    }
}

/// Ends every agent window of an account holder, inside the caller's write
/// (Base Session's sign-out of all the holder's sessions).
pub async fn end_agent_windows(
    sql: &impl tokio_postgres::GenericClient,
    tenant: i64,
    user: i64,
) -> Result<()> {
    sql.execute(
        "DELETE FROM base_step_up_agent_windows WHERE tenant_id=$1 AND user_id=$2",
        &[&tenant, &user],
    )
    .await?;
    Ok(())
}

fn in_range(value: i64, (low, high): (i64, i64)) -> bool {
    (low..=high).contains(&value)
}

/// (floor, effective mode) of one operation under stored choices and floors.
fn effective(
    declared: &Declared,
    choices: &Map<String, Value>,
    floors: &Map<String, Value>,
) -> (bool, Mode) {
    let key = declared.operation.key;
    if declared.operation.always {
        return (true, Mode::EveryTime);
    }
    let floor = floors
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(declared.operation.floor);
    let default = if declared.operation.default_on {
        Mode::On
    } else {
        Mode::Off
    };
    let chosen = choices
        .get(key)
        .and_then(Value::as_str)
        .and_then(Mode::parse)
        .unwrap_or(default);
    (
        floor,
        if floor && chosen == Mode::Off {
            Mode::On
        } else {
            chosen
        },
    )
}

// Owners return destinations, never a redirect supplied verbatim by a request.
fn local_destination(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && path.len() <= 4096
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
}
