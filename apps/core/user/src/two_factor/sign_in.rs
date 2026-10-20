//! Sign-in as one more step-up consumer. A right password opens a session at
//! once unless the tenant's policy turns [`SIGN_IN`] on; then the browser
//! gets a pending sign-in (a random token, only its SHA-256 stored, ten
//! minutes) and no session exists until a code is accepted. The code goes
//! through `StepUp::require` with the holder's code presented, exactly as an
//! agent's does, so the policy, the provider, the failure lock and the audit
//! are the ones every other protected action uses. Bilimbi has no
//! counterpart; the flow follows GitHub's two-factor sign-in.
use super::{TwoFactorError, SIGN_IN};
use crate::{identity, CredentialError, Login, UserService};
use bilibi_database::Error;
use bilibi_tenancy::Scope;
use sha2::{Digest, Sha256};

pub const PENDING_MINUTES: i64 = 10;

/// What a right password led to.
pub enum SignIn {
    Session(Login),
    /// A code is owed; the token goes to the browser and nowhere else.
    Pending(String),
}

/// What the verification page shows about a pending sign-in.
pub struct PendingSignIn {
    /// The account has the authenticator app.
    pub authenticator: bool,
    /// The verified phone, masked, when the account has one.
    pub phone: Option<String>,
    /// Seconds until another text message may be sent.
    pub wait_seconds: i64,
}
impl PendingSignIn {
    /// Neither method: the holder cannot sign in until an administrator helps.
    pub fn enrolled(&self) -> bool {
        self.authenticator || self.phone.is_some()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("This sign-in has expired. Sign in again.")]
    Expired,
    #[error("Your account needs 2FA to sign in, but no verified phone or authenticator app is set up. Contact your administrator.")]
    NotEnrolled,
    #[error(transparent)]
    Refused(TwoFactorError),
    #[error(transparent)]
    Other(#[from] Error),
}
impl From<TwoFactorError> for SignInError {
    fn from(error: TwoFactorError) -> Self {
        match error {
            TwoFactorError::Other(error) => Self::Other(error),
            TwoFactorError::NoPhone => Self::NotEnrolled,
            refusal => Self::Refused(refusal),
        }
    }
}

fn token_hash(token: &str) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(token.as_bytes()))
}

impl UserService {
    /// The sign-in page's step: the credential, then either a session or a
    /// pending sign-in. An account whose only method is a text message is
    /// sent its code at once.
    pub async fn sign_in(
        &self,
        email: &str,
        password: &str,
    ) -> std::result::Result<SignIn, CredentialError> {
        let (tenant, user) = identity::verify_credentials(&self.db, email, password).await?;
        let scope = self.scope_for(tenant, user, String::new()).await?;
        if !self.step_up.applies(&scope, SIGN_IN).await? {
            return identity::open_session(&self.db, tenant, user)
                .await
                .map(SignIn::Session);
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let client = self.db.get().await.map_err(Error::from)?;
        client
            .execute(
                "DELETE FROM user_pending_sign_ins WHERE expires_at<now()",
                &[],
            )
            .await
            .map_err(Error::from)?;
        client
            .execute(
                "INSERT INTO user_pending_sign_ins(token_hash,tenant_id,user_id,expires_at) VALUES($1,$2,$3,now()+make_interval(mins => $4::int))",
                &[&token_hash(&token), &tenant, &user, &(PENDING_MINUTES as i32)],
            )
            .await
            .map_err(Error::from)?;
        drop(client);
        let status = self.two_factor_status(&scope).await?;
        if status.sms && !status.authenticator {
            match self.send_check_code(&scope).await {
                Ok(_) | Err(TwoFactorError::ResendTooSoon { .. } | TwoFactorError::ResendLimit) => {
                }
                Err(TwoFactorError::Other(error)) => return Err(error.into()),
                Err(error) => eprintln!("warning: sign-in code not sent: {error}"),
            }
        }
        Ok(SignIn::Pending(token))
    }

    /// The pending sign-in `token` names, while it lasts.
    pub async fn pending_sign_in(&self, token: &str) -> Result<Option<PendingSignIn>, Error> {
        let Some(scope) = self.pending_scope(token).await? else {
            return Ok(None);
        };
        let status = self.two_factor_status(&scope).await?;
        let phone = if status.sms {
            self.phone_status(&scope).await?.map(|phone| phone.masked)
        } else {
            None
        };
        Ok(Some(PendingSignIn {
            authenticator: status.authenticator,
            phone,
            wait_seconds: self.sms_wait_seconds(&scope).await?,
        }))
    }

    /// "Resend code": another text message for the pending sign-in.
    pub async fn send_sign_in_code(&self, token: &str) -> Result<String, SignInError> {
        let scope = self
            .pending_scope(token)
            .await?
            .ok_or(SignInError::Expired)?;
        Ok(self.send_check_code(&scope).await?)
    }

    /// The code for a pending sign-in: through step-up with the code
    /// presented, then the pending row is spent and a session opens. The
    /// accepted code also counts as that session's step-up.
    pub async fn complete_sign_in(&self, token: &str, code: &str) -> Result<Login, SignInError> {
        let scope = self
            .pending_scope(token)
            .await?
            .ok_or(SignInError::Expired)?;
        let (tenant, user) = (scope.tenant(), scope.user());
        self.unlocked(&scope).await?;
        let presented = scope.clone().presenting_step_up(code);
        match self
            .step_up
            .require(&presented, SIGN_IN, &format!("user:{user}"))
            .await
        {
            Ok(()) => {}
            Err(Error::StepUpRequired { reason, .. }) => {
                return Err(match reason {
                    "not_enrolled" => SignInError::NotEnrolled,
                    "locked" => self.unlocked(&scope).await.err().map_or(
                        SignInError::Refused(TwoFactorError::InvalidCode),
                        Into::into,
                    ),
                    _ => SignInError::Refused(TwoFactorError::InvalidCode),
                })
            }
            Err(error) => return Err(error.into()),
        }
        let client = self.db.get().await.map_err(Error::from)?;
        let spent = client
            .execute(
                "DELETE FROM user_pending_sign_ins WHERE token_hash=$1 AND expires_at>now()",
                &[&token_hash(token)],
            )
            .await
            .map_err(Error::from)?;
        drop(client);
        if spent != 1 {
            return Err(SignInError::Expired);
        }
        let login = identity::open_session(&self.db, tenant, user)
            .await
            .map_err(|error| match error {
                CredentialError::Other(error) => SignInError::Other(error),
                _ => SignInError::Expired,
            })?;
        let session = self.scope_for(tenant, user, login.csrf.clone()).await?;
        let client = self.db.get().await.map_err(Error::from)?;
        self.sessions.step_up(&**client, &session).await?;
        Ok(login)
    }

    /// Abandons a pending sign-in ("Back to sign in").
    pub async fn cancel_sign_in(&self, token: &str) -> Result<(), Error> {
        let client = self.db.get().await?;
        client
            .execute(
                "DELETE FROM user_pending_sign_ins WHERE token_hash=$1",
                &[&token_hash(token)],
            )
            .await?;
        Ok(())
    }

    async fn pending_scope(&self, token: &str) -> Result<Option<Scope>, Error> {
        if token.is_empty() || token.len() > 64 {
            return Ok(None);
        }
        let client = self.db.get().await?;
        let row = client
            .query_opt(
                "SELECT tenant_id,user_id FROM user_pending_sign_ins WHERE token_hash=$1 AND expires_at>now()",
                &[&token_hash(token)],
            )
            .await?;
        drop(client);
        let Some(row) = row else {
            return Ok(None);
        };
        // An account removed or frozen meanwhile has no pending sign-in.
        match self.scope_for(row.get(0), row.get(1), String::new()).await {
            Ok(scope) => Ok(Some(scope)),
            Err(Error::Unauthorized) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn scope_for(&self, tenant: i64, user: i64, csrf: String) -> Result<Scope, Error> {
        let client = self.db.get().await?;
        self.authentication
            .authenticate(&**client, tenant, user, csrf)
            .await
    }
}
