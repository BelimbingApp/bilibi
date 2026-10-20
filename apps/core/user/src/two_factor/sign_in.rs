//! The sign-in second factor: Base Step-up operation `core.user.sign_in`, off
//! by default, which a tenant turns on to ask every account holder for an
//! authenticator code after the password (ADR 01, section 4: 2FA is a
//! per-company policy, on sign-in as on any action). Declared `everywhere`,
//! so the tenant's choice holds on a development deployment too.
//!
//! The session is born waiting (`identity::login`) and authenticates nothing
//! until this module lets it act: at once when the policy leaves sign-in off,
//! after a current authenticator or SMS code, or, for a holder with no
//! authenticator yet, after setting one up. The pages are the host's
//! (`apps/web/src/auth.rs`: `/login/code`, `/login/setup`). No Bilimbi
//! counterpart.
use super::{sms, Enrollment, Outcome, SmsSent, Spend, TwoFactorError};
use crate::UserService;
use bilibi_database::{Error, Result};
use bilibi_session::SecondFactor;
use bilibi_step_up::{declare, Operation};
use bilibi_tenancy::Scope;

pub const SIGN_IN: &str = "core.user.sign_in";
declare!(
    "core/user",
    Operation::new(SIGN_IN, "Sign in")
        .off_by_default()
        .everywhere()
);

/// What the code page offers a session that owes a code.
#[derive(Clone, Debug)]
pub struct SignInChallenge {
    /// A code from the authenticator app works.
    pub authenticator: bool,
    /// SMS codes are on: the masked number.
    pub phone: Option<String>,
    /// Seconds before another text may be sent.
    pub resend_in: i64,
    /// A text went out just now, as this page opened.
    pub sent: bool,
    /// Why an automatic text could not go out.
    pub notice: String,
}

/// What setup at sign-in can offer an account with no second factor.
#[derive(Clone, Debug)]
pub struct SignInSetupOptions {
    /// The administrator's number on the account, masked, when this server
    /// sends texts.
    pub phone: Option<String>,
}

impl UserService {
    /// Right after the password: what the new session owes under its
    /// tenant's policy. A session that owes nothing is finished here, so it
    /// acts at once, as before this check existed.
    pub(crate) async fn sign_in_owed(&self, token: &str) -> Result<Option<SecondFactor>> {
        let client = self.db.get().await?;
        let (scope, _) = self.sessions.pending(&**client, token).await?;
        if !self.step_up.applies(&scope, SIGN_IN).await? {
            self.sessions
                .finish_sign_in(&**client, &scope, false)
                .await?;
            return Ok(None);
        }
        if self.two_factor_status(&scope).await?.any() {
            return Ok(Some(SecondFactor::Code));
        }
        self.sessions
            .owe(&**client, &scope, SecondFactor::Setup)
            .await?;
        Ok(Some(SecondFactor::Setup))
    }

    /// What a session still signing in owes. `Unauthorized` when the token
    /// names no such session (expired, finished, abandoned or never begun).
    pub async fn sign_in_pending(&self, token: &str) -> Result<SecondFactor> {
        let client = self.db.get().await?;
        Ok(self.sessions.pending(&**client, token).await?.1)
    }

    /// A current authenticator or SMS code: the session may
    /// act from now on, and its step-up window opens.
    pub async fn sign_in_with_code(&self, token: &str, code: &str) -> Outcome<()> {
        let scope = self.owing(token, SecondFactor::Code).await?;
        let proof = self.prove(&scope, code).await?;
        self.consume(&scope, proof, Spend::SignIn).await
    }

    /// Setup at sign-in, step one: the password again, then a fresh secret,
    /// exactly as on the settings page.
    pub async fn begin_sign_in_setup(&self, token: &str, password: &str) -> Outcome<Enrollment> {
        let scope = self.owing(token, SecondFactor::Setup).await?;
        self.begin_two_factor(&scope, password).await
    }

    /// Setup at sign-in, step two: the first code turns the authenticator on
    /// and lets the session act in the same transaction.
    pub async fn confirm_sign_in_setup(&self, token: &str, code: &str) -> Outcome<()> {
        let scope = self.owing(token, SecondFactor::Setup).await?;
        self.confirm(&scope, code, true).await
    }

    /// The code page for a session that owes a code. With `auto_send` (the
    /// page opening), an account whose only second factor is SMS is sent a
    /// code, unless one is still usable or the sending bounds say wait.
    pub async fn sign_in_challenge(
        &self,
        token: &str,
        auto_send: bool,
    ) -> Outcome<SignInChallenge> {
        let scope = self.owing(token, SecondFactor::Code).await?;
        let status = self.two_factor_status(&scope).await?;
        let mut challenge = SignInChallenge {
            authenticator: status.enabled,
            phone: status
                .sms
                .then(|| status.phone.as_deref().map(bilibi_sms::mask))
                .flatten(),
            resend_in: 0,
            sent: false,
            notice: String::new(),
        };
        if status.sms {
            let live = {
                let client = self.db.get().await.map_err(Error::from)?;
                sms::live_check(&**client, scope.tenant(), scope.user()).await?
            };
            if auto_send && !status.enabled && !live {
                match self.send_sms_check(&scope).await {
                    Ok(_) => challenge.sent = true,
                    Err(TwoFactorError::Other(error)) => return Err(error.into()),
                    Err(refusal) => challenge.notice = refusal.to_string(),
                }
            }
            challenge.resend_in = self.sms_wait(&scope).await?;
        }
        Ok(challenge)
    }

    /// "Send code" / "Resend code" on the code page.
    pub async fn sign_in_send_sms(&self, token: &str) -> Outcome<SmsSent> {
        let scope = self.owing(token, SecondFactor::Code).await?;
        self.send_sms_check(&scope).await
    }

    /// What setup at sign-in may offer.
    pub async fn sign_in_setup_options(&self, token: &str) -> Outcome<SignInSetupOptions> {
        let scope = self.owing(token, SecondFactor::Setup).await?;
        let status = self.two_factor_status(&scope).await?;
        Ok(SignInSetupOptions {
            phone: status
                .phone
                .as_deref()
                .filter(|_| self.sms.available())
                .map(bilibi_sms::mask),
        })
    }

    /// Setup at sign-in with SMS, step one: the password again, then a code
    /// to the administrator's number on the account (never a number typed
    /// here: an account without one asks an administrator).
    pub async fn begin_sign_in_sms(&self, token: &str, password: &str) -> Outcome<SmsSent> {
        let scope = self.owing(token, SecondFactor::Setup).await?;
        self.begin_sms(&scope, password, "").await
    }

    /// Setup at sign-in with SMS, step two: the code proves the number, SMS
    /// is on and the session acts, in one transaction.
    pub async fn confirm_sign_in_sms(&self, token: &str, code: &str) -> Outcome<()> {
        let scope = self.owing(token, SecondFactor::Setup).await?;
        self.confirm_sms_for(&scope, code, true).await
    }

    /// Seconds before a waiting session's account may be sent another text.
    pub async fn sign_in_sms_wait(&self, token: &str) -> Result<i64> {
        let client = self.db.get().await?;
        let (scope, _) = self.sessions.pending(&**client, token).await?;
        drop(client);
        self.sms_wait(&scope).await
    }

    /// Another code during SMS setup at sign-in.
    pub async fn resend_sign_in_sms(&self, token: &str) -> Outcome<SmsSent> {
        let scope = self.owing(token, SecondFactor::Setup).await?;
        self.resend_sms_verify(&scope).await
    }

    /// The holder starts again: the waiting session ends.
    pub async fn abandon_sign_in(&self, token: &str) -> Result<()> {
        let client = self.db.get().await?;
        self.sessions.abandon(&**client, token).await
    }

    /// The waiting session's scope, for finishing sign-in only, when it owes
    /// what the caller is about to take.
    async fn owing(&self, token: &str, owed: SecondFactor) -> Outcome<Scope> {
        let client = self.db.get().await.map_err(Error::from)?;
        let (scope, current) = self.sessions.pending(&**client, token).await?;
        if current != owed {
            return Err(Error::Unauthorized.into());
        }
        Ok(scope)
    }
}
