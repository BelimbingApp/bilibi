//! Names and ranks the principals a Base screen already references. Base owns
//! grants, sessions and assignments; Core owns the people behind them, so Core
//! binds one provider per kind at boot and Base never imports Core.
//!
//! A principal is `{kind, id}`, never an id alone: user 5 and agent 5 differ.
//! The caller names its candidates (the distinct pairs its filtered query
//! references, before pagination); the directory resolves only those, inside the
//! actor's scope. A principal the provider cannot see is not an error and not a
//! removed row: it is `unresolved` and shows its durable kind and id.
use bilibi_database::Error;
use bilibi_tenancy::Scope;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio_postgres::{types::ToSql, GenericClient, Row};

pub const DEFAULT_CEILING: usize = 2_000;
/// Providers receive the candidate IDs as one array; never let a caller make
/// that unbounded.
pub const MAX_CEILING: usize = 10_000;
const MAX_SEARCH: usize = 200;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum PrincipalKind {
    User,
    Agent,
    /// The approved party model's Person.
    Person,
}
impl PrincipalKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Person => "person",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Agent => "Agent",
            Self::Person => "Person",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PrincipalRef {
    kind: PrincipalKind,
    id: i64,
}
impl PrincipalRef {
    pub fn new(kind: PrincipalKind, id: i64) -> Result<Self, DirectoryError> {
        if id <= 0 {
            return Err(DirectoryError::InvalidCandidate);
        }
        Ok(Self { kind, id })
    }
    pub fn kind(self) -> PrincipalKind {
        self.kind
    }
    pub fn id(self) -> i64 {
        self.id
    }
    /// The durable presentation of a principal nobody could name.
    pub fn durable_label(self) -> String {
        format!("{} {}", self.kind.title(), self.id)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("Principal IDs must be positive")]
    InvalidCandidate,
    #[error("Search text is too long")]
    InvalidSearch,
    #[error("More than {ceiling} principals were referenced; show durable IDs instead")]
    TooManyCandidates { ceiling: usize },
    /// A search never degrades: a silent no-match and a silent match-all are
    /// both wrong answers to the question asked.
    #[error("Name search is unavailable for {}", .0.key())]
    NameSearchUnavailable(PrincipalKind),
    #[error("The {} directory failed: {source}", .kind.key())]
    Provider {
        kind: PrincipalKind,
        #[source]
        source: Error,
    },
}

pub type DirectoryFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;
/// Object-safe read access that keeps the caller's transaction for providers.
pub trait DirectoryClient: Sync {
    fn query<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> DirectoryFuture<'a, Vec<Row>>;
}
impl<T: GenericClient + Sync> DirectoryClient for T {
    fn query<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> DirectoryFuture<'a, Vec<Row>> {
        Box::pin(async move { Ok(GenericClient::query(self, sql, params).await?) })
    }
}

/// What an owning Core module implements, one per kind. Providers answer only
/// for their own kind and only inside `scope`, scoped by the query itself rather
/// than by filtering afterwards. They never order, and never decide what a
/// screen shows for someone they cannot see: deleted, archived and
/// cross-tenant IDs are simply left out.
pub trait PrincipalProvider: Send + Sync {
    fn kind(&self) -> PrincipalKind;
    fn names<'a>(
        &'a self,
        client: &'a dyn DirectoryClient,
        scope: &'a Scope,
        ids: &'a [i64],
    ) -> DirectoryFuture<'a, HashMap<i64, String>>;
    /// IDs, from `ids` only, matching a provider-owned attribute such as an
    /// account email, so Base never learns that attribute exists. `needle` is
    /// trimmed and lower-cased. Name matching stays with the directory.
    fn search<'a>(
        &'a self,
        _client: &'a dyn DirectoryClient,
        _scope: &'a Scope,
        _ids: &'a [i64],
        _needle: &'a str,
    ) -> DirectoryFuture<'a, Vec<i64>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[derive(Clone, Debug)]
pub struct Query {
    search: Option<String>,
    ceiling: usize,
}
impl Default for Query {
    fn default() -> Self {
        Self {
            search: None,
            ceiling: DEFAULT_CEILING,
        }
    }
}
impl Query {
    pub fn search(mut self, text: impl Into<String>) -> Self {
        self.search = Some(text.into());
        self
    }
    pub fn ceiling(mut self, ceiling: usize) -> Self {
        self.ceiling = ceiling.min(MAX_CEILING);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Named {
    pub principal: PrincipalRef,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Ranked {
    /// Resolved principals by `(lower-cased name, kind, id)`. Feed this order to
    /// the database (`array_position`) before `offset`/`limit`.
    pub named: Vec<Named>,
    /// Candidates nobody could name, by `(kind, id)`; empty under a search,
    /// where they cannot match. The consumer keeps these rows.
    pub unresolved: Vec<PrincipalRef>,
}
impl Ranked {
    /// Every principal, named first, in the order a consumer should rank rows.
    pub fn order(&self) -> impl Iterator<Item = PrincipalRef> + '_ {
        self.named
            .iter()
            .map(|named| named.principal)
            .chain(self.unresolved.iter().copied())
    }
    /// The name to show, or the durable kind and id when there is none.
    pub fn label(&self, principal: PrincipalRef) -> String {
        self.named
            .iter()
            .find(|named| named.principal == principal)
            .map(|named| named.name.clone())
            .unwrap_or_else(|| principal.durable_label())
    }
}

type Bound = (&'static str, Arc<dyn PrincipalProvider>);

#[derive(Default)]
pub struct PrincipalDirectory {
    providers: Mutex<BTreeMap<PrincipalKind, Bound>>,
    sealed: AtomicBool,
}
impl PrincipalDirectory {
    /// Boot seam for the owning module. A second provider for a kind is a
    /// defect: the name a screen shows must not depend on install order.
    pub fn bind(
        &self,
        owner: &'static str,
        provider: Arc<dyn PrincipalProvider>,
    ) -> bilibi_module_registry::Result<()> {
        let invalid = |message: String| bilibi_module_registry::RegistryError::Service(message);
        if self.sealed.load(Ordering::Acquire) {
            return Err(invalid(format!(
                "{owner} bound a principal provider after boot"
            )));
        }
        let kind = provider.kind();
        let mut providers = self
            .providers
            .lock()
            .map_err(|_| invalid("poisoned".into()))?;
        if let Some((first, _)) = providers.get(&kind) {
            return Err(invalid(format!(
                "{owner} claims principal kind {} already claimed by {first}",
                kind.key()
            )));
        }
        providers.insert(kind, (owner, provider));
        Ok(())
    }
    /// Kinds with a bound provider and the module that owns each.
    pub fn bound(&self) -> Vec<(PrincipalKind, &'static str)> {
        self.providers
            .lock()
            .map(|providers| {
                providers
                    .iter()
                    .map(|(kind, (owner, _))| (*kind, *owner))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn provider(&self, kind: PrincipalKind) -> Option<Arc<dyn PrincipalProvider>> {
        self.providers
            .lock()
            .ok()?
            .get(&kind)
            .map(|(_, provider)| provider.clone())
    }
    pub async fn rank(
        &self,
        client: &dyn DirectoryClient,
        scope: &Scope,
        candidates: &[PrincipalRef],
        query: &Query,
    ) -> Result<Ranked, DirectoryError> {
        let mut seen = BTreeSet::new();
        let candidates: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|candidate| seen.insert(*candidate))
            .collect();
        if candidates.len() > query.ceiling {
            return Err(DirectoryError::TooManyCandidates {
                ceiling: query.ceiling,
            });
        }
        let needle = match query
            .search
            .as_deref()
            .map(|text| text.trim().to_lowercase())
        {
            Some(text) if text.chars().count() > MAX_SEARCH => {
                return Err(DirectoryError::InvalidSearch)
            }
            Some(text) if !text.is_empty() => Some(text),
            _ => None,
        };
        let mut by_kind: BTreeMap<PrincipalKind, Vec<i64>> = BTreeMap::new();
        for candidate in &candidates {
            by_kind
                .entry(candidate.kind)
                .or_default()
                .push(candidate.id);
        }
        if needle.is_some() {
            if let Some(kind) = by_kind.keys().find(|kind| self.provider(**kind).is_none()) {
                return Err(DirectoryError::NameSearchUnavailable(*kind));
            }
        }
        let mut named = BTreeMap::new();
        let mut matched = BTreeSet::new();
        for (kind, ids) in &by_kind {
            let Some(provider) = self.provider(*kind) else {
                continue;
            };
            let fail = |source| DirectoryError::Provider {
                kind: *kind,
                source,
            };
            // A provider may only name what it was asked about.
            for (id, name) in provider.names(client, scope, ids).await.map_err(fail)? {
                if ids.contains(&id) && !name.trim().is_empty() {
                    named.insert(PrincipalRef { kind: *kind, id }, name);
                }
            }
            if let Some(needle) = &needle {
                for id in provider
                    .search(client, scope, ids, needle)
                    .await
                    .map_err(fail)?
                {
                    if ids.contains(&id) {
                        matched.insert(PrincipalRef { kind: *kind, id });
                    }
                }
            }
        }
        let unresolved = if needle.is_some() {
            Vec::new()
        } else {
            let mut missing: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|candidate| !named.contains_key(candidate))
                .collect();
            missing.sort();
            missing
        };
        let mut named: Vec<_> = named
            .into_iter()
            .filter(|(principal, name)| {
                needle.as_ref().is_none_or(|needle| {
                    name.to_lowercase().contains(needle.as_str()) || matched.contains(principal)
                })
            })
            .map(|(principal, name)| Named { principal, name })
            .collect();
        named.sort_by_cached_key(|named| (named.name.to_lowercase(), named.principal));
        Ok(Ranked { named, unresolved })
    }
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/principal_directory", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/database", "base/tenancy"], migrations: &[],
        services: &[bilibi_module_registry::Service { id: "base/principal_directory.directory", factory: |_| Ok(Arc::new(PrincipalDirectory::default())) }],
    }
}
// Providers bind during their owners' factories; nothing may bind afterwards.
inventory::submit! {
    bilibi_module_registry::Ready {
        module: "base/principal_directory",
        check: |services| {
            services.get::<PrincipalDirectory>("base/principal_directory.directory")?.sealed.store(true, Ordering::Release);
            Ok(())
        },
    }
}
