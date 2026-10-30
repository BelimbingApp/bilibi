//! A column is a path walked from the root table to a field, and, after a
//! many-link, how the reached rows roll up into one cell. The spec is its stable
//! text form, used in URLs and in what an account's list remembers:
//!
//! ```text
//! name                           a field of the root table
//! company.name                   a field reached through one-links
//! employees:count                rows reached through a many-link, counted
//! lines.qty:sum                  a field of those rows, aggregated
//! company.employees.department.status:list   one-links may follow the many-link
//! ```
//!
//! A path holds at most one many-link, and a path through one must end in an
//! aggregate. `count` takes no field; every other aggregate needs one. Parsing is
//! bounded and total: it never panics, allocates from, or indexes anything by a
//! client string beyond the catalog's own tables.
use crate::model::{id_valid, Field, FieldType, Link, LinkKind, Table};
use std::fmt;

pub const MAX_SPEC: usize = 160;
pub const MAX_SEGMENTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Agg {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Latest,
    List,
}
impl Agg {
    pub const ALL: [Agg; 7] = [
        Self::Count,
        Self::Sum,
        Self::Avg,
        Self::Min,
        Self::Max,
        Self::Latest,
        Self::List,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Sum => "sum",
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
            Self::Latest => "latest",
            Self::List => "list",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|agg| agg.key() == key)
    }
    /// The aggregate's word in a label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Count => "Count",
            Self::Sum => "Sum",
            Self::Avg => "Average",
            Self::Min => "Min",
            Self::Max => "Max",
            Self::Latest => "Latest",
            Self::List => "List",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ColumnError {
    Malformed,
    UnknownSegment {
        segment: String,
        table: String,
    },
    ForbiddenTable(String),
    SecondManyLink(String),
    NoField,
    AggregateWithoutManyLink,
    ManyLinkNeedsAggregate,
    AggregateNeedsField,
    CountTakesNoField,
    AggregateNeedsNumber,
    AggregateNeedsOrderedValue,
    LatestNeedsTimeField,
    TooManyColumns,
    /// A sum or average of a field the reader sees masked (ADR 05).
    MaskedAggregate,
}
impl fmt::Display for ColumnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => write!(f, "not a column path"),
            Self::UnknownSegment { segment, table } => {
                write!(f, "{segment} is not a field or link of {table}")
            }
            Self::ForbiddenTable(table) => write!(f, "{table} cannot be reached"),
            Self::SecondManyLink(segment) => {
                write!(f, "{segment} is a second many-link; a path holds one")
            }
            Self::NoField => write!(f, "the path ends at a link, not a field"),
            Self::AggregateWithoutManyLink => write!(f, "only a many-link rolls up"),
            Self::ManyLinkNeedsAggregate => write!(f, "a many-link needs an aggregate"),
            Self::AggregateNeedsField => write!(f, "this aggregate needs a field"),
            Self::CountTakesNoField => write!(f, "count takes no field"),
            Self::AggregateNeedsNumber => write!(f, "sum and avg need a number field"),
            Self::AggregateNeedsOrderedValue => write!(f, "min and max need an ordered value"),
            Self::LatestNeedsTimeField => write!(f, "latest needs a table with a time field"),
            Self::TooManyColumns => write!(f, "too many columns"),
            Self::MaskedAggregate => write!(f, "a masked field is not summed or averaged"),
        }
    }
}
impl std::error::Error for ColumnError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColumnKind {
    Field,
    Rollup,
}

/// The shape of a spec before any catalog is consulted.
#[derive(Debug, Eq, PartialEq)]
pub struct Parsed<'a> {
    pub segments: Vec<&'a str>,
    pub agg: Option<Agg>,
}
/// Bounded parse: at most `MAX_SPEC` bytes, `MAX_SEGMENTS` lowercase identifier
/// segments, and one optional known aggregate after a single colon.
pub fn parse(spec: &str) -> Result<Parsed<'_>, ColumnError> {
    if spec.is_empty() || spec.len() > MAX_SPEC {
        return Err(ColumnError::Malformed);
    }
    let (path, agg) = match spec.split_once(':') {
        Some((path, agg)) => (
            path,
            Some(Agg::from_key(agg).ok_or(ColumnError::Malformed)?),
        ),
        None => (spec, None),
    };
    let segments: Vec<_> = path.split('.').collect();
    if segments.len() > MAX_SEGMENTS || !segments.iter().all(|segment| id_valid(segment)) {
        return Err(ColumnError::Malformed);
    }
    Ok(Parsed { segments, agg })
}
pub fn well_formed(spec: &str) -> bool {
    parse(spec).is_ok()
}

/// A resolved column: the links it walked and what it reads.
#[derive(Clone, Debug)]
pub struct Column {
    pub spec: String,
    pub id: String,
    pub label: String,
    pub short_label: String,
    /// What the cell holds; a count is an integer, an average a float, a list a string.
    pub ty: FieldType,
    pub kind: ColumnKind,
    pub root: &'static str,
    /// The table the last link reaches (the root for an own field).
    pub table: &'static str,
    pub hops: Vec<Link>,
    pub many: Option<Link>,
    pub tail: Vec<Link>,
    pub field: Option<Field>,
    pub agg: Option<Agg>,
    pub depth: usize,
    /// The many-link target's time field when the rollup ends on that target
    /// (no tail): what dates a trend's months and a change-since date.
    pub time_field: Option<&'static str>,
}
impl Column {
    /// Whether values order on a number line (a bar or band scales to it).
    pub fn numeric(&self) -> bool {
        match (self.kind, self.agg) {
            (ColumnKind::Rollup, Some(Agg::Count | Agg::Sum | Agg::Avg)) => true,
            (ColumnKind::Rollup, Some(Agg::Min | Agg::Max)) => {
                self.field.is_some_and(|f| f.ty.numeric())
            }
            (ColumnKind::Rollup, _) => false,
            _ => self.ty.numeric(),
        }
    }
    pub fn expandable(&self) -> bool {
        self.kind == ColumnKind::Rollup
    }
}

/// A DOM-safe id for the spec: dots become hyphens, the colon an underscore.
pub fn dom_id(spec: &str) -> String {
    spec.replace('.', "-").replace(':', "_")
}

#[derive(Default)]
struct Walk {
    hops: Vec<Link>,
    many: Option<Link>,
    tail: Vec<Link>,
    field: Option<Field>,
}

/// Resolves `spec` against `root`, with `fetch` returning a visible table by id.
/// Every table on the path must be visible, so a path through a table the account
/// may not read fails exactly as an unknown one does.
pub fn resolve<'a>(
    spec: &str,
    root: &Table,
    fetch: &dyn Fn(&str) -> Option<&'a Table>,
) -> Result<Column, ColumnError> {
    let parsed = parse(spec)?;
    let mut walk = Walk::default();
    let mut table = root;
    let count = parsed.segments.len();
    for (index, segment) in parsed.segments.iter().enumerate() {
        if let Some(link) = table.link(segment) {
            let target =
                fetch(link.to()).ok_or_else(|| ColumnError::ForbiddenTable(link.to().into()))?;
            match (link.kind(), walk.many.is_some()) {
                (LinkKind::One, false) => walk.hops.push(*link),
                (LinkKind::One, true) => walk.tail.push(*link),
                (LinkKind::Many, false) => walk.many = Some(*link),
                (LinkKind::Many, true) => {
                    return Err(ColumnError::SecondManyLink((*segment).into()))
                }
            }
            table = target;
        } else if let Some(field) = table
            .field(segment)
            .filter(|f| !f.hidden && index + 1 == count)
        {
            walk.field = Some(*field);
        } else {
            return Err(ColumnError::UnknownSegment {
                segment: (*segment).into(),
                table: table.id().into(),
            });
        }
    }
    build(spec, root, table, walk, parsed.agg)
}

fn build(
    spec: &str,
    root: &Table,
    table: &Table,
    walk: Walk,
    agg: Option<Agg>,
) -> Result<Column, ColumnError> {
    let Walk {
        hops,
        many,
        tail,
        field,
    } = walk;
    let Some(many) = many else {
        let field = field.ok_or(ColumnError::NoField)?;
        if agg.is_some() {
            return Err(ColumnError::AggregateWithoutManyLink);
        }
        return Ok(Column {
            spec: spec.into(),
            id: dom_id(spec),
            label: label(&hops, field.label),
            short_label: short_label(&hops, field.label),
            ty: field.ty,
            kind: ColumnKind::Field,
            root: root.id(),
            table: table.id(),
            depth: hops.len(),
            hops,
            many: None,
            tail: Vec::new(),
            field: Some(field),
            agg: None,
            time_field: None,
        });
    };
    let agg = agg.ok_or(ColumnError::ManyLinkNeedsAggregate)?;
    match (agg, field) {
        (Agg::Count, Some(_)) => return Err(ColumnError::CountTakesNoField),
        (Agg::Count, None) => {}
        (_, None) => return Err(ColumnError::AggregateNeedsField),
        (Agg::Sum | Agg::Avg, Some(f)) if !f.ty.number() => {
            return Err(ColumnError::AggregateNeedsNumber)
        }
        (Agg::Min | Agg::Max, Some(f)) if f.ty == FieldType::Boolean => {
            return Err(ColumnError::AggregateNeedsOrderedValue)
        }
        (Agg::Latest, Some(_)) if table.time_field().is_none() => {
            return Err(ColumnError::LatestNeedsTimeField)
        }
        _ => {}
    }
    let links: Vec<Link> = hops
        .iter()
        .copied()
        .chain([many])
        .chain(tail.iter().copied())
        .collect();
    let short = field.map_or_else(
        || Agg::Count.label().to_string(),
        |f| format!("{} ({})", f.label, agg.label()),
    );
    let heading = match field {
        None => format!("{} {}", many.label(), Agg::Count.label().to_lowercase()),
        Some(f) => format!(
            "{} {} {}",
            links.last().expect("many link").label(),
            lower_first(f.label),
            agg.label().to_lowercase()
        ),
    };
    let ty = match (agg, field) {
        (Agg::Count, _) => FieldType::Integer,
        (Agg::Avg, _) => FieldType::Float,
        (Agg::List, _) => FieldType::String,
        (_, Some(f)) => f.ty,
        (_, None) => FieldType::Integer,
    };
    Ok(Column {
        spec: spec.into(),
        id: dom_id(spec),
        label: label(&links, &short),
        short_label: heading,
        ty,
        kind: ColumnKind::Rollup,
        root: root.id(),
        table: table.id(),
        depth: links.len(),
        hops,
        many: Some(many),
        time_field: if tail.is_empty() {
            table.time_field()
        } else {
            None
        },
        tail,
        field,
        agg: Some(agg),
    })
}

fn label(links: &[Link], short: &str) -> String {
    links
        .iter()
        .map(|link| link.label())
        .chain([short])
        .collect::<Vec<_>>()
        .join(" › ")
}

// The heading a column scans by: a walked field carries the link it came through
// ("Company name"), so three counts side by side stay apart.
fn short_label(hops: &[Link], field: &str) -> String {
    match hops.last() {
        None => field.to_string(),
        Some(link) => format!("{} {}", link.label(), lower_first(field)),
    }
}

// "Name" reads as "name" after a link's label; an acronym such as "SKU" keeps its case.
fn lower_first(label: &str) -> String {
    let mut chars = label.chars();
    match (chars.next(), chars.next()) {
        (Some(first), Some(second))
            if first.is_ascii_uppercase() && second.is_ascii_lowercase() =>
        {
            first.to_ascii_lowercase().to_string() + &label[1..]
        }
        _ => label.to_string(),
    }
}
