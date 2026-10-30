//! Legal holds as erasure and sweeps see them (ADR 05). The module that keeps
//! holds binds one [`HoldSource`]; [`Holds::acquire`] opens a session, bound
//! to the erasure or sweep grant it was acquired for, that holds the tenant's
//! hold guard until it is released, so a hold placed
//! meanwhile waits for the erasure or sweep to commit instead of racing it.
//! Reads ask only about the records in hand. With no source bound nothing can
//! be held.
use crate::{store::HandlerFuture, Cause, Erasure};
use bilibi_database::{Error, Result};
use bilibi_module_registry::{RegistryError, Result as RegistryResult};
use bilibi_party_ids::PersonId;
use bilibi_tenancy::Scope;
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

/// The holds one erasure or sweep must respect, guarded until
/// [`Holds::release`]. A session is bound to the grant it was acquired for:
/// its tenant and person come from that sealed [`Erasure`], and every read
/// presents the grant again, so a session acquired for another tenant,
/// person or run fails closed (`Forbidden`) instead of answering for the
/// wrong holds.
pub struct Holds {
    session: Option<Box<dyn HoldSession>>,
    tenant: i64,
    cause: Cause,
    person: Option<PersonId>,
}
impl Holds {
    /// Opens the guard for `grant`, in `scope`'s tenant: the request's
    /// subject, whose person hold keeps every record, or no person for a sweep.
    pub async fn acquire(scope: &Scope, grant: &Erasure) -> Result<Self> {
        grant.check(scope)?;
        Ok(Self {
            session: open(grant.tenant()).await?,
            tenant: grant.tenant(),
            cause: grant.cause(),
            person: grant.person(),
        })
    }
    /// `Forbidden` unless `grant` is the one this session was acquired for.
    fn bound(&self, grant: &Erasure) -> Result<()> {
        if grant.tenant() == self.tenant
            && grant.cause() == self.cause
            && grant.person() == self.person
        {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
    /// The subject's person hold.
    pub async fn person_hold(&self, grant: &Erasure) -> Result<Option<i64>> {
        self.bound(grant)?;
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
    pub async fn records(
        &self,
        grant: &Erasure,
        kind: &str,
        ids: &[i64],
    ) -> Result<BTreeMap<i64, i64>> {
        self.bound(grant)?;
        let Some(session) = &self.session else {
            return Ok(BTreeMap::new());
        };
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        if let Some(hold) = self.person_hold(grant).await? {
            return Ok(ids.iter().map(|id| (*id, hold)).collect());
        }
        session.records(kind, ids).await
    }
    /// Whether one record is held: its hold ID.
    pub async fn record(&self, grant: &Erasure, kind: &str, id: i64) -> Result<Option<i64>> {
        Ok(self.records(grant, kind, &[id]).await?.remove(&id))
    }
    /// Which of `persons` are held: person ID → hold ID. For a sweep that
    /// keeps everything about a held person.
    pub async fn persons(&self, grant: &Erasure, persons: &[i64]) -> Result<BTreeMap<i64, i64>> {
        self.bound(grant)?;
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

/// Record holds for a module's own retention of its records, outside any
/// erasure or sweep grant (Integration's payload pruning). It answers only
/// whether records are held, and is a type of its own, so it can never be
/// passed where an erasure expects [`Holds`].
pub struct RecordHolds {
    session: Option<Box<dyn HoldSession>>,
}
impl RecordHolds {
    /// Opens the guard over `tenant`'s holds.
    pub async fn acquire(tenant: i64) -> Result<Self> {
        Ok(Self {
            session: open(tenant).await?,
        })
    }
    /// Which of `ids` of `kind` a record hold names: record ID → hold ID.
    pub async fn records(&self, kind: &str, ids: &[i64]) -> Result<BTreeMap<i64, i64>> {
        match &self.session {
            Some(session) if !ids.is_empty() => session.records(kind, ids).await,
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

async fn open(tenant: i64) -> Result<Option<Box<dyn HoldSession>>> {
    match SOURCE.get() {
        Some(source) => Ok(Some(source.open(tenant).await?)),
        None => Ok(None),
    }
}
