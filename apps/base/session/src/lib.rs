//! Durable sessions (Bilimbi `Bilimbi.Base.Session`): validation where
//! background checks never renew activity, the operator list and its
//! termination commands, and the termination broadcast the host turns into a
//! prompt sign-out of every other live page of that session.
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_settings::Settings;
use bilibi_tenancy::{AccountKind, Authentication, Scope};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_postgres::GenericClient;

mod contributions;
mod expiry;
mod web;
pub use contributions::{LIFETIME_KEY, LIST, MANAGE, TOUCH_KEY};

/// The operator list's page sizes, Bilimbi's.
pub const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];
pub const DEFAULT_PAGE_SIZE: u64 = 25;
const MAX_SEARCH: usize = 255;
const MAX_IP: usize = 45;
const MAX_USER_AGENT: usize = 512;

/// Idle lifetime and the activity-renewal throttle, both from the operator
/// settings `session.lifetime_minutes` and `session.last_activity_touch_minutes`.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    idle_seconds: i64,
    touch_seconds: i64,
}
impl Policy {
    pub fn new(idle_seconds: i64, touch_seconds: i64) -> Result<Self> {
        if !(60..=31_536_000).contains(&idle_seconds)
            || touch_seconds < 1
            || touch_seconds > idle_seconds / 2
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            idle_seconds,
            touch_seconds,
        })
    }
    /// Bilimbi's reading of the two settings: the throttle never exceeds half
    /// the lifetime, so continuous use renews a session before it can expire.
    pub fn from_minutes(lifetime: i64, touch: i64) -> Result<Self> {
        let idle = lifetime.saturating_mul(60);
        Self::new(idle, touch.saturating_mul(60).min(idle / 2).max(1))
    }
    pub fn idle_seconds(&self) -> i64 {
        self.idle_seconds
    }
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            idle_seconds: 7200,
            touch_seconds: 300,
        }
    }
}

/// A session ended on purpose: logout, an operator's termination or a user's
/// other sessions ending after a credential change. Carries the durable id so
/// the host can match the sockets it holds for it; never a display value.
#[derive(Clone, Debug)]
pub struct Terminated {
    pub id: String,
}

/// Payload-free metadata of one stored session for the operator list.
#[derive(Clone, Debug)]
pub struct Summary {
    pub id: String,
    pub tenant: i64,
    pub user_id: i64,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    /// Seconds since the last recorded activity.
    pub idle_seconds: i64,
}
impl Summary {
    /// A stable row key that is not the bearer token: the page keys rows and
    /// termination requests by it, so the cookie value never reaches the DOM.
    pub fn key(&self) -> String {
        session_key(&self.id)
    }
}
fn session_key(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    digest.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortBy {
    UserId,
    IpAddress,
    UserAgent,
    #[default]
    LastActivity,
}
#[derive(Clone, Debug)]
pub struct ListQuery {
    pub search: Option<String>,
    pub page: u64,
    pub page_size: u64,
    pub sort_by: SortBy,
    pub descending: bool,
}
impl Default for ListQuery {
    fn default() -> Self {
        Self {
            search: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
            sort_by: SortBy::LastActivity,
            descending: true,
        }
    }
}
pub struct ListPage {
    pub entries: Vec<Summary>,
    pub total_entries: u64,
}
/// A session's step-up window as [`Sessions::step_up_window`] read it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StepUpWindow {
    /// The session passed a check at some point.
    pub checked: bool,
    pub within_cap: bool,
    pub within_window: bool,
}
impl StepUpWindow {
    pub fn holds(self) -> bool {
        self.checked && self.within_cap && self.within_window
    }
}
/// Who a session acts as: the account, and the operator behind it while
/// impersonating. A switch names the identity it expects to replace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Holder {
    pub user: i64,
    pub impersonator: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Termination {
    Terminated,
    NotFound,
    /// The caller's own session is refused before any row is read.
    CurrentSession,
}

pub struct Sessions {
    authentication: Arc<Authentication>,
    settings: Arc<Settings>,
    terminations: broadcast::Sender<Terminated>,
    factors: Option<Arc<bilibi_step_up::StepUp>>,
}
impl Sessions {
    pub fn new(authentication: Arc<Authentication>, settings: Arc<Settings>) -> Self {
        let (terminations, _) = broadcast::channel(256);
        Self {
            authentication,
            settings,
            terminations,
            factors: None,
        }
    }
    /// Wire the factor registry for account-wide revocation. A standalone
    /// fixture that omits this cannot revoke account sessions.
    pub fn with_factors(mut self, factors: Arc<bilibi_step_up::StepUp>) -> Self {
        self.factors = Some(factors);
        self
    }
    /// Revoke pending challenges of every factor and all holder sessions in
    /// the owner's audited transaction. Tenant comes from its trusted context.
    pub async fn revoke(
        &self,
        tx: &mut MutationTransaction<'_>,
        user: i64,
        except: Option<&str>,
    ) -> Result<Vec<String>> {
        self.factors
            .as_ref()
            .ok_or(Error::Internal)?
            .lifecycle(tx, user, bilibi_step_up::Lifecycle::Revoke)
            .await?;
        Self::end_user_sessions(tx.sql(), tx.context().tenant, user, except).await
    }
    /// The current operator policy. Read on every check, so a changed lifetime
    /// applies to the next request without a restart.
    pub async fn policy(&self, client: &(impl GenericClient + Sync)) -> Result<Policy> {
        let lifetime = self.settings.get_global(client, LIFETIME_KEY).await?;
        let touch = self.settings.get_global(client, TOUCH_KEY).await?;
        Policy::from_minutes(
            lifetime.as_i64().ok_or(Error::Internal)?,
            touch.as_i64().ok_or(Error::Internal)?,
        )
    }
    /// Opaque token lookup is the bootstrap exception to tenant scoping. One
    /// lookup checks session expiry, then Tenancy and the owner re-prove identity.
    /// A staff session. A customer's session (ADR 16) never resolves here,
    /// whichever cookie carried it: the host reads it only through
    /// [`Self::check_portal`] under `/portal`.
    pub async fn check(&self, client: &(impl GenericClient + Sync), token: &str) -> Result<Scope> {
        self.resolve(client, token, false, AccountKind::Staff).await
    }
    /// A customer's portal session; a staff session never resolves here.
    pub async fn check_portal(
        &self,
        client: &(impl GenericClient + Sync),
        token: &str,
    ) -> Result<Scope> {
        self.resolve(client, token, false, AccountKind::Customer)
            .await
    }
    /// `check`, then holds the row until `client`'s transaction ends, so a
    /// sign-out or termination waits for a write the session is making (Agent
    /// API's page caller). A key-share lock: the session's own step-up and
    /// activity updates still go ahead.
    pub async fn hold(&self, client: &(impl GenericClient + Sync), token: &str) -> Result<Scope> {
        let scope = self
            .resolve(client, token, false, AccountKind::Staff)
            .await?;
        client
            .query_opt(
                "SELECT id FROM sessions WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND expires_at>clock_timestamp() FOR KEY SHARE",
                &[&token, &scope.tenant(), &scope.user()],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
        Ok(scope)
    }
    /// Call only for authenticated HTTP navigation or accepted user activity.
    /// The conditional update cannot revive an idle or absolutely expired session.
    pub async fn activity(
        &self,
        client: &(impl GenericClient + Sync),
        token: &str,
    ) -> Result<Scope> {
        self.resolve(client, token, true, AccountKind::Staff).await
    }
    /// [`Self::activity`] for a customer's portal session.
    pub async fn activity_portal(
        &self,
        client: &(impl GenericClient + Sync),
        token: &str,
    ) -> Result<Scope> {
        self.resolve(client, token, true, AccountKind::Customer)
            .await
    }
    /// Records the signing-in client once, bounded; neither value is ever
    /// interpreted, only listed.
    pub async fn describe(
        &self,
        client: &impl GenericClient,
        token: &str,
        ip_address: Option<&str>,
        user_agent: Option<&str>,
    ) -> Result<()> {
        let ip = ip_address
            .map(str::trim)
            .filter(|ip| !ip.is_empty() && ip.len() <= MAX_IP)
            .map(str::to_owned);
        let agent = user_agent
            .map(|agent| {
                agent
                    .trim()
                    .chars()
                    .take(MAX_USER_AGENT)
                    .collect::<String>()
            })
            .filter(|agent| !agent.is_empty());
        client
            .execute(
                "UPDATE sessions SET ip_address=$2,user_agent=$3 WHERE id=$1",
                &[&token, &ip, &agent],
            )
            .await?;
        Ok(())
    }
    /// The signed-in actor's own session, from the proof Tenancy sealed; a
    /// page uses it to protect the current row without holding the token.
    pub async fn identify(&self, client: &impl GenericClient, scope: &Scope) -> Result<String> {
        let row = client
            .query_opt(
                "SELECT id FROM sessions WHERE tenant_id=$1 AND user_id=$2 AND csrf=$3",
                &[&scope.tenant(), &scope.user(), &scope.csrf],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
        Ok(row.get(0))
    }
    /// How many durable sessions exist right now, platform-wide: the store is
    /// not tenant-owned, and the dashboard asks the operational question.
    pub async fn count(&self, client: &impl GenericClient) -> Result<i64> {
        Ok(client
            .query_one("SELECT count(*) FROM sessions", &[])
            .await?
            .get(0))
    }
    /// One bounded page of payload-free metadata, platform-global like
    /// `count`. Search matches the address or the user agent; every other
    /// value is validated before SQL and no identifier comes from the caller.
    pub async fn list(&self, client: &impl GenericClient, query: &ListQuery) -> Result<ListPage> {
        if query.page == 0 || !PAGE_SIZES.contains(&query.page_size) {
            return Err(Error::Invalid);
        }
        let search = match query.search.as_deref().map(str::trim) {
            Some(text) if text.chars().count() > MAX_SEARCH => return Err(Error::Invalid),
            Some(text) if !text.is_empty() => Some(format!("%{}%", like_escape(text))),
            _ => None,
        };
        let total: i64 = client
            .query_one(
                "SELECT count(*) FROM sessions WHERE $1::text IS NULL OR ip_address ILIKE $1 OR user_agent ILIKE $1",
                &[&search],
            )
            .await?
            .get(0);
        let order = match (query.sort_by, query.descending) {
            (SortBy::LastActivity, true) => "last_activity DESC, id ASC",
            (SortBy::LastActivity, false) => "last_activity ASC, id ASC",
            (SortBy::UserId, true) => "user_id DESC, id ASC",
            (SortBy::UserId, false) => "user_id ASC, id ASC",
            (SortBy::IpAddress, true) => "ip_address DESC NULLS LAST, id ASC",
            (SortBy::IpAddress, false) => "ip_address ASC NULLS LAST, id ASC",
            (SortBy::UserAgent, true) => "user_agent DESC NULLS LAST, id ASC",
            (SortBy::UserAgent, false) => "user_agent ASC NULLS LAST, id ASC",
        };
        let sql = format!(
            "SELECT id,tenant_id,user_id,ip_address,user_agent,floor(extract(epoch FROM now()-last_activity))::bigint FROM sessions WHERE $1::text IS NULL OR ip_address ILIKE $1 OR user_agent ILIKE $1 ORDER BY {order} LIMIT $2 OFFSET $3"
        );
        let limit = query.page_size as i64;
        let offset = query.page.saturating_sub(1).saturating_mul(query.page_size) as i64;
        let rows = client.query(&sql, &[&search, &limit, &offset]).await?;
        Ok(ListPage {
            entries: rows
                .iter()
                .map(|row| Summary {
                    id: row.get(0),
                    tenant: row.get(1),
                    user_id: row.get(2),
                    ip_address: row.get(3),
                    user_agent: row.get(4),
                    idle_seconds: row.get::<_, i64>(5).max(0),
                })
                .collect(),
            total_entries: total.max(0) as u64,
        })
    }
    /// An operator ends one listed session. The caller already authorized
    /// `admin.system.session.manage`; the audit row records whose session ended
    /// and from where, never the token. Announced after the commit, so a rolled
    /// back termination signs nobody out.
    pub async fn terminate(
        &self,
        tx: &mut MutationTransaction<'_>,
        id: &str,
        current: &str,
    ) -> Result<Termination> {
        if id.is_empty() || current.is_empty() {
            return Err(Error::Invalid);
        }
        if id == current {
            return Ok(Termination::CurrentSession);
        }
        let id = id.to_owned();
        let ended = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_opt("SELECT tenant_id,user_id,ip_address,user_agent,last_activity::text FROM sessions WHERE id=$1 FOR UPDATE", &[&id]).await?;
                    let Some(row) = row else {
                        return Ok((None, Vec::new()));
                    };
                    sql.execute("DELETE FROM sessions WHERE id=$1", &[&id])
                        .await?;
                    let user: i64 = row.get(1);
                    let change = Change {
                        kind: "session".into(),
                        id: user,
                        old: json!({
                            "tenant_id": row.get::<_, i64>(0), "user_id": user,
                            "ip_address": row.get::<_, Option<String>>(2),
                            "user_agent": row.get::<_, Option<String>>(3),
                            "last_activity": row.get::<_, String>(4), "payload": "[redacted]",
                        }),
                        new: json!({}),
                    };
                    Ok((Some(id), vec![change]))
                })
            })
            .await?;
        match ended {
            Some(id) => {
                let sessions = self.terminations.clone();
                tx.after_commit(move || {
                    let _ = sessions.send(Terminated { id });
                });
                Ok(Termination::Terminated)
            }
            None => Ok(Termination::NotFound),
        }
    }
    /// Ends a user's sessions inside the caller's own write, except the one
    /// named: a credential change keeps the session that made it. A session
    /// the user holds as an impersonating operator ends too. Returns the
    /// ended ids for [`Sessions::announce`] after the caller's commit. The
    /// caller's audit row (the credential change) covers the sign-outs.
    async fn end_user_sessions(
        sql: &impl GenericClient,
        tenant: i64,
        user: i64,
        except: Option<&str>,
    ) -> Result<Vec<String>> {
        if tenant <= 0 || user <= 0 || except.is_some_and(str::is_empty) {
            return Err(Error::Invalid);
        }
        // The holder's agents lose their step-up windows with the sessions.
        bilibi_step_up::end_agent_windows(sql, tenant, user).await?;
        let rows = sql
            .query(
                "DELETE FROM sessions WHERE tenant_id=$1 AND (user_id=$2 OR impersonator_id=$2) AND ($3::text IS NULL OR id<>$3) RETURNING id",
                &[&tenant, &user, &except],
            )
            .await?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }
    /// The signed-in holder ends their own session (logout): nobody decided
    /// about someone else, so no audit row; every other live page of that
    /// session is told at once.
    pub async fn delete(&self, client: &impl GenericClient, scope: &Scope, id: &str) -> Result<()> {
        let count = client
            .execute(
                "DELETE FROM sessions WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
                &[&scope.tenant(), &scope.user(), &id],
            )
            .await?;
        if count > 0 {
            self.announce(vec![id.to_owned()]);
        }
        Ok(())
    }
    /// Moves a live session to another identity inside the caller's
    /// transaction: the impersonation switch, both ways. The row must still be
    /// valid and still hold `from`, so a stale or replayed switch (the
    /// session already moved, ended or expired) is `Conflict` and changes
    /// nothing. The CSRF secret is rotated, so every form, method link and
    /// live socket issued to the old identity is refused from now on.
    /// The company choice does not travel: entering an impersonation starts
    /// from the viewed account's own default and parks the operator's choice
    /// beside the row, and leaving restores it, so what the operator sees is
    /// what the account sees and the operator's choice survives the view.
    /// Returns the new CSRF secret. Nothing is announced: the host's live
    /// sockets bind the secret they were opened with and reload once it moves.
    pub async fn switch(
        &self,
        sql: &(impl GenericClient + Sync),
        token: &str,
        tenant: i64,
        from: Holder,
        to: Holder,
    ) -> Result<String> {
        if token.is_empty()
            || tenant <= 0
            || [from, to].iter().any(|holder| {
                holder.user <= 0
                    || holder
                        .impersonator
                        .is_some_and(|id| id <= 0 || id == holder.user)
            })
        {
            return Err(Error::Invalid);
        }
        let policy = self.policy(sql).await?;
        let csrf = uuid::Uuid::new_v4().simple().to_string();
        let moved = sql
            .query_opt(
                "UPDATE sessions SET held_company_id=CASE WHEN $6::bigint IS NULL THEN NULL WHEN $4::bigint IS NULL THEN company_id ELSE held_company_id END,company_id=CASE WHEN $6::bigint IS NULL THEN held_company_id END,user_id=$5,impersonator_id=$6,csrf=$7,last_activity=now(),stepped_up_at=NULL,step_up_used_at=NULL,step_up_spent_at=NULL WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND impersonator_id IS NOT DISTINCT FROM $4 AND expires_at>now() AND last_activity>now()-make_interval(secs => $8::double precision) RETURNING id",
                &[&token, &tenant, &from.user, &from.impersonator, &to.user, &to.impersonator, &csrf, &(policy.idle_seconds as f64)],
            )
            .await?;
        moved.map(|_| csrf).ok_or(Error::Conflict)
    }
    /// Records the company the signed-in session works in from now on
    /// (`Scope::current_company`), or `None` for the account's own company. The
    /// caller (Core User) proved the reach and audits the switch on the same
    /// transaction; the session is named by the proof Tenancy sealed, never a
    /// token. A session that no longer exists or already moved is `Conflict`.
    pub async fn choose_company(
        &self,
        sql: &impl GenericClient,
        scope: &Scope,
        company: Option<i64>,
    ) -> Result<()> {
        if company.is_some_and(|id| id <= 0) {
            return Err(Error::Invalid);
        }
        let updated = sql
            .execute(
                "UPDATE sessions SET company_id=$4 WHERE tenant_id=$1 AND user_id=$2 AND csrf=$3 AND expires_at>now()",
                &[&scope.tenant(), &scope.user(), &scope.csrf, &company],
            )
            .await?;
        if updated == 0 {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    /// Records that the signed-in session just passed a step-up check,
    /// inside the caller's transaction (the verification that proved it).
    /// The session is named by the proof Tenancy sealed, never a token; an
    /// impersonated session is refused, since the check proves the person
    /// behind the account and an operator is not the account's holder.
    pub async fn step_up(&self, sql: &impl GenericClient, scope: &Scope) -> Result<()> {
        if scope.impersonator().is_some() {
            return Err(Error::Forbidden);
        }
        let updated = sql
            .execute(
                "UPDATE sessions SET stepped_up_at=now(),step_up_used_at=now(),step_up_spent_at=NULL WHERE tenant_id=$1 AND user_id=$2 AND csrf=$3 AND impersonator_id IS NULL AND expires_at>now()",
                &[&scope.tenant(), &scope.user(), &scope.csrf],
            )
            .await?;
        if updated != 1 {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }
    /// The signed-in session's step-up window: whether a check exists, is
    /// within `cap_seconds` (absolute, from the check) and its last covered
    /// action within `window_seconds` (sliding). With `extend`, a holding
    /// window restarts from now, so continued use keeps it open to the cap.
    pub async fn step_up_window(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        window_seconds: i64,
        cap_seconds: i64,
        extend: bool,
    ) -> Result<StepUpWindow> {
        if window_seconds <= 0 || cap_seconds <= 0 {
            return Err(Error::Invalid);
        }
        let row = client
            .query_opt(
                "SELECT stepped_up_at IS NOT NULL, stepped_up_at>now()-make_interval(secs => $5::double precision), step_up_used_at>now()-make_interval(secs => $4::double precision) FROM sessions WHERE tenant_id=$1 AND user_id=$2 AND csrf=$3 AND impersonator_id IS NULL",
                &[&scope.tenant(), &scope.user(), &scope.csrf, &(window_seconds as f64), &(cap_seconds as f64)],
            )
            .await?;
        let window = row.map_or(StepUpWindow::default(), |row| StepUpWindow {
            checked: row.get(0),
            within_cap: row.get::<_, Option<bool>>(1).unwrap_or(false),
            within_window: row.get::<_, Option<bool>>(2).unwrap_or(false),
        });
        if extend && window.holds() {
            client
                .execute(
                    "UPDATE sessions SET step_up_used_at=now() WHERE tenant_id=$1 AND user_id=$2 AND csrf=$3 AND impersonator_id IS NULL",
                    &[&scope.tenant(), &scope.user(), &scope.csrf],
                )
                .await?;
        }
        Ok(window)
    }
    /// For an "ask every time" action: spends a check made within the last
    /// `fresh_seconds` that no such action used yet. One check, one action.
    pub async fn spend_fresh_step_up(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        fresh_seconds: i64,
    ) -> Result<bool> {
        if fresh_seconds <= 0 {
            return Err(Error::Invalid);
        }
        Ok(client
            .query_opt(
                "UPDATE sessions SET step_up_spent_at=now(),step_up_used_at=now() WHERE tenant_id=$1 AND user_id=$2 AND csrf=$3 AND impersonator_id IS NULL AND stepped_up_at>now()-make_interval(secs => $4::double precision) AND (step_up_spent_at IS NULL OR step_up_spent_at<stepped_up_at) RETURNING 1",
                &[&scope.tenant(), &scope.user(), &scope.csrf, &(fresh_seconds as f64)],
            )
            .await?
            .is_some())
    }
    /// Publishes terminations the caller already committed.
    pub fn announce(&self, ids: Vec<String>) {
        for id in ids {
            let _ = self.terminations.send(Terminated { id });
        }
    }
    /// The host subscribes once per live socket and closes the socket whose
    /// session is announced. A lagged receiver missed announcements; the next
    /// guarded action or refresh still refuses an ended session.
    pub fn subscribe_terminations(&self) -> broadcast::Receiver<Terminated> {
        self.terminations.subscribe()
    }
    /// Deletes sessions that can no longer authenticate: past the absolute
    /// expiry or idle longer than the policy allows. Expiry never depends on
    /// this sweep (`check` refuses them already); it only reclaims rows.
    /// Housekeeping that nobody decided, so no Audit row per session and no
    /// announcement: nothing live can still hold an expired session.
    pub async fn prune_expired(&self, client: &(impl GenericClient + Sync)) -> Result<u64> {
        let policy = self.policy(client).await?;
        Ok(client
            .execute(
                "DELETE FROM sessions WHERE expires_at<=now() OR last_activity<=now()-make_interval(secs => $1::double precision)",
                &[&(policy.idle_seconds as f64)],
            )
            .await?)
    }
    async fn resolve(
        &self,
        client: &(impl GenericClient + Sync),
        token: &str,
        activity: bool,
        kind: AccountKind,
    ) -> Result<Scope> {
        if token.is_empty() || token.len() > 256 {
            return Err(Error::Unauthorized);
        }
        let policy = self.policy(client).await?;
        let row = client.query_opt("SELECT tenant_id,user_id,csrf,impersonator_id,company_id FROM sessions WHERE id=$1 AND expires_at>now() AND last_activity>now()-make_interval(secs => $2::double precision)", &[&token, &(policy.idle_seconds as f64)]).await?.ok_or(Error::Unauthorized)?;
        let chosen: Option<i64> = row.get(4);
        let scope = self
            .authentication
            .authenticate_session(
                client,
                row.get(0),
                row.get(1),
                row.get(3),
                chosen,
                row.get(2),
            )
            .await?;
        // The host's kind check (ADR 16): a token is honoured only for the
        // audience its cookie serves.
        if scope.actor().kind() != kind {
            return Err(Error::Unauthorized);
        }
        if activity {
            // A chosen company the account no longer reaches (grant revoked,
            // company archived) is forgotten, so it cannot come back unasked
            // when the reach is granted again; the scope already fell back to
            // the affiliation for this request.
            if chosen.is_some() && scope.chosen_company().map(|id| id.get()) != chosen {
                client
                    .execute(
                        "UPDATE sessions SET company_id=NULL WHERE id=$1 AND company_id=$2",
                        &[&token, &chosen],
                    )
                    .await?;
            }
            // Lock the valid row, even when renewal is throttled. The UPDATE
            // executes only when due; a revoked/expired row cannot be revived.
            let touched = client.query_opt("WITH valid AS MATERIALIZED (SELECT id,last_activity FROM sessions WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND expires_at>now() AND last_activity>now()-make_interval(secs => $4::double precision) FOR UPDATE), touched AS (UPDATE sessions s SET last_activity=now() FROM valid v WHERE s.id=v.id AND v.last_activity<now()-make_interval(secs => $5::double precision) RETURNING s.id) SELECT id FROM valid", &[&token,&scope.tenant(),&scope.user(),&(policy.idle_seconds as f64),&(policy.touch_seconds as f64)]).await?;
            if touched.is_none() {
                return Err(Error::Unauthorized);
            }
        }
        Ok(scope)
    }
}
fn like_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/session", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/settings", "base/authz", "base/step_up", "base/queue", "base/schedule", "base/ui", "base/menu", "base/dashboard", "base/principal_directory"],
        migrations: &[
            ("base-session-001-client", include_str!("../migrations/001-client.sql")),
            ("base-session-002-impersonation", include_str!("../migrations/002-impersonation.sql")),
            ("base-session-003-step-up", include_str!("../migrations/003-step-up.sql")),
            ("base-session-004-company", include_str!("../migrations/004-company.sql")),
        ],
        services: &[
            bilibi_module_registry::Service { id: "base/session.sessions", factory: |services| Ok(Arc::new(Sessions::new(services.get("base/tenancy.authentication")?, services.get("base/settings.store")?).with_factors(services.get(bilibi_step_up::SERVICE)?))) },
            bilibi_module_registry::Service { id: "base/session.expiry", factory: |services| Ok(Arc::new(bilibi_queue::HandlerService(Arc::new(expiry::Expiry {
                schedule: services.get("base/schedule.store")?,
                sessions: services.get("base/session.sessions")?,
                db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
            })))) },
            bilibi_module_registry::Service { id: "base/session.pages", factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages::new(
                services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                services.get("base/session.sessions")?,
                services.get("base/database.mutations")?,
                services.get("base/principal_directory.directory")?,
            ))))) },
        ],
    }
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "base/session.pages", bilibi_module_registry::Provenance { module: "base/session", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "base/session.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Ending sessions writes through the Session facade; no contract yet." },
}) }
