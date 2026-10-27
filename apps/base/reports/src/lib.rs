//! Operator-built reports (BelimbingApp/bilibi#128, ADR 07): saved questions
//! over Grid tables, shared by definition and run as each reader. No Bilimbi
//! counterpart. The facade is [`Reports`] (service [`SERVICE`]); pages and
//! agents both reach it through the operation contracts in
//! `agent_operations.rs`. Contract: `README.md`.
mod agent_operations;
mod builtin;
mod contributions;
mod definition;
mod export;
mod exports;
mod schedules;
mod store;
mod web;

#[doc(hidden)]
pub use bilibi_module_registry::{Contribution, Provenance};
pub use builtin::{Builtin, BUILTINS};
pub use contributions::{AUTHOR, EXPORT, MANAGE, SCHEDULE, SHARE, VIEW};
pub use definition::{
    Definition, FilterDef, GroupingDef, Kind, MeasureDef, SortDef, CHARTS, MAX_COLUMNS,
};
pub use export::{File, Format};
pub use exports::Prepared;
#[doc(hidden)]
pub use inventory;
pub use schedules::{Frequency, Plan, Subscription};
pub use store::Report;

use bilibi_agent_api::schema::FieldErrors;
use bilibi_database::{Change, Db, Error, MutationTransaction};
use bilibi_datetime::DisplayPolicy;
use bilibi_grid::{
    Catalog, Column, FieldType, Filter, KeyPage, PageSpec, QueryError, SummarySpec, Test,
};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

pub const SERVICE: &str = "base/reports.reports";
const DOWNLOAD: &str = "base/reports.export";
const EXPORTED: &str = "base/reports.exported";
const PREPARER: &str = "base/reports.preparer";
const SWEEPER: &str = "base/reports.sweeper";
const DELIVERER: &str = "base/reports.deliverer";
const MIGRATIONS: [(&str, &str); 5] = [
    (
        "base-reports-001-reports",
        include_str!("../migrations/001-reports.sql"),
    ),
    (
        "base-reports-002-matrix",
        include_str!("../migrations/002-matrix.sql"),
    ),
    (
        "base-reports-003-exports",
        include_str!("../migrations/003-exports.sql"),
    ),
    (
        "base-reports-004-subscriptions",
        include_str!("../migrations/004-subscriptions.sql"),
    ),
    (
        "base-reports-005-export-access",
        include_str!("../migrations/005-export-access.sql"),
    ),
];
const KIND: &str = "base_report";
/// Columns offered to a builder for one record type, at most.
const MAX_OFFERED: usize = 300;
/// Roles one report is shared with, at most.
const MAX_SHARE_ROLES: usize = 50;

/// Why a report request was refused; pages and agents show the message.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("invalid report")]
    Invalid(FieldErrors),
    #[error("This report does not exist or is not shared with you.")]
    NotFound,
    #[error("You do not have permission to do this.")]
    Forbidden,
    #[error("Only the report's author can change it. Copy it to make your own.")]
    NotOwner,
    #[error("The report changed since you opened it. Reload and try again.")]
    Conflict,
    #[error("You cannot read this report's record type.")]
    Unreadable,
    #[error("This report filters on {0}, which you cannot see, so it cannot run for you.")]
    HiddenFilter(String),
    /// A Grid bound (too many groups, the cost bound, a filter value that
    /// does not fit): the message is the reader's to see.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Database(#[from] Error),
}
type Outcome<T> = Result<T, ReportError>;

/// What a create, update or copy saves.
#[derive(Clone, Debug)]
pub struct Draft {
    pub name: String,
    pub description: String,
    pub record_type: String,
    pub kind: Kind,
    pub definition: Definition,
}

/// What a reader passes to a run: answers to "ask when run" filters (by
/// filter position) and drill-down filters added on top.
#[derive(Clone, Debug, Default)]
pub struct RunInput {
    pub answers: BTreeMap<usize, Value>,
    pub drill: Vec<FilterDef>,
    /// Rows reports: the page, from 0.
    pub page: usize,
    /// A summary or matrix read as the rows behind it (a drill-down): its
    /// label field, groupings and measured columns, page by page.
    pub as_rows: bool,
}

/// One column of a result, as the reader's catalog names it.
#[derive(Clone, Debug, Serialize)]
pub struct ColumnInfo {
    pub spec: String,
    pub label: String,
    pub ty: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Run {
    pub report: Report,
    /// Column paths dropped because the reader cannot see them.
    pub left_out: Vec<String>,
    /// The reader's time zone the dates were read in.
    pub timezone: String,
    #[serde(flatten)]
    pub result: RunResult,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunResult {
    Rows {
        columns: Vec<ColumnInfo>,
        /// Each row: its key, then one cell per column.
        rows: Vec<(Value, Vec<Value>)>,
        /// Each row's own record page, where its module declares one.
        pages: Vec<Option<String>>,
        total: i64,
        page: usize,
        page_size: usize,
        /// The definition's totals line: one sum per number column over
        /// every row, `null` for any other column.
        totals: Option<Vec<Value>>,
    },
    Summary {
        groupings: Vec<(ColumnInfo, Option<&'static str>)>,
        measures: Vec<(ColumnInfo, &'static str)>,
        /// Each line: keys, subtotal flag, row count, one value per measure.
        lines: Vec<(Vec<Value>, bool, i64, Vec<Value>)>,
        total: (i64, Vec<Value>),
        chart: Option<String>,
    },
}

pub struct Reports {
    db: Db,
    settings: Arc<Settings>,
    display: Arc<DisplayPolicy>,
    queue: Arc<bilibi_queue::Queue>,
    artifacts: Arc<bilibi_artifacts::Artifacts>,
    notifications: Arc<bilibi_notifications::NotificationBoundary>,
    authentication: Arc<bilibi_tenancy::Authentication>,
    mutations: Arc<bilibi_database::MutationBoundary>,
    mailer: Arc<bilibi_mail::Mailer>,
    /// The public origin emailed links point at (`APP_ORIGIN`).
    origin: String,
    builtins: Vec<&'static Builtin>,
}

fn field_errors(field: &str, message: impl Into<String>) -> FieldErrors {
    let mut errors = FieldErrors::new();
    errors.insert(field.into(), vec![message.into()]);
    errors
}

fn type_key(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Integer => "integer",
        FieldType::Float => "float",
        FieldType::Decimal => "decimal",
        FieldType::String => "string",
        FieldType::Boolean => "boolean",
        FieldType::Date => "date",
        FieldType::DateTime => "datetime",
        FieldType::Enum => "choice",
    }
}

fn info(column: &Column) -> ColumnInfo {
    ColumnInfo {
        spec: column.spec.clone(),
        label: column.label.clone(),
        ty: type_key(column.ty),
    }
}

/// A form's or agent's text, typed for the column: numbers for a number
/// column, yes/no for a boolean one, text otherwise.
fn typed(value: &Value, ty: FieldType) -> Value {
    let Value::String(text) = value else {
        return value.clone();
    };
    match ty {
        FieldType::Integer | FieldType::Float | FieldType::Decimal => text
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| value.clone(), Value::Number),
        FieldType::Boolean => match text.trim() {
            "true" | "yes" => Value::Bool(true),
            "false" | "no" => Value::Bool(false),
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

/// The columns a summary's drill-down lists: the record's name, then each
/// grouping and measured column once, within the rows bound.
fn behind(root: &bilibi_grid::Table, definition: &Definition) -> Vec<String> {
    let mut specs: Vec<String> = Vec::new();
    let candidates = root
        .label_field()
        .map(str::to_owned)
        .into_iter()
        .chain(definition.groupings.iter().map(|g| g.column.clone()))
        .chain(definition.measures.iter().map(|m| m.column.clone()));
    for spec in candidates {
        if !specs.contains(&spec) && specs.len() < MAX_COLUMNS {
            specs.push(spec);
        }
    }
    if specs.is_empty() {
        specs.push(root.key().to_owned());
    }
    specs
}

/// One saved or drill filter as a Grid filter on `column`, with the reader's
/// answer for an ask filter and text typed for the column. `None` when an
/// ask filter is left without a value.
fn typed_filter(
    filter: &FilterDef,
    answer: Option<&Value>,
    column: &Column,
) -> Result<Option<Filter>, &'static str> {
    Ok(filter.test(answer)?.map(|test| Filter {
        column: filter.column.clone(),
        test: match test {
            Test::Equals(v) => Test::Equals(typed(&v, column.ty)),
            Test::OneOf(vs) => Test::OneOf(vs.iter().map(|v| typed(v, column.ty)).collect()),
            other => other,
        },
    }))
}

/// A Grid refusal as the reader's message, or the database error it was.
fn grid(error: QueryError) -> ReportError {
    match error {
        QueryError::ForbiddenRoot => ReportError::Unreadable,
        QueryError::Database(error) => ReportError::Database(error),
        QueryError::Catalog(error) => ReportError::Database(error.into()),
        QueryError::Limit(why) => ReportError::Refused(format!(
            "This report is too large to run ({why}). Add a filter or group more coarsely."
        )),
        QueryError::Column(spec, why) => ReportError::Refused(format!("Column {spec}: {why}")),
        QueryError::Unsupported(spec, why) => ReportError::Refused(format!("Column {spec}: {why}")),
    }
}

impl Reports {
    async fn setting(
        &self,
        client: &dyn bilibi_tenancy::ReadClient,
        scope: &Scope,
        key: &str,
    ) -> Result<usize, Error> {
        let value = self
            .settings
            .get(client, scope, settings_target(scope), key)
            .await?
            .value;
        value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(Error::Internal)
    }

    /// The reports the reader may run. Needs `reports.view`.
    pub async fn list(&self, scope: &Scope) -> Outcome<Vec<Report>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, VIEW).await?;
        Ok(store::list(&**client, scope.tenant(), scope.user()).await?)
    }

    /// One page of the reports the reader may run, searched by name and
    /// description, with the number that match. Needs `reports.view`.
    pub async fn page(
        &self,
        scope: &Scope,
        search: &str,
        offset: u64,
        limit: u64,
    ) -> Outcome<(Vec<Report>, u64)> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, VIEW).await?;
        let clamp = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        let (reports, total) = store::page(
            &**client,
            scope.tenant(),
            scope.user(),
            search,
            clamp(offset),
            clamp(limit),
        )
        .await?;
        Ok((reports, total.max(0) as u64))
    }

    /// One report the reader may run, with its definition.
    pub async fn get(&self, scope: &Scope, id: i64) -> Outcome<Report> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, VIEW).await?;
        match store::read(&**client, scope.tenant(), scope.user(), id, false).await {
            Err(Error::NotFound) => Err(ReportError::NotFound),
            other => Ok(other?),
        }
    }

    /// Checks a draft for its author: the shape, then every column against
    /// the author's own catalog, then a bounded dry run that proves each
    /// filter, grouping and measure suits its column. A dry run refused only
    /// by size or cost still saves: the bound applies to each run.
    pub async fn check(&self, scope: &Scope, draft: &Draft) -> Outcome<()> {
        let mut errors = FieldErrors::new();
        let name = draft.name.trim();
        if name.is_empty() || name.chars().count() > 120 {
            errors.insert("name".into(), vec!["must be 1 to 120 characters".into()]);
        }
        if draft.description.chars().count() > 1000 {
            errors.insert(
                "description".into(),
                vec!["must be at most 1000 characters".into()],
            );
        }
        if let Err(more) = draft.definition.check(draft.kind) {
            errors.extend(more);
        }
        if !errors.is_empty() {
            return Err(ReportError::Invalid(errors));
        }
        let client = self.db.get().await.map_err(Error::from)?;
        let catalog = Catalog::for_scope(&**client, scope)
            .await
            .map_err(|e| ReportError::Database(e.into()))?;
        let root = catalog.table(&draft.record_type).ok_or_else(|| {
            ReportError::Invalid(field_errors(
                "record_type",
                "is not a record type you can read",
            ))
        })?;
        for spec in draft.definition.specs() {
            if let Err(error) = catalog.resolve(root, spec) {
                errors
                    .entry(format!("column {spec}"))
                    .or_default()
                    .push(error.to_string());
            }
        }
        let mut filters = Vec::new();
        for filter in &draft.definition.filters {
            let Ok(column) = catalog.resolve(root, &filter.column) else {
                continue;
            };
            match typed_filter(filter, None, &column) {
                Ok(typed) => filters.extend(typed),
                Err(why) => errors
                    .entry(format!("filter {}", filter.column))
                    .or_default()
                    .push(why.into()),
            }
        }
        if !errors.is_empty() {
            return Err(ReportError::Invalid(errors));
        }
        drop(client);
        let probe = self.probe(scope, draft, filters).await;
        match probe {
            Ok(()) => Ok(()),
            Err(QueryError::Column(spec, error)) => Err(ReportError::Invalid(field_errors(
                &format!("column {spec}"),
                error.to_string(),
            ))),
            Err(QueryError::Unsupported(spec, why)) => Err(ReportError::Invalid(field_errors(
                &format!("column {spec}"),
                why,
            ))),
            Err(QueryError::ForbiddenRoot) => Err(ReportError::Invalid(field_errors(
                "record_type",
                "is not a record type you can read",
            ))),
            Err(QueryError::Limit(_)) => Ok(()),
            Err(other) => Err(grid(other)),
        }
    }

    /// One page or one summary under the author's scope, to prove the shape.
    async fn probe(
        &self,
        scope: &Scope,
        draft: &Draft,
        filters: Vec<Filter>,
    ) -> Result<(), QueryError> {
        match draft.kind {
            Kind::Rows => {
                bilibi_grid::project(
                    &self.db,
                    scope,
                    &draft.record_type,
                    &[],
                    &draft.definition.columns,
                )
                .await?;
                bilibi_grid::page_keys(
                    &self.db,
                    scope,
                    &draft.record_type,
                    &PageSpec {
                        filters,
                        sorts: draft.definition.sorts(),
                        timezone: "UTC".into(),
                        offset: 0,
                        limit: 1,
                    },
                )
                .await?;
            }
            Kind::Summary | Kind::Matrix => {
                bilibi_grid::summarize(
                    &self.db,
                    scope,
                    &draft.record_type,
                    &SummarySpec {
                        filters,
                        groupings: draft.definition.groupings(),
                        measures: draft.definition.measures(),
                        timezone: "UTC".into(),
                        max_groups: bilibi_grid::MAX_RESULT_GROUPS,
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    /// The record types the reader can report on, as `(id, label)`, by label.
    /// Needs `reports.view`.
    pub async fn record_types(&self, scope: &Scope) -> Outcome<Vec<(String, String)>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, VIEW).await?;
        let catalog = Catalog::for_scope(&**client, scope)
            .await
            .map_err(|e| ReportError::Database(e.into()))?;
        let mut types: Vec<_> = catalog
            .tables()
            .map(|table| (table.id().to_owned(), table.label().to_owned()))
            .collect();
        types.sort_by(|a, b| a.1.cmp(&b.1));
        Ok(types)
    }

    /// The columns a report on `record_type` can use for this reader: the
    /// table's own fields, then each field of a table one link away. Fields
    /// the reader cannot see are not offered. Needs `reports.view`.
    pub async fn columns(&self, scope: &Scope, record_type: &str) -> Outcome<Vec<ColumnInfo>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, VIEW).await?;
        let catalog = Catalog::for_scope(&**client, scope)
            .await
            .map_err(|e| ReportError::Database(e.into()))?;
        let root = catalog.table(record_type).ok_or(ReportError::Unreadable)?;
        let mut specs: Vec<String> = root.visible_fields().map(|f| f.id.to_owned()).collect();
        for link in root.links() {
            if link.kind() != bilibi_grid::LinkKind::One {
                continue;
            }
            if let Some(target) = catalog.table(link.to()) {
                specs.extend(
                    target
                        .visible_fields()
                        .map(|f| format!("{}.{}", link.id(), f.id)),
                );
            }
        }
        Ok(specs
            .iter()
            .filter_map(|spec| catalog.resolve(root, spec).ok())
            .take(MAX_OFFERED)
            .map(|column| info(&column))
            .collect())
    }

    /// The most rows an export holds, for the reader's tenant.
    pub async fn export_rows(&self, scope: &Scope) -> Outcome<usize> {
        let client = self.db.get().await.map_err(Error::from)?;
        Ok(self
            .setting(&**client, scope, contributions::EXPORT_ROWS_KEY)
            .await?)
    }

    /// The tenant's roles an author may share with, as `(id, name)`. Needs
    /// `reports.share`: a role's name is all the sharer sees.
    pub async fn share_roles(&self, scope: &Scope) -> Outcome<Vec<(i64, String)>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, SHARE).await?;
        Ok(store::roles(&**client, scope.tenant()).await?)
    }

    /// What this reader may see of a report now, as a key that changes when
    /// it does: its version and which of its columns their catalog resolves.
    /// A cached drawing is reused only while this key holds, so a field
    /// restriction or a lost share shows at the next refresh, not the next
    /// run. Cheaper than a run: no Grid read. Needs `reports.view`.
    pub async fn access(&self, scope: &Scope, id: i64) -> Outcome<String> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, VIEW).await?;
        let report = match store::read(&**client, scope.tenant(), scope.user(), id, false).await {
            Err(Error::NotFound) => return Err(ReportError::NotFound),
            other => other?,
        };
        let catalog = Catalog::for_scope(&**client, scope)
            .await
            .map_err(|e| ReportError::Database(e.into()))?;
        let Some(root) = catalog.table(&report.record_type) else {
            return Ok(format!("{}:unreadable", report.version));
        };
        let d = &report.definition;
        let seen: String = d
            .columns
            .iter()
            .chain(d.groupings.iter().map(|g| &g.column))
            .chain(d.measures.iter().map(|m| &m.column))
            .chain(d.sort.iter().map(|s| &s.column))
            .chain(d.filters.iter().map(|f| &f.column))
            .map(|spec| {
                if catalog.resolve(root, spec).is_ok() {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        Ok(format!("{}:{seen}", report.version))
    }

    /// Runs a report as the reader: their catalog, grants and time zone.
    /// Columns, groupings, measures and sorts the reader cannot see are left
    /// out and listed; a filter on one refuses the run, so it cannot be used
    /// to infer a hidden value. Needs `reports.view`.
    pub async fn run(&self, scope: &Scope, id: i64, input: &RunInput) -> Outcome<Run> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, VIEW).await?;
        let report = match store::read(&**client, scope.tenant(), scope.user(), id, false).await {
            Err(Error::NotFound) => return Err(ReportError::NotFound),
            other => other?,
        };
        drop(client);
        self.run_report(scope, report, input).await
    }

    /// Runs a built-in report as the reader, like a stored one.
    pub async fn run_builtin(&self, scope: &Scope, key: &str, input: &RunInput) -> Outcome<Run> {
        let report = self.builtin(scope, key).await?;
        self.run_report(scope, report, input).await
    }

    /// The built-in reports the reader can run: those over a record type
    /// their catalog includes. Needs `reports.view`.
    pub async fn builtins(&self, scope: &Scope) -> Outcome<Vec<Report>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, VIEW).await?;
        let catalog = Catalog::for_scope(&**client, scope)
            .await
            .map_err(|e| ReportError::Database(e.into()))?;
        Ok(self
            .builtins
            .iter()
            .filter(|b| catalog.table(b.record_type).is_some())
            .filter_map(|b| as_report(b))
            .collect())
    }

    /// One built-in report the reader can run.
    pub async fn builtin(&self, scope: &Scope, key: &str) -> Outcome<Report> {
        self.builtins(scope)
            .await?
            .into_iter()
            .find(|r| r.builtin == Some(key))
            .ok_or(ReportError::NotFound)
    }

    /// Saves the reader's own copy of a built-in report. Needs `reports.author`.
    pub async fn copy_builtin(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        key: &str,
        name: Option<&str>,
    ) -> Outcome<i64> {
        let original = self.builtin(scope, key).await?;
        self.create(tx, scope, &copy_of(&original, name)).await
    }

    async fn run_report(&self, scope: &Scope, report: Report, input: &RunInput) -> Outcome<Run> {
        let client = self.db.get().await.map_err(Error::from)?;
        let timezone = self
            .display
            .display(&**client, scope, None, "en")
            .await?
            .timezone;
        let catalog = Catalog::for_scope(&**client, scope)
            .await
            .map_err(|e| ReportError::Database(e.into()))?;
        let root = catalog
            .table(&report.record_type)
            .ok_or(ReportError::Unreadable)?;
        let resolve = |spec: &str| catalog.resolve(root, spec).ok();
        let mut left_out = Vec::new();
        let mut keep = |spec: &str| -> Option<Column> {
            let column = resolve(spec);
            if column.is_none() && !left_out.iter().any(|s| s == spec) {
                left_out.push(spec.to_owned());
            }
            column
        };
        let definition = &report.definition;
        let mut filters = Vec::new();
        let drill = input.drill.iter().map(|f| (f, None));
        let saved = definition
            .filters
            .iter()
            .enumerate()
            .map(|(i, f)| (f, input.answers.get(&i)));
        for (filter, answer) in saved.chain(drill) {
            let column = resolve(&filter.column)
                .ok_or_else(|| ReportError::HiddenFilter(filter.column.clone()))?;
            let typed = typed_filter(filter, answer, &column).map_err(|why| {
                ReportError::Invalid(field_errors(&format!("filter {}", filter.column), why))
            })?;
            filters.extend(typed);
        }
        let as_rows = report.kind == Kind::Rows || input.as_rows;
        let result = match report.kind {
            _ if as_rows => {
                let specs: Vec<String> = if report.kind == Kind::Rows {
                    definition.columns.clone()
                } else {
                    behind(root, definition)
                };
                // Each column once, as the projection draws it, so a totals
                // cell always sits under its own column (a definition saved
                // before duplicates were refused may still hold one).
                let mut once = std::collections::HashSet::new();
                let columns: Vec<Column> = specs
                    .iter()
                    .filter(|s| once.insert(s.as_str()))
                    .filter_map(|s| keep(s))
                    .collect();
                let sorts = if report.kind == Kind::Rows {
                    definition
                        .sorts()
                        .into_iter()
                        .filter(|s| keep(&s.column).is_some())
                        .collect()
                } else {
                    Vec::new()
                };
                let totals_of: Vec<Option<bilibi_grid::Measure>> = if report.kind == Kind::Rows
                    && definition.totals
                {
                    columns
                        .iter()
                        .map(|c| {
                            (c.ty.number() && c.spec != root.key()).then(|| bilibi_grid::Measure {
                                column: c.spec.clone(),
                                agg: bilibi_grid::Agg::Sum,
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let totals_filters = filters.clone();
                // A value saved before the setting's bound was 100 still runs.
                let page_size = self
                    .setting(&**client, scope, contributions::PAGE_SIZE_KEY)
                    .await?
                    .clamp(1, 100);
                drop(client);
                let page: KeyPage = bilibi_grid::page_keys(
                    &self.db,
                    scope,
                    &report.record_type,
                    &PageSpec {
                        filters,
                        sorts,
                        timezone: timezone.clone(),
                        offset: input.page.saturating_mul(page_size),
                        limit: page_size,
                    },
                )
                .await
                .map_err(grid)?;
                let specs: Vec<&str> = columns.iter().map(|c| c.spec.as_str()).collect();
                let projection =
                    bilibi_grid::project(&self.db, scope, &report.record_type, &page.keys, &specs)
                        .await
                        .map_err(grid)?;
                let rows: Vec<(Value, Vec<Value>)> = projection
                    .rows
                    .into_iter()
                    .map(|row| {
                        let cells = projection
                            .columns
                            .iter()
                            .map(|c| row.cells.get(&c.id).cloned().unwrap_or(Value::Null))
                            .collect();
                        (row.key, cells)
                    })
                    .collect();
                let pages = rows.iter().map(|(key, _)| root.page_for(key)).collect();
                let totals = if totals_of.iter().any(Option::is_some) {
                    // Grid totals up to 8 measures a read; a wider list
                    // (up to 32 columns) takes one read per eight.
                    let measures: Vec<_> = totals_of.iter().flatten().cloned().collect();
                    let mut values = Vec::with_capacity(measures.len());
                    for chunk in measures.chunks(bilibi_grid::MAX_MEASURES) {
                        let summary = bilibi_grid::summarize(
                            &self.db,
                            scope,
                            &report.record_type,
                            &SummarySpec {
                                filters: totals_filters.clone(),
                                groupings: Vec::new(),
                                measures: chunk.to_vec(),
                                timezone: timezone.clone(),
                                max_groups: 1,
                            },
                        )
                        .await
                        .map_err(grid)?;
                        values.extend(summary.total.values);
                    }
                    let mut sums = values.into_iter();
                    Some(
                        totals_of
                            .iter()
                            .map(|m| {
                                if m.is_some() {
                                    sums.next().unwrap_or(Value::Null)
                                } else {
                                    Value::Null
                                }
                            })
                            .collect(),
                    )
                } else {
                    None
                };
                RunResult::Rows {
                    columns: projection.columns.iter().map(info).collect(),
                    rows,
                    pages,
                    total: page.total,
                    page: input.page,
                    page_size,
                    totals,
                }
            }
            Kind::Rows | Kind::Summary | Kind::Matrix => {
                let groupings: Vec<_> = definition
                    .groupings()
                    .into_iter()
                    .filter(|g| keep(&g.column).is_some())
                    .collect();
                let measures: Vec<_> = definition
                    .measures()
                    .into_iter()
                    .filter(|m| keep(&m.column).is_some())
                    .collect();
                let max_groups = self
                    .setting(&**client, scope, contributions::MAX_GROUPS_KEY)
                    .await?;
                drop(client);
                let summary = bilibi_grid::summarize(
                    &self.db,
                    scope,
                    &report.record_type,
                    &SummarySpec {
                        filters,
                        groupings,
                        measures,
                        timezone: timezone.clone(),
                        max_groups,
                    },
                )
                .await
                .map_err(grid)?;
                RunResult::Summary {
                    groupings: summary
                        .groupings
                        .iter()
                        .map(|(c, b)| (info(c), b.map(|b| b.key())))
                        .collect(),
                    measures: summary
                        .measures
                        .iter()
                        .map(|(c, a)| (info(c), a.key()))
                        .collect(),
                    lines: summary
                        .lines
                        .into_iter()
                        .map(|l| (l.keys, l.subtotal, l.rows, l.values))
                        .collect(),
                    total: (summary.total.rows, summary.total.values),
                    chart: definition.chart.clone(),
                }
            }
        };
        Ok(Run {
            report,
            left_out,
            timezone,
            result,
        })
    }

    /// Saves a new report owned by the actor, inside the caller's audited
    /// transaction. Needs `reports.author`; the draft is checked first.
    pub async fn create(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        draft: &Draft,
    ) -> Outcome<i64> {
        bilibi_authz::authorize_key(tx.sql(), scope, AUTHOR)
            .await
            .map_err(denied)?;
        self.check(scope, draft).await?;
        let (tenant, owner) = (scope.tenant(), scope.user());
        let draft = draft.clone();
        Ok(tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let write = store::Write {
                        name: draft.name.trim(),
                        description: &draft.description,
                        record_type: &draft.record_type,
                        kind: draft.kind,
                        definition: &draft.definition,
                    };
                    let id = store::insert(sql, tenant, owner, &write).await?;
                    Ok((id, vec![change(id, Value::Null, after(&draft))]))
                })
            })
            .await?)
    }

    /// Saves the actor's own copy of a report they can run, checked against
    /// their own catalog. Needs `reports.author`.
    pub async fn copy(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        name: Option<&str>,
    ) -> Outcome<i64> {
        let original = match store::read(tx.sql(), scope.tenant(), scope.user(), id, false).await {
            Err(Error::NotFound) => return Err(ReportError::NotFound),
            other => other?,
        };
        self.create(tx, scope, &copy_of(&original, name)).await
    }

    /// Replaces an own report's definition at `version`.
    pub async fn update(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
        draft: &Draft,
    ) -> Outcome<()> {
        let before = self.owned(tx, scope, id, version, AUTHOR).await?;
        self.check(scope, draft).await?;
        let (tenant, draft) = (scope.tenant(), draft.clone());
        let updated = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let write = store::Write {
                        name: draft.name.trim(),
                        description: &draft.description,
                        record_type: &draft.record_type,
                        kind: draft.kind,
                        definition: &draft.definition,
                    };
                    if !store::update(sql, tenant, id, version, &write).await? {
                        return Err(Error::Conflict);
                    }
                    Ok(((), vec![change(id, snapshot(&before), after(&draft))]))
                })
            })
            .await;
        updated.map_err(conflict)
    }

    /// Shares an own report's definition with everyone, chosen roles, or
    /// nobody, at `version`. Needs `reports.share`.
    pub async fn share(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
        everyone: bool,
        roles: &[i64],
    ) -> Outcome<()> {
        let before = self.owned(tx, scope, id, version, SHARE).await?;
        if roles.len() > MAX_SHARE_ROLES {
            return Err(ReportError::Invalid(field_errors(
                "roles",
                "names at most 50 roles",
            )));
        }
        let mut roles = roles.to_vec();
        roles.sort_unstable();
        roles.dedup();
        let known = store::known_roles(tx.sql(), scope.tenant(), &roles).await?;
        if known != roles {
            return Err(ReportError::Invalid(field_errors(
                "roles",
                "names a role that does not exist",
            )));
        }
        let tenant = scope.tenant();
        let shared = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    if !store::share(sql, tenant, id, version, everyone, &roles).await? {
                        return Err(Error::Conflict);
                    }
                    let old = json!({"shared_with_everyone": before.shared_with_everyone, "roles": before.role_ids});
                    Ok(((), vec![change(id, old, json!({"shared_with_everyone": everyone, "roles": roles}))]))
                })
            })
            .await;
        shared.map_err(conflict)
    }

    /// Deletes an own report at `version`; its shares go with it.
    pub async fn delete(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
    ) -> Outcome<()> {
        let before = self.owned(tx, scope, id, version, AUTHOR).await?;
        let tenant = scope.tenant();
        let deleted = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    if !store::delete(sql, tenant, id, version).await? {
                        return Err(Error::Conflict);
                    }
                    Ok(((), vec![change(id, snapshot(&before), Value::Null)]))
                })
            })
            .await;
        deleted.map_err(conflict)
    }

    /// The actor's own report at `version`, locked for a write, after the
    /// capability. A stale version is refused here, before the write, as the
    /// mutation boundary asks.
    async fn owned(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
        capability: &str,
    ) -> Outcome<Report> {
        bilibi_authz::authorize_key(tx.sql(), scope, capability)
            .await
            .map_err(denied)?;
        let report = match store::read(tx.sql(), scope.tenant(), scope.user(), id, true).await {
            Err(Error::NotFound) => return Err(ReportError::NotFound),
            other => other?,
        };
        if !report.owned {
            return Err(ReportError::NotOwner);
        }
        if report.version != version {
            return Err(ReportError::Conflict);
        }
        Ok(report)
    }
}

/// Where a bound is read: the reader's company, which falls back to the
/// installation's value. An account linked to no company reads the tenant's
/// chain, as Tiling's shared layouts do.
fn settings_target(scope: &Scope) -> Target {
    match scope.actor().organisation() {
        Some(organisation) => Target::Company(organisation),
        None => Target::Tenant,
    }
}

async fn authorize(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    key: &str,
) -> Outcome<()> {
    bilibi_authz::authorize_key(client, scope, key)
        .await
        .map_err(denied)
}
fn denied(error: Error) -> ReportError {
    match error {
        Error::Forbidden => ReportError::Forbidden,
        other => ReportError::Database(other),
    }
}
fn conflict(error: Error) -> ReportError {
    match error {
        Error::Conflict => ReportError::Conflict,
        other => ReportError::Database(other),
    }
}
fn change(id: i64, old: Value, new: Value) -> Change {
    Change {
        kind: KIND.into(),
        id,
        old,
        new,
    }
}
fn after(draft: &Draft) -> Value {
    json!({"name": draft.name.trim(), "description": draft.description, "record_type": draft.record_type, "kind": draft.kind.key(), "definition": draft.definition})
}
/// A built-in as the report a reader runs: never owned, never shared.
fn as_report(builtin: &'static Builtin) -> Option<Report> {
    Some(Report {
        id: 0,
        owner_id: 0,
        owned: false,
        name: builtin.name.to_owned(),
        description: builtin.description.to_owned(),
        record_type: builtin.record_type.to_owned(),
        kind: builtin.kind,
        definition: Definition::from_input(&(builtin.definition)()).ok()?,
        shared_with_everyone: false,
        role_ids: Vec::new(),
        version: 1,
        updated_at: String::new(),
        builtin: Some(builtin.key),
    })
}

/// A copy's draft: the given name, or "Copy of" the original's, kept to the
/// name bound.
fn copy_of(report: &Report, name: Option<&str>) -> Draft {
    let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_owned(),
        None => format!("Copy of {}", report.name)
            .chars()
            .take(120)
            .collect(),
    };
    Draft {
        name,
        description: report.description.clone(),
        record_type: report.record_type.clone(),
        kind: report.kind,
        definition: report.definition.clone(),
    }
}
fn snapshot(report: &Report) -> Value {
    json!({"name": report.name, "description": report.description, "record_type": report.record_type, "kind": report.kind.key(), "definition": report.definition, "version": report.version})
}
fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
/// One CSV record: every field quoted when it holds a comma, quote or line
/// break, and a leading `=`, `+`, `-` or `@` neutralised so a spreadsheet
/// never reads a value as a formula.
fn line(out: &mut String, fields: impl IntoIterator<Item = String>) {
    let fields: Vec<String> = fields
        .into_iter()
        .map(|field| {
            let field = if field.starts_with(['=', '+', '@'])
                || (field.starts_with('-') && field.parse::<f64>().is_err())
            {
                format!("'{field}")
            } else {
                field
            };
            if field.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                field
            }
        })
        .collect();
    out.push_str(&fields.join(","));
    out.push_str("\r\n");
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/reports", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/audit", "base/settings", "base/datetime", "base/grid", "base/agent_api", "base/ui", "base/menu", "base/locale", "base/dashboard", "base/artifacts", "base/queue", "base/notifications", "base/party_ids", "base/schedule", "base/mail"],
        migrations: &MIGRATIONS,
        services: &[bilibi_module_registry::Service { id: SERVICE, factory: |services| {
            let reports = Arc::new(Reports {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                settings: services.get("base/settings.store")?,
                display: services.get("base/datetime.display")?,
                queue: services.get("base/queue.store")?,
                artifacts: services.get("base/artifacts.store")?,
                notifications: services.get("base/notifications.sink")?,
                authentication: services.get("base/tenancy.authentication")?,
                mutations: services.get("base/database.mutations")?,
                mailer: services.get("base/mail.mailer")?,
                origin: std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()).trim_end_matches('/').to_owned(),
                builtins: builtin::validate(bilibi_module_registry::Catalog::discover(&bilibi_module_registry::discover()?)?.entries::<Builtin>(BUILTINS)?)?,
            });
            agent_operations::bind(reports.clone())?;
            Ok(reports)
        }}, bilibi_module_registry::Service { id: web::SERVICE, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                reports: services.get(SERVICE)?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
            }))))
        }}, bilibi_module_registry::Service { id: PREPARER, factory: |services| {
            Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(exports::Preparer(services.get(SERVICE)?)))))
        }}, bilibi_module_registry::Service { id: SWEEPER, factory: |services| {
            Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(schedules::Sweeper { reports: services.get(SERVICE)?, schedule: services.get("base/schedule.store")? }))))
        }}, bilibi_module_registry::Service { id: DELIVERER, factory: |services| {
            Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(schedules::Deliverer(services.get(SERVICE)?)))))
        }}, bilibi_module_registry::Service { id: EXPORTED, factory: |services| {
            Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(web::download::Prepared {
                reports: services.get(SERVICE)?,
            }))))
        }}, bilibi_module_registry::Service { id: DOWNLOAD, factory: |services| {
            Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(web::download::Csv {
                reports: services.get(SERVICE)?,
            }))))
        }}],
    }
}

inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "base/reports.index", bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_module_registry::contributions::Route {
    method: bilibi_module_registry::contributions::Method::Get, path: web::INDEX, service: web::SERVICE, operation: "index",
    access: bilibi_module_registry::contributions::Access::Capability(VIEW), framed: true,
}) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "base/reports.show", bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_module_registry::contributions::Route {
    method: bilibi_module_registry::contributions::Method::Get, path: web::SHOW, service: web::SERVICE, operation: "show",
    access: bilibi_module_registry::contributions::Access::Capability(VIEW), framed: true,
}) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::DOWNLOADS, "base/reports.export", bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_module_registry::contributions::Download {
    path: web::EXPORT_PATH, service: DOWNLOAD, operation: "csv",
    access: bilibi_module_registry::contributions::Access::Capability(EXPORT),
}) }
// Every page write is an operation contract (ADR 01).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, web::SERVICE, bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: web::SERVICE,
    contracts: &[("create", "base.reports.create"), ("save_definition", "base.reports.update"), ("copy", "base.reports.copy"), ("save_sharing", "base.reports.share"), ("delete", "base.reports.delete"), ("prepare_export", "base.reports.prepare_export"), ("subscribe", "base.reports.subscribe"), ("pause_schedule", "base.reports.pause_subscription"), ("unsubscribe", "base.reports.unsubscribe")],
    pending: bilibi_module_registry::contributions::Pending::None,
}) }
bilibi_menu::contribute!(
    "base/reports",
    bilibi_menu::MenuItem::page("reports", "Reports", web::INDEX)
        .icon("chart-bar")
        .requires(bilibi_menu::Requirement::Capability(VIEW))
        .order(800)
);

// A report the reader may run can be placed on their dashboard.
bilibi_dashboard::provide!(
    "base/reports",
    bilibi_dashboard::Provider {
        prefix: web::panel::PREFIX,
        embed: "base/reports.dashboard",
        placement: bilibi_dashboard::Placement::Grid,
        capability: Some(VIEW),
        refresh_interval: 60_000,
        entries: web::panel::entries,
    }
);
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::EMBEDS, "base/reports.dashboard", bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_module_registry::contributions::Embed {
    slot: "dashboard.reports", context: bilibi_module_registry::contributions::EmbedContext::Dashboard,
    service: web::SERVICE, operation: "panel", access: bilibi_module_registry::contributions::Access::Capability(VIEW),
}) }

inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_queue::WORKERS, exports::WORKER, bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_queue::Worker { max_attempts: 5, validate: exports::validate }) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_queue::execution::HANDLERS, exports::WORKER, bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_queue::execution::HandlerBinding { service: PREPARER, capability: EXPORT }) }
// A prepared export downloads from its own path, for its requester only.
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::DOWNLOADS, "base/reports.exported", bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_module_registry::contributions::Download {
    path: "/report-exports/{id}", service: EXPORTED, operation: "file",
    access: bilibi_module_registry::contributions::Access::Capability(EXPORT),
}) }

// Scheduled deliveries: Schedule's recurrence sweeps due subscriptions as the
// `scheduler` principal; each delivery then runs as its recipient. Like every
// contributed recurrence it runs nothing until an operator enables it.
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_queue::WORKERS, schedules::SWEEP, bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_queue::Worker { max_attempts: 5, validate: schedules::validate_sweep }) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_queue::execution::HANDLERS, schedules::SWEEP, bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_queue::execution::HandlerBinding { service: SWEEPER, capability: bilibi_schedule::EXECUTE }) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_schedule::DEFINITIONS, "base/reports.deliveries", bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_schedule::Definition {
    name: "Deliver scheduled reports", expression: "*/15 * * * *", timezone: "Etc/UTC",
    task_name: "Base Reports scheduled deliveries", worker: schedules::SWEEP, args: || json!({}),
    overlap: bilibi_schedule::Overlap::Forbid, misfire: bilibi_schedule::Misfire::Coalesce, owner_route: Some(web::INDEX),
}) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_queue::WORKERS, schedules::DELIVER, bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_queue::Worker { max_attempts: 5, validate: schedules::validate_delivery }) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_queue::execution::HANDLERS, schedules::DELIVER, bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_queue::execution::HandlerBinding { service: DELIVERER, capability: EXPORT }) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "base/reports.builtin", bilibi_module_registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_module_registry::contributions::Route {
    method: bilibi_module_registry::contributions::Method::Get, path: web::BUILTIN, service: web::SERVICE, operation: "builtin",
    access: bilibi_module_registry::contributions::Access::Capability(VIEW), framed: true,
}) }
