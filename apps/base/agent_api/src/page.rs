//! Pages as callers of operation contracts (ADR 01). A page's write event
//! invokes the contract the agent doors call, through the same validation,
//! capability check, owner preview, audited transaction, version fence and
//! step-up; the page keeps its own rendering and live patching.
//!
//! Each page service declares its write events once (Module Registry's
//! `PageWrites`, so Base modules below this one can declare too): the
//! contract each event calls, and the events still writing through a facade
//! as a listed, visible allowance (`Pending`). The host dispatches every
//! live event under the permit this declaration gives
//! (`bilibi_database::page_write`), so a business write the declaration does
//! not cover is refused.
use crate::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Handler, Registry, Result,
};
use bilibi_database::{
    page_write::{self, Permit},
    Db,
};
use bilibi_module_registry::{
    contributions::{Embed, PageWrites, Pending, Route, EMBEDS, PAGE_WRITES, ROUTES},
    Catalog, Entry, RegistryError,
};
use bilibi_session::Sessions;
use bilibi_tenancy::{ProofFuture, Scope};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio_postgres::Transaction;

pub const SERVICE: &str = "base/agent_api.pages";

fn invalid(entry: &Entry, reason: &str) -> RegistryError {
    RegistryError::Contribution(format!(
        "page writes {} at {}:{} {reason}",
        entry.id, entry.provenance.source, entry.provenance.line
    ))
}

/// The page caller: one facade for every page that invokes a contract.
pub struct PageContracts {
    db: Db,
    registry: Arc<Registry>,
    plans: Arc<Plans>,
    sessions: Arc<Sessions>,
    writes: BTreeMap<&'static str, &'static PageWrites>,
}

/// One page write: who asks (the live request's scope and session), from
/// which page event, and the contract the page declared for it.
pub struct Call<'a> {
    pub scope: &'a Scope,
    /// The live request's credential (`LiveRequest::credential`).
    pub credential: &'a str,
    pub service: &'static str,
    pub event: &'a str,
    pub contract: &'static str,
}

impl PageContracts {
    pub fn new(
        db: Db,
        registry: Arc<Registry>,
        plans: Arc<Plans>,
        sessions: Arc<Sessions>,
        catalog: &Catalog,
    ) -> bilibi_module_registry::Result<Self> {
        let pages: BTreeSet<&str> = catalog
            .entries::<Route>(ROUTES)?
            .iter()
            .map(|entry| entry.get::<Route>().map(|route| route.service))
            .chain(
                catalog
                    .entries::<Embed>(EMBEDS)?
                    .iter()
                    .map(|entry| entry.get::<Embed>().map(|embed| embed.service)),
            )
            .collect::<bilibi_module_registry::Result<_>>()?;
        let mut writes = BTreeMap::new();
        for entry in catalog.entries::<PageWrites>(PAGE_WRITES)? {
            let declared = entry.get::<PageWrites>()?;
            if !pages.contains(declared.service) {
                return Err(invalid(entry, "names no route or embed service"));
            }
            for (event, key) in declared.contracts {
                let write = registry
                    .operations
                    .get(key)
                    .is_some_and(|op| matches!(op.definition.handler, Handler::SafeWrite(_)));
                if !write {
                    return Err(invalid(
                        entry,
                        &format!("event {event} names no registered write contract {key}"),
                    ));
                }
            }
            writes.insert(declared.service, declared);
        }
        Ok(Self {
            db,
            registry,
            plans,
            sessions,
            writes,
        })
    }

    /// What the host lets a page event write. An undeclared service or
    /// event may write nothing.
    pub fn permit(&self, service: &str, event: &str) -> Permit {
        let Some(writes) = self.writes.get(service) else {
            return Permit::Contracts(Vec::new());
        };
        let contracts: Vec<_> = writes
            .contracts
            .iter()
            .filter(|(name, _)| *name == event)
            .map(|(_, key)| *key)
            .collect();
        if !contracts.is_empty() {
            return Permit::Contracts(contracts);
        }
        match writes.pending {
            Pending::Every { .. } => Permit::Pending,
            Pending::Events { events, .. } if events.contains(&event) => Permit::Pending,
            _ => Permit::Contracts(Vec::new()),
        }
    }

    /// A read contract, called directly: the same validation and logged
    /// capability decision as the agent doors.
    pub async fn read(&self, scope: &Scope, key: &str, input: &Value) -> Result<Value> {
        self.registry.call(&self.db, scope, key, input).await
    }

    /// A form the person has just reviewed on the page: the owner's preview
    /// and commit in one transaction (`Plans::submit`). Answers the result
    /// envelope, or the owner's refusal typed (`InvalidInput`, `Forbidden`,
    /// `Backend(Conflict)`, `Backend(StepUpRequired)` …).
    pub async fn submit(&self, call: &Call<'_>, input: Value) -> Result<Value> {
        let op = self.declared(call, input)?;
        let session = self.session(call);
        page_write::contract(call.contract, self.plans.submit(call.scope, &session, &op)).await
    }

    /// The review half of a confirmation: the owner's preview and a code
    /// bound to this browser session. The page keeps the code server-side
    /// and shows the review.
    pub async fn preview(&self, call: &Call<'_>, input: Value) -> Result<Value> {
        let op = self.declared(call, input)?;
        self.plans
            .preview(&self.session(call), std::slice::from_ref(&op))
            .await
    }

    /// Commits a reviewed confirmation with the same input and its code.
    pub async fn commit(&self, call: &Call<'_>, code: &str, input: Value) -> Result<Value> {
        let op = self.declared(call, input)?;
        let session = self.session(call);
        page_write::contract(
            call.contract,
            self.plans
                .commit(call.scope, &session, code, std::slice::from_ref(&op)),
        )
        .await
    }

    /// The page must have declared this event as this contract.
    fn declared(&self, call: &Call<'_>, input: Value) -> Result<PlannedOperation> {
        let declared = self
            .writes
            .get(call.service)
            .is_some_and(|writes| writes.contracts.contains(&(call.event, call.contract)));
        if !declared {
            eprintln!(
                "error: page {} event {} calls {} without declaring it in its page writes",
                call.service, call.event, call.contract
            );
            return Err(bilibi_database::Error::Internal.into());
        }
        Ok(PlannedOperation {
            key: call.contract.into(),
            input,
        })
    }
    fn session<'a>(&'a self, call: &Call<'a>) -> Session<'a> {
        Session {
            sessions: &self.sessions,
            token: call.credential,
        }
    }
}

/// The browser session as the plan's credential: proved and held on the
/// plan's own transaction, so a sign-out cannot race the write.
struct Session<'a> {
    sessions: &'a Sessions,
    token: &'a str,
}
impl PlanAuthority for Session<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            let scope = self.sessions.hold(tx, self.token).await?;
            Ok(PlanIdentity {
                scope,
                credential_binding: format!("browser-session:{}", self.token),
            })
        })
    }
}
