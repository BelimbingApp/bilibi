//! The authenticator as Base Step-up's provider: Step-up asks, this answers
//! about the scope's own account holder. It learns only an operation key and
//! target as data to record, never what the operation is.
use super::{Spend, TwoFactorError};
use crate::UserService;
use bilibi_database::Error;
use bilibi_step_up::{Provider, Purpose, SessionProof, Spent, WindowState};
use bilibi_tenancy::{ProofFuture, Scope};
use std::sync::Arc;

pub struct Authenticator(pub Arc<UserService>);

impl Provider for Authenticator {
    fn enrolled<'a>(&'a self, scope: &'a Scope) -> ProofFuture<'a, bool> {
        Box::pin(async move { Ok(self.0.two_factor_status(scope).await?.any()) })
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
    fn prompt<'a>(&'a self, scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String> {
        Box::pin(async move { self.0.step_up_destination(scope, back).await })
    }
    /// A holder whose only second factor is SMS gets a text, so they have a
    /// code to give their agent. The sending bounds hold; a refusal to send
    /// (too soon, too many, no driver) leaves the refusal as it was.
    fn challenge<'a>(&'a self, scope: &'a Scope) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let status = self.0.two_factor_status(scope).await?;
            if status.sms && !status.enabled {
                if let Err(TwoFactorError::Other(error)) = self.0.send_sms_check(scope).await {
                    return Err(error);
                }
            }
            Ok::<(), Error>(())
        })
    }
}
