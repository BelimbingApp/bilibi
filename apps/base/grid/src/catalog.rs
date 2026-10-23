//! The tables one account may read, and the paths between them.
//!
//! `Catalog::for_scope` keeps the installed tables whose capability the actor
//! holds and, within them, only the fields no operator restricted for a role the
//! actor holds. Every later call takes the catalog, not the scope: a path is
//! resolved against it, so a table or field it left out cannot be reached by any
//! spelling. Build it again wherever the page refreshes its own capabilities.
use crate::column::{self, Agg, Column, ColumnError, MAX_SPEC};
use crate::model::{Field, FieldType, Installed, Join, LinkKind, Source, Table};
use bilibi_database::Error;
use bilibi_tenancy::Scope;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::{Arc, Mutex, OnceLock},
};
use tokio_postgres::{types::Type, GenericClient};

pub const MAX_COLUMNS: usize = 32;
pub const MAX_SUGGESTIONS: usize = 50;
const DEFAULT_SUGGESTIONS: usize = 12;
const MAX_SUGGEST_TEXT: usize = 100;
const MAX_TOKENS: usize = 6;
const MAX_CANDIDATES: usize = 4096;
const WALK_DEPTH: usize = 3;

/// A contributor defect or a database failure while building a catalog.
#[derive(Debug)]
pub enum CatalogError {
    /// A source does not select what its table declared. The owner must fix it.
    Misdeclared(String),
    Database(Error),
}
impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Misdeclared(why) => write!(f, "invalid grid declaration: {why}"),
            Self::Database(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for CatalogError {}
impl From<Error> for CatalogError {
    fn from(error: Error) -> Self {
        Self::Database(error)
    }
}
impl From<tokio_postgres::Error> for CatalogError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(error.into())
    }
}
impl From<CatalogError> for Error {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::Database(error) => error,
            CatalogError::Misdeclared(why) => {
                eprintln!("grid: {why}");
                Error::Internal
            }
        }
    }
}

/// The statement shape a source was found to have: selected keys, never scope data.
type Keys = Arc<Vec<String>>;
fn keys_cache() -> &'static Mutex<HashMap<&'static str, Keys>> {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Keys>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The keys a static statement selects, read from the server's description of it
/// (nothing runs) and kept for the process: the SQL text is static, so they
/// cannot change. Every placeholder must be a bigint the grid can bind.
async fn selected_keys(
    client: &impl GenericClient,
    what: &str,
    source: &Source,
) -> Result<Keys, CatalogError> {
    let cache_key = source.sql;
    if let Some(keys) = keys_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(cache_key).cloned())
    {
        return Ok(keys);
    }
    let statement = client.prepare(source.sql).await.map_err(|error| {
        CatalogError::Misdeclared(format!("{what}: the server refused the source: {error}"))
    })?;
    let expected = if source.tenant_scoped { 1..=2 } else { 0..=0 };
    if !expected.contains(&statement.params().len())
        || statement.params().iter().any(|ty| *ty != Type::INT8)
    {
        return Err(CatalogError::Misdeclared(format!(
            "{what}: a tenant-scoped source takes bigint $1 (tenant) and optionally $2 (user); a global one none"
        )));
    }
    let keys: Keys = Arc::new(
        statement
            .columns()
            .iter()
            .map(|column| column.name().to_string())
            .collect(),
    );
    if let Ok(mut cache) = keys_cache().lock() {
        cache.insert(cache_key, keys.clone());
    }
    Ok(keys)
}

/// What one account may read: the visible tables with restricted fields removed.
#[derive(Clone, Debug)]
pub struct Catalog {
    tenant: i64,
    user: i64,
    tables: BTreeMap<&'static str, Table>,
    /// Fields this account sees masked (ADR 05), by table, with their kinds.
    masks: BTreeMap<&'static str, BTreeMap<String, &'static str>>,
}
impl Catalog {
    /// The catalog this scope's actor may read, built from the installed snapshot
    /// with a fresh read of grants and field restrictions. A source is described to
    /// the server once per process to settle which of its keys each field reads.
    pub async fn for_scope(
        client: &impl GenericClient,
        scope: &Scope,
    ) -> Result<Self, CatalogError> {
        Self::build(client, scope, None).await
    }
    /// [`Catalog::for_scope`] with one masked field of one table unmasked, for
    /// a reveal the caller has already authorized and recorded.
    pub(crate) async fn revealing(
        client: &impl GenericClient,
        scope: &Scope,
        table: &str,
        field: &str,
    ) -> Result<Self, CatalogError> {
        Self::build(client, scope, Some((table, field))).await
    }
    async fn build(
        client: &impl GenericClient,
        scope: &Scope,
        reveal: Option<(&str, &str)>,
    ) -> Result<Self, CatalogError> {
        let installed = crate::installed().map_err(|_| Error::Internal)?;
        let allowed = bilibi_authz::effective_capabilities(client, scope).await?;
        let restricted = bilibi_authz::fields::restricted(client, scope).await?;
        let masked = bilibi_authz::fields::masked(client, scope).await?;
        let mut tables = BTreeMap::new();
        let mut masks = BTreeMap::new();
        for (id, table) in installed.map() {
            if !allowed.contains(table.capability()) {
                continue;
            }
            let mut own = masked.get(*id).cloned().unwrap_or_default();
            if let Some((_, field)) = reveal.filter(|(table, _)| table == id) {
                own.remove(field);
            }
            // Protected fields are never offered for restriction, so a stale row
            // naming one is inert here (record pages still mask by name). A
            // masked field stays, and its values are masked when projected.
            let hidden: BTreeSet<&str> = restricted
                .get(*id)
                .into_iter()
                .flatten()
                .map(String::as_str)
                .filter(|field| !table.is_protected(field))
                .filter(|field| !masked.get(*id).is_some_and(|m| m.contains_key(*field)))
                .collect();
            tables.insert(*id, table.without(&hidden));
            if !own.is_empty() {
                masks.insert(*id, own);
            }
        }
        let visible: BTreeSet<&str> = tables.keys().copied().collect();
        for table in tables.values_mut() {
            table.retain_links(|id| visible.contains(id));
        }
        let catalog = Self {
            tenant: scope.tenant(),
            user: scope.user(),
            tables,
            masks,
        };
        catalog.verify_sources(client).await?;
        Ok(catalog)
    }

    // A field no key answers to is refused, naming the field, before any statement
    // is built from it. An edge is checked once both its ends are visible.
    async fn verify_sources(&self, client: &impl GenericClient) -> Result<(), CatalogError> {
        for table in self.tables.values() {
            let what = format!("table {} from {}", table.id(), table.owner);
            let keys = selected_keys(client, &what, &table.source()).await?;
            for field in table.fields() {
                if !keys.iter().any(|key| key == field.source_key()) {
                    return Err(CatalogError::Misdeclared(format!(
                        "{what}: field {} names no key its source selects; it selects {keys:?}",
                        field.id
                    )));
                }
            }
            for link in table.links() {
                if let Join::Edge(source) = link.decl.join {
                    {
                        let what = format!("link {}.{} from {}", table.id(), link.id(), link.owner);
                        let keys = selected_keys(client, &what, &source).await?;
                        if !["from_key", "to_key"]
                            .iter()
                            .all(|name| keys.iter().any(|key| key == name))
                        {
                            return Err(CatalogError::Misdeclared(format!(
                                "{what}: an edge selects from_key and to_key; it selects {keys:?}"
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn tenant(&self) -> i64 {
        self.tenant
    }
    pub fn user(&self) -> i64 {
        self.user
    }
    /// The personal kind a column's values are masked by, when it reads a
    /// masked field's value (a value, list, minimum or maximum). A count, sum
    /// or average of it shows no value and is not masked.
    pub fn mask(&self, column: &Column) -> Option<&'static str> {
        if matches!(
            column.agg,
            Some(crate::column::Agg::Count | crate::column::Agg::Sum | crate::column::Agg::Avg)
        ) {
            return None;
        }
        let field = column.field.as_ref()?;
        self.masks.get(column.table)?.get(field.id).copied()
    }
    /// Whether this account sees `field` of `table` masked.
    pub fn masked(&self, table: &str, field: &str) -> bool {
        self.masks.get(table).is_some_and(|m| m.contains_key(field))
    }
    pub fn tables(&self) -> impl Iterator<Item = &Table> {
        self.tables.values()
    }
    /// A visible table by id.
    pub fn table(&self, id: &str) -> Option<&Table> {
        self.tables.get(id)
    }

    /// Resolves one column spec against a root table of this catalog.
    pub fn resolve(&self, root: &Table, spec: &str) -> Result<Column, ColumnError> {
        column::resolve(spec, root, &|id| self.table(id))
    }

    /// Resolves every spec, or reports the first that fails. Duplicates collapse to
    /// one column in first-seen order; at most `MAX_COLUMNS` are accepted.
    pub fn resolve_all<S: AsRef<str>>(
        &self,
        root: &Table,
        specs: &[S],
    ) -> Result<Vec<Column>, (String, ColumnError)> {
        let mut seen = BTreeSet::new();
        let mut columns = Vec::new();
        for spec in specs {
            let spec = spec.as_ref();
            if !seen.insert(spec) {
                continue;
            }
            if columns.len() == MAX_COLUMNS {
                return Err((truncated(spec), ColumnError::TooManyColumns));
            }
            match self.resolve(root, spec) {
                Ok(column) => columns.push(column),
                Err(error) => return Err((truncated(spec), error)),
            }
        }
        Ok(columns)
    }

    /// Columns a person could add, ranked against what they typed. Walks paths of up
    /// to three links from the root through visible tables only, offering each
    /// reachable field plus the rollups a many-link allows: a count, the numeric
    /// aggregates of number fields, min/max of dates, `list` of text and enum
    /// fields, and `latest` where the target table has a time field. Empty text
    /// ranks by path depth, so the root's own fields come first. Bounded by
    /// `MAX_SUGGEST_TEXT`, `MAX_TOKENS`, `MAX_CANDIDATES` and `MAX_SUGGESTIONS`.
    pub fn suggest(&self, root: &Table, text: &str, exclude: &[&str], limit: usize) -> Vec<Column> {
        let limit = if limit == 0 {
            DEFAULT_SUGGESTIONS
        } else {
            limit
        }
        .min(MAX_SUGGESTIONS);
        let text: String = text.chars().take(MAX_SUGGEST_TEXT).collect();
        let tokens: Vec<String> = text
            .to_lowercase()
            .split(|c: char| c.is_whitespace() || ".:>›".contains(c))
            .filter(|token| !token.is_empty())
            .take(MAX_TOKENS)
            .map(str::to_string)
            .collect();
        let mut specs = field_specs(root, "");
        self.link_specs(root, "", 1, &mut specs);
        let mut ranked: Vec<_> = specs
            .into_iter()
            .filter(|spec| !exclude.contains(&spec.as_str()))
            .filter_map(|spec| self.resolve(root, &spec).ok())
            .map(|column| (score(&column, &tokens), column))
            .filter(|(score, _)| tokens.is_empty() || *score > 0)
            .collect();
        ranked.sort_by(|(a, left), (b, right)| {
            b.cmp(a)
                .then(left.depth.cmp(&right.depth))
                .then_with(|| left.label.cmp(&right.label))
        });
        ranked.into_iter().take(limit).map(|(_, c)| c).collect()
    }

    fn link_specs(&self, table: &Table, prefix: &str, depth: usize, out: &mut Vec<String>) {
        if depth > WALK_DEPTH {
            return;
        }
        for link in table.links() {
            if out.len() >= MAX_CANDIDATES {
                return;
            }
            let Some(target) = self.table(link.to()) else {
                continue;
            };
            let path = format!("{prefix}{}", link.id());
            match link.kind() {
                LinkKind::One => {
                    out.extend(field_specs(target, &format!("{path}.")));
                    self.link_specs(target, &format!("{path}."), depth + 1, out);
                }
                LinkKind::Many => rollup_specs(target, &path, out),
            }
        }
    }
}

fn truncated(spec: &str) -> String {
    spec.chars().take(MAX_SPEC).collect()
}
fn field_specs(table: &Table, prefix: &str) -> Vec<String> {
    table
        .visible_fields()
        .map(|field| format!("{prefix}{}", field.id))
        .collect()
}
fn rollup_specs(target: &Table, path: &str, out: &mut Vec<String>) {
    out.push(format!("{path}:count"));
    for field in target.visible_fields() {
        let id = field.id;
        match field.ty {
            FieldType::Integer | FieldType::Float | FieldType::Decimal if id != target.key() => {
                out.extend(
                    [Agg::Sum, Agg::Avg, Agg::Min, Agg::Max]
                        .map(|agg| format!("{path}.{id}:{}", agg.key())),
                );
            }
            FieldType::Date | FieldType::DateTime => {
                out.extend([Agg::Min, Agg::Max].map(|agg| format!("{path}.{id}:{}", agg.key())))
            }
            FieldType::String | FieldType::Enum => out.push(format!("{path}.{id}:list")),
            _ => {}
        }
        if target.time_field().is_some() && id != target.key() {
            out.push(format!("{path}.{id}:latest"));
        }
    }
}

// Every token must be found somewhere in the label or the spec; a match on the
// field's own name counts more than one on a link it is reached through.
fn score(column: &Column, tokens: &[String]) -> usize {
    if tokens.is_empty() {
        return 1;
    }
    let haystack = format!("{} {}", column.label, column.spec).to_lowercase();
    let short = column.short_label.to_lowercase();
    if !tokens.iter().all(|token| haystack.contains(token.as_str())) {
        return 0;
    }
    tokens.iter().fold(1, |score, token| {
        score
            + if short.starts_with(token.as_str()) {
                4
            } else if short.contains(token.as_str()) {
                2
            } else {
                1
            }
    })
}

/// What an operator may restrict, supplied as data for the Authz screens that must
/// not depend on Grid: every installed table with its non-protected, non-hidden
/// fields and the audit record types it covers.
#[derive(Clone, Debug)]
pub struct Restrictable {
    pub table: &'static str,
    pub label: &'static str,
    pub record_types: &'static [&'static str],
    /// Id, label and personal kind.
    pub fields: Vec<(&'static str, &'static str, Option<&'static str>)>,
}
pub fn restriction_catalog(installed: &Installed) -> Vec<Restrictable> {
    installed
        .tables()
        .map(|table| Restrictable {
            table: table.id(),
            label: table.label(),
            record_types: table.decl.record_types,
            fields: table
                .visible_fields()
                .filter(|field: &&Field| !table.is_protected(field.id))
                .map(|field| (field.id, field.label, field.personal.map(|kind| kind.key())))
                .collect(),
        })
        .filter(|entry| !entry.fields.is_empty())
        .collect()
}
/// Whether `field` of `table` may be restricted: used to validate a bulk write
/// atomically before any row changes.
pub fn restrictable(installed: &Installed, table: &str, field: &str) -> bool {
    installed.table(table).is_some_and(|table| {
        table
            .field(field)
            .is_some_and(|f| !f.hidden && !table.is_protected(f.id))
    })
}
