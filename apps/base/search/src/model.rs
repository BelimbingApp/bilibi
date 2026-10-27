//! What an owner declares to make its Grid table searchable, and the checked
//! set of kinds the program searches. Declarations are static data: nothing
//! here reads a database, and no identifier ever comes from a request.
use bilibi_grid::{FieldType, Table};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Catalog, Entry, Provenance, RegistryError, Result,
};
use std::{collections::BTreeSet, sync::OnceLock};

const MAX_SUBTITLE: usize = 2;
const MAX_PREFIX: usize = 4;
const MAX_KINDS: usize = 64;
const MAX_VALUE: usize = 64;

/// One owner's Grid table made searchable (`bilibi_search::contribute!`).
/// The owner's Grid source selects `vector`, a stored generated `tsvector`
/// built only from fields operators cannot restrict, declared as a hidden
/// Grid field and indexed with GIN in the owner's migration.
#[derive(Clone, Copy, Debug)]
pub struct Searchable {
    /// The Grid table id; it is also the kind's id in settings and results.
    pub table: &'static str,
    /// The field a hit is named by; operators cannot restrict it.
    pub title: &'static str,
    /// Up to two fields shown under the title, each left out for a reader
    /// whose role restricts it.
    pub subtitle: &'static [&'static str],
    /// The hidden Grid field holding the stored `tsvector`.
    pub vector: &'static str,
    /// Up to four text fields that also match when they start with the
    /// query (a personal email or phone). A restrictable one takes part
    /// only for a reader no held role restricts.
    pub prefix: &'static [&'static str],
    /// The record's page, a GET route of the owner with `{id}` for the key:
    /// a hit is offered only to a reader that route admits.
    pub link: &'static str,
    /// Marks the records that are no longer current, which stay findable,
    /// ranked after current ones and labelled with their state.
    pub state: Option<State>,
    /// The date or time the results page shows as Updated.
    pub updated: Option<&'static str>,
    /// Whether the kind is searched until an operator says otherwise.
    pub default_on: bool,
}

/// A field whose listed values mean the record is retired or archived.
#[derive(Clone, Copy, Debug)]
pub struct State {
    pub field: &'static str,
    pub inactive: &'static [&'static str],
}

impl Searchable {
    pub const fn new(
        table: &'static str,
        title: &'static str,
        vector: &'static str,
        link: &'static str,
    ) -> Self {
        Self {
            table,
            title,
            subtitle: &[],
            vector,
            prefix: &[],
            link,
            state: None,
            updated: None,
            default_on: true,
        }
    }
    pub const fn subtitle(self, fields: &'static [&'static str]) -> Self {
        Self {
            subtitle: fields,
            ..self
        }
    }
    pub const fn prefix(self, fields: &'static [&'static str]) -> Self {
        Self {
            prefix: fields,
            ..self
        }
    }
    pub const fn state(self, field: &'static str, inactive: &'static [&'static str]) -> Self {
        Self {
            state: Some(State { field, inactive }),
            ..self
        }
    }
    pub const fn updated(self, field: &'static str) -> Self {
        Self {
            updated: Some(field),
            ..self
        }
    }
    /// Searched only once an operator turns the kind on.
    pub const fn off(self) -> Self {
        Self {
            default_on: false,
            ..self
        }
    }
}

/// One kind the program searches: a checked declaration with what its Grid
/// table and its record page say about it.
#[derive(Clone, Debug)]
pub struct Kind {
    /// The Grid table id (`persons`).
    pub id: &'static str,
    /// The Grid table's label (`Contacts`).
    pub label: &'static str,
    /// The module that owns the table and declared it searchable.
    pub owner: &'static str,
    pub(crate) decl: &'static Searchable,
    /// Who the record page admits; a hit is offered only to them.
    pub(crate) access: Access,
}

impl Kind {
    /// The record's page for its key, which is encoded as one path segment.
    pub fn href(&self, key: &str) -> String {
        let segment: String =
            url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>();
        self.decl
            .link
            .replacen("{id}", &segment.replace('+', "%20"), 1)
    }
}

fn invalid(provenance: &Provenance, why: impl std::fmt::Display) -> RegistryError {
    RegistryError::Contribution(format!(
        "searchable from {} ({}:{}): {why}",
        provenance.module, provenance.source, provenance.line
    ))
}

/// A value an operator's setting or a state may hold: what Grid allows for
/// an id, plus hyphens.
pub(crate) fn value_valid(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= MAX_VALUE
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}

/// The record page pattern: a local path with exactly one `{id}`.
fn link_valid(link: &str) -> bool {
    link.starts_with('/')
        && link.len() <= 200
        && link.matches("{id}").count() == 1
        && link.matches('{').count() == 1
        && link
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_{}".contains(&b))
}

/// The registry validator: each declaration's own shape and one per table.
/// Whether its fields and page exist is checked at boot ([`installed`]), once
/// Grid's catalog and the routes are known.
pub(crate) fn validate(entries: &[Entry]) -> Result<()> {
    let mut tables = BTreeSet::new();
    if entries.len() > MAX_KINDS {
        return Err(RegistryError::Contribution(format!(
            "more than {MAX_KINDS} searchable tables"
        )));
    }
    for entry in entries {
        let decl = entry.get::<Searchable>()?;
        let at = &entry.provenance;
        let fields = [decl.title, decl.vector]
            .into_iter()
            .chain(decl.subtitle.iter().copied())
            .chain(decl.prefix.iter().copied())
            .chain(decl.state.map(|state| state.field))
            .chain(decl.updated);
        if !bilibi_grid::model::id_valid(decl.table) {
            return Err(invalid(at, format!("invalid table id {}", decl.table)));
        }
        if let Some(field) = fields
            .clone()
            .find(|field| !bilibi_grid::model::id_valid(field))
        {
            return Err(invalid(at, format!("invalid field id {field}")));
        }
        if decl.subtitle.len() > MAX_SUBTITLE || decl.prefix.len() > MAX_PREFIX {
            return Err(invalid(
                at,
                format!("at most {MAX_SUBTITLE} subtitle and {MAX_PREFIX} prefix fields"),
            ));
        }
        if !link_valid(decl.link) {
            return Err(invalid(
                at,
                format!("link {} must be a local path with one {{id}}", decl.link),
            ));
        }
        if let Some(state) = decl.state {
            if state.inactive.is_empty() || !state.inactive.iter().all(|value| value_valid(value)) {
                return Err(invalid(
                    at,
                    "state needs inactive values of lowercase letters, digits, _ and -",
                ));
            }
        }
        if !tables.insert(decl.table) {
            return Err(invalid(
                at,
                format!("table {} is already searchable", decl.table),
            ));
        }
    }
    Ok(())
}

static INSTALLED: OnceLock<Vec<Kind>> = OnceLock::new();

/// Every searchable kind, by label, checked once against the Grid table it
/// names and the page it links to. Boot calls it from the facade's factory, so
/// a declaration naming an unknown table, field or page refuses to boot.
pub(crate) fn installed(catalog: &Catalog) -> Result<&'static [Kind]> {
    if let Some(kinds) = INSTALLED.get() {
        return Ok(kinds);
    }
    let grid = bilibi_grid::installed()?;
    let routes = catalog
        .entries::<Route>(ROUTES)?
        .iter()
        .map(Entry::get::<Route>)
        .collect::<Result<Vec<_>>>()?;
    let mut kinds = Vec::new();
    for entry in catalog.entries::<Searchable>(crate::SOURCES)? {
        let decl = entry.get::<Searchable>()?;
        let at = &entry.provenance;
        let table = grid.table(decl.table).ok_or_else(|| {
            invalid(
                at,
                format!("names grid table {}, which no module declares", decl.table),
            )
        })?;
        if table.owner != at.module {
            return Err(invalid(
                at,
                format!("{} does not own grid table {}", at.module, decl.table),
            ));
        }
        check_fields(table, decl).map_err(|why| invalid(at, why))?;
        let access = routes
            .iter()
            .find(|route| route.method == Method::Get && route.path == decl.link)
            .map(|route| route.access)
            .ok_or_else(|| invalid(at, format!("link {} is no page route", decl.link)))?;
        if matches!(access.base(), Access::Public) {
            return Err(invalid(at, format!("link {} is a public page", decl.link)));
        }
        kinds.push(Kind {
            id: decl.table,
            label: table.label(),
            owner: at.module,
            decl,
            access,
        });
    }
    kinds.sort_by_key(|kind| kind.label);
    Ok(INSTALLED.get_or_init(|| kinds))
}

/// The fields a declaration names, as its Grid table declares them: a title
/// and a state no operator can restrict, a hidden vector (never a column),
/// text prefix fields and a dated updated field.
fn check_fields(table: &Table, decl: &Searchable) -> std::result::Result<(), String> {
    let field = |id: &str| {
        table
            .field(id)
            .ok_or_else(|| format!("field {id} is not declared on grid table {}", table.id()))
    };
    let title = field(decl.title)?;
    if title.hidden || !table.is_protected(decl.title) {
        return Err(format!(
            "title {} must be a shown field operators cannot restrict",
            decl.title
        ));
    }
    if !field(decl.vector)?.hidden {
        return Err(format!(
            "vector {} must be a hidden grid field",
            decl.vector
        ));
    }
    for id in decl.subtitle {
        if field(id)?.hidden {
            return Err(format!("subtitle {id} is a hidden field"));
        }
    }
    for id in decl.prefix {
        let prefix = field(id)?;
        if prefix.hidden || prefix.ty != FieldType::String {
            return Err(format!("prefix {id} must be a shown text field"));
        }
    }
    if let Some(state) = decl.state {
        if field(state.field)?.hidden || !table.is_protected(state.field) {
            return Err(format!(
                "state {} must be a shown field operators cannot restrict",
                state.field
            ));
        }
    }
    if let Some(id) = decl.updated {
        if !field(id)?.ty.dated() {
            return Err(format!("updated {id} must be a date or datetime"));
        }
    }
    Ok(())
}
