//! The controls in front of every database-tables view: owner proof on each
//! request, behind the routes' Base Step-up operation
//! (`base.system.database_tables.view`, which the host re-admits on every
//! live event), and one Audit action per distinct view. No audit row, no
//! data: a view whose record cannot be written is refused.
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use serde_json::Value;

pub(super) const VIEWED: &str = "system.database_tables.viewed";

#[derive(Clone)]
pub(super) struct Gate {
    db: Db,
}
impl Gate {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The installation owner, proved afresh; anyone else is `Forbidden`.
    pub async fn owner(&self, scope: &Scope) -> Result<()> {
        let client = self.db.get().await?;
        bilibi_tenancy::prove_owner(&**client, scope).await?;
        Ok(())
    }

    /// Records one view: what was opened and how, never a row value.
    pub async fn viewed(&self, scope: &Scope, view: Value) -> Result<()> {
        let client = self.db.get().await?;
        record(&**client, scope, VIEWED, view).await
    }
}

async fn record(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    event: &str,
    payload: Value,
) -> Result<()> {
    record_action(
        client,
        &Attribution::from_scope(scope),
        None,
        event,
        payload,
        true,
    )
    .await
    .map(|_| ())
    .map_err(|_| Error::Internal)
}
