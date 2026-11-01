//! The authenticator as Base Step-up's provider: Step-up asks, this answers
//! about the scope's own account holder. It learns only an operation key and
//! target as data to record, never what the operation is.
use super::{Spend, TwoFactorError};
use crate::UserService;
use bilibi_database::Change;
use bilibi_step_up::{
    Availability, Challenge, FactorAccount, Lifecycle, Method, Provider, Purpose, SessionProof,
    SessionProvider, Spent, WindowState,
};
use bilibi_tenancy::{ProofFuture, Scope};
use serde_json::json;
use std::sync::Arc;

pub struct Authenticator(pub Arc<UserService>);

pub const METHOD: Method = Method::new("core.user.authenticator", "Authenticator app");

impl SessionProvider for Authenticator {
    fn session<'a>(
        &'a self,
        scope: &'a Scope,
        proof: SessionProof,
    ) -> ProofFuture<'a, WindowState> {
        Box::pin(async move {
            let client = self.0.db.get().await?;
            Ok(match proof {
                SessionProof::Window {
                    window_seconds,
                    cap_seconds,
                    extend,
                } => {
                    let window = self
                        .0
                        .sessions
                        .step_up_window(&**client, scope, window_seconds, cap_seconds, extend)
                        .await?;
                    WindowState::from_flags(window.checked, window.within_cap, window.within_window)
                }
                SessionProof::Fresh { seconds, consume } => {
                    if !consume {
                        return Ok(if client.query_opt("SELECT 1 FROM sessions WHERE tenant_id=$1 AND user_id=$2 AND csrf=$3 AND impersonator_id IS NULL AND stepped_up_at>now()-make_interval(secs => $4::double precision) AND (step_up_spent_at IS NULL OR step_up_spent_at<stepped_up_at)", &[&scope.tenant(), &scope.user(), &scope.csrf, &(seconds as f64)]).await?.is_some() { WindowState::Holds } else { WindowState::None });
                    }
                    if self
                        .0
                        .sessions
                        .spend_fresh_step_up(&**client, scope, seconds)
                        .await?
                    {
                        WindowState::Holds
                    } else {
                        WindowState::None
                    }
                }
            })
        })
    }
    fn prompt<'a>(&'a self, _scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String> {
        Box::pin(async move { Ok(super::with_return(super::CONFIRM_PATH, Some(back))) })
    }
}
impl Provider for Authenticator {
    fn enrolled<'a>(
        &'a self,
        sql: &'a dyn bilibi_tenancy::ProofClient,
        account: FactorAccount,
    ) -> ProofFuture<'a, bool> {
        Box::pin(async move {
            Ok(sql.query_opt("SELECT 1 FROM user_two_factor WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) AND confirmed_at IS NOT NULL", &[&account.tenant(), &account.user()]).await?.is_some())
        })
    }
    fn start<'a>(&'a self, scope: &'a Scope, _purpose: Purpose<'a>) -> ProofFuture<'a, Challenge> {
        Box::pin(async move {
            Ok(Challenge {
                availability: if self.0.two_factor_status(scope).await?.enabled {
                    Availability::Ready
                } else {
                    Availability::Unavailable
                },
                masked_destination: None,
                retry_after_seconds: None,
            })
        })
    }
    fn setup<'a>(&'a self, _scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String> {
        Box::pin(async move { Ok(selected(super::SETTINGS_PATH, back)) })
    }
    fn lifecycle<'a>(
        &'a self,
        sql: &'a tokio_postgres::Transaction<'_>,
        account: FactorAccount,
        event: Lifecycle,
    ) -> ProofFuture<'a, Vec<Change>> {
        Box::pin(async move {
            let mut changes = crate::sign_in::revoke(sql, account.tenant(), account.user()).await?;
            if event == Lifecycle::Revoke {
                return Ok(changes);
            }
            // Return every affected row, never the sealed secret or a code.
            if let Some(row) = sql.query_opt("DELETE FROM user_two_factor WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) RETURNING confirmed_at IS NOT NULL", &[&account.tenant(), &account.user()]).await? {
                changes.push(Change { kind: "user_two_factor".into(), id: account.user(), old: json!({"status": if row.get::<_, bool>(0) { "enabled" } else { "pending" }}), new: json!({"status": "off"}) });
            }
            // Only the two-factor counter: a sign-in lock is not a 2FA matter.
            let kind = crate::credential_attempts::Kind::StepUp;
            if crate::credential_attempts::forget(sql, account.tenant(), account.user(), kind)
                .await?
            {
                changes.push(crate::credential_attempts::change(
                    account.user(),
                    kind,
                    json!({}),
                    json!({"reset": true}),
                ));
            }
            Ok(changes)
        })
    }
    fn spend<'a>(
        &'a self,
        scope: &'a Scope,
        code: &'a str,
        purpose: Purpose<'a>,
    ) -> ProofFuture<'a, Spent> {
        Box::pin(async move {
            let spend = Spend::Purpose {
                operation: purpose.operation.to_owned(),
                channel: purpose.channel.as_str(),
            };
            let outcome = match self.0.prove(scope, code).await {
                Ok(step) => self.0.consume(scope, step, spend).await,
                Err(error) => Err(error),
            };
            match outcome {
                Ok(()) => Ok(Spent::Accepted),
                Err(TwoFactorError::Locked { .. }) => Ok(Spent::Locked),
                Err(TwoFactorError::NotEnabled) => Ok(Spent::NotEnrolled),
                Err(TwoFactorError::Other(error)) => Err(error),
                Err(_) => Ok(Spent::Rejected),
            }
        })
    }
    fn prompt<'a>(&'a self, _scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String> {
        Box::pin(async move { Ok(selected(super::CONFIRM_PATH, back)) })
    }
}

fn selected(base: &str, back: &str) -> String {
    let path = super::with_return(base, Some(back));
    format!(
        "{path}{}method={}",
        if path.contains('?') { "&" } else { "?" },
        METHOD.id
    )
}
