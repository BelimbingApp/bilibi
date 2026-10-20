//! The authenticator as Base Step-up's provider: Step-up asks, this answers
//! about the scope's own account holder. It learns only an operation key and
//! target as data to record, never what the operation is.
use super::{Spend, TwoFactorError};
use crate::UserService;
use bilibi_step_up::{Provider, Purpose, SessionProof, Spent, WindowState};
use bilibi_tenancy::{ProofFuture, Scope};
use std::sync::Arc;

pub struct Authenticator(pub Arc<UserService>);

impl Provider for Authenticator {
    fn enrolled<'a>(&'a self, scope: &'a Scope) -> ProofFuture<'a, bool> {
        Box::pin(async move { Ok(self.0.two_factor_status(scope).await?.enabled) })
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
    /// An agent's human owes a code: a holder with a verified phone is sent
    /// one (within the resend limits); the app needs nothing sent.
    fn challenge<'a>(&'a self, scope: &'a Scope, _purpose: Purpose<'a>) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            if !self.0.two_factor_status(scope).await?.sms {
                return Ok(());
            }
            match self.0.send_check_code(scope).await {
                Err(TwoFactorError::Other(error)) => Err(error),
                _ => Ok(()),
            }
        })
    }
}
