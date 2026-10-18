//! The page-write check of ADR 01: while a live page handles an event, a
//! business write goes through an operation contract. The host marks the
//! event it dispatches (`during`), Agent API marks the transaction a contract
//! opens (`contract`), and `MutationBoundary::begin` asks `check`. A page
//! event that is not yet migrated carries the permit its page declared, so
//! each gap is a visible, listed allowance rather than a silent facade call.
//! Writes outside a page event (agents, jobs, seeds) are not affected.
use crate::{Error, Result};
use std::future::Future;

tokio::task_local! {
    static EVENT: Event;
    static CONTRACT: &'static str;
}

/// One page event the host is dispatching.
pub struct Event {
    /// The page or panel service that owns the event.
    pub service: &'static str,
    pub name: String,
    pub permit: Permit,
}

/// What the page declared for this event.
pub enum Permit {
    /// Listed as not yet migrated: its facade writes are allowed.
    Pending,
    /// Only through one of these contracts. Empty: the event writes nothing.
    Contracts(Vec<&'static str>),
}

/// Runs a page's handling of one event under its permit.
pub async fn during<F: Future>(event: Event, future: F) -> F::Output {
    EVENT.scope(event, future).await
}

/// Runs one contract's transaction (Agent API's page caller).
pub async fn contract<F: Future>(key: &'static str, future: F) -> F::Output {
    CONTRACT.scope(key, future).await
}

/// Refuses a business transaction a page event opens without its contract.
pub(crate) fn check(action: &str) -> Result<()> {
    EVENT
        .try_with(|event| match &event.permit {
            Permit::Pending => Ok(()),
            Permit::Contracts(keys) => {
                if CONTRACT.try_with(|key| keys.contains(key)).unwrap_or(false) {
                    return Ok(());
                }
                eprintln!(
                    "error: page write without a contract: event {} on {} began {action}; call its operation contract, or list the event as pending",
                    event.name, event.service
                );
                Err(Error::Internal)
            }
        })
        .unwrap_or(Ok(()))
}
