//! Impersonation (Bilimbi `BilimbiWeb.Impersonation` and its controller): an
//! operator holding `admin.user.impersonate` moves their own live session onto
//! another account of the tenant, and back. The session row keeps its token
//! and names the operator (`sessions.impersonator_id`), so the pair is sealed
//! into every later `Scope` and every audit row names both identities.
//!
//! Each switch is one transaction: the session moves only while it still holds
//! the identity the request was made from (a stale tab, a replayed request or
//! an ended session is refused), its CSRF secret rotates so every form and
//! live socket issued to the old identity is refused afterwards, and the
//! retained `impersonation.started`/`impersonation.stopped` action commits with
//! it. A switch is not announced as a termination: the tab that asked is
//! already navigating, and the session's other live pages find themselves
//! stale at their next event or refresh and reload as the new identity. Bilimbi records those actions best-effort; here a trail that cannot be
//! written refuses the switch, except that leaving never traps an operator:
//! a failed return ends the session instead.
use crate::{
    identity::{standing, Freeze, Standing},
    UserService,
};
use bilibi_audit::{record_action, Attribution, RequestInfo};
use bilibi_database::{Error, Result};
use bilibi_security::Holder;
use bilibi_tenancy::{AccountKind, Scope};
use serde_json::json;

pub const IMPERSONATE: &str = "admin.user.impersonate";

/// Why an impersonation did not start; each maps to Bilimbi's flash.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("You do not have access to that action.")]
    Forbidden,
    #[error("Cannot impersonate while impersonating.")]
    AlreadyImpersonating,
    #[error("You cannot impersonate yourself.")]
    OwnAccount,
    #[error("Unable to find that user in this workspace.")]
    NotFound,
    /// The target's company is archived (Bilimbi refuses it at the session
    /// write), or the session moved underneath the request.
    #[error("Unable to impersonate that user.")]
    Unable,
    #[error(transparent)]
    Other(#[from] Error),
}

/// Where leaving landed: back in the operator's own account, nothing to
/// leave (a stale tab), or signed out because the return could not be made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Left {
    Restored,
    NotImpersonating,
    SignedOut,
}

/// Bilimbi's recorder payload, so both trails read the same: the operator is
/// the actor at each transition and the target is the subject.
fn payload(summary: &str, operator: i64, target: i64) -> serde_json::Value {
    json!({
        "semantic": true,
        "source": "Impersonation",
        "summary": summary,
        "surface": "admin.impersonate",
        "subject": {"name": "user", "id": target, "label": format!("User#{target}")},
        "context": {"impersonator_id": operator, "target_id": target},
        "result": "succeeded",
    })
}

/// The operator acting as themselves: no impersonator on the row, whichever
/// identity the session held when the request arrived.
fn operator(scope: &Scope, operator: i64) -> Attribution {
    Attribution {
        tenant: Some(scope.tenant()),
        actor_type: "user",
        actor_id: operator,
        impersonator_id: None,
        system_principal: None,
        trace_id: None,
        agent_self_report: None,
        client_identity: None,
    }
}

impl UserService {
    /// Starts viewing as `target`. `csrf` is the secret the request carried;
    /// it must be the session's current one.
    pub async fn impersonate(
        &self,
        token: &str,
        csrf: &str,
        target: i64,
        request: &RequestInfo,
    ) -> std::result::Result<String, Refusal> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.sessions.activity(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        if scope.impersonator().is_some() {
            return Err(Refusal::AlreadyImpersonating);
        }
        // Decided on the pooled connection so a refusal's decision-log row
        // commits even though nothing else does.
        match bilibi_authz::authorize_key(&**client, &scope, IMPERSONATE).await {
            Ok(()) => {}
            Err(Error::Forbidden) => return Err(Refusal::Forbidden),
            Err(error) => return Err(error.into()),
        }
        if target == scope.user() {
            return Err(Refusal::OwnAccount);
        }
        if target <= 0 {
            return Err(Refusal::NotFound);
        }
        let tx = client.transaction().await.map_err(Error::from)?;
        match standing(&*tx, scope.tenant(), target).await? {
            // A customer is never impersonated (ADR 15); it is not a staff
            // account, so it is not found here.
            Standing::Live(identity) if identity.kind != AccountKind::Staff => {
                return Err(Refusal::NotFound)
            }
            Standing::Live(_) => {}
            Standing::Frozen(Freeze::CompanyArchived) => return Err(Refusal::Unable),
            Standing::Frozen(Freeze::Identity) | Standing::Unknown => {
                return Err(Refusal::NotFound)
            }
        }
        let csrf = match self
            .sessions
            .switch(
                &*tx,
                token,
                scope.tenant(),
                Holder {
                    user: scope.user(),
                    impersonator: None,
                },
                Holder {
                    user: target,
                    impersonator: Some(scope.user()),
                },
            )
            .await
        {
            Ok(csrf) => csrf,
            Err(Error::Conflict) => return Err(Refusal::Unable),
            Err(error) => return Err(error.into()),
        };
        record_action(
            &*tx,
            &operator(&scope, scope.user()),
            Some(request),
            "impersonation.started",
            payload("Started impersonating", scope.user(), target),
            true,
        )
        .await?;
        tx.commit().await.map_err(Error::from)?;
        Ok(csrf)
    }

    /// Returns the session to the operator behind it. Any signed-in session
    /// may ask, as in Bilimbi (the route carries no capability): one that is
    /// not impersonating has nothing to leave. A return that cannot be made
    /// (the operator's account is gone or frozen, the trail cannot be
    /// written) ends the session rather than leave the operator inside the
    /// borrowed account.
    pub async fn leave_impersonation(
        &self,
        token: &str,
        csrf: &str,
        request: &RequestInfo,
    ) -> Result<Left> {
        let client = self.db.get().await?;
        // The operator may have been removed meanwhile: the session no longer
        // authenticates, which is already the signed-out outcome.
        let scope = self.sessions.check(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden);
        }
        let Some(impersonator) = scope.impersonator().map(|operator| operator.user()) else {
            return Ok(Left::NotImpersonating);
        };
        drop(client);
        match self.restore(token, &scope, impersonator, request).await {
            Ok(()) => Ok(Left::Restored),
            Err(error) => {
                eprintln!("warning: leaving impersonation failed, ending the session: {error:?}");
                let client = self.db.get().await?;
                self.sessions.delete(&**client, &scope, token).await?;
                Ok(Left::SignedOut)
            }
        }
    }

    async fn restore(
        &self,
        token: &str,
        scope: &Scope,
        impersonator: i64,
        request: &RequestInfo,
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let tx = client.transaction().await?;
        if !matches!(
            standing(&*tx, scope.tenant(), impersonator).await?,
            Standing::Live(_)
        ) {
            return Err(Error::Unauthorized);
        }
        self.sessions
            .switch(
                &*tx,
                token,
                scope.tenant(),
                Holder {
                    user: scope.user(),
                    impersonator: Some(impersonator),
                },
                Holder {
                    user: impersonator,
                    impersonator: None,
                },
            )
            .await?;
        record_action(
            &*tx,
            &operator(scope, impersonator),
            Some(request),
            "impersonation.stopped",
            payload("Stopped impersonating", impersonator, scope.user()),
            true,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
