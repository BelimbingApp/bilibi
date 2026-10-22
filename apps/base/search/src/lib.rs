//! Global search: one query over the records of every mounted module,
//! trimmed to what the person may open (issue 130). Bilimbi has no
//! cross-module record search, so this module mirrors none; `README.md` is
//! the contract.
//!
//! An owner makes its Grid table searchable with one [`contribute!`] entry
//! ([`Searchable`]) and a stored, generated `tsvector` column its Grid source
//! selects. A search builds a fresh `bilibi_grid::Catalog` for the scope,
//! keeps the kinds whose table the reader may read and whose record page
//! admits them, and runs one bounded read-only statement that matches inside
//! each owner's source (`query.rs`). The search box (the
//! `shell.search.records` panel), the `/search` page and the agent operation
//! `base.search.records` all call this facade, so they find the same records.
mod contributions;
mod model;
mod query;
mod web;

pub use model::{Kind, Searchable, State};
pub use query::{MAX_KIND_HITS, MAX_TEXT, MAX_WORDS};

use bilibi_authz::authorize_key;
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{contributions::Access, Catalog, RegistryError};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{ReadClient, Scope};
use query::Planned;
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
    time::SystemTime,
};
use tokio_postgres::error::SqlState;

/// The registry consumer every module's [`Searchable`] is submitted to.
pub const SOURCES: &str = "base/search.sources";
/// The [`Search`] facade's service.
pub const SERVICE: &str = "base/search.records";
/// What a person needs to search records: the search box's records, the
/// `/search` page and the `base.search.records` operation.
pub const SEARCH: &str = "base.search.records.list";

/// Which kinds the search covers, over each kind's default: `{"companies": false}`.
pub(crate) const KINDS: &str = "search.kinds";
/// Each kind's ranking multiplier: `{"persons": 2}`.
pub(crate) const BOOST: &str = "search.boost";
/// How many characters a query needs before records are searched.
pub(crate) const MIN_CHARS: &str = "search.min_chars";
/// How many records of each kind the search box shows.
pub(crate) const SUGGESTIONS: &str = "search.suggestions";
/// The kinds a person keeps out of their own results.
pub(crate) const HIDDEN: &str = "search.hidden_kinds";
const TUNING: [&str; 5] = [KINDS, BOOST, MIN_CHARS, SUGGESTIONS, HIDDEN];

#[doc(hidden)]
pub use bilibi_module_registry::{Contribution, Provenance};
#[doc(hidden)]
pub use inventory;

/// Make a Grid table searchable from the module that owns it:
/// `bilibi_search::contribute!("core/x", "x.search", &SEARCH)` with
/// `static SEARCH: Searchable`.
#[macro_export]
macro_rules! contribute {
    ($module:literal, $id:literal, $payload:expr) => {
        $crate::inventory::submit! {
            $crate::Contribution::new(
                $crate::SOURCES,
                $id,
                $crate::Provenance { module: $module, source: file!(), line: line!() },
                $payload,
            )
        }
    };
}

/// One record a search found, as the reader may see it.
#[derive(Clone, Debug)]
pub struct Hit {
    pub kind: &'static Kind,
    /// The record's key, as text.
    pub id: String,
    pub title: String,
    /// The subtitle values the reader may see, in declared order.
    pub detail: Vec<String>,
    /// The record's state when it is no longer current (`retired`).
    pub state: Option<String>,
    pub href: String,
    pub updated: Option<SystemTime>,
}

/// How many records one kind matched, counted up to [`MAX_KIND_HITS`].
#[derive(Clone, Copy, Debug)]
pub struct Count {
    pub kind: &'static Kind,
    pub matched: u64,
}
impl Count {
    /// The count stopped at the bound: there may be more.
    pub fn capped(&self) -> bool {
        self.matched >= MAX_KIND_HITS as u64
    }
}

/// What one search answered.
#[derive(Clone, Debug, Default)]
pub struct Found {
    /// The text as searched: trimmed, at most [`MAX_TEXT`] characters.
    pub text: String,
    /// The hits, best first: up to the search box's limit of each kind, or
    /// the results page asked for.
    pub hits: Vec<Hit>,
    /// Each kind searched and its matches, by label; results pages only.
    pub counts: Vec<Count>,
    /// The query length in force for this person.
    pub min_chars: usize,
    /// The text was shorter than `min_chars`; nothing was read.
    pub short: bool,
    /// So many records matched that ranking them would exceed the bounds;
    /// nothing is listed and the person is asked for another word.
    pub broad: bool,
}
impl Found {
    /// The matches of `kind`, or of every kind.
    pub fn total(&self, kind: Option<&str>) -> u64 {
        self.counts
            .iter()
            .filter(|count| kind.is_none_or(|kind| count.kind.id == kind))
            .map(|count| count.matched)
            .sum()
    }
}

/// A kind this person may search, and whether they keep it in their results.
#[derive(Clone, Copy, Debug)]
pub struct Preference {
    pub kind: &'static Kind,
    pub shown: bool,
}

/// What the search box or a page asks for.
#[derive(Clone, Copy, Debug)]
enum Mode<'a> {
    Suggest,
    Page {
        kind: Option<&'a str>,
        page: u64,
        page_size: u64,
    },
}

/// The operator's settings and the person's own, resolved once per search.
struct Tuning {
    kinds: Map<String, Value>,
    boost: Map<String, Value>,
    min_chars: usize,
    suggestions: i64,
    hidden: BTreeSet<String>,
}
impl Tuning {
    /// Whether the operator's settings search this kind for anyone.
    fn on(&self, kind: &Kind) -> bool {
        self.kinds
            .get(kind.id)
            .and_then(Value::as_bool)
            .unwrap_or(kind.decl.default_on)
    }
    fn searched(&self, kind: &Kind) -> bool {
        self.on(kind) && !self.hidden.contains(kind.id)
    }
    fn boost(&self, kind: &Kind) -> f64 {
        self.boost
            .get(kind.id)
            .and_then(Value::as_f64)
            .filter(|boost| (0.1..=10.0).contains(boost))
            .unwrap_or(1.0)
    }
}

/// What opens a record page for this reader: the page routes' access kinds,
/// decided as the menu decides (display only; the page checks again).
struct Standing {
    capabilities: BTreeSet<&'static str>,
    operator: bool,
    owner: bool,
}
impl Standing {
    fn admits(&self, access: Access) -> bool {
        match access.base() {
            Access::Public | Access::SignedIn => true,
            Access::Capability(key) => self.capabilities.contains(key),
            Access::AnyOf(keys) => keys.iter().any(|key| self.capabilities.contains(key)),
            Access::Operator => self.operator,
            Access::OperatorCapability(key) => self.operator && self.capabilities.contains(key),
            Access::Owner => self.owner,
            Access::StepUp { .. } => false,
        }
    }
}

fn proven<T>(result: Result<T>) -> Result<bool> {
    match result {
        Ok(_) => Ok(true),
        Err(Error::Forbidden) => Ok(false),
        Err(error) => Err(error),
    }
}

/// A statement the server cancelled at its timeout: too many matches.
fn cancelled(error: &tokio_postgres::Error) -> bool {
    error.code() == Some(&SqlState::QUERY_CANCELED)
}

/// The facade: the installed kinds and the one search behind every caller.
pub struct Search {
    db: Db,
    settings: Arc<Settings>,
    boundary: Arc<MutationBoundary>,
    kinds: &'static [Kind],
}

impl Search {
    /// Every installed kind, by label.
    pub fn kinds(&self) -> &'static [Kind] {
        self.kinds
    }
    pub fn kind(&self, id: &str) -> Option<&'static Kind> {
        self.kinds.iter().find(|kind| kind.id == id)
    }

    /// Whether this person may search records, with the query length in
    /// force; read without a decision-log row, since the search box asks on
    /// every render of the shell and the search itself decides again, logged.
    pub async fn offered(&self, scope: &Scope) -> Result<Option<usize>> {
        let client = self.db.get().await?;
        if !bilibi_authz::effective_capabilities(&**client, scope)
            .await?
            .contains(SEARCH)
        {
            return Ok(None);
        }
        Ok(Some(self.tuning(&**client, scope).await?.min_chars))
    }

    /// The search box: the best records of each kind, up to the tenant's
    /// per-kind limit, merged by rank.
    pub async fn suggest(&self, scope: &Scope, text: &str) -> Result<Found> {
        self.run(scope, text, Mode::Suggest).await
    }

    /// A results page: `page_size` hits from `page` of the merged ranking,
    /// of `kind` or of every kind, and how many records each kind matched.
    pub async fn find(
        &self,
        scope: &Scope,
        text: &str,
        kind: Option<&str>,
        page: u64,
        page_size: u64,
    ) -> Result<Found> {
        self.run(
            scope,
            text,
            Mode::Page {
                kind,
                page: page.max(1),
                page_size: page_size.clamp(1, 100),
            },
        )
        .await
    }

    /// The kinds this person may search, and whether they keep each in their
    /// results, for their own settings page.
    pub async fn preferences(&self, scope: &Scope) -> Result<Vec<Preference>> {
        let mut client = self.db.get().await?;
        authorize_key(&**client, scope, SEARCH).await?;
        let tx = client
            .build_transaction()
            .read_only(true)
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await?;
        tx.batch_execute(query::BOUNDS).await?;
        let tuning = self.tuning(&*tx, scope).await?;
        let catalog = bilibi_grid::Catalog::for_scope(&*tx, scope).await?;
        let standing = self.standing(&*tx, scope).await?;
        tx.commit().await?;
        Ok(self
            .reach(&catalog, &standing, |kind| tuning.on(kind))
            .map(|kind| Preference {
                kind,
                shown: !tuning.hidden.contains(kind.id),
            })
            .collect())
    }

    /// Keeps the person's own choice: `shown` names, of the kinds they may
    /// search now, those to keep in their results; a kind they cannot see
    /// keeps what they chose before. Their own view state, like a list's
    /// columns, so it is not an administration write.
    pub async fn keep(&self, scope: &Scope, shown: &BTreeSet<String>) -> Result<()> {
        let offered: Vec<&str> = self
            .preferences(scope)
            .await?
            .into_iter()
            .map(|preference| preference.kind.id)
            .collect();
        let mut client = self.db.get().await?;
        let stored = self.tuning(&**client, scope).await?.hidden;
        let mut hidden: BTreeSet<&str> = stored
            .iter()
            .map(String::as_str)
            .filter(|id| !offered.contains(id))
            .collect();
        hidden.extend(offered.iter().filter(|id| !shown.contains(**id)));
        let mut tx = self
            .boundary
            .begin_view_state(&mut client, scope.mutation_context("search.preferences"))
            .await?;
        self.settings
            .put(
                &mut tx,
                scope,
                Target::User { company: None },
                HIDDEN,
                json!(hidden),
            )
            .await?;
        tx.commit().await
    }

    async fn tuning(&self, client: &dyn ReadClient, scope: &Scope) -> Result<Tuning> {
        // The person's company may override the tenant; an account the
        // company directory refuses reads the tenant's values instead.
        let company = scope.actor().organisation();
        let mut values = match self
            .settings
            .resolve_many(client, scope, Target::User { company }, &TUNING)
            .await
        {
            Err(Error::Forbidden) if company.is_some() => {
                self.settings
                    .resolve_many(client, scope, Target::User { company: None }, &TUNING)
                    .await?
            }
            resolved => resolved?,
        };
        let mut take = |key: &str| {
            values
                .remove(key)
                .map(|resolved| resolved.value)
                .unwrap_or(Value::Null)
        };
        let object = |value: Value| match value {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        Ok(Tuning {
            kinds: object(take(KINDS)),
            boost: object(take(BOOST)),
            min_chars: take(MIN_CHARS)
                .as_u64()
                .map_or(2, |n| n.clamp(1, 10) as usize),
            suggestions: take(SUGGESTIONS).as_i64().map_or(5, |n| n.clamp(1, 10)),
            hidden: take(HIDDEN)
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        })
    }

    async fn standing(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Standing> {
        let needs =
            |test: fn(Access) -> bool| self.kinds.iter().any(|kind| test(kind.access.base()));
        let operator =
            needs(|access| matches!(access, Access::Operator | Access::OperatorCapability(_)));
        let owner = needs(|access| matches!(access, Access::Owner));
        Ok(Standing {
            capabilities: bilibi_authz::effective_capabilities(client, scope).await?,
            operator: operator && proven(bilibi_tenancy::prove_operator(client, scope).await)?,
            owner: owner && proven(bilibi_tenancy::prove_owner(client, scope).await)?,
        })
    }

    /// The kinds `searched` keeps whose table the reader's catalog holds and
    /// whose record page admits them.
    fn reach<'a>(
        &'a self,
        catalog: &'a bilibi_grid::Catalog,
        standing: &'a Standing,
        searched: impl Fn(&Kind) -> bool + 'a,
    ) -> impl Iterator<Item = &'static Kind> + 'a {
        self.kinds.iter().filter(move |kind| {
            catalog.table(kind.id).is_some() && searched(kind) && standing.admits(kind.access)
        })
    }

    async fn run(&self, scope: &Scope, text: &str, mode: Mode<'_>) -> Result<Found> {
        let mut client = self.db.get().await?;
        // One logged decision per search, as a list's own search makes.
        authorize_key(&**client, scope, SEARCH).await?;
        let tx = client
            .build_transaction()
            .read_only(true)
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await?;
        tx.batch_execute(query::BOUNDS).await?;
        let tuning = self.tuning(&*tx, scope).await?;
        let terms = query::terms(text);
        let mut found = Found {
            text: terms.text.clone(),
            min_chars: tuning.min_chars,
            ..Found::default()
        };
        if terms.text.chars().count() < tuning.min_chars {
            found.short = true;
            return Ok(found);
        }
        let Some(words) = terms.query.as_deref() else {
            return Ok(found);
        };
        let catalog = bilibi_grid::Catalog::for_scope(&*tx, scope).await?;
        let standing = self.standing(&*tx, scope).await?;
        let planned: Vec<Planned<'_>> = self
            .reach(&catalog, &standing, |kind| tuning.searched(kind))
            .filter_map(|kind| {
                Some(Planned {
                    kind,
                    table: catalog.table(kind.id)?,
                    boost: tuning.boost(kind),
                })
            })
            .collect();
        if planned.is_empty() {
            return Ok(found);
        }
        let prefix = terms.prefix.as_deref();
        let (tenant, user) = (scope.tenant(), scope.user());
        let pattern = prefix.unwrap_or_default();
        let (sql, filter, limit, offset) = match mode {
            Mode::Suggest => (
                query::suggestions(&planned, tuning.suggestions, prefix.is_some()),
                String::new(),
                0i64,
                0i64,
            ),
            Mode::Page {
                kind,
                page,
                page_size,
            } => (
                query::page(&planned, prefix.is_some()),
                kind.unwrap_or_default().to_owned(),
                i64::try_from(page_size).map_err(|_| Error::Invalid)?,
                i64::try_from((page - 1).saturating_mul(page_size)).map_err(|_| Error::Invalid)?,
            ),
        };
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            vec![&tenant, &user, &words, &pattern];
        if matches!(mode, Mode::Page { .. }) {
            params.extend([
                &filter as &(dyn tokio_postgres::types::ToSql + Sync),
                &limit,
                &offset,
            ]);
        }
        let explain: Value = tx
            .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), &params)
            .await?
            .try_get(0)?;
        let cost = explain[0]["Plan"]["Total Cost"]
            .as_f64()
            .ok_or(Error::Internal)?;
        if !cost.is_finite() || cost > query::MAX_ESTIMATED_COST {
            found.broad = true;
            return Ok(found);
        }
        let rows = match tx.query(&sql, &params).await {
            Ok(rows) => rows,
            Err(error) if cancelled(&error) => {
                found.broad = true;
                return Ok(found);
            }
            Err(error) => return Err(error.into()),
        };
        // Columns: counts, kind, id, title, sub0, sub1, state, inactive, updated.
        for row in &rows {
            if let (true, Some(Value::Object(counts))) =
                (found.counts.is_empty(), row.try_get::<_, Option<Value>>(0)?)
            {
                found.counts = planned
                    .iter()
                    .map(|planned| Count {
                        kind: planned.kind,
                        matched: counts
                            .get(planned.kind.id)
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    })
                    .collect();
            }
            let Some(kind) = row
                .try_get::<_, Option<String>>(1)?
                .and_then(|id| self.kind(&id))
            else {
                continue;
            };
            let id: String = row.try_get(2)?;
            let mut detail = Vec::new();
            for at in [4, 5] {
                if let Some(value) = row.try_get::<_, Option<String>>(at)? {
                    if !value.trim().is_empty() {
                        detail.push(value);
                    }
                }
            }
            let state: Option<String> = row.try_get(6)?;
            let inactive: bool = row.try_get(7)?;
            found.hits.push(Hit {
                kind,
                href: kind.href(&id),
                id,
                title: row.try_get::<_, Option<String>>(3)?.unwrap_or_default(),
                detail,
                state: state.filter(|_| inactive),
                updated: row.try_get(8)?,
            });
        }
        tx.commit().await?;
        Ok(found)
    }
}

/// The facade for the agent operation, set once at boot.
static BOUND: OnceLock<Arc<Search>> = OnceLock::new();
fn bound() -> Result<&'static Arc<Search>> {
    BOUND.get().ok_or(Error::Internal)
}

inventory::submit! { bilibi_module_registry::Consumer::new::<Searchable>(SOURCES, "base/search", model::validate) }
inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/search", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/grid", "base/settings", "base/datetime", "base/locale", "base/ui", "base/agent_api"],
        migrations: &[],
        services: &[
            bilibi_module_registry::Service { id: SERVICE, factory: |services| {
                let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
                let search = Arc::new(Search {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    settings: services.get("base/settings.store")?,
                    boundary: services.get("base/database.mutations")?,
                    kinds: model::installed(&catalog)?,
                });
                BOUND.set(search.clone()).map_err(|_| RegistryError::Service("duplicate search binding".into()))?;
                Ok(search)
            }},
            bilibi_module_registry::Service { id: contributions::PAGES, factory: |services| {
                Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages::new(
                    services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    services.get(SERVICE)?,
                    services.get("base/datetime.display")?,
                    services.get("base/locale.locales")?,
                )))))
            }},
        ],
    }
}
