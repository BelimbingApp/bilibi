//! Page-only projections. Every table and edge comes from its owner's scoped
//! source. The caller supplies specs, never trusted resolved columns or SQL.
use crate::lens::{self, Range, TREND_MONTHS};
use crate::{Agg, Bounds, Catalog, Column, ColumnError, Join, Link, LinkKind, Table, MAX_COLUMNS};
use bilibi_database::{Db, Error};
use bilibi_tenancy::Scope;
use jiff::{civil, ToSpan};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub const MAX_ROWS: usize = 100;
pub const MAX_HOPS: usize = 128;
pub const MAX_ESTIMATED_COST: f64 = 100_000.0;
pub const STATEMENT_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug)]
pub enum QueryError {
    Limit(&'static str),
    ForbiddenRoot,
    Column(String, ColumnError),
    /// A filter, grouping, measure or sort the column's type does not take.
    Unsupported(String, &'static str),
    Catalog(crate::CatalogError),
    Database(Error),
}
impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit(why) => write!(f, "grid projection limit: {why}"),
            Self::ForbiddenRoot => write!(f, "grid table unavailable"),
            Self::Column(spec, why) => write!(f, "grid column {spec}: {why}"),
            Self::Unsupported(spec, why) => write!(f, "grid column {spec}: {why}"),
            Self::Catalog(error) => error.fmt(f),
            Self::Database(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for QueryError {}
impl From<tokio_postgres::Error> for QueryError {
    fn from(error: tokio_postgres::Error) -> Self {
        // A statement past its timeout is the read's bound, not a fault.
        if error.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED) {
            return Self::Limit("the read took longer than its time bound");
        }
        Self::Database(error.into())
    }
}
impl From<Error> for QueryError {
    fn from(error: Error) -> Self {
        Self::Database(error)
    }
}
impl From<crate::CatalogError> for QueryError {
    fn from(error: crate::CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<QueryError> for Error {
    fn from(error: QueryError) -> Self {
        match error {
            QueryError::Limit(_) | QueryError::Column(_, _) | QueryError::Unsupported(_, _) => {
                Self::Invalid
            }
            QueryError::ForbiddenRoot => Self::Forbidden,
            QueryError::Catalog(error) => error.into(),
            QueryError::Database(error) => error,
        }
    }
}

/// Native JSON numbers/bools/null, with date/datetime represented by PostgreSQL's
/// ISO strings. `columns` carries the field type; consumers need not guess dates.
#[derive(Debug)]
pub struct Row {
    pub key: Value,
    pub cells: BTreeMap<String, Value>,
    /// Per trend column id: the aggregate for each of [`Projection::months`],
    /// a month without rows counting zero.
    pub series: BTreeMap<String, Vec<f64>>,
    /// Per delta column id: the aggregate over rows dated before the date.
    pub before: BTreeMap<String, Value>,
}
#[derive(Debug)]
pub struct Projection {
    pub columns: Vec<Column>,
    /// Authorized roots only, in the caller's page order; duplicate IDs collapse.
    pub rows: Vec<Row>,
    pub estimated_cost: Option<f64>,
    /// The first day of each trend month, oldest first (UTC calendar).
    pub months: Vec<civil::Date>,
}

/// What the trend and change-since lenses add to a projection. Specs name
/// projected columns; one that is not a count or sum over a dated many-link
/// ([`lens::trends`]) gains nothing.
#[derive(Clone, Debug, Default)]
pub struct Lenses {
    /// Columns whose last twelve calendar months are projected.
    pub trend: Vec<String>,
    /// Columns whose aggregate before `since` is projected.
    pub delta: Vec<String>,
    /// The comparison date: rows dated before it make up "before". Defaults
    /// to thirty days before today.
    pub since: Option<civil::Date>,
}

/// Today's date on the UTC calendar every lens month and date is read in.
pub fn today() -> civil::Date {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
}

/// The change-since date a view without one compares against.
pub fn default_since(today: civil::Date) -> civil::Date {
    today.saturating_sub(30.days())
}

/// The first day of each of the last twelve calendar months, oldest first.
pub fn trend_months(today: civil::Date) -> Vec<civil::Date> {
    let first = today.first_of_month();
    (0..TREND_MONTHS as i32)
        .rev()
        .map(|back| first.saturating_sub(back.months()))
        .collect()
}

/// A numeric column's range over every row the scope may read, for scaling
/// bars and bands, with the planner's estimate for reading it.
#[derive(Debug, Default)]
pub struct Stats {
    /// By column id; only numeric columns.
    pub ranges: BTreeMap<String, Range>,
    pub estimated_cost: Option<f64>,
    /// The estimate exceeded the admission bound, so nothing was read and
    /// cells draw without a position.
    pub skipped: bool,
}

/// Project columns over IDs the owner's list has already paged. The IDs are JSON
/// scalar keys (numbers or strings), converted to the source key's native type
/// by PostgreSQL. Missing/unauthorized IDs disappear. Grants/restrictions are
/// re-read in a read-only, repeatable-read transaction; no stale catalog or forged Column is used.
/// All statements have a local timeout, and the projection's estimated total
/// cost is checked before execution. No setting leaks back into the pool.
pub async fn project<S: AsRef<str>>(
    db: &Db,
    scope: &Scope,
    root: &str,
    ids: &[Value],
    specs: &[S],
) -> Result<Projection, QueryError> {
    project_lenses(db, scope, root, ids, specs, &Lenses::default()).await
}

/// [`project`] under the caller's [`Bounds`] (its statement timeout and cost
/// bound) instead of the interactive ones: the report builder fills a page
/// of keys it read under the same bounds, interactive or background.
pub async fn project_within<S: AsRef<str>>(
    db: &Db,
    scope: &Scope,
    root: &str,
    ids: &[Value],
    specs: &[S],
    bounds: Bounds,
) -> Result<Projection, QueryError> {
    read(db, scope, root, ids, specs, &Lenses::default(), bounds).await
}

/// [`project`] plus each trend column's monthly series and each delta column's
/// aggregate before the date, from the same statement under the same bounds.
/// Months and dates are UTC calendar days (`TimeZone` is UTC for the read).
pub async fn project_lenses<S: AsRef<str>>(
    db: &Db,
    scope: &Scope,
    root: &str,
    ids: &[Value],
    specs: &[S],
    lenses: &Lenses,
) -> Result<Projection, QueryError> {
    read(db, scope, root, ids, specs, lenses, Bounds::default()).await
}

async fn read<S: AsRef<str>>(
    db: &Db,
    scope: &Scope,
    root: &str,
    ids: &[Value],
    specs: &[S],
    lenses: &Lenses,
    bounds: Bounds,
) -> Result<Projection, QueryError> {
    bounds.check()?;
    if ids.len() > MAX_ROWS {
        return Err(QueryError::Limit("page exceeds 100 IDs"));
    }
    if specs.len() > MAX_COLUMNS {
        return Err(QueryError::Limit("more than 32 column specs"));
    }
    if ids
        .iter()
        .any(|id| !matches!(id, Value::Number(_) | Value::String(_)) || id.to_string().len() > 256)
    {
        return Err(QueryError::Limit(
            "keys must be scalar numbers/strings up to 256 bytes",
        ));
    }
    let mut client = db.get().await.map_err(Error::from)?;
    let tx = client
        .build_transaction()
        .read_only(true)
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await?;
    tx.batch_execute(&bounds.sql()).await?;
    let catalog = Catalog::for_scope(&*tx, scope).await?;
    let root = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let columns = catalog
        .resolve_all(root, specs)
        .map_err(|(spec, error)| QueryError::Column(spec, error))?;
    if columns.iter().map(|c| c.depth).sum::<usize>() > MAX_HOPS {
        return Err(QueryError::Limit("more than 128 total hops"));
    }
    let today = today();
    let months = trend_months(today);
    let wanted =
        |specs: &[String], column: &Column| lens::trends(column) && specs.contains(&column.spec);
    let trend: Vec<&Column> = columns
        .iter()
        .filter(|c| wanted(&lenses.trend, c))
        .collect();
    let delta: Vec<&Column> = columns
        .iter()
        .filter(|c| wanted(&lenses.delta, c))
        .collect();
    if ids.is_empty() {
        return Ok(Projection {
            columns,
            rows: vec![],
            estimated_cost: None,
            months,
        });
    }
    let shape = tx
        .prepare(&format!(
            "SELECT {} FROM {} r",
            field("r", root, root.key()),
            source(root)
        ))
        .await?;
    let key_type = shape.columns()[0].type_();
    let cast = format!("{}.{}", ident(key_type.schema()), ident(key_type.name()));
    let sql = statement(&catalog, root, &columns, &trend, &delta, &cast);
    let ids = Value::Array(ids.to_vec());
    let tenant = scope.tenant();
    let user = scope.user();
    let iso = |date: civil::Date| date.to_string();
    let trend_from = iso(months[0]);
    let trend_to = iso(today.first_of_month().saturating_add(1.month()));
    let since = iso(lenses.since.unwrap_or_else(|| default_since(today)));
    let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] =
        &[&tenant, &user, &ids, &trend_from, &trend_to, &since];
    let explain: Value = tx
        .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), params)
        .await?
        .try_get(0)?;
    let cost = explain[0]["Plan"]["Total Cost"]
        .as_f64()
        .ok_or(Error::Internal)?;
    if !cost.is_finite() || cost > bounds.max_cost {
        return Err(QueryError::Limit(
            "the estimated cost of the read exceeds its bound",
        ));
    }
    let selected = tx.query(&sql, params).await?;
    if selected.len() > MAX_ROWS {
        return Err(crate::CatalogError::Misdeclared(format!(
            "table {} source duplicated page keys",
            root.id()
        ))
        .into());
    }
    let mut keys = BTreeSet::new();
    let mut rows = Vec::with_capacity(selected.len());
    for row in selected {
        let key: Value = row.try_get(0)?;
        if !keys.insert(key.to_string()) {
            return Err(crate::CatalogError::Misdeclared(format!(
                "table {} source duplicated page keys",
                root.id()
            ))
            .into());
        }
        let mut cells = BTreeMap::new();
        for (i, column) in columns.iter().enumerate() {
            cells.insert(column.id.clone(), row.try_get(i + 1)?);
        }
        let mut at = columns.len() + 1;
        let mut series = BTreeMap::new();
        for column in &trend {
            let by_month: Value = row.try_get(at)?;
            at += 1;
            let value = |month: &civil::Date| {
                by_month[month.strftime("%Y-%m").to_string()]
                    .as_f64()
                    .unwrap_or(0.0)
            };
            series.insert(column.id.clone(), months.iter().map(value).collect());
        }
        let mut before = BTreeMap::new();
        for column in &delta {
            before.insert(column.id.clone(), row.try_get(at)?);
            at += 1;
        }
        rows.push(Row {
            key,
            cells,
            series,
            before,
        });
    }
    tx.commit().await?;
    Ok(Projection {
        columns,
        rows,
        estimated_cost: Some(cost),
        months,
    })
}

/// Each numeric column's smallest and largest value over every root row the
/// scope may read: what a bar or band scales to. This is the one Grid read over
/// the whole table rather than a page, so its planner estimate is returned for
/// the page to show; above [`MAX_ESTIMATED_COST`] it is not run (`skipped`).
/// Same catalog, transaction, timeouts and owner sources as [`project`].
pub async fn stats<S: AsRef<str>>(
    db: &Db,
    scope: &Scope,
    root: &str,
    specs: &[S],
) -> Result<Stats, QueryError> {
    if specs.len() > MAX_COLUMNS {
        return Err(QueryError::Limit("more than 32 column specs"));
    }
    let mut client = db.get().await.map_err(Error::from)?;
    let tx = client
        .build_transaction()
        .read_only(true)
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await?;
    tx.batch_execute(BOUNDS).await?;
    let catalog = Catalog::for_scope(&*tx, scope).await?;
    let root = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let columns = catalog
        .resolve_all(root, specs)
        .map_err(|(spec, error)| QueryError::Column(spec, error))?;
    let numeric: Vec<&Column> = columns.iter().filter(|c| c.numeric()).collect();
    if numeric.iter().map(|c| c.depth).sum::<usize>() > MAX_HOPS {
        return Err(QueryError::Limit("more than 128 total hops"));
    }
    if numeric.is_empty() {
        return Ok(Stats::default());
    }
    let values = numeric
        .iter()
        .enumerate()
        .map(|(i, column)| format!("{} AS c{i}", cell(&catalog, root, column)))
        .collect::<Vec<_>>()
        .join(", ");
    let ranges = (0..numeric.len())
        .map(|i| format!("to_jsonb(min(c{i})), to_jsonb(max(c{i}))"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("WITH actor AS (SELECT $1::bigint AS tenant, $2::bigint AS usr) SELECT {ranges} FROM (SELECT {values} FROM {} r CROSS JOIN actor) s", source(root));
    let tenant = scope.tenant();
    let user = scope.user();
    let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[&tenant, &user];
    let explain: Value = tx
        .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), params)
        .await?
        .try_get(0)?;
    let cost = explain[0]["Plan"]["Total Cost"]
        .as_f64()
        .filter(|cost| cost.is_finite())
        .ok_or(Error::Internal)?;
    if cost > MAX_ESTIMATED_COST {
        return Ok(Stats {
            ranges: BTreeMap::new(),
            estimated_cost: Some(cost),
            skipped: true,
        });
    }
    let row = tx.query_one(&sql, params).await?;
    let mut ranges = BTreeMap::new();
    for (i, column) in numeric.iter().enumerate() {
        let min: Option<Value> = row.try_get(2 * i)?;
        let max: Option<Value> = row.try_get(2 * i + 1)?;
        ranges.insert(
            column.id.clone(),
            Range {
                min: min.unwrap_or(Value::Null),
                max: max.unwrap_or(Value::Null),
            },
        );
    }
    tx.commit().await?;
    Ok(Stats {
        ranges,
        estimated_cost: Some(cost),
        skipped: false,
    })
}

/// Build an owner-list membership subquery over visible text paths. Execute
/// immediately with the same scope, binding tenant/user/normalized search to
/// $1/$2/$3, inside the owner's paginated SELECT and count. Unavailable paths
/// contribute no matches. Only catalog identifiers enter the returned SQL.
/// Many-links search every value before list-cell truncation or aggregation.
/// The caller owns its statement timeout and pagination, as for its own search.
pub async fn text_search_sql(
    client: &impl tokio_postgres::GenericClient,
    scope: &Scope,
    root: &str,
    specs: &[&str],
) -> Result<String, QueryError> {
    if specs.len() > MAX_COLUMNS {
        return Err(QueryError::Limit("more than 32 search paths"));
    }
    let catalog = Catalog::for_scope(client, scope).await?;
    let root = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let mut predicates = Vec::new();
    for spec in specs {
        let Ok(column) = catalog.resolve(root, spec) else {
            continue;
        };
        let Some(f) = column.field.filter(|f| f.ty == crate::FieldType::String) else {
            continue;
        };
        let predicate = match walk(&catalog, root, &column) {
            Some(w) => format!(
                "EXISTS (SELECT 1 {} AND position($3::text in lower({}))>0)",
                w.suffix,
                field(&w.alias, w.table, f.id)
            ),
            None => format!("position($3::text in lower({}))>0", field("r", root, f.id)),
        };
        predicates.push(predicate);
    }
    // Reference both scope parameters even for sources that only use tenant.
    let predicate = if predicates.is_empty() {
        "false".into()
    } else {
        predicates.join(" OR ")
    };
    Ok(format!("SELECT {} FROM {} r CROSS JOIN (SELECT $1::bigint AS tenant, $2::bigint AS usr) actor WHERE ({predicate})", field("r", root, root.key()), source(root)))
}

/// Every Grid read, and every read through an owner's Grid source (Base
/// Search): bounded locally, and on the UTC calendar so a trend month or a
/// change-since date means the same day whatever the pool's zone.
pub const BOUNDS: &str = "SET LOCAL statement_timeout = '2000ms'; SET LOCAL lock_timeout = '500ms'; SET LOCAL TimeZone = 'UTC'";

// Only catalog-owned identifiers enter SQL. Quote them even though declaration
// validation limits them to lowercase ASCII. Values always use bind parameters.
fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A scalar path resolved under the caller's fresh scoped catalog. The owner
/// executes this expression with its root row aliased `r`, binding the same
/// tenant/user to $1/$2 as every catalog source. No caller SQL is accepted.
/// Mirrors Bilimbi Base Grid's scoped column expressions.
pub fn scalar_expression(catalog: &Catalog, root: &str, path: &str) -> Result<String, QueryError> {
    let root = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let column = catalog
        .resolve(root, path)
        .map_err(|error| QueryError::Column(path.into(), error))?;
    if column.kind != crate::ColumnKind::Field || column.many.is_some() || column.agg.is_some() {
        return Err(QueryError::Unsupported(
            path.into(),
            "a scalar field is required",
        ));
    }
    Ok(cell(catalog, root, &column))
}

/// A declared scalar field reached through one Company-style `One` link,
/// expressed as a single join rather than a correlated lookup per root row.
#[derive(Clone, Debug)]
pub struct ScalarJoin {
    /// Stable catalog-derived alias, suitable for deduplicating fields on a link.
    pub alias: String,
    /// The joined scalar value, using only catalog-validated identifiers.
    pub expression: String,
    /// The `LEFT JOIN` clause for the alias. The caller binds the same `$1`/`$2`
    /// tenant and user values used by the declared owner source.
    pub clause: String,
}

/// Resolve a scalar field through exactly one declared `One` link with an
/// `On` join and return its safe join clause and value expression. This is for
/// owner list queries that need to sort or filter many root rows by fields
/// contributed from another owner's table. The link declaration guarantees at
/// most one target per root; other path shapes remain available through
/// [`scalar_expression`].
pub fn scalar_join(catalog: &Catalog, root: &str, path: &str) -> Result<ScalarJoin, QueryError> {
    let root_table = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let column = catalog
        .resolve(root_table, path)
        .map_err(|error| QueryError::Column(path.into(), error))?;
    if column.kind != crate::ColumnKind::Field
        || column.many.is_some()
        || column.agg.is_some()
        || !column.tail.is_empty()
        || column.hops.len() != 1
    {
        return Err(QueryError::Unsupported(
            path.into(),
            "a scalar field on one direct link is required",
        ));
    }
    let link = &column.hops[0];
    let target = catalog.table(link.to()).ok_or(QueryError::ForbiddenRoot)?;
    let crate::Join::On {
        from_field,
        to_field,
    } = link.decl.join
    else {
        return Err(QueryError::Unsupported(
            path.into(),
            "a direct field join is required",
        ));
    };
    if link.kind() != crate::LinkKind::One {
        return Err(QueryError::Unsupported(
            path.into(),
            "a one-to-one field link is required",
        ));
    }
    let alias = format!("grid_{}", link.id());
    let clause = format!(
        "LEFT JOIN {} {alias} ON {} = {}",
        source(target),
        field("r", root_table, from_field),
        field(&alias, target, to_field)
    );
    let expression = field(
        &alias,
        target,
        column
            .field
            .ok_or_else(|| QueryError::Unsupported(path.into(), "a scalar field is required"))?
            .id,
    );
    Ok(ScalarJoin {
        alias,
        expression,
        clause,
    })
}

pub(crate) fn field(alias: &str, table: &Table, id: &str) -> String {
    format!(
        "{alias}.{}",
        ident(table.field(id).expect("resolved field").source_key())
    )
}
pub(crate) fn source(table: &Table) -> String {
    format!("({})", table.source().sql)
}
fn condition(link: &Link, parent: &Table, target: &Table, p: &str, t: &str) -> String {
    match link.decl.join {
        Join::On {
            from_field,
            to_field,
        } => format!(
            "{} = {}",
            field(p, parent, from_field),
            field(t, target, to_field)
        ),
        Join::Edge(edge) => format!(
            "{} IN (SELECT e.to_key FROM ({}) e WHERE e.from_key = {})",
            field(t, target, target.key()),
            edge.sql,
            field(p, parent, parent.key())
        ),
    }
}

// Each column is a correlated scalar subquery, so independent many paths never
// multiply each other or the page. One-hops select a scalar key first: a broken
// owner declaration returning two targets fails instead of silently inflating
// aggregates. Edge duplicates are set membership, not duplicate business rows.
pub(crate) fn cell(catalog: &Catalog, root: &Table, column: &Column) -> String {
    let Some(Walked {
        suffix,
        alias: p,
        table: parent,
    }) = walk(catalog, root, column)
    else {
        return field("r", root, column.field.expect("field column").id);
    };
    let value = column.field.map(|f| field(&p, parent, f.id));
    match column.agg {
        None => format!("(SELECT {} {suffix})", value.unwrap()),
        Some(Agg::Count) => format!("(SELECT {} {suffix})", aggregate(catalog, column, &p, parent)),
        Some(Agg::Latest) => format!("(SELECT {} {suffix} ORDER BY {} DESC NULLS LAST, {} DESC LIMIT 1)", value.unwrap(), field(&p, parent, parent.time_field().unwrap()), field(&p, parent, parent.key())),
        Some(Agg::List) => format!("(SELECT left(string_agg(v, ', ' ORDER BY v), 300) FROM (SELECT DISTINCT left(({})::text, 300) AS v {suffix} ORDER BY v LIMIT 301) listed)", value.unwrap()),
        Some(agg) => format!("(SELECT {}({}) {suffix})", agg.key(), value.unwrap()),
    }
}

/// The links a column walks, as the `FROM .. WHERE` of a correlated subquery
/// on the root row `r`, with the alias and table of the last one.
struct Walked<'c> {
    suffix: String,
    alias: String,
    table: &'c Table,
}

/// `count` of the many-link's rows or the column's aggregate of its field.
fn aggregate(catalog: &Catalog, column: &Column, alias: &str, table: &Table) -> String {
    match (column.agg, column.field) {
        (Some(Agg::Count), _) => {
            let many_index = column.hops.len();
            let many_table = catalog.table(column.many.unwrap().to()).unwrap();
            format!(
                "count({})",
                field(&format!("t{many_index}"), many_table, many_table.key())
            )
        }
        (Some(agg), Some(f)) => format!("{}({})", agg.key(), field(alias, table, f.id)),
        _ => unreachable!("resolved rollups aggregate"),
    }
}

/// A trend: the aggregate per calendar month of the many-link target's time
/// field, from `actor.trend_from` up to `actor.trend_to`, as a JSON object
/// keyed `YYYY-MM`. A lens column ends on its dated many-link target.
fn trend(catalog: &Catalog, root: &Table, column: &Column) -> String {
    let w = walk(catalog, root, column).expect("rollup walks a link");
    let time = field(&w.alias, w.table, column.time_field.expect("dated rollup"));
    format!(
        "(SELECT COALESCE(jsonb_object_agg(m, v), '{{}}'::jsonb) FROM (SELECT to_char(date_trunc('month', {time}), 'YYYY-MM') AS m, ({})::float8 AS v {} AND {time} >= actor.trend_from AND {time} < actor.trend_to GROUP BY 1) months)",
        aggregate(catalog, column, &w.alias, w.table),
        w.suffix
    )
}

/// A change since a date: the same aggregate over rows dated before it.
fn before(catalog: &Catalog, root: &Table, column: &Column) -> String {
    let w = walk(catalog, root, column).expect("rollup walks a link");
    let time = field(&w.alias, w.table, column.time_field.expect("dated rollup"));
    format!(
        "(SELECT {} {} AND {time} < actor.since)",
        aggregate(catalog, column, &w.alias, w.table),
        w.suffix
    )
}

fn walk<'c>(catalog: &'c Catalog, root: &'c Table, column: &Column) -> Option<Walked<'c>> {
    let links: Vec<_> = column
        .hops
        .iter()
        .chain(column.many.iter())
        .chain(column.tail.iter())
        .collect();
    if links.is_empty() {
        return None;
    }
    let mut from = String::new();
    let mut parent = root;
    let mut p = "r".to_string();
    let mut first_condition = String::new();
    for (i, link) in links.iter().enumerate() {
        let target = catalog.table(link.to()).expect("resolved target");
        let t = format!("t{i}");
        let predicate = if link.kind() == LinkKind::One {
            // For an edge, EXISTS/set membership alone could hide two distinct
            // targets; this scalar SELECT enforces the declared cardinality.
            format!(
                "{} = (SELECT {} FROM {} lookup WHERE {})",
                field(&t, target, target.key()),
                field("lookup", target, target.key()),
                source(target),
                condition(link, parent, target, &p, "lookup")
            )
        } else {
            condition(link, parent, target, &p, &t)
        };
        if i == 0 {
            from = format!("{} {t}", source(target));
            first_condition = predicate;
        } else {
            from.push_str(&format!(" LEFT JOIN {} {t} ON {predicate}", source(target)));
        }
        p = t;
        parent = target;
    }
    Some(Walked {
        suffix: format!("FROM {from} WHERE {first_condition}"),
        alias: p,
        table: parent,
    })
}
fn statement(
    catalog: &Catalog,
    root: &Table,
    columns: &[Column],
    trends: &[&Column],
    deltas: &[&Column],
    key_cast: &str,
) -> String {
    let mut select = format!("to_jsonb({})", field("r", root, root.key()));
    for column in columns {
        select.push_str(&format!(
            ", COALESCE(to_jsonb({}), 'null'::jsonb)",
            cell(catalog, root, column)
        ));
    }
    for column in trends {
        select.push_str(&format!(", {}", trend(catalog, root, column)));
    }
    for column in deltas {
        select.push_str(&format!(
            ", COALESCE(to_jsonb({}), 'null'::jsonb)",
            before(catalog, root, column)
        ));
    }
    // Cast input values to the server-described key type. This keeps the owner
    // key bare, allowing its index to bound the page lookup. Type identifiers
    // come from PostgreSQL metadata, never from a request.
    format!("WITH input AS (SELECT (id #>> '{{}}')::{key_cast} AS key, min(ord) AS ord FROM jsonb_array_elements($3::jsonb) WITH ORDINALITY AS x(id,ord) GROUP BY 1), actor AS (SELECT $1::bigint AS tenant, $2::bigint AS usr, $4::text::date AS trend_from, $5::text::date AS trend_to, $6::text::date AS since) SELECT {select} FROM input JOIN {} r ON {} = input.key CROSS JOIN actor ORDER BY input.ord LIMIT 101", source(root), field("r", root, root.key()))
}
