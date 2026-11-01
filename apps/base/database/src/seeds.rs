//! Explicit production seeds and development sample seeds; never invoked by
//! app boot.
//! Ledger insertion and owner data commit atomically. Failure/drop leaves both
//! absent, so retry is safe without a persistent `running` status to recover.
use crate::{Db, Error, Result};
use bilibi_module_registry::{
    contributions::{Seed, SeedAction, SeedMode, SEEDS},
    Catalog, Entry, Module, Services,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use tokio_postgres::Transaction;

pub type SeedFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
/// Service seeds must use this transaction; no pool checkout, external IO or
/// commit is permitted. Register a SeedService at the contributed service ID.
pub trait SeedExecutor: Send + Sync {
    fn execute<'a>(&'a self, tx: &'a Transaction<'_>, operation: &'a str) -> SeedFuture<'a>;
}
pub struct SeedService(pub Arc<dyn SeedExecutor>);
#[derive(Debug)]
pub struct SeedRun {
    pub version: &'static str,
    pub applied: bool,
}

type Ordered<'c> = Vec<(&'c Entry, &'c Seed, Option<Arc<SeedService>>)>;

/// One mode's seeds in module dependency order, then contribution id.
fn ordered<'c>(
    modules: &[&'static Module],
    catalog: &'c Catalog,
    services: &Services,
    mode: SeedMode,
) -> Result<Ordered<'c>> {
    let modules = bilibi_module_registry::order(modules.to_vec())?;
    let positions: BTreeMap<_, _> = modules.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
    let mut seeds = Vec::new();
    for entry in catalog.entries::<Seed>(SEEDS)? {
        let seed = entry.get::<Seed>()?;
        if seed.mode != mode {
            continue;
        }
        let executor = match seed.action {
            SeedAction::Service { service, .. } => Some(services.get::<SeedService>(service)?),
            SeedAction::Sql(_) => None,
        };
        seeds.push((entry, seed, executor));
    }
    seeds.sort_by_key(|(entry, _, _)| (positions[entry.provenance.module], entry.id));
    Ok(seeds)
}

async fn execute(
    tx: &Transaction<'_>,
    seed: &Seed,
    executor: Option<&Arc<SeedService>>,
) -> Result<()> {
    match seed.action {
        SeedAction::Sql(sql) => tx.batch_execute(sql).await?,
        SeedAction::Service { operation, .. } => {
            executor
                .ok_or(Error::Internal)?
                .0
                .execute(tx, operation)
                .await?
        }
    }
    Ok(())
}

/// Development sample data, on the
/// caller's transaction, in dependency order. These seeds keep no ledger:
/// each must be idempotent and find its own subject, and each runs only for
/// the development seed, after the platform operator is bootstrapped.
pub async fn run_development(
    tx: &Transaction<'_>,
    modules: &[&'static Module],
    catalog: &Catalog,
    services: &Services,
) -> Result<usize> {
    let seeds = ordered(modules, catalog, services, SeedMode::Development)?;
    for (_, seed, executor) in &seeds {
        execute(tx, seed, executor.as_ref()).await?;
    }
    Ok(seeds.len())
}

pub async fn run(
    db: &Db,
    modules: &[&'static Module],
    catalog: &Catalog,
    services: &Services,
) -> Result<Vec<SeedRun>> {
    let seeds = ordered(modules, catalog, services, SeedMode::Production)?;
    let mut client = db.get().await?;
    let tx = client.transaction().await?;
    // Shares the migration lock: no concurrent seed or migration can interleave.
    tx.execute("SELECT pg_advisory_xact_lock(841317)", &[])
        .await?;
    let mut runs = Vec::new();
    for (entry, seed, executor) in seeds {
        let definition: Value = match seed.action {
            SeedAction::Sql(sql) => json!({"sql":sql}),
            SeedAction::Service { service, operation } => {
                json!({"service":service,"operation":operation})
            }
        };
        if let Some(row) = tx.query_opt("SELECT module_id,contribution_id,definition FROM bilibi_production_seeds WHERE version=$1", &[&seed.version]).await? {
            if row.get::<_, String>(0) != entry.provenance.module
                || row.get::<_, String>(1) != entry.id || row.get::<_, Value>(2) != definition {
                return Err(Error::SeedDefinition(seed.version.into()));
            }
            runs.push(SeedRun { version: seed.version, applied: false });
            continue;
        }
        execute(&tx, seed, executor.as_ref()).await?;
        tx.execute("INSERT INTO bilibi_production_seeds(version,module_id,contribution_id,definition) VALUES($1,$2,$3,$4)", &[&seed.version,&entry.provenance.module,&entry.id,&definition]).await?;
        runs.push(SeedRun {
            version: seed.version,
            applied: true,
        });
    }
    tx.commit().await?;
    Ok(runs)
}
