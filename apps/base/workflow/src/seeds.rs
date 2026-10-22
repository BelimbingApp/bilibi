//! The production flow seed. An owner contributes one versioned `Seed` per
//! flow, naming this service and its flow code as the operation, so the host's
//! explicit `seed` command inserts it once (insert-only, like
//! `Engine::initialize`) and a flow added later gets its own version. Boot
//! never calls it.
use crate::{definitions::Registry, engine::insert_flow};
use bilibi_database::{
    seeds::{SeedExecutor, SeedFuture},
    Error,
};
use std::sync::Arc;
use tokio_postgres::Transaction;

/// The service a flow seed names: `SeedAction::Service { service: FLOW_SEEDS, operation: "<flow code>" }`.
pub const FLOW_SEEDS: &str = "base/workflow.flow-seeds";

pub(crate) struct FlowSeeds {
    pub(crate) registry: Arc<Registry>,
}
impl SeedExecutor for FlowSeeds {
    fn execute<'a>(&'a self, tx: &'a Transaction<'_>, operation: &'a str) -> SeedFuture<'a> {
        Box::pin(async move {
            let &(owner, flow) = self.registry.flows.get(operation).ok_or(Error::Invalid)?;
            insert_flow(tx, owner, &flow).await.map(|_| ())
        })
    }
}
