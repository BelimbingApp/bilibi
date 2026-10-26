//! The store contract (ADR 05): each module that keeps a person's data
//! contributes one `PersonalData` handler per store, and the owner of each
//! audit record kind declares which of its fields are personal.
use crate::{
    grant::{Disclosure, Erasure},
    holds::Holds,
};
use bilibi_database::{MutationTransaction, Result};
use bilibi_module_registry::{Catalog, Entry, Module, RegistryError, Result as RegistryResult};
use bilibi_party_ids::PersonId;
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};
use tokio_postgres::Client;

/// The catalog stores contribute to:
/// `Contribution::new(PERSONAL_DATA, "<module>.<store>", provenance, &PersonalData { .. })`.
pub const PERSONAL_DATA: &str = "base/personal_data.stores";
/// The catalog owners of audit record kinds contribute to:
/// `Contribution::new(RECORD_KINDS, "<module>.<name>", provenance, &RecordKind { .. })`.
pub const RECORD_KINDS: &str = "base/personal_data.record_kinds";
/// The value an erased field takes, in records and in audit evidence.
pub const ERASED: &str = "[erased]";

pub type HandlerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Contribution payload: one store of personal data and its handler.
pub struct PersonalData {
    /// Shown on the request page and the completion certificate.
    pub label: &'static str,
    /// The audit record kinds (`Change::kind`) the store erases. Each must be
    /// declared by a [`RecordKind`]; holds name records by these kinds.
    pub kinds: &'static [&'static str],
    pub handler: &'static dyn Handler,
}

/// Contribution payload: an audit record kind and the fields of its changes
/// that hold personal data. Audit erasure replaces exactly these, so a caller
/// can never name a business field such as an amount.
pub struct RecordKind {
    /// `Change::kind`, as the owner's writes spell it.
    pub kind: &'static str,
    /// Top-level keys of the kind's `Change` values. Empty for a kind whose
    /// changes hold no personal value but whose records can be held.
    pub fields: &'static [&'static str],
    /// Whether the evidence's subject identifier names the person.
    pub subject_identifier: bool,
    /// Whether a record of this kind is a person, its ID the `PersonId`:
    /// Audit exports that record's history for an access request.
    pub person: bool,
}

/// Who a request is about: a Core person, plus identifiers the requester gave
/// and the records the operator confirmed are the person's although they
/// match only by an identifier.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Subject {
    pub person: PersonId,
    #[serde(default)]
    pub identifiers: Vec<Identifier>,
    /// This store's confirmed candidates. A handler acts on records linked to
    /// the person and on these; never on another identifier match.
    #[serde(default)]
    pub confirmed: Vec<Confirmed>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Identifier {
    Email(String),
    Phone(String),
}
impl Identifier {
    pub fn value(&self) -> &str {
        match self {
            Self::Email(value) | Self::Phone(value) => value,
        }
    }
}
/// A record the operator confirmed belongs to the subject.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Confirmed {
    pub kind: String,
    pub id: String,
}
impl Subject {
    /// `ILIKE` patterns for finding candidates: each email as given, each phone
    /// as its digits (match it against text with every non-digit removed, so
    /// `+60 12-345 6789` is found). `%`, `_` and `\` are escaped.
    pub fn patterns(&self) -> (Vec<String>, Vec<String>) {
        let like = |value: &str| {
            let escaped = value
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        };
        let (mut emails, mut phones) = (Vec::new(), Vec::new());
        for identifier in &self.identifiers {
            match identifier {
                Identifier::Email(value) if !value.is_empty() => emails.push(like(value)),
                Identifier::Phone(value) => {
                    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
                    if digits.len() >= 6 {
                        phones.push(like(&digits));
                    }
                }
                Identifier::Email(_) => {}
            }
        }
        (emails, phones)
    }
    /// The confirmed record IDs of one kind that parse as numbers.
    pub fn confirmed_ids(&self, kind: &str) -> Vec<i64> {
        self.confirmed
            .iter()
            .filter(|c| c.kind == kind)
            .filter_map(|c| c.id.parse().ok())
            .collect()
    }
}

/// What a store holds about a subject.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Located {
    pub kinds: Vec<KindCount>,
    /// Records that match only an identifier, for the operator to confirm one
    /// by one. Never another person's record: those are only counted.
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    /// More candidates exist than were listed.
    #[serde(default)]
    pub truncated: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KindCount {
    pub kind: String,
    /// Records linked to the person.
    pub by_person: i64,
    /// Records that match only an identifier.
    pub by_identifier: i64,
}
/// The most candidates a store lists for one request.
pub const MAX_CANDIDATES: usize = 200;
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Candidate {
    pub kind: String,
    pub id: String,
    /// What the record is, without its personal values: "Event x.created, 3 May 2026".
    pub summary: String,
}

/// A store's export: its records as JSON and the files they reference.
/// `base/privacy` asks the store for each file's bytes when it writes the
/// bundle, so the handler never copies bytes into the records.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Exported {
    pub records: Value,
    pub files: Vec<FileRef>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FileRef {
    /// The store's own reference, passed back to [`Handler::file`].
    pub reference: String,
    /// The file's name in the bundle, without a path.
    pub name: String,
    pub content_type: String,
}
/// One file's bytes, read through its owner.
pub struct File {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// What an erasure did.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Erased {
    pub actions: Vec<KindAction>,
    /// Every record anonymised or deleted: `base/privacy` clears its kind's
    /// declared personal fields from its earlier audit evidence in the same
    /// transaction.
    #[serde(default)]
    pub records: Vec<ErasedRecord>,
    pub kept: Vec<Kept>,
    /// Set when the store cannot finish now (a backup, an external system).
    pub deferral: Option<Deferral>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ErasedRecord {
    /// The audit record kind (`Change::kind`), declared by a [`RecordKind`].
    pub kind: String,
    pub id: i64,
}
impl Erased {
    /// Adds another step's answer.
    pub fn merge(&mut self, other: Erased) {
        for action in other.actions {
            match self
                .actions
                .iter_mut()
                .find(|a| a.kind == action.kind && a.action == action.action)
            {
                Some(known) => known.count += action.count,
                None => self.actions.push(action),
            }
        }
        self.records.extend(other.records);
        self.kept.extend(other.kept);
        if other.deferral.is_some() {
            self.deferral = other.deferral;
        }
    }
    /// One more record of `kind` handled with `action`.
    pub fn count(&mut self, kind: &str, action: Action, count: i64) {
        if count > 0 {
            self.merge(Erased {
                actions: vec![KindAction {
                    kind: kind.into(),
                    action,
                    count,
                }],
                ..Erased::default()
            });
        }
    }
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
    /// The record's ID as its store names it (a number, or an artifact's UUID).
    pub id: String,
    pub reason: KeptReason,
    /// RFC 3339, when the reason has an end.
    pub until: Option<String>,
    /// The legal hold that kept it.
    #[serde(default)]
    pub hold: Option<i64>,
}
impl Kept {
    pub fn held(kind: &str, id: impl ToString, hold: i64) -> Self {
        Self {
            kind: kind.into(),
            id: id.to_string(),
            reason: KeptReason::LegalHold,
            until: None,
            hold: Some(hold),
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptReason {
    LegalHold,
    RetainedUntil,
    LegalMinimum,
    /// The record also belongs to another party (a company's address).
    Shared,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Deferral {
    pub reason: String,
    /// RFC 3339 date by which the store will complete.
    pub complete_by: String,
}

/// Where a store's `after_erase` records each step as it goes, so a retry
/// after a failure still reports what earlier attempts did.
pub trait Progress: Send + Sync {
    fn record<'a>(&'a self, step: Erased) -> HandlerFuture<'a, ()>;
}

/// Implement on the store owner's own type. `base/privacy` checks the
/// request's approval and Step-up and hands the handler a grant; a handler
/// does not recheck its own module's edit capability, it reads only `scope`'s
/// tenant, and it asks the grant-checked paths of Media and Artifacts for
/// files. Every call is idempotent: a retried job that finds the work done
/// answers the same.
pub trait Handler: Send + Sync {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located>;
    /// Every record, however many: never a first page. Too much to export is
    /// an error, never a silently shorter export.
    fn export<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
        grant: &'a Disclosure,
    ) -> HandlerFuture<'a, Exported>;
    /// One file the store's export listed, read through its owner.
    fn file<'a>(
        &'a self,
        _client: &'a mut Client,
        _scope: &'a Scope,
        _grant: &'a Disclosure,
        _reference: &'a str,
    ) -> HandlerFuture<'a, File> {
        Box::pin(async { Err(bilibi_database::Error::NotFound) })
    }
    /// Anonymise (the default) or delete, writing through `tx` and returning
    /// every change through it, with erased values as `[erased]`. Keep what
    /// `holds` names, and anything the store must keep, and list it with its
    /// reason rather than erasing it.
    fn erase<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        grant: &'a Erasure,
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased>;
    /// Work that needs a plain connection after the erasure committed, such as
    /// removing files or deleting artifacts. Record each step through
    /// `progress` as soon as it is done. Retried until it succeeds, so it must
    /// be idempotent, and it must keep what `holds` names.
    fn after_erase<'a>(
        &'a self,
        _client: &'a mut Client,
        _scope: &'a Scope,
        _subject: &'a Subject,
        _grant: &'a Erasure,
        _holds: &'a Holds,
        _progress: &'a dyn Progress,
    ) -> HandlerFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A store or kind key is its module ID, a dot and a snake_case name.
pub fn module_key(module: &str, id: &str) -> bool {
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
fn at(entry: &Entry) -> String {
    format!(
        "{} at {}:{}",
        entry.id, entry.provenance.source, entry.provenance.line
    )
}

pub(crate) fn validate_stores(entries: &[Entry]) -> RegistryResult<()> {
    for entry in entries {
        let data = entry.get::<PersonalData>()?;
        if !module_key(entry.provenance.module, entry.id) {
            return Err(RegistryError::Contribution(format!(
                "personal-data store {} must be named {}.<snake_case name>",
                at(entry),
                entry.provenance.module
            )));
        }
        if data.label.is_empty() || data.label.len() > 120 {
            return Err(RegistryError::Contribution(format!(
                "personal-data store {} needs a label of 1-120 bytes",
                at(entry)
            )));
        }
        if data.kinds.iter().any(|k| k.is_empty() || k.len() > 255) {
            return Err(RegistryError::Contribution(format!(
                "personal-data store {} names an empty or long record kind",
                at(entry)
            )));
        }
    }
    Ok(())
}
pub(crate) fn validate_kinds(entries: &[Entry]) -> RegistryResult<()> {
    let mut seen = BTreeMap::new();
    for entry in entries {
        let kind = entry.get::<RecordKind>()?;
        let fields_ok = kind.fields.len() <= 64
            && kind
                .fields
                .iter()
                .all(|f| !f.is_empty() && f.len() <= 128 && !f.chars().any(char::is_control));
        if !module_key(entry.provenance.module, entry.id)
            || kind.kind.is_empty()
            || kind.kind.len() > 255
            || !fields_ok
        {
            return Err(RegistryError::Contribution(format!(
                "record kind {} must be named {}.<snake_case name> and list at most 64 personal fields",
                at(entry),
                entry.provenance.module
            )));
        }
        if let Some(other) = seen.insert(kind.kind, entry.id) {
            return Err(RegistryError::Contribution(format!(
                "record kind {} is declared twice, by {other} and {}",
                kind.kind,
                at(entry)
            )));
        }
    }
    Ok(())
}

/// The mounted stores and declared record kinds, by key.
pub struct Stores {
    stores: BTreeMap<&'static str, (&'static str, &'static PersonalData)>,
    kinds: BTreeMap<&'static str, &'static RecordKind>,
}
impl Stores {
    /// A contributor must depend on `base/personal_data`, so the graph shows
    /// which modules answer for personal data, and every kind a store erases
    /// must be declared.
    pub fn from_catalog(catalog: &Catalog, modules: &[&Module]) -> RegistryResult<Self> {
        let mut kinds = BTreeMap::new();
        for entry in catalog.entries::<RecordKind>(RECORD_KINDS)? {
            let kind = entry.get::<RecordKind>()?;
            kinds.insert(kind.kind, kind);
        }
        let mut stores = BTreeMap::new();
        for entry in catalog.entries::<PersonalData>(PERSONAL_DATA)? {
            let module = entry.provenance.module;
            if !modules
                .iter()
                .any(|m| m.id == module && m.dependencies.contains(&"base/personal_data"))
            {
                return Err(RegistryError::Contribution(format!(
                    "personal-data store {} is contributed by {module}, which does not depend on base/personal_data",
                    at(entry)
                )));
            }
            let data = entry.get::<PersonalData>()?;
            if let Some(kind) = data.kinds.iter().find(|k| !kinds.contains_key(*k)) {
                return Err(RegistryError::Contribution(format!(
                    "personal-data store {} erases record kind {kind}, which no base/personal_data.record_kinds contribution declares",
                    at(entry)
                )));
            }
            stores.insert(entry.id, (module, data));
        }
        Ok(Self { stores, kinds })
    }
    /// The stores of every module this binary mounts, read once.
    pub fn installed() -> RegistryResult<&'static Self> {
        static INSTALLED: OnceLock<Arc<Stores>> = OnceLock::new();
        if let Some(stores) = INSTALLED.get() {
            return Ok(stores);
        }
        let modules = bilibi_module_registry::discover()?;
        let catalog = Catalog::discover(&modules)?;
        let stores = Arc::new(Self::from_catalog(&catalog, &modules)?);
        Ok(INSTALLED.get_or_init(|| stores))
    }
    /// Every store: key, owning module and contribution, sorted by key.
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (&'static str, &'static str, &'static PersonalData)> + '_ {
        self.stores
            .iter()
            .map(|(key, (module, data))| (*key, *module, *data))
    }
    pub fn get(&self, key: &str) -> Option<(&'static str, &'static PersonalData)> {
        self.stores
            .get_key_value(key)
            .map(|(key, (_, data))| (*key, *data))
    }
    pub fn is_empty(&self) -> bool {
        self.stores.is_empty()
    }
    /// The modules that answer for personal data.
    pub fn modules(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.stores.values().map(|(module, _)| *module)
    }
    /// A declared record kind.
    pub fn kind(&self, kind: &str) -> Option<&'static RecordKind> {
        self.kinds.get(kind).copied()
    }
    /// The kinds whose records are people.
    pub fn person_kinds(&self) -> Vec<&'static str> {
        self.kinds
            .values()
            .filter(|k| k.person)
            .map(|k| k.kind)
            .collect()
    }
}
