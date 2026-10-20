//! Contribution data and the validated catalog it becomes. Contributors declare
//! static tables, fields and links; nothing here queries a database, and no part
//! of a path or SQL identifier ever comes from a client.
use bilibi_module_registry::{Entry, Provenance, RegistryError, Result};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_TABLES: usize = 512;
pub const MAX_FIELDS: usize = 64;
pub const MAX_LINKS_PER_TABLE: usize = 64;
const MAX_ID: usize = 64;
const MAX_LABEL: usize = 120;
const MAX_SQL: usize = 20_000;

/// Field types a contribution may declare.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldType {
    Integer,
    Float,
    Decimal,
    String,
    Boolean,
    Date,
    DateTime,
    Enum,
}
impl FieldType {
    /// Whether values order on a number line (bars, bands and heat).
    pub fn numeric(self) -> bool {
        matches!(
            self,
            Self::Integer | Self::Float | Self::Decimal | Self::Date | Self::DateTime
        )
    }
    /// Whether sum/avg make sense.
    pub fn number(self) -> bool {
        matches!(self, Self::Integer | Self::Float | Self::Decimal)
    }
    pub fn dated(self) -> bool {
        matches!(self, Self::Date | Self::DateTime)
    }
}

/// One value a column can show. `column` is the key the owner's source selects it
/// under; it defaults to the field id. `hidden` fields exist for links to join on
/// and never appear as columns. `protected` marks a field no operator may
/// restrict because the owner's own logic needs every reader to see it. A table's
/// key, label and time fields, hidden fields and join fields are protected
/// without saying so.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub id: &'static str,
    pub label: &'static str,
    pub ty: FieldType,
    pub column: Option<&'static str>,
    pub hidden: bool,
    /// The closed set an enum field takes, which the band lens colours.
    pub values: &'static [&'static str],
    pub protected: bool,
}
impl Field {
    pub const fn new(id: &'static str, label: &'static str, ty: FieldType) -> Self {
        Self {
            id,
            label,
            ty,
            column: None,
            hidden: false,
            values: &[],
            protected: false,
        }
    }
    pub const fn choice(
        id: &'static str,
        label: &'static str,
        values: &'static [&'static str],
    ) -> Self {
        Self {
            values,
            ..Self::new(id, label, FieldType::Enum)
        }
    }
    pub const fn column(self, column: &'static str) -> Self {
        Self {
            column: Some(column),
            ..self
        }
    }
    pub const fn hidden(self) -> Self {
        Self {
            hidden: true,
            ..self
        }
    }
    pub const fn protected(self) -> Self {
        Self {
            protected: true,
            ..self
        }
    }
    /// The key of the owner's source this field is read from.
    pub fn source_key(&self) -> &'static str {
        self.column.unwrap_or(self.id)
    }
}

/// The owner's statement for the rows a scope may read. It is static SQL that the
/// grid wraps in a subquery, so every hop carries its owner's boundary and the
/// grid adds no tenant predicate of its own. A tenant-scoped source takes the
/// tenant id as `$1` (bigint) and may take the actor's user id as `$2`; a global
/// source (reference data with no tenant) takes none. It must expose every
/// declared field under its `column` key, and the keys it selects must not
/// depend on the scope.
#[derive(Clone, Copy, Debug)]
pub struct Source {
    pub sql: &'static str,
    pub tenant_scoped: bool,
}
impl Source {
    pub const fn tenant(sql: &'static str) -> Self {
        Self {
            sql,
            tenant_scoped: true,
        }
    }
    pub const fn global(sql: &'static str) -> Self {
        Self {
            sql,
            tenant_scoped: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TableDecl {
    pub id: &'static str,
    pub label: &'static str,
    /// What an account needs to read any row; checked before a path may touch it.
    pub capability: &'static str,
    pub source: Source,
    /// The field that identifies a row.
    pub key: &'static str,
    /// The field that names a row to a person.
    pub label_field: Option<&'static str>,
    /// The date field a rollup orders by for "latest" and buckets by for a trend.
    pub time_field: Option<&'static str>,
    pub fields: &'static [Field],
    /// The audit `auditable_type` values rows are recorded under, so a field
    /// restriction reaches the audit views of those rows. One table claims a type.
    pub record_types: &'static [&'static str],
}
impl TableDecl {
    pub const fn new(
        id: &'static str,
        label: &'static str,
        capability: &'static str,
        source: Source,
        key: &'static str,
        fields: &'static [Field],
    ) -> Self {
        Self {
            id,
            label,
            capability,
            source,
            key,
            label_field: None,
            time_field: None,
            fields,
            record_types: &[],
        }
    }
    pub const fn label_field(self, field: &'static str) -> Self {
        Self {
            label_field: Some(field),
            ..self
        }
    }
    pub const fn time_field(self, field: &'static str) -> Self {
        Self {
            time_field: Some(field),
            ..self
        }
    }
    pub const fn record_types(self, types: &'static [&'static str]) -> Self {
        Self {
            record_types: types,
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkKind {
    /// At most one row of `to` per row of `from`; its fields become plain columns.
    One,
    /// Several rows, rolled up into one cell.
    Many,
}
/// How a link joins: field ids on the two tables, or an edge query the declaring
/// module owns, selecting `from_key` and `to_key`. A one-link's edge must yield at
/// most one `to_key` per `from_key`, or joined rows multiply.
#[derive(Clone, Copy, Debug)]
pub enum Join {
    On {
        from_field: &'static str,
        to_field: &'static str,
    },
    Edge(Source),
}
/// A walkable relation. The declaring module need not own `from`: the module that
/// owns the edge is the one that knows it. It must own one end.
#[derive(Clone, Copy, Debug)]
pub struct LinkDecl {
    pub id: &'static str,
    pub label: &'static str,
    pub from: &'static str,
    pub to: &'static str,
    pub kind: LinkKind,
    pub join: Join,
}
impl LinkDecl {
    pub const fn on(
        id: &'static str,
        label: &'static str,
        kind: LinkKind,
        from: &'static str,
        from_field: &'static str,
        to: &'static str,
        to_field: &'static str,
    ) -> Self {
        Self {
            id,
            label,
            from,
            to,
            kind,
            join: Join::On {
                from_field,
                to_field,
            },
        }
    }
    pub const fn edge(
        id: &'static str,
        label: &'static str,
        kind: LinkKind,
        from: &'static str,
        to: &'static str,
        source: Source,
    ) -> Self {
        Self {
            id,
            label,
            from,
            to,
            kind,
            join: Join::Edge(source),
        }
    }
}

/// The payload a module contributes to `base/grid.tables`.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordFields {
    pub table: &'static str,
    pub fields: &'static [bilibi_ui::RecordField],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GridContribution {
    /// The same schema used to label the owning detail page, including panels.
    pub record_fields: &'static [RecordFields],
    pub tables: &'static [TableDecl],
    pub links: &'static [LinkDecl],
}

/// A validated link with the module that declared it.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub decl: &'static LinkDecl,
    pub owner: &'static str,
}
impl Link {
    pub fn id(&self) -> &'static str {
        self.decl.id
    }
    pub fn label(&self) -> &'static str {
        self.decl.label
    }
    pub fn kind(&self) -> LinkKind {
        self.decl.kind
    }
    pub fn to(&self) -> &'static str {
        self.decl.to
    }
}

/// A validated table. A scoped catalog holds copies with restricted fields removed.
#[derive(Clone, Debug)]
pub struct Table {
    pub decl: &'static TableDecl,
    pub owner: &'static str,
    pub provenance: Provenance,
    pub(crate) record_fields: Vec<bilibi_ui::RecordField>,
    fields: Vec<Field>,
    links: Vec<Link>,
    protected: BTreeSet<&'static str>,
}
impl Table {
    pub fn id(&self) -> &'static str {
        self.decl.id
    }
    pub fn label(&self) -> &'static str {
        self.decl.label
    }
    pub fn key(&self) -> &'static str {
        self.decl.key
    }
    pub fn time_field(&self) -> Option<&'static str> {
        self.decl.time_field
    }
    pub fn capability(&self) -> &'static str {
        self.decl.capability
    }
    pub fn source(&self) -> Source {
        self.decl.source
    }
    /// Declared fields in declaration order, hidden ones included.
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }
    pub fn field(&self, id: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.id == id)
    }
    /// The fields a column may show: declaration order, without hidden ones.
    pub fn visible_fields(&self) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(|field| !field.hidden)
    }
    /// Links starting at this table, in declaration order.
    pub fn links(&self) -> &[Link] {
        &self.links
    }
    pub fn link(&self, id: &str) -> Option<&Link> {
        self.links.iter().find(|link| link.decl.id == id)
    }
    pub fn key_field(&self) -> &Field {
        self.field(self.decl.key).expect("validated key")
    }
    /// Whether an operator may never restrict the field.
    pub fn is_protected(&self, field: &str) -> bool {
        self.protected.contains(field)
    }
    /// A link to a table the actor cannot read does not exist for them: a path
    /// through it is refused exactly as an unknown one is, revealing nothing.
    pub(crate) fn retain_links(&mut self, visible: impl Fn(&str) -> bool) {
        self.links.retain(|link| visible(link.to()));
    }
    pub(crate) fn without(&self, restricted: &BTreeSet<&str>) -> Self {
        let mut copy = self.clone();
        copy.fields.retain(|field| !restricted.contains(field.id));
        copy
    }
}

fn invalid(message: String) -> RegistryError {
    RegistryError::Contribution(message)
}
fn where_(provenance: &Provenance) -> String {
    format!(
        "{} ({}:{})",
        provenance.module, provenance.source, provenance.line
    )
}
pub fn id_valid(id: &str) -> bool {
    let mut chars = id.bytes();
    id.len() <= MAX_ID
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}
fn label_valid(label: &str) -> bool {
    !label.trim().is_empty() && label.len() <= MAX_LABEL
}
fn source_valid(source: &Source) -> bool {
    let sql = source.sql.trim();
    let head = sql
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    !sql.is_empty()
        && sql.len() <= MAX_SQL
        && (head == "select" || head == "with")
        && !sql.contains(';')
        && source.sql.contains("$1") == source.tenant_scoped
}

/// Every installed table, validated into one catalog.
#[derive(Debug, Default)]
pub struct Installed {
    tables: BTreeMap<&'static str, Table>,
}
impl Installed {
    /// Validates the complete sorted set. Any defect fails the snapshot build, so a
    /// broken catalog never reaches a page. Nothing here runs a source.
    pub fn from_entries(entries: &[Entry]) -> Result<Self> {
        let mut tables: BTreeMap<&'static str, Table> = BTreeMap::new();
        let mut pending = Vec::new();
        let mut types: BTreeMap<&str, &str> = BTreeMap::new();
        for entry in entries {
            let contribution = entry.get::<GridContribution>()?;
            let owner = entry.provenance.module;
            let at = where_(&entry.provenance);
            for decl in contribution.tables {
                let table = Table::new(decl, owner, entry.provenance, &at)?;
                for kind in decl.record_types {
                    if let Some(first) = types.insert(kind, decl.id) {
                        return Err(invalid(format!(
                            "grid record type {kind} claimed by {first} and {} from {at}",
                            decl.id
                        )));
                    }
                }
                if tables.insert(decl.id, table).is_some() {
                    return Err(invalid(format!(
                        "duplicate grid table {} from {at}",
                        decl.id
                    )));
                }
                if tables.len() > MAX_TABLES {
                    return Err(invalid(format!("more than {MAX_TABLES} grid tables")));
                }
            }
            pending.extend(
                contribution
                    .links
                    .iter()
                    .map(|link| (link, entry.provenance)),
            );
        }
        for (link, provenance) in pending {
            link_into(&mut tables, link, &provenance)?;
        }
        for entry in entries {
            let contribution = entry.get::<GridContribution>()?;
            for schema in contribution.record_fields {
                let table = tables.get_mut(schema.table).ok_or_else(|| {
                    invalid(format!("record fields name unknown table {}", schema.table))
                })?;
                if table.owner != entry.provenance.module
                    && !contribution
                        .links
                        .iter()
                        .any(|link| link.from == schema.table)
                {
                    return Err(invalid(format!(
                        "record fields for {} must own the table or a panel link",
                        schema.table
                    )));
                }
                for field in schema.fields {
                    if field.id.is_empty()
                        || field.label.is_empty()
                        || field.column.is_none() && field.exclusion.is_none()
                    {
                        return Err(invalid(format!(
                            "invalid record field for {}",
                            schema.table
                        )));
                    }
                    if let Some(column) = field.column {
                        if table
                            .record_fields
                            .iter()
                            .any(|previous| previous.column == Some(column))
                        {
                            return Err(invalid(format!(
                                "duplicate record column {column} on {}",
                                schema.table
                            )));
                        }
                    }
                    table.record_fields.push(*field);
                }
            }
        }
        for table in tables.values() {
            for field in &table.record_fields {
                if let Some(path) = field.column {
                    crate::column::resolve(path, table, &|id| tables.get(id)).map_err(|error| {
                        invalid(format!(
                            "record field {} on {}: {error}",
                            field.id,
                            table.id()
                        ))
                    })?;
                }
            }
        }
        Ok(Self { tables })
    }
    pub fn from_inventory() -> Result<Self> {
        let catalog =
            bilibi_module_registry::Catalog::discover(&bilibi_module_registry::discover()?)?;
        Self::from_entries(catalog.entries::<GridContribution>(crate::TABLES)?)
    }
    pub fn tables(&self) -> impl Iterator<Item = &Table> {
        self.tables.values()
    }
    pub fn table(&self, id: &str) -> Option<&Table> {
        self.tables.get(id)
    }
    pub(crate) fn map(&self) -> &BTreeMap<&'static str, Table> {
        &self.tables
    }
}

impl Table {
    fn new(
        decl: &'static TableDecl,
        owner: &'static str,
        provenance: Provenance,
        at: &str,
    ) -> Result<Self> {
        let bad = |why: String| invalid(format!("grid table {} from {at}: {why}", decl.id));
        if !id_valid(decl.id) || !label_valid(decl.label) {
            return Err(bad("invalid id or label".into()));
        }
        if !bilibi_authz::vocabulary::valid_key(decl.capability) {
            return Err(bad(format!("invalid capability key {}", decl.capability)));
        }
        if !source_valid(&decl.source) {
            return Err(bad(
                "source must be one SELECT/WITH statement that takes $1 exactly when tenant-scoped"
                    .into(),
            ));
        }
        if decl.fields.is_empty() || decl.fields.len() > MAX_FIELDS {
            return Err(bad(format!("declare 1 to {MAX_FIELDS} fields")));
        }
        let mut seen = BTreeSet::new();
        for field in decl.fields {
            if !id_valid(field.id)
                || !label_valid(field.label)
                || !seen.insert(field.id)
                || field.column.is_some_and(|column| !id_valid(column))
            {
                return Err(bad(format!("invalid or duplicate field {}", field.id)));
            }
            let mut values = BTreeSet::new();
            let closed = field.ty == FieldType::Enum;
            if closed != !field.values.is_empty()
                || !field
                    .values
                    .iter()
                    .all(|v| label_valid(v) && values.insert(*v))
            {
                return Err(bad(format!(
                    "field {} needs distinct enum values exactly when it is an enum",
                    field.id
                )));
            }
        }
        for (name, id) in [
            ("key", Some(decl.key)),
            ("label field", decl.label_field),
            ("time field", decl.time_field),
        ] {
            if id.is_some_and(|id| !seen.contains(id)) {
                return Err(bad(format!("{name} is not a declared field")));
            }
        }
        if decl
            .time_field
            .and_then(|id| decl.fields.iter().find(|f| f.id == id))
            .is_some_and(|field| !field.ty.dated())
        {
            return Err(bad("time field must be a date or datetime".into()));
        }
        let mut kinds = BTreeSet::new();
        if !decl
            .record_types
            .iter()
            .all(|kind| !kind.is_empty() && kind.len() <= 200 && kinds.insert(*kind))
        {
            return Err(bad("record types must be distinct and non-empty".into()));
        }
        let mut protected: BTreeSet<&'static str> =
            [Some(decl.key), decl.label_field, decl.time_field]
                .into_iter()
                .flatten()
                .collect();
        for field in decl.fields {
            if field.hidden || field.protected {
                protected.insert(field.id);
            }
        }
        Ok(Self {
            decl,
            record_fields: Vec::new(),
            owner,
            provenance,
            fields: decl.fields.to_vec(),
            links: Vec::new(),
            protected,
        })
    }
}

fn link_into(
    tables: &mut BTreeMap<&'static str, Table>,
    decl: &'static LinkDecl,
    provenance: &Provenance,
) -> Result<()> {
    let at = where_(provenance);
    let bad = |why: String| invalid(format!("grid link {} from {at}: {why}", decl.id));
    if !id_valid(decl.id) || !label_valid(decl.label) {
        return Err(bad("invalid id or label".into()));
    }
    let from = tables.get(decl.from).ok_or_else(|| {
        bad(format!(
            "names table {}, which no module declares",
            decl.from
        ))
    })?;
    let to = tables
        .get(decl.to)
        .ok_or_else(|| bad(format!("names table {}, which no module declares", decl.to)))?;
    // The module that owns the edge knows it; one that owns neither end has no
    // standing to put a link between two other modules' tables.
    if from.owner != provenance.module && to.owner != provenance.module {
        return Err(bad(format!(
            "{} owns neither {} nor {}",
            provenance.module, decl.from, decl.to
        )));
    }
    if from.link(decl.id).is_some() || from.field(decl.id).is_some() {
        return Err(bad(format!(
            "id is already a field or link of {}",
            decl.from
        )));
    }
    if from.links.len() >= MAX_LINKS_PER_TABLE {
        return Err(bad(format!("{} has too many links", decl.from)));
    }
    let mut joined = Vec::new();
    match decl.join {
        Join::On {
            from_field,
            to_field,
        } => {
            if from.field(from_field).is_none() || to.field(to_field).is_none() {
                return Err(bad(format!(
                    "joins on {}.{from_field} and {}.{to_field}, which must both be declared",
                    decl.from, decl.to
                )));
            }
            if decl.kind == LinkKind::One && to_field != to.decl.key {
                return Err(bad(format!(
                    "a one-link joins on the target key {}, not {to_field}",
                    to.decl.key
                )));
            }
            joined.push((decl.from, from_field));
            joined.push((decl.to, to_field));
        }
        Join::Edge(source) => {
            if !source_valid(&source) || !source.tenant_scoped {
                return Err(bad(
                    "an edge must be one tenant-scoped SELECT/WITH statement".into(),
                ));
            }
        }
    }
    let link = Link {
        decl,
        owner: provenance.module,
    };
    tables.get_mut(decl.from).expect("checked").links.push(link);
    for (table, field) in joined {
        tables
            .get_mut(table)
            .expect("checked")
            .protected
            .insert(field);
    }
    Ok(())
}
