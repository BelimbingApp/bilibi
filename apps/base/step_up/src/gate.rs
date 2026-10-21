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
use bilibi_database::{Db, Error, MutationTransaction, Result};
use bilibi_module_registry::Catalog;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Channel, ProofFuture, Scope};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
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

/// What a provider did when an agent's holder owes a code and sent none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Challenge {
    /// Nothing to deliver: the holder reads codes from an app.
    None,
    /// A code is on its way to the holder, or one sent moments ago still holds.
    Sent,
    /// No code can be sent now: too many were sent recently.
    Throttled,
    /// The holder's checks are locked after repeated failures.
    Locked,
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
    Fresh { seconds: i64 },
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

/// The second factor behind the contract (Core User's authenticator).
/// Every answer is about the scope's own account holder: never an operator
/// behind an impersonated session, never an agent.
pub trait Provider: Send + Sync {
    fn enrolled<'a>(&'a self, scope: &'a Scope) -> ProofFuture<'a, bool>;
    fn session<'a>(&'a self, scope: &'a Scope, proof: SessionProof)
        -> ProofFuture<'a, WindowState>;
    /// Verifies and uses a code in one step: a code is spent once, on this
    /// purpose, whatever happens to the operation afterwards.
    fn spend<'a>(
        &'a self,
        scope: &'a Scope,
        code: &'a str,
        purpose: Purpose<'a>,
    ) -> ProofFuture<'a, Spent>;
    /// Where a browser checks in (or enrolls first) and returns to `back`.
    fn prompt<'a>(&'a self, scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String>;
    /// Delivers a code to the holder when the factor needs one delivered
    /// (a text message), for an agent that must ask its human for it.
    fn challenge<'a>(
        &'a self,
        scope: &'a Scope,
        purpose: Purpose<'a>,
    ) -> ProofFuture<'a, Challenge>;
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

pub struct StepUp {
    db: Db,
    settings: Arc<Settings>,
    operations: BTreeMap<&'static str, Declared>,
    provider: OnceLock<Arc<dyn Provider>>,
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
            provider: OnceLock::new(),
            enforced: AtomicBool::new(true),
        })
    }
    /// The second-factor owner binds itself once at boot.
    pub fn bind(&self, provider: Arc<dyn Provider>) -> bilibi_module_registry::Result<()> {
        self.provider.set(provider).map_err(|_| {
            bilibi_module_registry::RegistryError::Service("duplicate step-up provider".into())
        })
    }
    /// For a fixture binary that installs its own services and enrolls no
    /// authenticator; the host never calls it, so every server role enforces.
    pub fn set_enforced(&self, enforced: bool) {
        self.enforced.store(enforced, Ordering::SeqCst);
    }
    fn enforced(&self) -> bool {
        self.enforced.load(Ordering::SeqCst)
    }
    pub fn declared(&self, key: &str) -> Option<&Declared> {
        self.operations.get(key)
    }
    /// The bound provider's live enrollment state for this account holder.
    /// Presentation only: mutations still call `require` or `require_code`.
    pub async fn enrolled(&self, scope: &Scope) -> Result<bool> {
        match self.provider.get() {
            Some(provider) => provider.enrolled(scope).await,
            None => Ok(false),
        }
    }
    /// Whether policy currently protects an operation, without spending proof.
    /// Used to explain protected fields before a person changes them.
    pub async fn required(&self, scope: &Scope, key: &str) -> Result<bool> {
        let declared = self.operations.get(key).ok_or(Error::Invalid)?;
        Ok(declared.operation.always
            || (self.enforced() && self.mode(scope, declared).await? != Mode::Off))
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
    /// tenant's policy leaves it off or a proof for this channel holds;
    /// otherwise `Error::StepUpRequired`. Every server role enforces it,
    /// Dev included; only a fixture binary turns it off (`set_enforced`).
    /// An operation declared `always` ignores both: it needs a fresh check
    /// on every action. Every
    /// check that applies is audited (`step_up.passed`, `.refused`, `.failed`)
    /// with the operation, target, channel and actor, never a code.
    pub async fn require(&self, scope: &Scope, key: &str, target: &str) -> Result<()> {
        let declared = self.declared(key).ok_or(Error::Internal)?;
        let mode = if declared.operation.always {
            // Declared `always`: a fresh code on every action, whatever the
            // tenant chose.
            Mode::EveryTime
        } else {
            if !self.enforced() {
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
    /// An agent's holder whose codes come by text message is sent one, as by
    /// [`Self::require`].
    pub async fn require_code(&self, scope: &Scope, key: &str, target: &str) -> Result<()> {
        let declared = self.declared(key).ok_or(Error::Internal)?;
        if !declared.operation.always {
            return Err(Error::Invalid);
        }
        if scope.step_up_code().is_none() {
            let reason = if scope.impersonator().is_some() {
                "impersonating"
            } else if let Some(provider) = self.provider.get() {
                if !provider.enrolled(scope).await? {
                    "not_enrolled"
                } else if scope.agent().is_some() {
                    self.challenge(scope, provider.as_ref(), declared, target)
                        .await?
                        .unwrap_or("code_required")
                } else {
                    "code_required"
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
            if !self.enforced() || self.mode(scope, declared).await? == Mode::Off {
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
        let provider = self.provider.get().ok_or(Error::NotFound)?;
        provider.prompt(scope, back).await
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
        let Some(provider) = self.provider.get() else {
            return Ok(Outcome::Refused("unavailable"));
        };
        if let Some(code) = scope.step_up_code() {
            // One action may need several checks (a role change and a
            // full-access role): the holder's code covers this whole request.
            if code.accepted() {
                return Ok(Outcome::Passed("request_code"));
            }
            let purpose = Purpose {
                operation: declared.operation.key,
                target,
                channel: scope.channel(),
            };
            let spent = provider.spend(scope, code.expose(), purpose).await?;
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
        let outcome = match state {
            WindowState::Holds => Outcome::Passed(if mode == Mode::EveryTime {
                "fresh_check"
            } else {
                "window"
            }),
            _ if !provider.enrolled(scope).await? => Outcome::Refused("not_enrolled"),
            WindowState::Capped => Outcome::Refused("window_capped"),
            WindowState::Expired => Outcome::Refused("window_expired"),
            WindowState::None if mode == Mode::EveryTime => Outcome::Refused("every_time"),
            WindowState::None => Outcome::Refused(if scope.agent().is_some() {
                "code_required"
            } else {
                "required"
            }),
        };
        // An agent that owes a code cannot read the holder's phone: when the
        // factor delivers codes (a text message), one goes out now and the
        // answer says so. A browser's prompt page sends its own; `holds`
        // (no `extend`) never sends.
        if let (Outcome::Refused(reason), Some(_), true) = (&outcome, scope.agent(), extend) {
            if *reason != "not_enrolled" {
                if let Some(reason) = self
                    .challenge(scope, provider.as_ref(), declared, target)
                    .await?
                {
                    return Ok(Outcome::Refused(reason));
                }
            }
        }
        Ok(outcome)
    }

    /// Asks the provider to deliver the holder a code (a text message) for an
    /// agent that owes one: the refusal reason that says what happened, or
    /// `None` when the holder reads codes from an app.
    async fn challenge(
        &self,
        scope: &Scope,
        provider: &dyn Provider,
        declared: &Declared,
        target: &str,
    ) -> Result<Option<&'static str>> {
        let purpose = Purpose {
            operation: declared.operation.key,
            target,
            channel: scope.channel(),
        };
        Ok(match provider.challenge(scope, purpose).await? {
            Challenge::None => None,
            Challenge::Sent => Some("code_sent"),
            Challenge::Throttled => Some("throttled"),
            Challenge::Locked => Some("locked"),
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
