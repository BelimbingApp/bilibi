//! Two-factor as Base Step-up's provider: Step-up asks, this answers about
//! the scope's own account holder, by whichever method the account uses (an
//! authenticator app, else a text message). It learns only an operation key
//! and target as data to record, never what the operation is.
use super::{Method, Spend, TwoFactorError};
use crate::UserService;
use bilibi_step_up::{Challenge, Provider, Purpose, SessionProof, Spent, WindowState};
use bilibi_tenancy::{ProofFuture, Scope};
use std::sync::Arc;

pub struct Authenticator(pub Arc<UserService>);

impl Provider for Authenticator {
    fn enrolled<'a>(&'a self, scope: &'a Scope) -> ProofFuture<'a, bool> {
        Box::pin(async move { Ok(self.0.two_factor_status(scope).await?.method() != Method::None) })
    }
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
                SessionProof::Fresh { seconds } => {
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
            let outcome = match self.0.prove(scope, code, true).await {
                Ok(proof) => self.0.consume(scope, proof, spend).await,
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
    fn prompt<'a>(&'a self, scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String> {
        Box::pin(async move { self.0.step_up_destination(scope, back).await })
    }
    /// A text-message account gets a code now, unless one it can still
    /// enter is already out; an authenticator account has nothing to send.
    fn challenge<'a>(
        &'a self,
        scope: &'a Scope,
        _purpose: Purpose<'a>,
    ) -> ProofFuture<'a, Challenge> {
        Box::pin(async move {
            if self.0.two_factor_status(scope).await?.method() != Method::TextMessage {
                return Ok(Challenge::None);
            }
            Ok(match self.0.send_two_factor_code(scope, false).await {
                // Too soon after the last one: that one is still on its way.
                Ok(_) | Err(TwoFactorError::ResendTooSoon { .. }) => Challenge::Sent,
                Err(TwoFactorError::TooManyCodes { .. }) => Challenge::Throttled,
                Err(TwoFactorError::Locked { .. }) => Challenge::Locked,
                Err(TwoFactorError::Other(error)) => return Err(error),
                Err(_) => Challenge::None,
            })
        })
    }
}
