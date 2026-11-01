//! Company list predicates. Extension paths are resolved by Base Grid, never by page-owned joins.
use crate::{field_contributions, Company, CompanyField, CompanyFieldKind, Page, Params};
use bilibi_database::{Error, Result};
use bilibi_grid::Catalog;
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use tokio_postgres::{types::ToSql, Client, GenericClient};

pub const MAX_FILTERS: usize = 10;
pub const MAX_FILTER_BYTES: usize = 16_384;
/// Keeps invalid text drafts visible while bounding their serialized URL state.
pub const MAX_FILTER_DRAFT_BYTES: usize = 1_024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldFilter {
    pub field: String,
    pub operator: String,
    pub value: String,
}

/// What a list filter or sort may hold, whatever field it is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ListKind {
    Text,
    /// A tenant-defined number: an exact decimal string, compared as a number.
    Number,
    Date,
    Choice {
        picklist: String,
    },
}
impl ListKind {
    fn of(kind: CompanyFieldKind) -> Self {
        match kind {
            CompanyFieldKind::Text { .. } => Self::Text,
            CompanyFieldKind::Date => Self::Date,
            CompanyFieldKind::Choice { picklist } => Self::Choice {
                picklist: picklist.into(),
            },
        }
    }
}

/// One field the list can filter and sort by: a contributed Extension field
/// (`definition`, read through a Grid path) or a tenant's custom field
/// (`custom_key`, read from `companies.custom_fields`).
pub(crate) struct AvailableField {
    pub id: String,
    pub label: String,
    pub kind: ListKind,
    pub definition: Option<CompanyField>,
    pub custom_key: Option<String>,
    pub choices: Vec<bilibi_ui::Choice>,
}

/// The id a custom field has in the list's sort, filter and column state.
pub(crate) fn custom_id(key: &str) -> String {
    field_contributions::custom_name(key)
}

pub(crate) async fn available(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    catalog: &Catalog,
    masked: &HashSet<String>,
) -> Result<Vec<AvailableField>> {
    let mut result = Vec::new();
    for (_, definition) in field_contributions::definitions()? {
        if masked.contains(definition.id)
            || bilibi_grid::scalar_join(catalog, "companies", definition.grid_column).is_err()
        {
            continue;
        }
        let choices = match definition.kind {
            CompanyFieldKind::Choice { picklist } => {
                field_contributions::picklists()?
                    .choices(client, scope, picklist, None)
                    .await?
            }
            _ => Vec::new(),
        };
        result.push(AvailableField {
            id: definition.id.into(),
            label: definition.label.into(),
            kind: ListKind::of(definition.kind),
            definition: Some(definition),
            custom_key: None,
            choices,
        });
    }
    let custom = field_contributions::custom_fields()?
        .active(client, scope, field_contributions::CUSTOM_RECORD_TYPE)
        .await?;
    for definition in custom.into_iter().take(MAX_CUSTOM_COLUMNS) {
        let (kind, choices) = match (definition.kind, definition.picklist.as_deref()) {
            (bilibi_custom_fields::Kind::Number, _) => (ListKind::Number, Vec::new()),
            (bilibi_custom_fields::Kind::Date, _) => (ListKind::Date, Vec::new()),
            (bilibi_custom_fields::Kind::Choice, Some(list)) => (
                ListKind::Choice {
                    picklist: list.into(),
                },
                field_contributions::picklists()?
                    .choices(client, scope, list, None)
                    .await?,
            ),
            _ => (ListKind::Text, Vec::new()),
        };
        result.push(AvailableField {
            id: custom_id(&definition.key),
            label: definition.label,
            kind,
            definition: None,
            custom_key: Some(definition.key),
            choices,
        });
    }
    Ok(result)
}

/// A tenant may define many custom fields; the list offers the first of them
/// (in the order they were defined) as columns, sorts and filters.
pub(crate) const MAX_CUSTOM_COLUMNS: usize = 50;

pub(crate) fn operators(kind: &ListKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        ListKind::Text => &[("contains", "Contains"), ("eq", "Equals")],
        ListKind::Number => &[
            ("eq", "Equals"),
            ("lt", "Less than"),
            ("lte", "At most"),
            ("gt", "More than"),
            ("gte", "At least"),
        ],
        ListKind::Date => &[
            ("eq", "On"),
            ("lt", "Before"),
            ("lte", "On or before"),
            ("gt", "After"),
            ("gte", "On or after"),
        ],
        ListKind::Choice { .. } => &[("eq", "Equals")],
    }
}

fn filter_error(filter: &FieldFilter, field: &AvailableField) -> Option<String> {
    if !operators(&field.kind)
        .iter()
        .any(|(op, _)| *op == filter.operator)
    {
        return Some("Choose an operator supported by this field.".into());
    }
    let valid = match &field.kind {
        ListKind::Text => !filter.value.is_empty() && filter.value.chars().count() <= 256,
        ListKind::Number => bilibi_custom_fields::valid_number(&filter.value),
        ListKind::Date => field_contributions::valid_date(&filter.value),
        ListKind::Choice { .. } => field
            .choices
            .iter()
            .any(|choice| choice.value == filter.value),
    };
    (!valid).then(|| {
        match &field.kind {
            ListKind::Text => "Enter text up to 256 characters.",
            ListKind::Number => "Enter a number such as 12 or -3.5.",
            ListKind::Date => "Enter a valid date from 0001-01-01 to 9999-12-31.",
            ListKind::Choice { .. } => "Choose an active value from this list.",
        }
        .into()
    })
}

pub(crate) async fn list(client: &mut Client, scope: &Scope, params: &Params) -> Result<Page> {
    params.validate()?;
    bilibi_authz::authorize_key(client, scope, crate::LIST).await?;
    let tx = client
        .build_transaction()
        .read_only(true)
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await?;
    tx.batch_execute(bilibi_grid::BOUNDS).await?;
    let result = read(&tx, scope, params).await;
    match result {
        Ok(page) => {
            tx.commit().await?;
            Ok(page)
        }
        Err(error)
            if error.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED)
                || error.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE) =>
        {
            tx.rollback().await?;
            Ok(Page::limited())
        }
        Err(error) => Err(error.into()),
    }
}

// Keep database errors intact until the owner can turn timeout into a bounded
// refusal. Validation/catalog failures are handled before running statements.
async fn read(
    tx: &tokio_postgres::Transaction<'_>,
    scope: &Scope,
    params: &Params,
) -> std::result::Result<Page, ReadError> {
    let masked = field_contributions::effective_restrictions(tx, scope).await?;
    let catalog = Catalog::for_scope(tx, scope).await.map_err(Error::from)?;
    let fields = available(tx, scope, &catalog, &masked).await?;
    let mut errors = Vec::with_capacity(params.field_filters.len());
    let mut notice = String::new();
    let mut used_fields = HashSet::new();
    for filter in &params.field_filters {
        if filter.field.is_empty() {
            errors.push(None);
            continue;
        }
        let Some(field) = fields.iter().find(|field| field.id == filter.field) else {
            errors.push(Some(
                "This filter is unavailable and was not applied.".into(),
            ));
            notice = "An unavailable field was removed from the query.".into();
            continue;
        };
        let error = filter_error(filter, field);
        if error.is_none() && field.definition.is_some() {
            used_fields.insert(field.id.clone());
        }
        errors.push(error);
    }

    let mut sort = params.sort.clone();
    let sort_field = fields.iter().find(|field| field.id == sort);
    if let Some(field) = sort_field {
        if field.definition.is_some() {
            used_fields.insert(field.id.clone());
        }
    } else if !["name", "status", "jurisdiction", "created_at"].contains(&sort.as_str())
        || masked.contains(&sort)
    {
        sort = "name".into();
        notice = "An unavailable field was removed from the query.".into();
    }

    let mut expressions = HashMap::new();
    let mut joins = BTreeMap::new();
    for field in fields
        .iter()
        .filter(|field| used_fields.contains(&field.id))
    {
        let Some(definition) = &field.definition else {
            continue;
        };
        let join = bilibi_grid::scalar_join(&catalog, "companies", definition.grid_column)
            .map_err(Error::from)?;
        joins.entry(join.alias).or_insert(join.clause);
        expressions.insert(field.id.clone(), join.expression);
    }

    let mut args: Vec<Box<dyn ToSql + Sync + Send>> = vec![
        Box::new(scope.tenant()),
        Box::new(scope.user()),
        Box::new(params.status.clone()),
        Box::new(format!("%{}%", crate::organisation::escape_like(&params.q))),
    ];
    let searchable = ["name", "code", "legal_name", "email", "jurisdiction"]
        .into_iter()
        .filter(|name| !masked.contains(*name))
        .map(|name| format!("r.{name} ILIKE $4"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let search = if searchable.is_empty() {
        // Empty search stays unrestricted; when every searchable field is
        // masked, a nonempty search must match nothing without invalid SQL.
        "$4='%%'".to_owned()
    } else {
        searchable
    };
    let mut predicates = vec![
        "r.tenant_id=$1".into(),
        "r.deleted_at IS NULL".into(),
        "($3='' OR r.status=$3)".into(),
        format!("({search})"),
    ];
    for filter in &params.field_filters {
        // A newly added row is an editable draft, not a missing-field query.
        if filter.field.is_empty() {
            continue;
        }
        let Some(field) = fields.iter().find(|field| field.id == filter.field) else {
            notice = "An unavailable field was removed from the query.".into();
            continue;
        };
        if filter_error(filter, field).is_some() {
            continue;
        }
        let expression = match &field.custom_key {
            // The key is a bound value, never part of the statement text.
            Some(key) => {
                args.push(Box::new(key.clone()));
                custom_expression(args.len(), &field.kind)
            }
            None => expressions.get(&field.id).ok_or(Error::Internal)?.clone(),
        };
        let operator = match filter.operator.as_str() {
            "contains" => "ILIKE",
            "eq" => "=",
            "lt" => "<",
            "lte" => "<=",
            "gt" => ">",
            "gte" => ">=",
            _ => return Err(Error::Invalid.into()),
        };
        let value = if filter.operator == "contains" {
            format!("%{}%", crate::organisation::escape_like(&filter.value))
        } else {
            filter.value.clone()
        };
        args.push(Box::new(value));
        let cast = match field.kind {
            ListKind::Date => "::text::date",
            ListKind::Number => "::text::numeric",
            _ => "::text",
        };
        predicates.push(format!("{expression} {operator} ${}{cast}", args.len()));
    }
    let sort_field = fields.iter().find(|field| field.id == sort);
    let dir = if sort != params.sort {
        "asc"
    } else {
        &params.dir
    };
    let from = format!(
        "companies r {}",
        joins.values().cloned().collect::<Vec<_>>().join(" ")
    );
    let tail = format!("FROM {from} WHERE {}", predicates.join(" AND "));
    // Reserve $2 even when no contributed path uses a user-scoped source.
    let head = "WITH actor AS (SELECT $1::bigint AS tenant, $2::bigint AS usr)";
    let count = format!("{head} SELECT count(*) {tail}");
    let binds = refs(&args);
    if !admit(tx, &count, &binds).await? {
        return Ok(Page::limited());
    }
    let total = tx.query_one(&count, &binds).await?.get(0);
    // A custom sort key is a bound value of the page statement only, so the
    // count statement above has no unused parameter.
    let expression = if let Some(field) = sort_field {
        match &field.custom_key {
            Some(key) => {
                args.push(Box::new(key.clone()));
                custom_expression(args.len(), &field.kind)
            }
            None => expressions.get(&field.id).ok_or(Error::Internal)?.clone(),
        }
    } else if ["name", "status", "jurisdiction", "created_at"].contains(&sort.as_str())
        && !masked.contains(&sort)
    {
        format!("r.{sort}")
    } else {
        return Err(Error::Internal.into());
    };
    args.push(Box::new(params.size));
    let limit = args.len();
    args.push(Box::new((params.page - 1) * params.size));
    let columns = crate::organisation::COLUMNS
        .split(',')
        .map(|column| format!("r.{}", column.trim()))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("{head} SELECT {columns} {tail} ORDER BY {expression} {dir} NULLS LAST, r.id ASC LIMIT ${limit} OFFSET ${}", args.len());
    let binds = refs(&args);
    if !admit(tx, &sql, &binds).await? {
        return Ok(Page::limited());
    }
    let rows = tx
        .query(&sql, &binds)
        .await?
        .iter()
        .map(|row| Company::from_row(row, &masked))
        .collect();
    Ok(Page {
        rows,
        total,
        filter_errors: errors,
        notice,
        sort,
        limited: false,
    })
}

/// A custom field's value as the SQL type its sort and comparisons need, read
/// from the owner's JSON column with the key as bound parameter `$index`.
fn custom_expression(index: usize, kind: &ListKind) -> String {
    let cast = match kind {
        ListKind::Number => "::numeric",
        ListKind::Date => "::date",
        _ => "",
    };
    format!("(r.custom_fields ->> ${index}::text){cast}")
}

fn refs(args: &[Box<dyn ToSql + Sync + Send>]) -> Vec<&(dyn ToSql + Sync)> {
    args.iter()
        .map(|arg| arg.as_ref() as &(dyn ToSql + Sync))
        .collect()
}
async fn admit(
    tx: &tokio_postgres::Transaction<'_>,
    sql: &str,
    args: &[&(dyn ToSql + Sync)],
) -> std::result::Result<bool, tokio_postgres::Error> {
    let value: Value = tx
        .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), args)
        .await?
        .get(0);
    Ok(value[0]["Plan"]["Total Cost"]
        .as_f64()
        .is_some_and(|cost| cost.is_finite() && cost <= bilibi_grid::MAX_ESTIMATED_COST))
}
enum ReadError {
    Database(tokio_postgres::Error),
    Other(Error),
}
impl ReadError {
    fn code(&self) -> Option<&tokio_postgres::error::SqlState> {
        match self {
            Self::Database(e) => e.code(),
            _ => None,
        }
    }
}
impl From<tokio_postgres::Error> for ReadError {
    fn from(e: tokio_postgres::Error) -> Self {
        Self::Database(e)
    }
}
impl From<Error> for ReadError {
    fn from(e: Error) -> Self {
        Self::Other(e)
    }
}
impl From<ReadError> for Error {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::Database(e) => e.into(),
            ReadError::Other(e) => e,
        }
    }
}
