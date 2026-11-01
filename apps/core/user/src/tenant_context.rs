//! This mirrors Bilimbi's user tenant-switching boundary. The tenant a staff
//! browser session works in is separate from a desk:
//! a tenant membership changes the authorization boundary, while desks remain
//! workplaces inside one tenant (ADR 18).
use crate::{Login, UserService, SIGN_IN};
use bilibi_audit::{record_action, Attribution, RequestInfo};
use bilibi_database::{Change, Error};
use bilibi_security::Scope;
use bilibi_tenancy::AccountKind;
use serde_json::json;
use tokio_postgres::GenericClient;

#[derive(Clone, Debug)]
pub struct TenantChoice {
    pub id: i64,
    pub name: String,
    pub current: bool,
}

/// How entering another tenant continues. `Challenge` carries a pending
/// sign-in for the existing second-factor page; `Sso` names the target
/// company's enforced sign-in connection.
pub enum TenantEntry {
    Entered(Login),
    Challenge(Login),
    Sso(String),
}

#[derive(Debug, thiserror::Error)]
pub enum TenantChoiceRefusal {
    #[error("That tenant is not available to you.")]
    Unavailable,
    #[error(
        "This tenant needs a second factor you have not set up. Ask its administrator for help."
    )]
    EntryPolicy,
    #[error(transparent)]
    Other(#[from] Error),
}

impl UserService {
    /// Display-only live staff memberships belonging to the signed-in
    /// identity. The switch repeats every proof under a row lock.
    pub async fn tenant_choices(
        &self,
        sql: &impl GenericClient,
        scope: &Scope,
    ) -> bilibi_database::Result<Vec<TenantChoice>> {
        if scope.impersonator().is_some() {
            return Ok(Vec::new());
        }
        let rows = sql
            .query(
                "SELECT other.tenant_id,t.name,other.tenant_id=$1 FROM users current JOIN users other ON other.identity_id=current.identity_id JOIN tenants t ON t.id=other.tenant_id WHERE current.tenant_id=$1 AND current.id=$2 AND current.kind='staff' AND other.kind='staff' AND other.status='active' AND other.deleted_at IS NULL AND t.status='active' AND t.deleted_at IS NULL ORDER BY lower(t.name),other.tenant_id",
                &[&scope.tenant(), &scope.user()],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|row| TenantChoice {
                id: row.get(0),
                name: row.get(1),
                current: row.get(2),
            })
            .collect())
    }

    /// Replaces one ordinary staff session with a session in another live
    /// membership of the same identity. The bearer and CSRF secret rotate.
    /// A target enforcing company sign-in continues at that connection, and a
    /// target requiring a sign-in factor continues at the second-factor page;
    /// neither changes the current session. A target requiring a factor the
    /// holder has not enrolled, and impersonation, are refused.
    pub async fn choose_tenant(
        &self,
        token: &str,
        csrf: &str,
        target: i64,
        request: &RequestInfo,
    ) -> std::result::Result<TenantEntry, TenantChoiceRefusal> {
        if target <= 0 {
            return Err(Error::Invalid.into());
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let current = self.sessions.activity(&**client, token).await?;
        if current.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        if current.impersonator().is_some() {
            return Err(TenantChoiceRefusal::Unavailable);
        }
        if target == current.tenant() {
            return Ok(TenantEntry::Entered(Login {
                token: token.to_owned(),
                csrf: csrf.to_owned(),
                pending: false,
            }));
        }
        let target_user = client
            .query_opt(
                "SELECT other.id FROM users current JOIN users other ON other.identity_id=current.identity_id JOIN tenants t ON t.id=other.tenant_id WHERE current.tenant_id=$1 AND current.id=$2 AND other.tenant_id=$3 AND current.kind='staff' AND other.kind='staff' AND other.status='active' AND other.deleted_at IS NULL AND t.status='active' AND t.deleted_at IS NULL",
                &[&current.tenant(), &current.user(), &target],
            )
            .await
            .map_err(Error::from)?
            .map(|row| row.get::<_, i64>(0))
            .ok_or(TenantChoiceRefusal::Unavailable)?;
        let new_csrf = uuid::Uuid::new_v4().simple().to_string();
        let target_scope = self
            .authentication
            .authenticate_session(&**client, target, target_user, None, None, new_csrf.clone())
            .await
            .map_err(|_| TenantChoiceRefusal::Unavailable)?;
        if !self.password_sso_allowed(&**client, &target_scope).await? {
            let company = target_scope.current_company().map(|id| id.get());
            return client
                .query_opt(
                    "SELECT public_id FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 AND enabled AND enforce_sso ORDER BY id LIMIT 1",
                    &[&target, &company],
                )
                .await
                .map_err(Error::from)?
                .map(|row| TenantEntry::Sso(row.get(0)))
                .ok_or(TenantChoiceRefusal::EntryPolicy);
        }
        if self.step_up.required(&target_scope, SIGN_IN).await? {
            if self
                .step_up
                .enrolled_methods(&target_scope)
                .await?
                .is_empty()
            {
                return Err(TenantChoiceRefusal::EntryPolicy);
            }
            let login = crate::sign_in::credential(true);
            let mut tx = self
                .mutations
                .begin(
                    &mut client,
                    target_scope.mutation_context("user.tenant.entry_challenged"),
                )
                .await?;
            let row = tx
                .sql()
                .query_one(
                    "SELECT i.password_hash,i.id FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.deleted_at IS NULL AND i.status='active' WHERE u.tenant_id=$1 AND u.id=$2 AND u.deleted_at IS NULL AND u.status='active' FOR SHARE OF i",
                    &[&target, &target_user],
                )
                .await
                .map_err(Error::from)?;
            let stored: String = row.get(0);
            self.insert_password_pending(
                &mut tx,
                &target_scope,
                row.get(1),
                &stored,
                &login,
                AccountKind::Staff.as_str(),
            )
            .await?;
            tx.commit().await?;
            return Ok(TenantEntry::Challenge(login));
        }
        let new_token = uuid::Uuid::new_v4().simple().to_string();
        let company = target_scope.current_company().map(|id| id.get());
        let from_tenant = current.tenant();
        let from_user = current.user();
        let old_csrf = current.csrf.clone();
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                target_scope.mutation_context("user.tenant.switch"),
            )
            .await?;
        let token_for_update = token.to_owned();
        let next_token = new_token.clone();
        let next_csrf = new_csrf.clone();
        let moved = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_opt(
                        "UPDATE sessions s SET id=$6,tenant_id=$3,user_id=$4,csrf=$7,company_id=$8,held_company_id=NULL,impersonator_id=NULL,last_activity=now(),stepped_up_at=NULL,step_up_used_at=NULL,step_up_spent_at=NULL FROM users current,users target_user,tenants target_tenant WHERE s.id=$5 AND s.tenant_id=$1 AND s.user_id=$2 AND s.csrf=$9 AND s.impersonator_id IS NULL AND current.tenant_id=$1 AND current.id=$2 AND target_user.tenant_id=$3 AND target_user.id=$4 AND target_user.identity_id=current.identity_id AND target_user.status='active' AND target_user.deleted_at IS NULL AND target_tenant.id=$3 AND target_tenant.status='active' AND target_tenant.deleted_at IS NULL RETURNING current.identity_id",
                        &[&from_tenant,&from_user,&target,&target_user,&token_for_update,&next_token,&next_csrf,&company,&old_csrf],
                    ).await?;
                    let Some(row) = row else { return Err(Error::Unauthorized); };
                    let identity: i64 = row.get(0);
                    sql.execute("UPDATE user_identities SET last_tenant_id=$2,updated_at=now() WHERE id=$1 AND status='active' AND deleted_at IS NULL", &[&identity,&target]).await?;
                    Ok(((), vec![Change { kind: "user.tenant_choice".into(), id: identity, old: json!({"tenant_id":from_tenant}), new: json!({"tenant_id":target}) }]))
                })
            })
            .await;
        if let Err(error) = moved {
            return Err(error.into());
        }
        record_action(
            tx.sql(),
            &Attribution::from_scope(&target_scope),
            Some(request),
            "user.tenant.switched",
            json!({
                "semantic": true,
                "source": "TenantContext",
                "summary": "Entered another tenant membership",
                "surface": "shell.tenant",
                "context": {"from": from_tenant, "to": target},
                "result": "succeeded"
            }),
            false,
        )
        .await?;
        tx.commit().await?;
        self.sessions.announce_tenant_switch(token.to_owned());
        Ok(TenantEntry::Entered(Login {
            token: new_token,
            csrf: new_csrf,
            pending: false,
        }))
    }

    /// Ends the session a browser held before a second-factor or company
    /// sign-in carried it into another membership of the same identity, and
    /// closes that session's live pages. Any other new session leaves it be.
    pub async fn retire_replaced_session(&self, old: &str, new: &str) -> Result<(), Error> {
        let mut client = self.db.get().await?;
        let Ok(current) = self.sessions.activity(&**client, old).await else {
            return Ok(());
        };
        if current.impersonator().is_some() {
            return Ok(());
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                current.mutation_context("user.tenant.session_replaced"),
            )
            .await?;
        let (tenant, user) = (current.tenant(), current.user());
        let (old_id, new_id) = (old.to_owned(), new.to_owned());
        let ended = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let rows = sql.query("DELETE FROM sessions s USING sessions n,users nu,users ou WHERE s.id=$1 AND s.tenant_id=$2 AND s.user_id=$3 AND s.impersonator_id IS NULL AND n.id=$4 AND n.tenant_id<>s.tenant_id AND nu.tenant_id=n.tenant_id AND nu.id=n.user_id AND ou.tenant_id=s.tenant_id AND ou.id=s.user_id AND ou.identity_id=nu.identity_id RETURNING nu.tenant_id", &[&old_id,&tenant,&user,&new_id]).await?;
                    let changes = rows.iter().map(|row| Change { kind: "user.tenant_session".into(), id: user, old: json!({"tenant_id":tenant}), new: json!({"tenant_id":row.get::<_, i64>(0)}) }).collect::<Vec<_>>();
                    Ok((!changes.is_empty(), changes))
                })
            })
            .await?;
        tx.commit().await?;
        if ended {
            self.sessions.announce_tenant_switch(old.to_owned());
        }
        Ok(())
    }
}
