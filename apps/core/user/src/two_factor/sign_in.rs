//! Sign-in as one more step-up operation (ADR 03). A correct password writes
//! the session pending its second factor (`identity::login`); this file asks
//! Base Step-up whether the tenant's `core.user.sign_in` policy applies and
//! either admits the session, or keeps it pending for the code page
//! (`/login/verify`, served by the host), or deletes it when the account has
//! nothing to offer. Step-up decides; this module never reads the policy.
use super::{sms::SmsPurpose, Outcome, Spend, TwoFactorError};
use crate::UserService;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;

/// Off by default: a tenant turns it on at `/authz/step-up`.
pub const SIGN_IN: &str = "core.user.sign_in";
/// An administrator setting or changing someone's second-factor phone.
pub const PHONE_SET: &str = "core.user.account.phone.set";

bilibi_step_up::declare!(
    "core/user",
    bilibi_step_up::Operation::new(SIGN_IN, "Sign in").off_by_default()
);
bilibi_step_up::declare!(
    "core/user",
    bilibi_step_up::Operation::new(PHONE_SET, "Set or change a user's 2FA phone number")
);

/// Where a fresh sign-in stands after the password.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignInStep {
    /// No check applies: the session is admitted.
    Admitted,
    /// A code is owed; the session stays pending.
    Owed,
    /// A code is owed but the account has no method: the session is gone.
    Unavailable,
}

/// What the sign-in code page shows.
pub struct SignInChallenge {
    /// The masked phone codes go to, when SMS can answer the check.
    pub phone: Option<String>,
    /// The account has a confirmed authenticator.
    pub authenticator: bool,
    /// Seconds before another code may be sent.
    pub resend_in: i64,
    /// An SMS code is waiting to be typed.
    pub sms_waiting: bool,
    /// Why no SMS code is waiting and none can be sent yet (the throttle).
    pub sms_held: Option<String>,
}

impl UserService {
    async fn pending_scope(&self, token: &str) -> Result<Scope> {
        let client = self.db.get().await?;
        self.sessions.pending(&**client, token).await
    }

    /// The tenant's sign-in check on a session the password just created.
    /// When the account's only method is SMS, the first code is sent now.
    pub(crate) async fn begin_sign_in(&self, token: &str) -> Result<SignInStep> {
        let scope = self.pending_scope(token).await?;
        match self.step_up.require(&scope, SIGN_IN, "session").await {
            Ok(()) => {
                let client = self.db.get().await?;
                self.sessions.admit(&**client, &scope, false).await?;
                Ok(SignInStep::Admitted)
            }
            Err(Error::StepUpRequired {
                reason: "not_enrolled" | "unavailable" | "impersonating",
                ..
            }) => {
                let client = self.db.get().await?;
                self.sessions.discard_pending(&**client, token).await?;
                Ok(SignInStep::Unavailable)
            }
            Err(Error::StepUpRequired { .. }) => {
                let status = self.two_factor_status(&scope).await?;
                if status.sms_ready() && !status.enabled {
                    // A throttled, locked or failed send leaves the code page
                    // to say so and offer Resend.
                    if let Err(TwoFactorError::Other(error)) =
                        self.send_sms_code(&scope, SmsPurpose::Check).await
                    {
                        return Err(error);
                    }
                }
                Ok(SignInStep::Owed)
            }
            Err(error) => Err(error),
        }
    }

    /// The code page's facts for a pending session (`Unauthorized` for any
    /// other token).
    pub async fn sign_in_challenge(&self, token: &str) -> Result<SignInChallenge> {
        let scope = self.pending_scope(token).await?;
        let status = self.two_factor_status(&scope).await?;
        let (sms_waiting, sms_held) = if status.sms_ready() {
            self.sms_waiting(&scope).await?
        } else {
            (false, None)
        };
        Ok(SignInChallenge {
            phone: status.phone.clone().filter(|_| status.sms_ready()),
            authenticator: status.enabled,
            resend_in: self.sms_resend_in(&scope).await?,
            sms_waiting,
            sms_held,
        })
    }

    /// A code for the pending session: an authenticator code or the latest
    /// SMS code. A right one admits the session and opens its
    /// step-up window (`user.two_factor.verified`, for `core.user.sign_in`).
    pub async fn complete_sign_in(&self, token: &str, code: &str) -> Outcome<()> {
        let scope = self.pending_scope(token).await?;
        let proof = self.prove(&scope, code, true).await?;
        self.consume(&scope, proof, Spend::SignIn).await
    }

    /// Another SMS code for the pending session, under the resend throttle.
    pub async fn resend_sign_in_code(&self, token: &str) -> Outcome<String> {
        let scope = self.pending_scope(token).await?;
        Ok(self.send_sms_code(&scope, SmsPurpose::Check).await?.phone)
    }

    /// "Back to login": the pending session ends.
    pub async fn cancel_sign_in(&self, token: &str) -> Result<()> {
        let client = self.db.get().await?;
        self.sessions.discard_pending(&**client, token).await
    }
}
