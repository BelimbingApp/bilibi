//! The personal-data registry (ADR 05): each module that keeps a person's data
//! contributes one `PersonalData` handler per store and answers for it. Base
//! never reads another module's tables; it asks the handler.
use bilibi_database::{MutationTransaction, Result};
use bilibi_module_registry::{Catalog, Entry, Module, RegistryError, Result as RegistryResult};
use bilibi_party_ids::PersonId;
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin};
use tokio_postgres::Client;

/// The catalog stores contribute to:
/// `Contribution::new(PERSONAL_DATA, "<module>.<store>", provenance, &PersonalData { .. })`.
pub const PERSONAL_DATA: &str = "base/privacy.personal_data";

pub type HandlerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Contribution payload: one store of personal data and its handler.
pub struct PersonalData {
    /// Shown on the request page and the completion certificate.
    pub label: &'static str,
    /// The audit record kinds this store writes (`Change::kind`), so the audit
    /// erasure path knows whose evidence to clear.
    pub record_kinds: &'static [&'static str],
    pub handler: &'static dyn Handler,
}

/// Who a request is about: a Core person, plus identifiers the requester gave.
/// Handlers match by person first; a match by identifier alone is reported,
/// never acted on, until the operator confirms it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Subject {
    pub person: PersonId,
    #[serde(default)]
    pub identifiers: Vec<Identifier>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Identifier {
    Email(String),
    Phone(String),
}

/// What a store holds about a subject, per record kind.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Located {
    pub kinds: Vec<KindCount>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KindCount {
    pub kind: String,
    /// Records linked to the person.
    pub by_person: i64,
    /// Records that match only an identifier: the operator must confirm them.
    pub by_identifier: i64,
}

/// A store's export: its records as JSON and the files they reference. Base
/// resolves the files; a handler never copies bytes.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Exported {
    pub records: Value,
    pub files: Vec<FileRef>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "store", content = "id", rename_all = "snake_case")]
pub enum FileRef {
    Media(String),
    Artifact(String),
}

/// What an erasure did.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Erased {
    pub actions: Vec<KindAction>,
    pub kept: Vec<Kept>,
    /// Set when the store cannot finish now (a backup, an external system).
    pub deferral: Option<Deferral>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KindAction {
    pub kind: String,
    pub action: Action,
    pub count: i64,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Anonymised,
    Deleted,
}
/// A record a store kept, and why. The certificate lists every one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Kept {
    pub kind: String,
    pub id: i64,
    pub reason: KeptReason,
    /// RFC 3339, when the reason has an end.
    pub until: Option<String>,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptReason {
    LegalHold,
    RetainedUntil,
    LegalMinimum,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Deferral {
    pub reason: String,
    /// RFC 3339 date by which the store will complete.
    pub complete_by: String,
}

/// Implement on the store owner's own type. `base/privacy` has already checked
/// the request's approval and Step-up, so a handler does not recheck its own
/// module's edit capability; it still reads only `scope`'s tenant. Every call
/// is idempotent: a retried job that finds the work done answers the same.
pub trait Handler: Send + Sync {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located>;
    fn export<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Exported>;
    /// Anonymise (the default) or delete, writing through `tx` and returning
    /// every change through it, with erased values as `[erased]`. List what
    /// the store must keep, with its reason, rather than erasing it.
    fn erase<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Erased>;
}

/// The value an erased field takes, in records and in audit evidence.
pub const ERASED: &str = "[erased]";

/// A store key is its module ID, a dot and a snake_case name.
fn store_key(module: &str, id: &str) -> bool {
    id.strip_prefix(module)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|name| {
            name.len() <= 64
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

pub(crate) fn validate(entries: &[Entry]) -> RegistryResult<()> {
    for entry in entries {
        let data = entry.get::<PersonalData>()?;
        let at = || {
            format!(
                "{} at {}:{}",
                entry.id, entry.provenance.source, entry.provenance.line
            )
        };
        if !store_key(entry.provenance.module, entry.id) {
            return Err(RegistryError::Contribution(format!(
                "personal-data store {} must be named {}.<snake_case name>",
                at(),
                entry.provenance.module
            )));
        }
        if data.label.is_empty() || data.label.len() > 120 {
            return Err(RegistryError::Contribution(format!(
                "personal-data store {} needs a label of 1-120 bytes",
                at()
            )));
        }
        if data
            .record_kinds
            .iter()
            .any(|kind| kind.is_empty() || kind.len() > 255)
        {
            return Err(RegistryError::Contribution(format!(
                "personal-data store {} names an empty or long record kind",
                at()
            )));
        }
    }
    Ok(())
}

/// The mounted stores, by key.
pub(crate) struct Stores {
    pub(crate) by_key: BTreeMap<&'static str, (&'static str, &'static PersonalData)>,
}
impl Stores {
    /// A contributor must depend on `base/privacy`, so the graph shows which
    /// modules answer for personal data.
    pub(crate) fn from_catalog(catalog: &Catalog, modules: &[&Module]) -> RegistryResult<Self> {
        let mut by_key = BTreeMap::new();
        for entry in catalog.entries::<PersonalData>(PERSONAL_DATA)? {
            let module = entry.provenance.module;
            let depends = modules
                .iter()
                .any(|m| m.id == module && m.dependencies.contains(&"base/privacy"));
            if !depends {
                return Err(RegistryError::Contribution(format!(
                    "personal-data store {} at {}:{} is contributed by a module that does not depend on base/privacy",
                    entry.id, entry.provenance.source, entry.provenance.line
                )));
            }
            by_key.insert(entry.id, (module, entry.get::<PersonalData>()?));
        }
        Ok(Self { by_key })
    }
}

/// Boot refuses a Grid table with a personal field whose module contributes
/// no personal-data store: nothing a person asks about may be left out quietly.
pub(crate) fn check_marks(stores: &Stores) -> RegistryResult<()> {
    let answering: Vec<&str> = stores.by_key.values().map(|(module, _)| *module).collect();
    for table in bilibi_grid::installed()?.tables() {
        if let Some(field) = table.personal_fields().next() {
            if !answering.contains(&table.owner) {
                return Err(RegistryError::Contribution(format!(
                    "grid table {} from {} ({}:{}) marks field {} personal, but {} contributes no base/privacy.personal_data store",
                    table.id(),
                    table.owner,
                    table.provenance.source,
                    table.provenance.line,
                    field.id,
                    table.owner
                )));
            }
        }
    }
    Ok(())
}
