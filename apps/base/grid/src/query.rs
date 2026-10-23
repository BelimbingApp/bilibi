//! Page-only projections. Every table and edge comes from its owner's scoped
//! source. The caller supplies specs, never trusted resolved columns or SQL.
use crate::lens::{self, Range, TREND_MONTHS};
use crate::{Agg, Catalog, Column, ColumnError, Join, Link, LinkKind, Table, MAX_COLUMNS};
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
    Catalog(crate::CatalogError),
    Database(Error),
}
impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit(why) => write!(f, "grid projection limit: {why}"),
            Self::ForbiddenRoot => write!(f, "grid table unavailable"),
            Self::Column(spec, why) => write!(f, "grid column {spec}: {why}"),
            Self::Catalog(error) => error.fmt(f),
            Self::Database(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for QueryError {}
impl From<tokio_postgres::Error> for QueryError {
    fn from(error: tokio_postgres::Error) -> Self {
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
            QueryError::Limit(_) | QueryError::Column(_, _) => Self::Invalid,
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

/// A typed field expression for an owner list's bounded sort or filter query.
/// Dynamic JSON keys are returned separately so callers bind them after their
/// own parameters; no tenant key enters the SQL text.
#[derive(Clone, Debug)]
pub struct FieldExpression {
    pub sql: String,
    pub ty: crate::FieldType,
    pub bind_values: Vec<String>,
}

pub fn field_expression(
    catalog: &Catalog,
    table_id: &str,
    field_id: &str,
    alias: &str,
    bind_offset: usize,
) -> Option<FieldExpression> {
    if !crate::model::id_valid(alias) || bind_offset > 65_534 {
        return None;
    }
    let table = catalog.table(table_id)?;
    let field = table.field(field_id)?;
    if field.hidden || field.readable_values.is_some() {
        return None;
    }
    if let Some(key) = field.json_key.clone() {
        let raw = format!(
            "jsonb_extract_path_text({alias}.{}, ${}::text)",
            ident(field.source_key()),
            bind_offset + 1
        );
        let sql = match field.ty {
            crate::FieldType::Integer => format!("NULLIF({raw}, '')::bigint"),
            crate::FieldType::Decimal => format!("NULLIF({raw}, '')::numeric"),
            crate::FieldType::Date => format!("NULLIF({raw}, '')::date"),
            crate::FieldType::DateTime => format!("NULLIF({raw}, '')::timestamptz"),
            crate::FieldType::Boolean => format!("NULLIF({raw}, '')::boolean"),
            _ => raw,
        };
        Some(FieldExpression {
            sql,
            ty: field.ty,
            bind_values: vec![key],
        })
    } else {
        Some(FieldExpression {
            sql: format!("{alias}.{}", ident(field.source_key())),
            ty: field.ty,
            bind_values: Vec::new(),
        })
    }
}

#[derive(Debug)]
pub struct TextSearchSql {
    pub sql: String,
    pub json_keys: Vec<String>,
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
    tx.batch_execute(BOUNDS).await?;
    let catalog = Catalog::for_scope_with_runtime(&*tx, scope, Some((&*tx).into())).await?;
    let root = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let columns = catalog
        .resolve_all(root, specs)
        .map_err(|(spec, error)| QueryError::Column(spec, error))?;
    if columns.iter().map(|c| c.depth).sum::<usize>() > MAX_HOPS {
        return Err(QueryError::Limit("more than 128 total hops"));
    }
    let (key_params, json_keys) = key_params(&columns, 6);
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
            field("r", root, root.key(), &KeyParams::new()),
            source(root)
        ))
        .await?;
    let key_type = shape.columns()[0].type_();
    let cast = format!("{}.{}", ident(key_type.schema()), ident(key_type.name()));
    let sql = statement(&catalog, root, &columns, &trend, &delta, &cast, &key_params);
    let ids = Value::Array(ids.to_vec());
    let tenant = scope.tenant();
    let user = scope.user();
    let iso = |date: civil::Date| date.to_string();
    let trend_from = iso(months[0]);
    let trend_to = iso(today.first_of_month().saturating_add(1.month()));
    let since = iso(lenses.since.unwrap_or_else(|| default_since(today)));
    let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
        vec![&tenant, &user, &ids, &trend_from, &trend_to, &since];
    params.extend(
        json_keys
            .iter()
            .map(|key| key as &(dyn tokio_postgres::types::ToSql + Sync)),
    );
    let explain: Value = tx
        .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), &params)
        .await?
        .try_get(0)?;
    let cost = explain[0]["Plan"]["Total Cost"]
        .as_f64()
        .ok_or(Error::Internal)?;
    if !cost.is_finite() || cost > MAX_ESTIMATED_COST {
        return Err(QueryError::Limit("estimated query cost exceeds 100000"));
    }
    let selected = tx.query(&sql, &params).await?;
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
            let mut value: Value = row.try_get(i + 1)?;
            if let Some(allowed) = column
                .field
                .as_ref()
                .and_then(|field| field.readable_values.as_ref())
            {
                if !value
                    .as_str()
                    .is_some_and(|key| allowed.iter().any(|allowed| allowed == key))
                {
                    value = Value::Null;
                }
            }
            cells.insert(column.id.clone(), value);
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
    let catalog = Catalog::for_scope_with_runtime(&*tx, scope, Some((&*tx).into())).await?;
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
    let (key_params, json_keys) = key_params(numeric.iter().copied(), 2);
    let values = numeric
        .iter()
        .enumerate()
        .map(|(i, column)| format!("{} AS c{i}", cell(&catalog, root, column, &key_params)))
        .collect::<Vec<_>>()
        .join(", ");
    let ranges = (0..numeric.len())
        .map(|i| format!("to_jsonb(min(c{i})), to_jsonb(max(c{i}))"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "WITH actor AS (SELECT $1::bigint AS tenant, $2::bigint AS usr) SELECT {ranges} FROM (SELECT {values} FROM {} r CROSS JOIN actor) s",
        source(root)
    );
    let tenant = scope.tenant();
    let user = scope.user();
    let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&tenant, &user];
    params.extend(
        json_keys
            .iter()
            .map(|key| key as &(dyn tokio_postgres::types::ToSql + Sync)),
    );
    let explain: Value = tx
        .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), &params)
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
    let row = tx.query_one(&sql, &params).await?;
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
    runtime_client: Option<bilibi_ports::PortClient<'_>>,
    scope: &Scope,
    root: &str,
    specs: &[&str],
    bind_offset: usize,
) -> Result<TextSearchSql, QueryError> {
    if specs.len() > MAX_COLUMNS {
        return Err(QueryError::Limit("more than 32 search paths"));
    }
    let catalog = Catalog::for_scope_with_runtime(client, scope, runtime_client).await?;
    let root = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let mut predicates = Vec::new();
    let mut columns = Vec::new();
    for spec in specs {
        let Ok(column) = catalog.resolve(root, spec) else {
            continue;
        };
        if !column
            .field
            .as_ref()
            .is_some_and(|f| f.ty == crate::FieldType::String && f.readable_values.is_none())
        {
            continue;
        }
        columns.push(column.clone());
        predicates.push(String::new());
    }
    let (keys, json_keys) = key_params(columns.iter(), bind_offset);
    for (index, column) in columns.iter().enumerate() {
        let f = column.field.as_ref().expect("field column");
        let predicate = match walk(&catalog, root, column, &keys) {
            Some(w) => format!(
                "EXISTS (SELECT 1 {} AND position($3::text in lower({}))>0)",
                w.suffix,
                field(&w.alias, w.table, &f.id, &keys)
            ),
            None => format!(
                "position($3::text in lower({}))>0",
                field("r", root, &f.id, &keys)
            ),
        };
        predicates[index] = predicate;
    }
    // Reference both scope parameters even for sources that only use tenant.
    let predicate = if predicates.is_empty() {
        "false".into()
    } else {
        predicates.join(" OR ")
    };
    Ok(TextSearchSql {
        sql: format!(
            "SELECT {} FROM {} r CROSS JOIN (SELECT $1::bigint AS tenant, $2::bigint AS usr) actor WHERE ({predicate})",
            field("r", root, root.key(), &keys),
            source(root)
        ),
        json_keys,
    })
}

// Every Grid read: bounded locally, and on the UTC calendar so a trend month
// or a change-since date means the same day whatever the pool's zone.
const BOUNDS: &str = "SET LOCAL statement_timeout = '2000ms'; SET LOCAL lock_timeout = '500ms'; SET LOCAL TimeZone = 'UTC'";

// Only catalog-owned identifiers enter SQL. Quote them even though declaration
// validation limits them to lowercase ASCII. Values always use bind parameters.
fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
type KeyParams = BTreeMap<(String, String), usize>;
fn key_params<'a>(
    columns: impl IntoIterator<Item = &'a Column>,
    start: usize,
) -> (KeyParams, Vec<String>) {
    let mut params = KeyParams::new();
    let mut keys = Vec::new();
    for column in columns {
        let Some(field) = column
            .field
            .as_ref()
            .filter(|field| field.json_key.is_some())
        else {
            continue;
        };
        let key = (column.table.to_owned(), field.id.clone());
        if params.contains_key(&key) {
            continue;
        }
        keys.push(field.json_key.clone().expect("custom key"));
        params.insert(key, start + keys.len());
    }
    (params, keys)
}

fn field(alias: &str, table: &Table, id: &str, keys: &KeyParams) -> String {
    let field = table.field(id).expect("resolved field");
    if field.json_key.is_some() {
        let Some(index) = keys.get(&(table.id().into(), field.id.clone())) else {
            return "NULL".into();
        };
        let raw = format!("({alias}.{} ->> ${index}::text)", ident(field.source_key()));
        return match field.ty {
            crate::FieldType::Integer => format!("NULLIF({raw}, '')::bigint"),
            crate::FieldType::Decimal => format!("NULLIF({raw}, '')::numeric"),
            crate::FieldType::Date => format!("NULLIF({raw}, '')::date"),
            crate::FieldType::DateTime => format!("NULLIF({raw}, '')::timestamptz"),
            crate::FieldType::Boolean => format!("NULLIF({raw}, '')::boolean"),
            _ => raw,
        };
    }
    format!("{alias}.{}", ident(field.source_key()))
}
fn source(table: &Table) -> String {
    format!("({})", table.source().sql)
}
fn condition(
    link: &Link,
    parent: &Table,
    target: &Table,
    p: &str,
    t: &str,
    keys: &KeyParams,
) -> String {
    match link.decl.join {
        Join::On {
            from_field,
            to_field,
        } => format!(
            "{} = {}",
            field(p, parent, from_field, keys),
            field(t, target, to_field, keys)
        ),
        Join::Edge(edge) => format!(
            "{} IN (SELECT e.to_key FROM ({}) e WHERE e.from_key = {})",
            field(t, target, target.key(), keys),
            edge.sql,
            field(p, parent, parent.key(), keys)
        ),
    }
}

// Each column is a correlated scalar subquery, so independent many paths never
// multiply each other or the page. One-hops select a scalar key first: a broken
// owner declaration returning two targets fails instead of silently inflating
// aggregates. Edge duplicates are set membership, not duplicate business rows.
fn cell(catalog: &Catalog, root: &Table, column: &Column, keys: &KeyParams) -> String {
    let Some(Walked {
        suffix,
        alias: p,
        table: parent,
    }) = walk(catalog, root, column, keys)
    else {
        return field(
            "r",
            root,
            &column.field.as_ref().expect("field column").id,
            keys,
        );
    };
    let value = column
        .field
        .as_ref()
        .map(|f| field(&p, parent, &f.id, keys));
    match column.agg {
        None => format!("(SELECT {} {suffix})", value.unwrap()),
        Some(Agg::Count) => format!(
            "(SELECT {} {suffix})",
            aggregate(catalog, column, &p, parent, keys)
        ),
        Some(Agg::Latest) => format!(
            "(SELECT {} {suffix} ORDER BY {} DESC NULLS LAST, {} DESC LIMIT 1)",
            value.unwrap(),
            field(&p, parent, parent.time_field().unwrap(), keys),
            field(&p, parent, parent.key(), keys)
        ),
        Some(Agg::List) => format!(
            "(SELECT left(string_agg(v, ', ' ORDER BY v), 300) FROM (SELECT DISTINCT left(({})::text, 300) AS v {suffix} ORDER BY v LIMIT 301) listed)",
            value.unwrap()
        ),
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
fn aggregate(
    catalog: &Catalog,
    column: &Column,
    alias: &str,
    table: &Table,
    keys: &KeyParams,
) -> String {
    match (column.agg, column.field.as_ref()) {
        (Some(Agg::Count), _) => {
            let many_index = column.hops.len();
            let many_table = catalog.table(column.many.unwrap().to()).unwrap();
            format!(
                "count({})",
                field(
                    &format!("t{many_index}"),
                    many_table,
                    many_table.key(),
                    keys
                )
            )
        }
        (Some(agg), Some(f)) => format!("{}({})", agg.key(), field(alias, table, &f.id, keys)),
        _ => unreachable!("resolved rollups aggregate"),
    }
}

/// A trend: the aggregate per calendar month of the many-link target's time
/// field, from `actor.trend_from` up to `actor.trend_to`, as a JSON object
/// keyed `YYYY-MM`. A lens column ends on its dated many-link target.
fn trend(catalog: &Catalog, root: &Table, column: &Column, keys: &KeyParams) -> String {
    let w = walk(catalog, root, column, keys).expect("rollup walks a link");
    let time = field(
        &w.alias,
        w.table,
        column.time_field.expect("dated rollup"),
        keys,
    );
    format!(
        "(SELECT COALESCE(jsonb_object_agg(m, v), '{{}}'::jsonb) FROM (SELECT to_char(date_trunc('month', {time}), 'YYYY-MM') AS m, ({})::float8 AS v {} AND {time} >= actor.trend_from AND {time} < actor.trend_to GROUP BY 1) months)",
        aggregate(catalog, column, &w.alias, w.table, keys),
        w.suffix
    )
}

/// A change since a date: the same aggregate over rows dated before it.
fn before(catalog: &Catalog, root: &Table, column: &Column, keys: &KeyParams) -> String {
    let w = walk(catalog, root, column, keys).expect("rollup walks a link");
    let time = field(
        &w.alias,
        w.table,
        column.time_field.expect("dated rollup"),
        keys,
    );
    format!(
        "(SELECT {} {} AND {time} < actor.since)",
        aggregate(catalog, column, &w.alias, w.table, keys),
        w.suffix
    )
}

fn walk<'c>(
    catalog: &'c Catalog,
    root: &'c Table,
    column: &Column,
    keys: &KeyParams,
) -> Option<Walked<'c>> {
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
                field(&t, target, target.key(), keys),
                field("lookup", target, target.key(), keys),
                source(target),
                condition(link, parent, target, &p, "lookup", keys)
            )
        } else {
            condition(link, parent, target, &p, &t, keys)
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
    keys: &KeyParams,
) -> String {
    let mut select = format!("to_jsonb({})", field("r", root, root.key(), keys));
    for column in columns {
        select.push_str(&format!(
            ", COALESCE(to_jsonb({}), 'null'::jsonb)",
            cell(catalog, root, column, keys)
        ));
    }
    for column in trends {
        select.push_str(&format!(", {}", trend(catalog, root, column, keys)));
    }
    for column in deltas {
        select.push_str(&format!(
            ", COALESCE(to_jsonb({}), 'null'::jsonb)",
            before(catalog, root, column, keys)
        ));
    }
    // Cast input values to the server-described key type. This keeps the owner
    // key bare, allowing its index to bound the page lookup. Type identifiers
    // come from PostgreSQL metadata, never from a request.
    format!(
        "WITH input AS (SELECT (id #>> '{{}}')::{key_cast} AS key, min(ord) AS ord FROM jsonb_array_elements($3::jsonb) WITH ORDINALITY AS x(id,ord) GROUP BY 1), actor AS (SELECT $1::bigint AS tenant, $2::bigint AS usr, $4::text::date AS trend_from, $5::text::date AS trend_to, $6::text::date AS since) SELECT {select} FROM input JOIN {} r ON {} = input.key CROSS JOIN actor ORDER BY input.ord LIMIT 101",
        source(root),
        field("r", root, root.key(), keys)
    )
}
