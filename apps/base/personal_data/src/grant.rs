//! Sealed grants (ADR 05). Only the module that runs approved requests and
//! sweeps holds the [`Issuer`]; it claims it once at boot. Media, Artifacts and
//! Audit accept a grant as the authority for their erasure and export paths,
//! so no capability, session flag or request number can stand in for an
//! approved request.
use bilibi_database::{Error, Result};
use bilibi_party_ids::PersonId;
use bilibi_tenancy::Scope;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a grant was issued for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cause {
    /// An approved, running data-subject request.
    Request(i64),
    /// A retention sweep run.
    Sweep(i64),
}

/// The authority to erase: an approved erasure request for one person, or a
/// retention sweep in one tenant.
#[derive(Debug)]
pub struct Erasure {
    tenant: i64,
    cause: Cause,
    person: Option<PersonId>,
}
impl Erasure {
    pub fn tenant(&self) -> i64 {
        self.tenant
    }
    pub fn cause(&self) -> Cause {
        self.cause
    }
    /// The request's person; `None` for a sweep.
    pub fn person(&self) -> Option<PersonId> {
        self.person
    }
    /// `Forbidden` unless `scope` is in the grant's tenant.
    pub fn check(&self, scope: &Scope) -> Result<()> {
        if scope.tenant() == self.tenant {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
}

/// The authority to read a person's records and files for an approved access
/// or portability request.
#[derive(Debug)]
pub struct Disclosure {
    tenant: i64,
    request: i64,
    person: PersonId,
}
impl Disclosure {
    pub fn tenant(&self) -> i64 {
        self.tenant
    }
    pub fn request(&self) -> i64 {
        self.request
    }
    pub fn person(&self) -> PersonId {
        self.person
    }
    /// `Forbidden` unless `scope` is in the grant's tenant.
    pub fn check(&self, scope: &Scope) -> Result<()> {
        if scope.tenant() == self.tenant {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
}

/// The authority to read one masked field in full: issued only after the
/// reader's `privacy.field.reveal`, their reason and the tenant's Step-up
/// check, and only once the reveal is recorded. Grid's reveal read takes it.
#[derive(Debug)]
pub struct Reveal {
    tenant: i64,
    user: i64,
    table: String,
    field: String,
}
impl Reveal {
    /// `Forbidden` unless `scope` is the reader the grant was issued to, and
    /// `table.field` is the field it names.
    pub fn check(&self, scope: &Scope, table: &str, field: &str) -> Result<()> {
        if scope.tenant() == self.tenant
            && scope.user() == self.user
            && table == self.table
            && field == self.field
        {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
}

static CLAIMED: AtomicBool = AtomicBool::new(false);

/// Issues grants. There is at most one per process.
pub struct Issuer(());
impl Issuer {
    /// The issuer, the first time it is asked for; `None` after that, so a
    /// second claimant (or a module claiming before `base/privacy`) makes boot
    /// fail rather than share it.
    pub fn claim() -> Option<Self> {
        (!CLAIMED.swap(true, Ordering::SeqCst)).then_some(Self(()))
    }
    pub fn erasure(&self, tenant: i64, cause: Cause, person: Option<PersonId>) -> Erasure {
        Erasure {
            tenant,
            cause,
            person,
        }
    }
    pub fn reveal(&self, scope: &Scope, table: &str, field: &str) -> Reveal {
        Reveal {
            tenant: scope.tenant(),
            user: scope.user(),
            table: table.to_owned(),
            field: field.to_owned(),
        }
    }
    pub fn disclosure(&self, tenant: i64, request: i64, person: PersonId) -> Disclosure {
        Disclosure {
            tenant,
            request,
            person,
        }
    }
}
