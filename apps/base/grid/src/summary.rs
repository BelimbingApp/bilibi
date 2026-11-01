//! Grouped reads and filtered row pages over every row a scope may read: what
//! the report builder (BelimbingApp/bilibi#128) asks a question with. [`summarize`] groups by up to two columns (a date by
//! day, week, month, quarter or year in the reader's zone) and returns
//! aggregates per group, per first-level group and in total. [`page_keys`]
//! filters and sorts root rows and returns one page of keys for
//! [`crate::project`] to fill.
//!
//! The rules are [`crate::project`]'s: a fresh scoped catalog, a read-only
//! repeatable-read transaction, the owner's source with its tenant and user
//! bindings on the root and every link, an `EXPLAIN` admission bound and the
//! same statement and lock timeouts. Filters, groupings, measures and sorts
//! are column specs resolved against that catalog, so a column the reader
//! cannot see is refused like an unknown one and cannot be used to infer a
//! hidden value. Only catalog identifiers enter SQL; every value is bound.
use crate::query::{cell, field, source, QueryError, MAX_ESTIMATED_COST, MAX_HOPS, MAX_ROWS};
use crate::{Agg, Catalog, Column, FieldType, Table};
use bilibi_database::{Db, Error};
use bilibi_tenancy::Scope;
use bytes::BytesMut;
use jiff::{civil::Date, tz::TimeZone, Timestamp, ToSpan};
use serde_json::Value;
use tokio_postgres::types::{Format, IsNull, ToSql, Type};

pub const MAX_FILTERS: usize = 16;
pub const MAX_GROUPINGS: usize = 2;
pub const MAX_MEASURES: usize = 8;
pub const MAX_SORTS: usize = 3;
/// Groups (detail rows) one summary may return. Above it the read is refused,
/// never truncated.
pub const MAX_RESULT_GROUPS: usize = 1_000;
/// The furthest a row page may start.
pub const MAX_OFFSET: usize = 10_000;
/// Values in a one-of filter.
pub const MAX_CHOICES: usize = 100;
/// Bytes of a text filter value.
pub const MAX_TEXT: usize = 256;

/// One condition on a column's value. The column is a spec, as for
/// [`crate::project`]; a rollup filters on its rolled-up value.
#[derive(Clone, Debug)]
pub struct Filter {
    pub column: String,
    pub test: Test,
}

#[derive(Clone, Debug)]
pub enum Test {
    /// A scalar JSON value (number, string or boolean).
    Equals(Value),
    /// Any of up to [`MAX_CHOICES`] scalar values.
    OneOf(Vec<Value>),
    /// Text containing this, ignoring case. Text and choice columns only.
    Contains(String),
    /// Inclusive bounds, at least one: JSON numbers for a number column,
    /// `YYYY-MM-DD` days for a date or date-time column (days in the
    /// reader's zone).
    Range {
        min: Option<Value>,
        max: Option<Value>,
    },
    /// Calendar days relative to today in the reader's zone. Date and
    /// date-time columns only.
    Within(Period),
    /// No value (and, for text, an empty one).
    Empty,
    NotEmpty,
}

/// A relative run of whole days, both ends included, in the reader's zone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Period {
    Today,
    /// Monday to Sunday of the current ISO week.
    ThisWeek,
    Last7Days,
    Last30Days,
    ThisMonth,
    ThisQuarter,
    ThisYear,
}
impl Period {
    pub const ALL: [Period; 7] = [
        Self::Today,
        Self::ThisWeek,
        Self::Last7Days,
        Self::Last30Days,
        Self::ThisMonth,
        Self::ThisQuarter,
        Self::ThisYear,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::ThisWeek => "this_week",
            Self::Last7Days => "last_7_days",
            Self::Last30Days => "last_30_days",
            Self::ThisMonth => "this_month",
            Self::ThisQuarter => "this_quarter",
            Self::ThisYear => "this_year",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|period| period.key() == key)
    }
    /// The first and last day, both included.
    pub fn days(self, today: Date) -> (Date, Date) {
        let month = today.first_of_month();
        match self {
            Self::Today => (today, today),
            Self::ThisWeek => {
                let back = i64::from(today.weekday().to_monday_zero_offset());
                let monday = today.saturating_sub(back.days());
                (monday, monday.saturating_add(6.days()))
            }
            Self::Last7Days => (today.saturating_sub(6.days()), today),
            Self::Last30Days => (today.saturating_sub(29.days()), today),
            Self::ThisMonth => (month, month.last_of_month()),
            Self::ThisQuarter => {
                let first = month.saturating_sub(((today.month() - 1) % 3).months());
                let last = first.saturating_add(2.months()).last_of_month();
                (first, last)
            }
            Self::ThisYear => (
                today.first_of_year(),
                today
                    .first_of_year()
                    .saturating_add(1.year())
                    .saturating_sub(1.day()),
            ),
        }
    }
}

/// How a date or date-time grouping is bucketed: the bucket's first day.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bucket {
    Day,
    /// Starting on Monday (ISO).
    Week,
    Month,
    Quarter,
    Year,
}
impl Bucket {
    pub const ALL: [Bucket; 5] = [
        Self::Day,
        Self::Week,
        Self::Month,
        Self::Quarter,
        Self::Year,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Quarter => "quarter",
            Self::Year => "year",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|bucket| bucket.key() == key)
    }
}

#[derive(Clone, Debug)]
pub struct Grouping {
    pub column: String,
    /// Required for, and only allowed on, a date or date-time column.
    pub bucket: Option<Bucket>,
}

/// An aggregate over each group's rows of one column's value: `count` (rows
/// with a value), `sum`/`avg` (numbers) and `min`/`max` (ordered values).
#[derive(Clone, Debug)]
pub struct Measure {
    pub column: String,
    pub agg: Agg,
}

/// The bounds one grouped read or row page runs under: its statement
/// timeout and the planner cost it is admitted up to. `Default` is Grid's
/// interactive bound (2 s, [`MAX_ESTIMATED_COST`]); a background caller (a
/// prepared export) may pass a larger pair, within [`MAX_STATEMENT_MS`] and
/// [`MAX_COST_BOUND`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub statement_ms: u32,
    pub max_cost: f64,
}
impl Default for Bounds {
    fn default() -> Self {
        Self {
            statement_ms: 2_000,
            max_cost: MAX_ESTIMATED_COST,
        }
    }
}
/// The longest statement timeout a caller may ask for.
pub const MAX_STATEMENT_MS: u32 = 300_000;
/// The largest planner cost a caller may admit.
pub const MAX_COST_BOUND: f64 = 100_000_000.0;
impl Bounds {
    pub(crate) fn check(&self) -> Result<(), QueryError> {
        if !(100..=MAX_STATEMENT_MS).contains(&self.statement_ms)
            || !self.max_cost.is_finite()
            || !(1.0..=MAX_COST_BOUND).contains(&self.max_cost)
        {
            return Err(QueryError::Limit("bounds out of range"));
        }
        Ok(())
    }
    /// The transaction settings: the timeout is a validated number.
    pub(crate) fn sql(&self) -> String {
        format!(
            "SET LOCAL statement_timeout = '{}ms'; SET LOCAL lock_timeout = '500ms'; SET LOCAL TimeZone = 'UTC'",
            self.statement_ms
        )
    }
}

#[derive(Clone, Debug)]
pub struct SummarySpec {
    pub filters: Vec<Filter>,
    pub groupings: Vec<Grouping>,
    pub measures: Vec<Measure>,
    /// The reader's IANA zone: date-time buckets and day filters use it.
    pub timezone: String,
    /// The most groups the caller will show, at most [`MAX_RESULT_GROUPS`].
    pub max_groups: usize,
    pub bounds: Bounds,
}

/// One result line. `keys` holds one JSON value per grouping (a bucket is its
/// first day, `YYYY-MM-DD`; a missing value is null). A subtotal line carries
/// only the first key (the second is null) and `subtotal` set; the total line
/// has no keys.
#[derive(Clone, Debug)]
pub struct Line {
    pub keys: Vec<Value>,
    pub subtotal: bool,
    /// Rows in the group.
    pub rows: i64,
    /// One value per measure, in order; null where no row has a value.
    pub values: Vec<Value>,
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub groupings: Vec<(Column, Option<Bucket>)>,
    pub measures: Vec<(Column, Agg)>,
    /// Groups in key order, null keys last; with two groupings each first-level
    /// group ends with its subtotal line.
    pub lines: Vec<Line>,
    pub total: Line,
    pub estimated_cost: f64,
    /// PostgreSQL text preserves numeric precision until cross-child arithmetic.
    pub(crate) combine_values: Vec<Option<String>>,
    /// Unrounded sum and non-null count for each average, for parent totals.
    pub(crate) averages: Vec<Option<(Option<String>, i64)>>,
}

#[derive(Clone, Debug)]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

#[derive(Clone, Debug)]
pub struct PageSpec {
    pub filters: Vec<Filter>,
    /// Up to [`MAX_SORTS`]; the root key always breaks ties.
    pub sorts: Vec<Sort>,
    pub timezone: String,
    pub offset: usize,
    /// At most [`MAX_ROWS`], the page [`crate::project`] takes.
    pub limit: usize,
    pub bounds: Bounds,
}

#[derive(Debug)]
pub struct KeyPage {
    /// Root keys in sort order, for [`crate::project`].
    pub keys: Vec<Value>,
    /// Every matching row the scope may read.
    pub total: i64,
    pub estimated_cost: f64,
}

/// Group every row the scope may read that passes the filters, and aggregate
/// each group. See the module documentation for the rules.
pub async fn summarize(
    db: &Db,
    scope: &Scope,
    root: &str,
    spec: &SummarySpec,
) -> Result<Summary, QueryError> {
    let mut client = db.get().await.map_err(Error::from)?;
    let tx = client
        .build_transaction()
        .read_only(true)
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await?;
    spec.bounds.check()?;
    tx.batch_execute(&spec.bounds.sql()).await?;
    let catalog = Catalog::for_scope(&*tx, scope).await?;
    let result = summarize_catalog(&tx, &catalog, root, spec).await?;
    tx.commit().await?;
    Ok(result)
}

pub(crate) async fn summarize_catalog(
    tx: &tokio_postgres::Transaction<'_>,
    catalog: &Catalog,
    root: &str,
    spec: &SummarySpec,
) -> Result<Summary, QueryError> {
    if spec.groupings.len() > MAX_GROUPINGS {
        return Err(QueryError::Limit("more than 2 groupings"));
    }
    if spec.measures.len() > MAX_MEASURES {
        return Err(QueryError::Limit("more than 8 measures"));
    }
    if spec.max_groups == 0 || spec.max_groups > MAX_RESULT_GROUPS {
        return Err(QueryError::Limit("max groups must be 1 to 1000"));
    }
    let today = reader_today(&spec.timezone)?;
    let root = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let mut binds = Binds::new(catalog, &spec.timezone);
    let mut hops = 0;
    let predicate = filters(catalog, root, &spec.filters, today, &mut binds, &mut hops)?;

    let mut groupings = Vec::with_capacity(spec.groupings.len());
    let mut selected = Vec::new();
    for (i, grouping) in spec.groupings.iter().enumerate() {
        let column = resolve(catalog, root, &grouping.column, &mut hops)?;
        let dated = matches!(column.ty, FieldType::Date | FieldType::DateTime);
        let value = cell(catalog, root, &column);
        let key = match (grouping.bucket, dated) {
            (Some(bucket), true) => format!(
                "to_jsonb(date_trunc('{}', {})::date)",
                bucket.key(),
                local(&value, column.ty)
            ),
            (None, false) => format!("to_jsonb({value})"),
            (Some(_), false) => return Err(unsupported(&column, "only dates group by a period")),
            (None, true) => return Err(unsupported(&column, "a date groups by a period")),
        };
        selected.push(format!("{key} AS g{i}"));
        groupings.push((column, grouping.bucket));
    }
    let mut measures = Vec::with_capacity(spec.measures.len());
    let mut aggregates = Vec::new();
    for (i, measure) in spec.measures.iter().enumerate() {
        let column = match measure.agg {
            Agg::Count => resolve_any(catalog, root, &measure.column, &mut hops)?,
            _ => resolve(catalog, root, &measure.column, &mut hops)?,
        };
        let fits = match measure.agg {
            Agg::Count => true,
            Agg::Sum | Agg::Avg => column.ty.number(),
            Agg::Min | Agg::Max => column.ty.numeric() || column.ty == FieldType::String,
            Agg::Latest | Agg::List => false,
        };
        if !fits {
            return Err(unsupported(
                &column,
                "this aggregate does not fit the column",
            ));
        }
        selected.push(format!("{} AS m{i}", cell(catalog, root, &column)));
        aggregates.push(format!(
            "COALESCE(to_jsonb({}(m{i})), 'null'::jsonb)",
            measure.agg.key()
        ));
        measures.push((column, measure.agg));
    }
    if hops > MAX_HOPS {
        return Err(QueryError::Limit("more than 128 total hops"));
    }

    let keys: Vec<String> = (0..groupings.len()).map(|i| format!("g{i}")).collect();
    let sets = match keys.len() {
        0 => "()".to_owned(),
        1 => "(g0), ()".to_owned(),
        _ => "(g0, g1), (g0), ()".to_owned(),
    };
    let flags = if keys.is_empty() {
        "1, 1".to_owned()
    } else {
        let g1 = if keys.len() == 2 { "GROUPING(g1)" } else { "1" };
        format!("GROUPING(g0), {g1}")
    };
    let mut columns = vec![flags];
    columns.extend(keys.iter().cloned());
    columns.push("count(*)".into());
    columns.extend(aggregates);
    for (i, (_, agg)) in measures.iter().enumerate() {
        columns.push(format!("{}(m{i})::text", agg.key()));
        if *agg == Agg::Avg {
            columns.push(format!("sum(m{i})::text"));
            columns.push(format!("count(m{i})"));
        }
    }
    let order = match keys.len() {
        0 => String::new(),
        1 => " ORDER BY GROUPING(g0), g0 NULLS LAST".into(),
        _ => " ORDER BY GROUPING(g0), g0 NULLS LAST, GROUPING(g1), g1 NULLS LAST".into(),
    };
    // Detail and subtotal lines together never exceed twice the details plus
    // the total, so one more page than that proves an overflow.
    let fetch = spec.max_groups * 2 + 2;
    let inner = if selected.is_empty() {
        "1 AS one".to_owned()
    } else {
        selected.join(", ")
    };
    let sql = format!(
        "WITH actor AS (SELECT $1::bigint AS tenant, $2::bigint AS usr, $3::text AS zone), base AS (SELECT {inner} FROM {} r CROSS JOIN actor WHERE {predicate}) SELECT {} FROM base GROUP BY GROUPING SETS ({sets}){order} LIMIT {fetch}",
        source(root),
        columns.join(", ")
    );
    let estimated_cost = admit(tx, &sql, &binds, spec.bounds.max_cost).await?;
    let fetched = tx.query(&sql, &binds.refs()).await?;
    let width = groupings.len();
    let mut lines = Vec::new();
    let mut total = None;
    let mut combine_values = vec![None; measures.len()];
    let mut averages = vec![None; measures.len()];
    for row in &fetched {
        let first: i32 = row.try_get(0)?;
        let second: i32 = row.try_get(1)?;
        let mut keys = Vec::with_capacity(width);
        for i in 0..width {
            keys.push(
                row.try_get::<_, Option<Value>>(2 + i)?
                    .unwrap_or(Value::Null),
            );
        }
        let mut values = Vec::with_capacity(measures.len());
        for i in 0..measures.len() {
            values.push(row.try_get(3 + width + i)?);
        }
        let line = Line {
            keys,
            subtotal: width == 2 && first == 0 && second == 1,
            rows: row.try_get(2 + width)?,
            values,
        };
        if width == 0 || first == 1 {
            let mut at = 3 + width + measures.len();
            for (i, (_, agg)) in measures.iter().enumerate() {
                combine_values[i] = row.try_get(at)?;
                at += 1;
                if *agg == Agg::Avg {
                    averages[i] = Some((row.try_get(at)?, row.try_get(at + 1)?));
                    at += 2;
                }
            }
            total = Some(Line {
                keys: Vec::new(),
                ..line
            });
        } else {
            lines.push(line);
        }
    }
    let details = lines.iter().filter(|line| !line.subtotal).count();
    if details > spec.max_groups || fetched.len() >= fetch {
        return Err(QueryError::Limit("too many groups for one summary"));
    }
    Ok(Summary {
        groupings,
        measures,
        lines,
        total: total.ok_or(Error::Internal)?,
        estimated_cost,
        combine_values,
        averages,
    })
}

/// One page of root keys, filtered and sorted over every row the scope may
/// read, with the matching total. Pass the keys to [`crate::project`].
pub async fn page_keys(
    db: &Db,
    scope: &Scope,
    root: &str,
    spec: &PageSpec,
) -> Result<KeyPage, QueryError> {
    let mut client = db.get().await.map_err(Error::from)?;
    let tx = client
        .build_transaction()
        .read_only(true)
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await?;
    spec.bounds.check()?;
    tx.batch_execute(&spec.bounds.sql()).await?;
    let catalog = Catalog::for_scope(&*tx, scope).await?;
    let result = page_keys_catalog(&tx, &catalog, root, spec).await?;
    tx.commit().await?;
    Ok(result)
}

pub(crate) async fn page_keys_catalog(
    tx: &tokio_postgres::Transaction<'_>,
    catalog: &Catalog,
    root: &str,
    spec: &PageSpec,
) -> Result<KeyPage, QueryError> {
    if spec.sorts.len() > MAX_SORTS {
        return Err(QueryError::Limit("more than 3 sorts"));
    }
    if spec.limit == 0 || spec.limit > MAX_ROWS {
        return Err(QueryError::Limit("a page holds 1 to 100 rows"));
    }
    if spec.offset > MAX_OFFSET {
        return Err(QueryError::Limit(
            "a page starts within the first 10000 rows",
        ));
    }
    let today = reader_today(&spec.timezone)?;
    let root = catalog.table(root).ok_or(QueryError::ForbiddenRoot)?;
    let mut binds = Binds::new(catalog, &spec.timezone);
    let mut hops = 0;
    let predicate = filters(catalog, root, &spec.filters, today, &mut binds, &mut hops)?;
    let mut order = Vec::new();
    for sort in &spec.sorts {
        let column = resolve(catalog, root, &sort.column, &mut hops)?;
        let direction = if sort.descending { "DESC" } else { "ASC" };
        order.push(format!(
            "{} {direction} NULLS LAST",
            cell(catalog, root, &column)
        ));
    }
    if hops > MAX_HOPS {
        return Err(QueryError::Limit("more than 128 total hops"));
    }
    let key = field("r", root, root.key());
    order.push(format!("{key} ASC"));
    // Composed from its parts, never by replacing a marker: the owner's
    // source SQL may itself contain `{}` and is copied in untouched.
    let head = "WITH actor AS (SELECT $1::bigint AS tenant, $2::bigint AS usr, $3::text AS zone)";
    let tail = format!("FROM {} r CROSS JOIN actor WHERE {predicate}", source(root));
    let page = format!(
        "{head} SELECT to_jsonb({key}) {tail} ORDER BY {} LIMIT {} OFFSET {}",
        order.join(", "),
        spec.limit,
        spec.offset
    );
    let count = format!("{head} SELECT count(*) {tail}");
    let estimated_cost = admit(tx, &page, &binds, spec.bounds.max_cost)
        .await?
        .max(admit(tx, &count, &binds, spec.bounds.max_cost).await?);
    let keys = tx
        .query(&page, &binds.refs())
        .await?
        .iter()
        .map(|row| row.try_get(0))
        .collect::<Result<Vec<Value>, _>>()?;
    let total: i64 = tx.query_one(&count, &binds.refs()).await?.try_get(0)?;
    Ok(KeyPage {
        keys,
        total,
        estimated_cost,
    })
}

/// Bound parameters: `$1` tenant and `$2` user (owner sources use both), `$3`
/// the reader's zone, then one per filter value.
struct Binds(Vec<Box<dyn ToSql + Sync + Send>>);
impl Binds {
    fn new(catalog: &Catalog, timezone: &str) -> Self {
        Self(vec![
            Box::new(catalog.tenant()),
            Box::new(catalog.user()),
            Box::new(timezone.to_owned()),
        ])
    }
    /// Binds a value and returns its placeholder.
    fn push(&mut self, value: impl ToSql + Sync + Send + 'static) -> String {
        self.0.push(Box::new(value));
        format!("${}", self.0.len())
    }
    fn refs(&self) -> Vec<&(dyn ToSql + Sync)> {
        self.0
            .iter()
            .map(|value| &**value as &(dyn ToSql + Sync))
            .collect()
    }
}

/// The planner's estimate, refused above the caller's `max_cost`.
async fn admit(
    tx: &tokio_postgres::Transaction<'_>,
    sql: &str,
    binds: &Binds,
    max_cost: f64,
) -> Result<f64, QueryError> {
    let explain: Value = tx
        .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), &binds.refs())
        .await?
        .try_get(0)?;
    let cost = explain[0]["Plan"]["Total Cost"]
        .as_f64()
        .filter(|cost| cost.is_finite())
        .ok_or(Error::Internal)?;
    if cost > max_cost {
        return Err(QueryError::TooHeavy(
            "the estimated cost of the read exceeds its bound",
        ));
    }
    Ok(cost)
}

fn reader_today(timezone: &str) -> Result<Date, QueryError> {
    if !bilibi_datetime::valid_timezone(timezone) {
        return Err(QueryError::Limit("unknown time zone"));
    }
    let zone = TimeZone::get(timezone).map_err(|_| QueryError::Limit("unknown time zone"))?;
    Ok(Timestamp::now().to_zoned(zone).date())
}

/// A column a grouping, filter, sort or value aggregate may use. A field the
/// reader sees masked (ADR 05) is refused: its group keys, minimum or maximum
/// would show the full value, and a filter or sort on it would say what it
/// holds.
fn resolve(
    catalog: &Catalog,
    root: &Table,
    spec: &str,
    hops: &mut usize,
) -> Result<Column, QueryError> {
    let column = resolve_any(catalog, root, spec, hops)?;
    if catalog.mask(&column).is_some() {
        return Err(unsupported(&column, "a masked field shows no value here"));
    }
    Ok(column)
}
/// Any readable column, masked or not: only for a count, which shows no value.
fn resolve_any(
    catalog: &Catalog,
    root: &Table,
    spec: &str,
    hops: &mut usize,
) -> Result<Column, QueryError> {
    let column = catalog
        .resolve(root, spec)
        .map_err(|error| QueryError::Column(spec.to_owned(), error))?;
    *hops += column.depth;
    Ok(column)
}

fn unsupported(column: &Column, why: &'static str) -> QueryError {
    QueryError::Unsupported(column.spec.clone(), why)
}

/// A date-time as a wall-clock timestamp in the reader's zone; a date as is.
fn local(value: &str, ty: FieldType) -> String {
    match ty {
        FieldType::DateTime => format!("(({value}) AT TIME ZONE actor.zone)"),
        _ => format!("({value})"),
    }
}

/// Every filter, joined with AND (`true` when there are none).
fn filters(
    catalog: &Catalog,
    root: &Table,
    filters: &[Filter],
    today: Date,
    binds: &mut Binds,
    hops: &mut usize,
) -> Result<String, QueryError> {
    if filters.len() > MAX_FILTERS {
        return Err(QueryError::Limit("more than 16 filters"));
    }
    let mut predicates = Vec::with_capacity(filters.len());
    for filter in filters {
        let column = resolve(catalog, root, &filter.column, hops)?;
        let value = cell(catalog, root, &column);
        predicates.push(test(&column, &value, &filter.test, today, binds)?);
    }
    Ok(if predicates.is_empty() {
        "true".into()
    } else {
        predicates.join(" AND ")
    })
}

/// An equals or one-of value checked against the column's type and written
/// in one canonical form, with the SQL type it compares as: the comparison is
/// then of typed values, so `…+00:00` and `…Z` name the same instant and a
/// number column refuses text instead of matching nothing. The cast names
/// come from the column type, never from the request.
fn operand(column: &Column, value: &Value) -> Result<(&'static str, String), QueryError> {
    let refused = |why: &'static str| Err(unsupported(column, why));
    match (column.ty, value) {
        (FieldType::Integer | FieldType::Float | FieldType::Decimal, Value::Number(n)) => {
            Ok(("numeric", n.to_string()))
        }
        (FieldType::Integer | FieldType::Float | FieldType::Decimal, _) => {
            refused("this column takes numbers")
        }
        (FieldType::Boolean, Value::Bool(b)) => Ok(("boolean", b.to_string())),
        (FieldType::Boolean, _) => refused("this column takes yes or no"),
        (FieldType::String | FieldType::Enum, Value::String(text)) if text.len() <= MAX_TEXT => {
            Ok(("text", text.clone()))
        }
        (FieldType::String | FieldType::Enum, _) => {
            refused("this column takes text of at most 256 bytes")
        }
        (FieldType::Date, Value::String(text)) => match calendar_day(text) {
            Some(day) => Ok(("date", day.to_string())),
            None => refused("this column takes a YYYY-MM-DD date from year 1"),
        },
        (FieldType::Date, _) => refused("this column takes a YYYY-MM-DD date from year 1"),
        (FieldType::DateTime, Value::String(text)) => match calendar_instant(text) {
            Some(at) => Ok(("timestamptz", at.to_string())),
            None => refused("this column takes a date and time with its offset, from year 1"),
        },
        (FieldType::DateTime, _) => {
            refused("this column takes a date and time with its offset, from year 1")
        }
    }
}

/// `value op operand` for a float column. An operand a `real` can hold is
/// bound [`Untyped`], so it compares in the column's own type. Any other
/// (`1e100`, `1e-100`) compares as `float8`: a `double precision` column
/// reads it natively, and a `real` column, which can never equal it, widens
/// exactly, so equality finds nothing and a range bound orders correctly,
/// instead of PostgreSQL refusing the operand as out of range.
fn float_test(value: &str, op: &str, text: String, binds: &mut Binds) -> String {
    if fits_real(&text) {
        format!("({value}) {op} {}", binds.push(Untyped(text)))
    } else {
        format!("({value})::float8 {op} {}::text::float8", binds.push(text))
    }
}

/// Whether PostgreSQL reads this number as a `real`: zero, or a value that
/// neither overflows nor underflows to zero in single precision.
fn fits_real(text: &str) -> bool {
    text.parse::<f64>().is_ok_and(|v| {
        let single = v as f32;
        v == 0.0 || (single.is_finite() && single != 0.0)
    })
}

/// A float operand, bound in PostgreSQL's text format with no cast, so the
/// server types it from the column it is compared with, as it would a
/// literal: a `real` column compares in `real` and a `double precision` one
/// in `float8`. Through `numeric` a double rounds to 15 digits (a stored
/// value misses itself, a near neighbour matches); through `float8` a
/// `real` widens (`0.1` misses itself).
#[derive(Debug)]
struct Untyped(String);
impl ToSql for Untyped {
    fn to_sql(
        &self,
        _: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        out.extend_from_slice(self.0.as_bytes());
        Ok(IsNull::No)
    }
    fn accepts(_: &Type) -> bool {
        true
    }
    fn encode_format(&self, _: &Type) -> Format {
        Format::Text
    }
    tokio_postgres::types::to_sql_checked!();
}

/// A `YYYY-MM-DD` day PostgreSQL can take. It has no year 0 and reads no
/// negative years, so days before year 1 are refused here, not as a
/// database error.
fn calendar_day(text: &str) -> Option<Date> {
    text.parse::<Date>().ok().filter(|day| day.year() >= 1)
}

/// An instant with its offset, from year 1 in UTC (see [`calendar_day`]).
fn calendar_instant(text: &str) -> Option<Timestamp> {
    text.parse::<Timestamp>()
        .ok()
        .filter(|at| at.to_zoned(TimeZone::UTC).year() >= 1)
}

fn test(
    column: &Column,
    value: &str,
    test: &Test,
    today: Date,
    binds: &mut Binds,
) -> Result<String, QueryError> {
    let ty = column.ty;
    let dated = matches!(ty, FieldType::Date | FieldType::DateTime);
    Ok(match test {
        Test::Equals(wanted) => {
            let (cast, text) = operand(column, wanted)?;
            if ty == FieldType::Float {
                float_test(value, "=", text, binds)
            } else {
                format!("({value})::{cast} = {}::text::{cast}", binds.push(text))
            }
        }
        Test::OneOf(choices) => {
            if choices.is_empty() || choices.len() > MAX_CHOICES {
                return Err(unsupported(column, "one of takes 1 to 100 values"));
            }
            let mut cast = "text";
            let mut texts = Vec::with_capacity(choices.len());
            for choice in choices {
                let (typed, text) = operand(column, choice)?;
                cast = typed;
                texts.push(text);
            }
            if ty == FieldType::Float {
                // Values a `real` can hold, as an array literal in the
                // column's own type; any other as `float8` (see `float_test`).
                let (native, wide): (Vec<String>, Vec<String>) =
                    texts.into_iter().partition(|text| fits_real(text));
                let mut parts = Vec::new();
                if !native.is_empty() {
                    let list = format!("{{{}}}", native.join(","));
                    parts.push(format!("({value}) = ANY({})", binds.push(Untyped(list))));
                }
                if !wide.is_empty() {
                    parts.push(format!(
                        "({value})::float8 = ANY({}::text[]::float8[])",
                        binds.push(wide)
                    ));
                }
                format!("({})", parts.join(" OR "))
            } else {
                format!(
                    "({value})::{cast} = ANY({}::text[]::{cast}[])",
                    binds.push(texts)
                )
            }
        }
        Test::Contains(text) => {
            if !matches!(ty, FieldType::String | FieldType::Enum) {
                return Err(unsupported(column, "contains takes a text column"));
            }
            if text.is_empty() || text.len() > MAX_TEXT {
                return Err(unsupported(column, "contains takes 1 to 256 bytes of text"));
            }
            format!(
                "position(lower({}::text) in lower(({value})::text)) > 0",
                binds.push(text.clone())
            )
        }
        Test::Range { min, max } if ty.number() => {
            if min.is_none() && max.is_none() {
                return Err(unsupported(column, "a range needs a minimum or a maximum"));
            }
            let mut parts = Vec::new();
            for (bound, op) in [(min, ">="), (max, "<=")] {
                let Some(bound) = bound else { continue };
                let Value::Number(number) = bound else {
                    return Err(unsupported(column, "a number range takes numbers"));
                };
                parts.push(if ty == FieldType::Float {
                    float_test(value, op, number.to_string(), binds)
                } else {
                    let placeholder = binds.push(number.to_string());
                    format!("({value})::numeric {op} {placeholder}::text::numeric")
                });
            }
            parts.join(" AND ")
        }
        Test::Range { min, max } if dated => {
            let day = |bound: &Option<Value>| -> Result<Option<Date>, QueryError> {
                match bound {
                    None => Ok(None),
                    Some(Value::String(text)) => calendar_day(text).map(Some).ok_or_else(|| {
                        unsupported(column, "a date range takes YYYY-MM-DD days from year 1")
                    }),
                    Some(_) => Err(unsupported(
                        column,
                        "a date range takes YYYY-MM-DD days from year 1",
                    )),
                }
            };
            let (from, to) = (day(min)?, day(max)?);
            if from.is_none() && to.is_none() {
                return Err(unsupported(column, "a range needs a minimum or a maximum"));
            }
            days(value, ty, from, to, binds)
        }
        Test::Range { .. } => return Err(unsupported(column, "ranges take numbers or dates")),
        Test::Within(period) => {
            if !dated {
                return Err(unsupported(column, "a period takes a date column"));
            }
            let (from, to) = period.days(today);
            days(value, ty, Some(from), Some(to), binds)
        }
        Test::Empty | Test::NotEmpty => {
            let empty = if ty == FieldType::String {
                format!("(({value}) IS NULL OR ({value})::text = '')")
            } else {
                format!("({value}) IS NULL")
            };
            if matches!(test, Test::Empty) {
                empty
            } else {
                format!("NOT {empty}")
            }
        }
    })
}

/// Whole days, both ends included. A date-time compares against the
/// instants those days start and end at in the reader's zone, so the
/// owner's index on the instant still bounds the read.
fn days(
    value: &str,
    ty: FieldType,
    from: Option<Date>,
    to: Option<Date>,
    binds: &mut Binds,
) -> String {
    let mut parts = Vec::new();
    if let Some(from) = from {
        let day = binds.push(from.to_string());
        parts.push(match ty {
            FieldType::DateTime => {
                format!("({value}) >= ({day}::text::date)::timestamp AT TIME ZONE actor.zone")
            }
            _ => format!("({value})::date >= {day}::text::date"),
        });
    }
    if let Some(to) = to {
        let day = binds.push(to.to_string());
        parts.push(match ty {
            FieldType::DateTime => {
                format!("({value}) < ({day}::text::date + 1)::timestamp AT TIME ZONE actor.zone")
            }
            _ => format!("({value})::date <= {day}::text::date"),
        });
    }
    parts.join(" AND ")
}
