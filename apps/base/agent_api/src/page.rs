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

/// A shared page explicitly accepts contributed owner operations. The page
/// owner declares the family; each operation owner binds only its own write.
pub const PAGE_FAMILIES: &str = "base/agent_api.page_families";
pub const PAGE_BINDINGS: &str = "base/agent_api.page_bindings";
pub struct PageFamily {
    pub service: &'static str,
    pub event_prefix: &'static str,
}
pub struct PageBinding {
    pub service: &'static str,
    pub event: &'static str,
    pub contract: &'static str,
}
fn validate_families(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        let family = entry.get::<PageFamily>()?;
        if !family
            .service
            .starts_with(&format!("{}.", entry.provenance.module))
            || family.event_prefix.is_empty()
        {
            return Err(invalid(
                entry,
                "has a foreign service or empty event prefix",
            ));
        }
    }
    Ok(())
}
fn validate_bindings(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        let binding = entry.get::<PageBinding>()?;
        if !binding
            .contract
            .starts_with(&format!("{}.", entry.provenance.module.replace('/', ".")))
            || binding.event.is_empty()
        {
            return Err(invalid(entry, "binds a foreign contract or empty event"));
        }
    }
    Ok(())
}
inventory::submit! {bilibi_module_registry::Consumer::new::<PageFamily>(PAGE_FAMILIES,"base/agent_api",validate_families)}
inventory::submit! {bilibi_module_registry::Consumer::new::<PageBinding>(PAGE_BINDINGS,"base/agent_api",validate_bindings)}

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
    extensions: BTreeMap<(&'static str, &'static str), &'static str>,
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
        let families = catalog.entries::<PageFamily>(PAGE_FAMILIES)?;
        let mut extensions = BTreeMap::new();
        for entry in catalog.entries::<PageBinding>(PAGE_BINDINGS)? {
            let binding = entry.get::<PageBinding>()?;
            let accepted = families.iter().any(|family| {
                family.get::<PageFamily>().is_ok_and(|f| {
                    f.service == binding.service && binding.event.starts_with(f.event_prefix)
                })
            });
            let owned = registry.operations.get(binding.contract).is_some_and(|op| {
                op.provenance.module == entry.provenance.module
                    && matches!(op.definition.handler, Handler::SafeWrite(_))
            });
            if !accepted
                || !owned
                || !pages.contains(binding.service)
                || !writes.contains_key(binding.service)
                || extensions
                    .insert((binding.service, binding.event), binding.contract)
                    .is_some()
                || writes
                    .get(binding.service)
                    .is_some_and(|w| w.contracts.iter().any(|(event, _)| *event == binding.event))
            {
                return Err(invalid(
                    entry,
                    "has no opted-in page, duplicates an event, or names no owner write",
                ));
            }
        }
        Ok(Self {
            extensions,
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
        if let Some(key) = self.extensions.get(&(service, event)) {
            return Permit::Contracts(vec![*key]);
        }
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
        self.registry.read(&self.db, scope, key, input).await
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

    /// The review half of a confirmation: the owner's dry-run preview. The
    /// page keeps the input server-side and shows the review.
    pub async fn preview(&self, call: &Call<'_>, input: Value) -> Result<Value> {
        let op = self.declared(call, input)?;
        self.plans
            .preview(&self.session(call), std::slice::from_ref(&op))
            .await
    }

    /// Rechecks and commits a reviewed confirmation with the same input.
    pub async fn commit(&self, call: &Call<'_>, input: Value) -> Result<Value> {
        let op = self.declared(call, input)?;
        let session = self.session(call);
        page_write::contract(
            call.contract,
            self.plans
                .commit(call.scope, &session, std::slice::from_ref(&op)),
        )
        .await
    }

    /// The page must have declared this event as this contract.
    fn declared(&self, call: &Call<'_>, input: Value) -> Result<PlannedOperation> {
        let declared = self
            .extensions
            .get(&(call.service, call.event))
            .is_some_and(|key| *key == call.contract)
            || self
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
