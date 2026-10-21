//! Neutral admission around Bilimbi `UserAuth.complete_login`. Bilimbi has
//! no pending-sign-in counterpart. Pending credentials live outside sessions;
//! only this facade can turn their freshly proved identity into a session.
use crate::{CredentialError, Login, UserService};
use bilibi_database::{Change, Context, Error, MutationTransaction, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use bilibi_step_up::{Availability, Challenge, Method, Purpose, Spent};
use bilibi_tenancy::{Channel, Scope};
use serde_json::json;
use sha2::{Digest, Sha256};

pub const SIGN_IN: &str = "core.user.sign_in";
const LIFETIME: &str = "user.sign_in_minutes";
pub(crate) const MIGRATION: (&str, &str) = (
    "core-user-015-sign-in",
    include_str!("../migrations/015-sign-in.sql"),
);
bilibi_step_up::declare!(
    "core/user",
    bilibi_step_up::Operation::new(SIGN_IN, "Sign in").off_by_default()
);
inventory::submit! { Contribution::new(DEFINITIONS, LIFETIME, Provenance { module: "core/user", source: file!(), line: line!() }, &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global], default: || json!(5),
    nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(30.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Sign-in challenge lifetime",
        help: "Minutes to finish the second factor after entering a password.",
        capability: Some(crate::contributions::UPDATE), reveal_duration_ms: 5_000 }),
})) }

/// Presentation only. No Scope, session, API grant or live identity escapes.
pub struct PendingSignIn {
    pub csrf: String,
    pub methods: Vec<Method>,
}
struct Pending {
    id: i64,
    scope: Scope,
    hash: Vec<u8>,
}
fn fingerprint(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
fn credential(pending: bool) -> Login {
    Login {
        token: uuid::Uuid::new_v4().simple().to_string(),
        csrf: uuid::Uuid::new_v4().simple().to_string(),
        pending,
    }
}
fn change(id: i64, old: &str, new: &str) -> Change {
    Change {
        kind: "user_sign_in".into(),
        id,
        old: json!({"state":old}),
        new: json!({"state":new}),
    }
}
async fn transition(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    state: &'static str,
) -> Result<()> {
    let tenant = scope.tenant();
    tx.mutate(move |sql| Box::pin(async move {
        sql.execute("UPDATE user_sign_ins SET state=$3 WHERE tenant_id=$1 AND id=$2 AND state='pending'", &[&tenant, &id, &state]).await?;
        Ok(((), vec![change(id, "pending", state)]))
    })).await
}
async fn admit(tx: &mut MutationTransaction<'_>, scope: &Scope, login: &Login) -> Result<()> {
    let (tenant, user) = (scope.tenant(), scope.user());
    let (token, csrf) = (login.token.clone(), login.csrf.clone());
    tx.mutate(move |sql| Box::pin(async move {
        sql.execute("INSERT INTO sessions(id,tenant_id,user_id,csrf,expires_at) VALUES($1,$2,$3,$4,now()+interval '24 hours')", &[&token,&tenant,&user,&csrf]).await?;
        // Never put the session bearer or its CSRF value in the audit trail.
        Ok(((), vec![Change {kind: "user_session_admission".into(), id: user, old: json!({}), new: json!({"admitted":true})}]))
    })).await
}
impl UserService {
    pub(crate) async fn begin_sign_in(
        &self,
        email: &str,
        password: &str,
    ) -> std::result::Result<Login, CredentialError> {
        let (tenant, user, stored) =
            crate::identity::password_identity(&self.db, email, password).await?;
        let pending = {
            let client = self.db.get().await.map_err(Error::from)?;
            let scope = self
                .authentication
                .authenticate(&**client, tenant, user, String::new())
                .await?;
            drop(client);
            self.step_up.required(&scope, SIGN_IN).await?
        };
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::user(tenant, user, "user.sign_in.password_verified"),
            )
            .await?;
        // Serialize against password replacement between verification and admission.
        let row = tx.sql().query_opt("SELECT password_hash FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL AND status='active' FOR SHARE", &[&tenant,&user]).await?.ok_or(CredentialError::Rejected)?;
        if row.get::<_, String>(0) != stored {
            return Err(CredentialError::Rejected);
        }
        let scope = self
            .authentication
            .authenticate(tx.sql(), tenant, user, String::new())
            .await?;
        if !self.password_sso_allowed(tx.sql(), &scope).await? {
            return Err(CredentialError::Rejected);
        }
        let login = credential(pending);
        if pending {
            let minutes = self
                .settings
                .get_global(tx.sql(), LIFETIME)
                .await?
                .as_i64()
                .ok_or(Error::Internal)?;
            let (token_hash, csrf) = (fingerprint(&login.token), login.csrf.clone());
            let credential_hash = fingerprint(&stored);
            tx.mutate(move |sql| Box::pin(async move {
                let id = sql.query_one("INSERT INTO user_sign_ins(tenant_id,user_id,token_hash,csrf,credential_hash,state,expires_at) VALUES($1,$2,$3,$4,$5,'pending',now()+make_interval(mins => $6::int)) RETURNING id", &[&tenant,&user,&token_hash,&csrf,&credential_hash,&(minutes as i32)]).await?.get(0);
                Ok(((), vec![change(id, "password_verified", "pending")]))
            })).await?;
        } else {
            admit(&mut tx, &scope, &login).await?;
        }
        tx.commit().await?;
        Ok(login)
    }

    /// Both credential methods enter the same pending/session storage and use
    /// the same second-factor completion, standing and password-version checks.
    pub(crate) async fn external_admission(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        pending: bool,
        provider: i64,
    ) -> Result<Login> {
        let login = credential(pending);
        if pending {
            let tenant = scope.tenant();
            let user = scope.user();
            let stored: String = tx
                .sql()
                .query_one(
                    "SELECT password_hash FROM users WHERE tenant_id=$1 AND id=$2 FOR SHARE",
                    &[&tenant, &user],
                )
                .await?
                .get(0);
            let credential_hash = fingerprint(&stored);
            let token_hash = fingerprint(&login.token);
            let csrf = login.csrf.clone();
            let minutes = self
                .settings
                .get_global(tx.sql(), LIFETIME)
                .await?
                .as_i64()
                .ok_or(Error::Internal)? as i32;
            tx.mutate(move|sql|Box::pin(async move{
                let id=sql.query_one("INSERT INTO user_sign_ins(tenant_id,user_id,token_hash,csrf,credential_hash,state,expires_at,identity_provider_id) VALUES($1,$2,$3,$4,$5,'pending',now()+make_interval(mins=>$6),$7) RETURNING id",&[&tenant,&user,&token_hash,&csrf,&credential_hash,&minutes,&provider]).await?.get(0);
                Ok(((),vec![change(id,"external_identity_verified","pending")]))
            })).await?;
        } else {
            admit(tx, scope, &login).await?;
        }
        Ok(login)
    }
    async fn external_pending_live(
        &self,
        sql: &impl tokio_postgres::GenericClient,
        scope: &Scope,
        id: i64,
    ) -> Result<()> {
        let live:bool=sql.query_one("SELECT p.identity_provider_id IS NULL OR EXISTS(SELECT 1 FROM user_identity_providers c JOIN user_external_identities i ON i.tenant_id=c.tenant_id AND i.provider_id=c.id JOIN user_external_sessions e ON e.tenant_id=c.tenant_id AND e.provider_id=c.id WHERE c.tenant_id=p.tenant_id AND c.id=p.identity_provider_id AND c.enabled AND i.user_id=p.user_id AND e.user_id=p.user_id AND c.company_id=$3 AND e.id=p.external_session_id) FROM user_sign_ins p WHERE p.tenant_id=$1 AND p.id=$2",&[&scope.tenant(),&id,&scope.actor().organisation().map(|id|id.get())]).await?.get(0);
        if !live {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    // An opaque credential lookup is the same bootstrap exception as Session.
    // The resulting scope remains private and is re-proved for each action.
    async fn pending(&self, token: &str) -> Result<Pending> {
        if token.len() != 32 {
            return Err(Error::Unauthorized);
        }
        let hash = fingerprint(token);
        let mut client = self.db.get().await?;
        let row = client.query_opt("SELECT id,tenant_id,user_id,csrf,credential_hash FROM user_sign_ins WHERE token_hash=$1 AND state='pending'", &[&hash]).await?.ok_or(Error::Unauthorized)?;
        let (id, tenant, user, csrf, stored): (i64, i64, i64, String, Vec<u8>) =
            (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4));
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::user(tenant, user, "user.sign_in.expired"),
            )
            .await?;
        let expired = tx.sql().query_opt("SELECT expires_at<=clock_timestamp() FROM user_sign_ins WHERE tenant_id=$1 AND id=$2 AND state='pending' FOR UPDATE", &[&tenant,&id]).await?.ok_or(Error::Unauthorized)?.get::<_,bool>(0);
        if expired {
            tx.mutate(move |sql| {
                Box::pin(async move {
                    sql.execute(
                        "UPDATE user_sign_ins SET state='expired' WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &id],
                    )
                    .await?;
                    Ok(((), vec![change(id, "pending", "expired")]))
                })
            })
            .await?;
            tx.commit().await?;
            return Err(Error::Unauthorized);
        }
        tx.rollback().await?;
        let scope = self
            .authentication
            .authenticate(&**client, tenant, user, csrf)
            .await?;
        let current = client
            .query_opt(
                "SELECT password_hash FROM users WHERE tenant_id=$1 AND id=$2",
                &[&tenant, &user],
            )
            .await?;
        if !current.is_some_and(|row| fingerprint(&row.get::<_, String>(0)) == stored) {
            return Err(Error::Unauthorized);
        }
        self.external_pending_live(&**client, &scope, id).await?;
        Ok(Pending { id, scope, hash })
    }

    pub async fn pending_sign_in(&self, token: &str) -> Result<PendingSignIn> {
        let pending = self.pending(token).await?;
        Ok(PendingSignIn {
            csrf: pending.scope.csrf.clone(),
            methods: self.step_up.enrolled_methods(&pending.scope).await?,
        })
    }

    async fn hold_pending(
        &self,
        tx: &MutationTransaction<'_>,
        pending: &Pending,
        csrf: &str,
    ) -> Result<()> {
        if csrf != pending.scope.csrf {
            return Err(Error::Forbidden);
        }
        let row = tx.sql().query_opt("SELECT u.password_hash,p.credential_hash FROM user_sign_ins p JOIN users u ON u.tenant_id=p.tenant_id AND u.id=p.user_id WHERE p.tenant_id=$1 AND p.id=$2 AND p.token_hash=$3 AND p.state='pending' AND p.expires_at>clock_timestamp() FOR UPDATE OF p", &[&pending.scope.tenant(),&pending.id,&pending.hash]).await?;
        if !row.is_some_and(|row| fingerprint(&row.get::<_, String>(0)) == row.get::<_, Vec<u8>>(1))
        {
            return Err(Error::Unauthorized);
        }
        self.external_pending_live(tx.sql(), &pending.scope, pending.id)
            .await?;
        self.authentication
            .reprove(tx.sql(), &pending.scope)
            .await?;
        Ok(())
    }

    /// Explicit start/resend. GET never delivers a challenge.
    pub async fn start_sign_in_challenge(
        &self,
        token: &str,
        csrf: &str,
        method: &str,
    ) -> Result<Challenge> {
        let pending = self.pending(token).await?;
        if csrf != pending.scope.csrf {
            return Err(Error::Forbidden);
        }
        let target = pending.id.to_string();
        let challenge = self
            .step_up
            .start(
                &pending.scope,
                method,
                Purpose {
                    operation: SIGN_IN,
                    target: &target,
                    channel: Channel::Web,
                },
            )
            .await?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                pending
                    .scope
                    .mutation_context("user.sign_in.challenge_requested"),
            )
            .await?;
        if let Err(error) = self.hold_pending(&tx, &pending, csrf).await {
            tx.rollback().await?;
            drop(client);
            // Record an expiry observed while delivery/verification was in flight.
            self.pending(token).await?;
            return Err(error);
        }
        let id = pending.id;
        let method = method.to_owned();
        let availability = match challenge.availability {
            Availability::Ready => "ready",
            Availability::RetryLater => "retry_later",
            Availability::Unavailable => "unavailable",
        };
        tx.mutate(move |_| {
            Box::pin(async move {
                Ok((
                    (),
                    vec![Change {
                        kind: "user_sign_in".into(),
                        id,
                        old: json!({"state":"pending"}),
                        new: json!({"state":"pending","method":method,"challenge":availability}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(challenge)
    }

    /// Returns no admission for a wrong/locked/unavailable proof. A pending
    /// row lock serializes competing submits; proof alone never admits twice.
    pub async fn complete_sign_in(
        &self,
        token: &str,
        csrf: &str,
        method: &str,
        code: &str,
    ) -> Result<Option<Login>> {
        let pending = self.pending(token).await?;
        if csrf != pending.scope.csrf {
            return Err(Error::Forbidden);
        }
        let target = pending.id.to_string();
        let spent = self
            .step_up
            .verify(
                &pending.scope,
                method,
                code,
                Purpose {
                    operation: SIGN_IN,
                    target: &target,
                    channel: Channel::Web,
                },
            )
            .await?;
        if spent != Spent::Accepted {
            return Ok(None);
        }
        // Verification commits through its provider before admission begins.
        // Never nest a provider's transaction in the admission boundary.
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                pending.scope.mutation_context("user.sign_in.admitted"),
            )
            .await?;
        if let Err(error) = self.hold_pending(&tx, &pending, csrf).await {
            tx.rollback().await?;
            drop(client);
            // Record an expiry observed while delivery/verification was in flight.
            self.pending(token).await?;
            return Err(error);
        }
        // Delivery/verification can take time. Expiry and standing still win.
        let login = credential(false);
        transition(&mut tx, &pending.scope, pending.id, "admitted").await?;
        admit(&mut tx, &pending.scope, &login).await?;
        let tenant = pending.scope.tenant();
        let id = pending.id;
        let session_hash = crate::sso::hash(&login.token);
        let external: Option<i64> = tx
            .sql()
            .query_one(
                "SELECT external_session_id FROM user_sign_ins WHERE tenant_id=$1 AND id=$2",
                &[&tenant, &id],
            )
            .await?
            .get(0);
        if external.is_some() {
            tx.mutate(move|sql|Box::pin(async move{
            let rows=sql.query("UPDATE user_external_sessions SET session_hash=$3 WHERE tenant_id=$1 AND id=(SELECT external_session_id FROM user_sign_ins WHERE tenant_id=$1 AND id=$2) RETURNING id",&[&tenant,&id,&session_hash]).await?;
            Ok(((),rows.iter().map(|r|crate::sso::change("user.external_session",r.get(0),json!({"pending":true}),json!({"pending":false}))).collect()))
        })).await?;
        }
        tx.commit().await?;
        Ok(Some(login))
    }

    pub async fn cancel_sign_in(&self, token: &str, csrf: &str) -> Result<()> {
        let pending = self.pending(token).await?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                pending.scope.mutation_context("user.sign_in.cancelled"),
            )
            .await?;
        if let Err(error) = self.hold_pending(&tx, &pending, csrf).await {
            tx.rollback().await?;
            drop(client);
            // Record an expiry observed while delivery/verification was in flight.
            self.pending(token).await?;
            return Err(error);
        }
        transition(&mut tx, &pending.scope, pending.id, "cancelled").await?;
        tx.commit().await
    }
}

/// Called by Core User's always-installed factor lifecycle adapter, in the
/// owner's transaction, so reset, deletion and sign-out revoke pending entry.
pub(crate) async fn revoke(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    user: i64,
) -> Result<Vec<Change>> {
    Ok(sql.query("UPDATE user_sign_ins SET state='revoked' WHERE tenant_id=$1 AND user_id=$2 AND state='pending' RETURNING id", &[&tenant,&user]).await?.into_iter().map(|row| change(row.get(0),"pending","revoked")).collect())
}
