//! Legal holds as erasure and sweeps see them (ADR 05). The module that keeps
//! holds binds one [`HoldSource`]; [`Holds::acquire`] opens a session that
//! holds the tenant's hold guard until it is released, so a hold placed
//! meanwhile waits for the erasure or sweep to commit instead of racing it.
//! Reads ask only about the records in hand. With no source bound nothing can
//! be held.
use crate::store::HandlerFuture;
use bilibi_database::Result;
use bilibi_module_registry::{RegistryError, Result as RegistryResult};
use bilibi_party_ids::PersonId;
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

/// Opens hold sessions; bound once by the module that keeps holds.
pub trait HoldSource: Send + Sync {
    fn open<'a>(&'a self, tenant: i64) -> HandlerFuture<'a, Box<dyn HoldSession>>;
}
/// One open guard over a tenant's holds.
pub trait HoldSession: Send + Sync {
    /// Active person holds among `persons`: person ID → hold ID.
    fn persons<'a>(&'a self, persons: &'a [i64]) -> HandlerFuture<'a, BTreeMap<i64, i64>>;
    /// Active record holds on `ids` of `kind`: record ID → hold ID.
    fn records<'a>(
        &'a self,
        kind: &'a str,
        ids: &'a [i64],
    ) -> HandlerFuture<'a, BTreeMap<i64, i64>>;
    /// Ends the guard. A session dropped without it closes its connection.
    fn release(self: Box<Self>) -> HandlerFuture<'static, ()>;
}

static SOURCE: OnceLock<Arc<dyn HoldSource>> = OnceLock::new();
pub fn bind_holds(source: Arc<dyn HoldSource>) -> RegistryResult<()> {
    SOURCE
        .set(source)
        .map_err(|_| RegistryError::Service("duplicate hold source".into()))
}

/// The holds one erasure or sweep must respect, guarded until [`Holds::release`].
pub struct Holds {
    session: Option<Box<dyn HoldSession>>,
    person: Option<PersonId>,
}
impl Holds {
    /// Opens the guard for `tenant`. `person` is the request's subject, whose
    /// person hold keeps every record; `None` for a sweep.
    pub async fn acquire(tenant: i64, person: Option<PersonId>) -> Result<Self> {
        let session = match SOURCE.get() {
            Some(source) => Some(source.open(tenant).await?),
            None => None,
        };
        Ok(Self { session, person })
    }
    /// The subject's person hold.
    pub async fn person_hold(&self) -> Result<Option<i64>> {
        match (&self.session, self.person) {
            (Some(session), Some(person)) => Ok(session
                .persons(&[person.get()])
                .await?
                .remove(&person.get())),
            _ => Ok(None),
        }
    }
    /// Which of `ids` of `kind` are held, by a record hold or by the subject's
    /// person hold: record ID → hold ID.
    pub async fn records(&self, kind: &str, ids: &[i64]) -> Result<BTreeMap<i64, i64>> {
        let Some(session) = &self.session else {
            return Ok(BTreeMap::new());
        };
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        if let Some(hold) = self.person_hold().await? {
            return Ok(ids.iter().map(|id| (*id, hold)).collect());
        }
        session.records(kind, ids).await
    }
    /// Whether one record is held: its hold ID.
    pub async fn record(&self, kind: &str, id: i64) -> Result<Option<i64>> {
        Ok(self.records(kind, &[id]).await?.remove(&id))
    }
    /// Which of `persons` are held: person ID → hold ID. For a sweep that
    /// keeps everything about a held person.
    pub async fn persons(&self, persons: &[i64]) -> Result<BTreeMap<i64, i64>> {
        match &self.session {
            Some(session) if !persons.is_empty() => session.persons(persons).await,
            _ => Ok(BTreeMap::new()),
        }
    }
    /// Releases the guard; call it after the work it guards has committed.
    pub async fn release(mut self) -> Result<()> {
        match self.session.take() {
            Some(session) => session.release().await,
            None => Ok(()),
        }
    }
}
