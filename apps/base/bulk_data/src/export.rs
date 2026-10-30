//! CSV export (issue 133): a list's filtered records, or the records ticked
//! on it, with the columns the list shows, as a CSV file. No Bilimbi or
//! Belimbing counterpart.
//!
//! A page (or an agent) starts an export with the list's filters (or the
//! ticked ids) and the shown columns. A queued job, as the person who started
//! it (ADR 10), pages through the record type's list contract and projects
//! each page through Grid, so a field restricted for the person comes out
//! blank and another tenant's or company's record never appears; ticked ids
//! go straight to Grid, which reads only rows the person may. The file is a
//! private artifact owned by the export, downloaded through `Artifacts::read`
//! every time, and only while the person still holds the grants and sees the
//! columns as they did when it was made (as Reports' prepared exports); it
//! expires after `bulk_data.file_days`, and each new export first purges the
//! company's expired run files.
//!
//! The list is read until two full reads in a row agree, so a record the
//! list moved, added or dropped while it was paged is never skipped; a list
//! that keeps changing fails in words. The file is written a row at a time
//! and stops at the private-document size limit, so an export holds at most
//! that much of its file in memory. More rows than
//! `bulk_data.export_max_rows`, or a read longer than [`BUDGET`], also fail
//! the export in words; nothing is cut off.
use crate::store::instant;
use crate::{
    csv, files::FILES, record::Registered, refused, BulkData, WriteError, EXPORT_MAX_ROWS,
    FILE_DAYS, VIEW,
};
use bilibi_database::{Change, Error, Result};
use bilibi_notifications::Notice;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Outcome as JobOutcome};
use bilibi_tenancy::Scope;
use jiff::Timestamp;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::{GenericClient, Row};

pub(crate) const SERVICE: &str = "base/bulk_data.export-job";
pub(crate) const WORKER: &str = "base/bulk_data-export";
/// The artifact kind of an exported file.
pub const EXPORTED: &str = "export";
/// Records asked of the list contract per page.
const PAGE: i64 = 100;
/// The most columns one export takes.
const MAX_COLUMNS: usize = 64;
/// How long one attempt may read and project before the export fails in
/// words, well inside Queue's 15-minute attempt timeout, so a list too big
/// to read never retries from the start until its attempts run out.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(600);
/// Full reads of the list before one must agree with the read before it.
const READS: usize = 3;
const COLUMNS: &str = "id,record_type,company_id,filter,ids,columns,state,row_count,error,artifact,access,job_id,version,floor(extract(epoch FROM created_at))::bigint AS created,floor(extract(epoch FROM finished_at))::bigint AS finished,floor(extract(epoch FROM expires_at))::bigint AS expires";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportState {
    /// The job is reading the list.
    Running,
    /// The file is ready to download.
    Completed,
    /// It ended without a file; `error` says why.
    Failed,
}
impl ExportState {
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => return Err(Error::Internal),
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// One export, as its person reads it.
#[derive(Clone, Debug, Serialize)]
pub struct ExportRun {
    pub id: i64,
    pub record_type: String,
    #[serde(skip)]
    pub company: i64,
    /// The list contract's filters, without `page` or `page_size`, while it
    /// runs; cleared (`{}`) once it ends, as a search may be personal data.
    pub filter: Value,
    /// How many ticked records it takes, when it takes ticked records
    /// rather than the list's filters.
    pub selected: Option<usize>,
    #[serde(skip)]
    ids: Option<Vec<String>>,
    /// Grid column specs, in the order the list showed them.
    pub columns: Vec<String>,
    pub state: ExportState,
    pub row_count: Option<i32>,
    pub error: Option<String>,
    #[serde(skip)]
    pub artifact: Option<String>,
    /// What the person could see of the columns when the file was made.
    #[serde(skip)]
    access: Option<String>,
    /// The file may be downloaded now: it is ready, it has not expired, and
    /// the person still holds the grants and sees the columns as when it was
    /// made.
    pub downloadable: bool,
    /// When the file expires (Artifacts purges it after), and whether it has.
    #[serde(serialize_with = "maybe_stamp")]
    pub expires_at: Option<Timestamp>,
    pub expired: bool,
    #[serde(skip)]
    pub job_id: Option<i64>,
    /// Its job ended without finishing it (its person lost access, or every
    /// attempt failed): start the export again.
    pub stopped: bool,
    pub version: i64,
    #[serde(serialize_with = "stamp")]
    pub created_at: Timestamp,
    #[serde(serialize_with = "maybe_stamp")]
    pub finished_at: Option<Timestamp>,
}
fn stamp<S: serde::Serializer>(
    at: &Timestamp,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(at)
}
fn maybe_stamp<S: serde::Serializer>(
    at: &Option<Timestamp>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match at {
        Some(at) => serializer.collect_str(at),
        None => serializer.serialize_none(),
    }
}
impl ExportRun {
    fn from_row(row: &Row) -> Result<Self> {
        let columns: Value = row.get("columns");
        let ids: Option<Vec<String>> = row
            .get::<_, Option<Value>>("ids")
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::Internal)?;
        Ok(Self {
            selected: ids.as_ref().map(Vec::len),
            ids,
            id: row.get("id"),
            record_type: row.get("record_type"),
            company: row.get("company_id"),
            filter: row.get("filter"),
            columns: serde_json::from_value(columns).map_err(|_| Error::Internal)?,
            state: ExportState::parse(row.get("state"))?,
            row_count: row.get("row_count"),
            error: row.get("error"),
            artifact: row.get("artifact"),
            access: row.get("access"),
            downloadable: false,
            expires_at: row.get::<_, Option<i64>>("expires").map(instant),
            expired: false,
            job_id: row.get("job_id"),
            stopped: false,
            version: row.get("version"),
            created_at: instant(row.get("created")),
            finished_at: row.get::<_, Option<i64>>("finished").map(instant),
        })
    }
    /// Content-free audit evidence: never the filter's values.
    fn evidence(&self) -> Value {
        json!({"record_type": self.record_type, "columns": self.columns.len(), "selected": self.selected, "state": self.state.as_str(), "rows": self.row_count, "version": self.version})
    }
}

/// The person's own export.
pub(crate) async fn load(
    client: &impl GenericClient,
    scope: &Scope,
    id: i64,
    lock: bool,
) -> Result<ExportRun> {
    let sql = format!(
        "SELECT {COLUMNS} FROM base_bulk_data_exports WHERE tenant_id=$1 AND id=$2 AND started_by=$3{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    client
        .query_opt(&sql, &[&scope.tenant(), &id, &scope.user()])
        .await?
        .map(|row| ExportRun::from_row(&row))
        .transpose()?
        .ok_or(Error::NotFound)
}

/// What a person may see, read once for any number of exports.
struct Seeing {
    held: std::collections::BTreeSet<&'static str>,
    catalog: bilibi_grid::Catalog,
    /// Each table's restricted fields, sorted and joined.
    restricted: std::collections::BTreeMap<&'static str, String>,
}

/// Why an export whose job ended first did not finish.
const STRANDED: &str =
    "The export stopped before it finished: its job ended first, for instance when your access changed. Export the list again.";

/// A made file as the export records it.
struct Made {
    rows: i32,
    artifact: String,
    /// What the person could see when it was made.
    access: String,
    /// When Artifacts lets the file expire, as it said when storing it.
    expires_at: Option<String>,
}

/// A refusal in words: the export ends without a file.
pub(crate) struct Failed(pub String);

/// The records a list contract answers for a filter, each id with its
/// version, in the list's order.
pub(crate) type Listed = Vec<(Value, Option<i64>)>;

impl BulkData {
    /// The record type's export, or why it cannot be exported.
    fn exportable(
        &self,
        key: &str,
    ) -> std::result::Result<(&Registered, &'static crate::Export), WriteError> {
        let registered = self.registered(key)?;
        let export = registered
            .definition
            .export
            .as_ref()
            .ok_or_else(|| refused("record_type", "cannot be exported"))?;
        Ok((registered, export))
    }

    /// Whether the person may export this record type: its export
    /// capability and the Imports pages. Pages ask before showing Export.
    pub async fn may_export(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        key: &str,
    ) -> Result<bool> {
        let Ok((_, export)) = self.exportable(key) else {
            return Ok(false);
        };
        let held = bilibi_authz::effective_capabilities(client, scope).await?;
        Ok(held.contains(VIEW) && held.contains(export.capability))
    }

    /// Checks an export request before it starts: the capabilities, a
    /// company, the filters against the list contract's schema (or the
    /// ticked ids) and the columns against what Grid lets this person see.
    pub(crate) async fn ready_export(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        record_type: &str,
        filter: &Value,
        ids: &[String],
        columns: &[String],
    ) -> std::result::Result<(), WriteError> {
        let (registered, export) = self.exportable(record_type)?;
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        bilibi_authz::authorize_key(client, scope, export.capability).await?;
        crate::current_company(scope, "export")?;
        if ids.is_empty() {
            self.ready_filter(export.list, filter)?;
        } else {
            if filter.as_object().is_none_or(|object| !object.is_empty()) {
                return Err(refused(
                    "filter",
                    "give the list's filters or ticked ids, not both",
                ));
            }
            let max = self
                .settings
                .get_global(client, EXPORT_MAX_ROWS)
                .await?
                .as_i64()
                .ok_or(Error::Internal)?;
            crate::distinct_ids("ids", ids, max)?;
        }
        if columns.is_empty() || columns.len() > MAX_COLUMNS {
            return Err(refused(
                "columns",
                &format!("name between 1 and {MAX_COLUMNS} columns"),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        if columns
            .iter()
            .any(|spec| spec.is_empty() || spec.len() > 200 || !seen.insert(spec.as_str()))
        {
            return Err(refused("columns", "name each column once"));
        }
        let catalog = bilibi_grid::Catalog::for_scope(client, scope)
            .await
            .map_err(|_| refused("columns", "cannot be read by you"))?;
        if catalog.table(registered.definition.table).is_none() {
            return Err(refused("record_type", "is not a list you may read"));
        }
        if let Err(spec) = shown(&catalog, registered.definition.table, columns) {
            return Err(refused(
                "columns",
                &format!("names {spec}, which is not a column of this list"),
            ));
        }
        Ok(())
    }

    /// Opens an export and queues its job, on the caller's transaction.
    pub(crate) async fn start_export(
        &self,
        tx: &mut bilibi_database::MutationTransaction<'_>,
        scope: &Scope,
        record_type: &str,
        filter: &Value,
        ids: &[String],
        columns: &[String],
    ) -> std::result::Result<ExportRun, WriteError> {
        self.ready_export(tx.sql(), scope, record_type, filter, ids, columns)
            .await?;
        let (registered, _) = self.exportable(record_type)?;
        let company = crate::current_company(scope, "export")?.get();
        self.finish_stranded(tx, scope, company).await?;
        let (tenant, user, kind) = (scope.tenant(), scope.user(), registered.key);
        let (filter, columns) = (filter.clone(), json!(columns));
        let ids = (!ids.is_empty()).then(|| json!(ids));
        let trace = tx.context().trace_id.clone();
        let opened = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(&format!("INSERT INTO base_bulk_data_exports(tenant_id,company_id,record_type,started_by,filter,ids,columns,trace_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {COLUMNS}"), &[&tenant, &company, &kind, &user, &filter, &ids, &columns, &trace]).await?;
                    let run = ExportRun::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.export".into(), id: run.id, old: Value::Null, new: run.evidence() }]))
                })
            })
            .await?;
        let job = self
            .queue
            .enqueue_for(tx, scope, WORKER, json!({"export": opened.id}))
            .await?;
        let id = opened.id;
        let old = opened.evidence();
        let run = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(&format!("UPDATE base_bulk_data_exports SET job_id=$3,version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id, &job.id()]).await?;
                    let run = ExportRun::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.export".into(), id, old, new: run.evidence() }]))
                })
            })
            .await?;
        Ok(run)
    }

    /// Checks a list's filters as its list contract takes them: an object,
    /// without the `page` and `page_size` a run sets itself, valid against
    /// the contract's input schema. Exports and bulk updates both ask.
    pub(crate) fn ready_filter(
        &self,
        list: &str,
        filter: &Value,
    ) -> std::result::Result<(), WriteError> {
        let Some(object) = filter.as_object() else {
            return Err(refused("filter", "must be an object of the list's filters"));
        };
        if object.contains_key("page") || object.contains_key("page_size") {
            return Err(refused(
                "filter",
                "names page or page_size, which the run sets itself",
            ));
        }
        let list = self
            .jobs
            .readable(list)
            .map_err(|_| WriteError::Backend(Error::Internal))?;
        let mut paged = filter.clone();
        paged["page"] = json!(1);
        paged["page_size"] = json!(PAGE);
        bilibi_agent_api::schema::validate(&list.input, &paged).map_err(|errors| {
            WriteError::Invalid(
                errors
                    .into_iter()
                    .map(|(path, messages)| (format!("filter.{path}"), messages))
                    .collect(),
            )
        })
    }

    /// Ends as failed the company's exports whose job Queue ended before it
    /// could finish them (its person lost access before it ran, its lease
    /// ran out on the last attempt): their filters are dropped, as a search
    /// may quote a person, and each starter is told. Queue calls no owner
    /// code for such a job, so this runs when the company's next export
    /// starts, in its audited transaction; a bounded batch.
    pub(crate) async fn finish_stranded(
        &self,
        tx: &mut bilibi_database::MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
    ) -> Result<()> {
        let tenant = scope.tenant();
        let running = tx
            .sql()
            .query(
                "SELECT id, job_id FROM base_bulk_data_exports WHERE tenant_id=$1 AND company_id=$2 AND state='running' AND job_id IS NOT NULL ORDER BY id LIMIT 100",
                &[&tenant, &company],
            )
            .await?;
        if running.is_empty() {
            return Ok(());
        }
        let jobs: Vec<i64> = running.iter().map(|row| row.get(1)).collect();
        let ended: std::collections::HashSet<i64> =
            bilibi_queue::store::states(tx.sql(), tenant, &jobs)
                .await?
                .into_iter()
                .filter(|status| status.state.terminal())
                .map(|status| status.id)
                .collect();
        for row in running {
            let (id, job): (i64, i64) = (row.get(0), row.get(1));
            if !ended.contains(&job) {
                continue;
            }
            let ended = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let Some(old) = sql.query_opt(&format!("SELECT {COLUMNS} FROM base_bulk_data_exports WHERE tenant_id=$1 AND id=$2 AND state='running' FOR UPDATE"), &[&tenant, &id]).await? else {
                            return Ok((None, Vec::new()));
                        };
                        let old = ExportRun::from_row(&old)?;
                        let row = sql.query_one(&format!("UPDATE base_bulk_data_exports SET state='failed',error=$3,filter='{{}}'::jsonb,finished_at=clock_timestamp(),version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}, started_by"), &[&tenant, &id, &STRANDED]).await?;
                        let starter: i64 = row.get("started_by");
                        let run = ExportRun::from_row(&row)?;
                        Ok((Some((starter, run.record_type.clone())), vec![Change { kind: "base/bulk_data.export".into(), id, old: old.evidence(), new: run.evidence() }]))
                    })
                })
                .await?;
            if let Some((starter, record_type)) = ended {
                let notice = Notice::new(format!(
                    "Export of {} did not finish",
                    self.label(&record_type)
                ))
                .body(STRANDED)
                .kind("bulk_data.export")
                .url(format!("/exports/{id}"));
                self.notifications
                    .notify(tx, scope, &[starter], &notice)
                    .await?;
            }
        }
        Ok(())
    }

    /// Ends the export as failed on its job's last attempt, so it does not
    /// stay running with its filters once Queue gives up on it.
    pub(crate) async fn give_up_export(&self, execution: &Execution, id: i64) -> Result<()> {
        let scope = execution.scope().ok_or(Error::Forbidden)?.clone();
        let mut object = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut object;
        self.finish_export(
            execution,
            client,
            &scope,
            id,
            Err(Failed(
                "The export could not be finished: every attempt failed. Export the list again."
                    .into(),
            )),
        )
        .await
        .map(|_| ())
    }

    /// The person's own export, marked as [`BulkData::mark`] says.
    pub async fn export(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        id: i64,
    ) -> Result<ExportRun> {
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        let mut runs = [load(client, scope, id, false).await?];
        self.mark(client, scope, &mut runs).await?;
        let [run] = runs;
        Ok(run)
    }

    /// Marks exports as their person sees them now: `stopped` when Queue
    /// ended the job of one still running, `expired` past its file's expiry,
    /// and `downloadable` only for a completed, unexpired file whose access
    /// fingerprint still matches. What the person may see is read once for
    /// every export given.
    async fn mark(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        runs: &mut [ExportRun],
    ) -> Result<()> {
        let jobs: Vec<i64> = runs
            .iter()
            .filter(|run| run.state == ExportState::Running)
            .filter_map(|run| run.job_id)
            .collect();
        if !jobs.is_empty() {
            let ended: std::collections::HashSet<i64> =
                bilibi_queue::store::states(client, scope.tenant(), &jobs)
                    .await?
                    .into_iter()
                    .filter(|status| status.state.terminal())
                    .map(|status| status.id)
                    .collect();
            for run in runs.iter_mut() {
                run.stopped = run.state == ExportState::Running
                    && run.job_id.is_some_and(|job| ended.contains(&job));
            }
        }
        let now = Timestamp::now();
        for run in runs.iter_mut() {
            run.expired = run.expires_at.is_some_and(|at| at <= now);
        }
        let ready = |run: &ExportRun| {
            run.state == ExportState::Completed && !run.expired && run.access.is_some()
        };
        if runs.iter().any(ready) {
            let seeing = self.seeing(client, scope, runs).await?;
            for run in runs.iter_mut() {
                if ready(run) {
                    run.downloadable = self.access(&seeing, run) == run.access;
                }
            }
        }
        Ok(())
    }

    /// What the person may see, read once for any number of exports: their
    /// capabilities, Grid's catalog for them and the restricted fields of
    /// each export's table.
    async fn seeing(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        runs: &[ExportRun],
    ) -> Result<Seeing> {
        let held = bilibi_authz::effective_capabilities(client, scope).await?;
        let catalog = bilibi_grid::Catalog::for_scope(client, scope)
            .await
            .map_err(|_| Error::Internal)?;
        let mut restricted = std::collections::BTreeMap::new();
        for run in runs {
            let Ok((registered, _)) = self.exportable(&run.record_type) else {
                continue;
            };
            let table = registered.definition.table;
            if !restricted.contains_key(table) {
                let mut fields: Vec<String> = bilibi_authz::restrictions_for(client, scope, table)
                    .await?
                    .into_iter()
                    .collect();
                fields.sort();
                restricted.insert(table, fields.join(","));
            }
        }
        Ok(Seeing {
            held,
            catalog,
            restricted,
        })
    }

    /// What the person may see of an export's columns, as text: `None` when
    /// they no longer hold the Imports pages, the type's export capability
    /// or its list contract's capability; else which columns Grid lets them
    /// read and which of the type's fields are restricted for them. Taken
    /// when the file is made and compared on every read of the export, so a
    /// revoked grant or a newly restricted field refuses a file made before
    /// it (Reports' prepared exports do the same).
    fn access(&self, seeing: &Seeing, run: &ExportRun) -> Option<String> {
        let (registered, export) = self.exportable(&run.record_type).ok()?;
        let list = self.jobs.readable(export.list).ok()?.definition.capability;
        let held = &seeing.held;
        if !(held.contains(VIEW) && held.contains(export.capability) && held.contains(list)) {
            return None;
        }
        let table = registered.definition.table;
        let shown = shown(&seeing.catalog, table, &run.columns).ok()?;
        let seen: String = shown
            .iter()
            .map(|(_, visible)| if *visible { '1' } else { '0' })
            .collect();
        Some(format!(
            "{seen}:{}",
            seeing.restricted.get(table).map_or("", String::as_str)
        ))
    }

    /// The file of a finished export, read through Artifacts (audited), as
    /// a file name and its bytes.
    pub async fn export_file(&self, scope: &Scope, id: i64) -> Result<(String, Vec<u8>)> {
        let mut object = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut object;
        let run = self.export(&*client, scope, id).await?;
        let artifact = run.artifact.clone().ok_or(Error::NotFound)?;
        if run.expired {
            // Past its expiry the file is gone, or about to be purged.
            return Err(Error::NotFound);
        }
        if !run.downloadable {
            // What the person may see has changed since the file was made.
            return Err(Error::Forbidden);
        }
        let company = OrganisationId::new(run.company).map_err(|_| Error::Internal)?;
        let document = self
            .artifacts
            .for_owner(scope, company, &FILES)
            .read(client, &artifact)
            .await
            .map_err(crate::artifact)?;
        let stem: String = self
            .label(&run.record_type)
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        Ok((format!("{stem}-export-{id}.csv"), document.bytes))
    }

    /// The job's work: read every page, project it, file the CSV.
    pub(crate) async fn work_export(&self, execution: &Execution, id: i64) -> Result<()> {
        let scope = execution.scope().ok_or(Error::Forbidden)?.clone();
        let mut object = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut object;
        let run = load(&*client, &scope, id, false).await?;
        if run.state != ExportState::Running {
            return Ok(());
        }
        let company = OrganisationId::new(run.company).map_err(|_| Error::Internal)?;
        self.purge(client, &scope, company).await;
        match self.build(execution, client, &scope, &run).await? {
            Ok((rows, bytes, access)) => {
                let days = self
                    .settings
                    .get_global(&*client, FILE_DAYS)
                    .await?
                    .as_i64()
                    .ok_or(Error::Internal)?;
                let stored = self
                    .artifacts
                    .for_owner(&scope, company, &FILES)
                    .put_expiring(
                        client,
                        bilibi_artifacts::Reference {
                            subject: format!("export:{id}"),
                            kind: EXPORTED.into(),
                        },
                        &bytes,
                        "text/csv",
                        days,
                    )
                    .await;
                match stored {
                    Ok(metadata) => {
                        let stored = metadata.id.clone();
                        let finished = self
                            .finish_export(
                                execution,
                                client,
                                &scope,
                                id,
                                Ok(Made {
                                    rows,
                                    artifact: metadata.id,
                                    access,
                                    expires_at: metadata.expires_at,
                                }),
                            )
                            .await;
                        if !matches!(finished, Ok(true)) {
                            // Not recorded (the export ended meanwhile, or the
                            // attempt failed and will make another file):
                            // nothing names this file, so it goes now rather
                            // than when it expires.
                            let _ = self
                                .artifacts
                                .for_owner(&scope, company, &FILES)
                                .delete(client, &stored)
                                .await;
                        }
                        finished.map(|_| ())
                    }
                    Err(bilibi_artifacts::Error::Invalid) => {
                        self.finish_export(execution, client, &scope, id, Err(Failed(
                            "The file is larger than private documents may be. Narrow the list or show fewer columns.".into(),
                        )))
                        .await
                        .map(|_| ())
                    }
                    Err(bilibi_artifacts::Error::Database(error)) => Err(error),
                    Err(error) => {
                        // A content-free code: the folder is missing or unsafe,
                        // or the disk refused the write.
                        eprintln!(
                            "error: an export could not be stored as a private document: {}",
                            error.code()
                        );
                        self.finish_export(execution, client, &scope, id, Err(Failed(
                            "The file could not be stored: private documents are not available on this server. Ask your administrator.".into(),
                        )))
                        .await
                        .map(|_| ())
                    }
                }
            }
            Err(failed) => self
                .finish_export(execution, client, &scope, id, Err(failed))
                .await
                .map(|_| ()),
        }
    }

    /// Reads the list and projects it into the file, as the job's person:
    /// the rows, the file and what the person could see when it was made, or
    /// `Ok(Err(..))` when the export cannot be made, in words.
    async fn build(
        &self,
        execution: &Execution,
        client: &tokio_postgres::Client,
        scope: &Scope,
        run: &ExportRun,
    ) -> Result<std::result::Result<(i32, Vec<u8>, String), Failed>> {
        let Ok((registered, export)) = self.exportable(&run.record_type) else {
            return Ok(Err(Failed(
                "This kind of record can no longer be exported.".into(),
            )));
        };
        // Taken before the read: a grant revoked or a field restricted while
        // it runs refuses the file at download either way.
        let seeing = self
            .seeing(client, scope, std::slice::from_ref(run))
            .await?;
        let Some(access) = self.access(&seeing, run) else {
            return Ok(Err(Failed(
                "You no longer have permission to export these records.".into(),
            )));
        };
        let max = self
            .settings
            .get_global(client, EXPORT_MAX_ROWS)
            .await?
            .as_i64()
            .ok_or(Error::Internal)?;
        let largest = self
            .settings
            .get_global(client, "artifacts.max_bytes")
            .await?
            .as_u64()
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or(Error::Internal)?;
        let started = std::time::Instant::now();
        let too_long = || {
            Failed("The list took too long to read in one go. Narrow the filters or show fewer columns, and export again.".into())
        };
        // Ticked records keep the order they were given in, and Grid reads
        // only those the person may.
        let ids: Vec<Value> = match &run.ids {
            Some(ticked) => ticked.iter().map(|id| crate::typed(id)).collect(),
            None => match self
                .read_list(
                    execution,
                    client,
                    registered,
                    export.list,
                    &run.filter,
                    max,
                    started,
                )
                .await?
            {
                Ok(listed) => listed.into_iter().map(|(id, _)| id).collect(),
                Err(failed) => return Ok(Err(failed)),
            },
        };
        let table = registered.definition.table;
        let Ok(shown) = shown(&seeing.catalog, table, &run.columns) else {
            return Ok(Err(Failed("A column of the list no longer exists.".into())));
        };
        // Only the columns the person may see are read; the others stay blank.
        let visible: Vec<&String> = run
            .columns
            .iter()
            .zip(&shown)
            .filter(|(_, (_, visible))| *visible)
            .map(|(spec, _)| spec)
            .collect();
        let too_large = || {
            Failed("The file is larger than private documents may be. Narrow the list or show fewer columns.".into())
        };
        // Written a row at a time and stopped at the size limit: no export
        // holds more of its file than a private document may be.
        // With no column the person may read, Grid is never asked which of
        // the records exist for them, and the file would hold nothing.
        if visible.is_empty() {
            return Ok(Err(Failed(
                "None of the shown columns can be read by you, so the file would hold nothing. Show a column you may see and export again.".into(),
            )));
        }
        let mut file = csv::Writer::new(shown.iter().map(|(label, _)| label.as_str()));
        let mut count: i32 = 0;
        {
            for batch in ids.chunks(bilibi_grid::query::MAX_ROWS) {
                if started.elapsed() > BUDGET {
                    return Ok(Err(too_long()));
                }
                let projection =
                    match bilibi_grid::project(&self.db, scope, table, batch, &visible).await {
                        Ok(projection) => projection,
                        Err(bilibi_grid::QueryError::Database(error))
                            if !matches!(error, Error::Forbidden | Error::Unauthorized) =>
                        {
                            return Err(error)
                        }
                        Err(_) => {
                            return Ok(Err(Failed(
                                "The list could not be read with these columns.".into(),
                            )))
                        }
                    };
                let ids_of: std::collections::BTreeMap<&str, &str> = projection
                    .columns
                    .iter()
                    .map(|column| (column.spec.as_str(), column.id.as_str()))
                    .collect();
                for row in &projection.rows {
                    let cells: Vec<String> = run
                        .columns
                        .iter()
                        .zip(&shown)
                        .map(|(spec, (_, visible))| match ids_of.get(spec.as_str()) {
                            Some(id) if *visible => text(row.cells.get(*id)),
                            _ => String::new(),
                        })
                        .collect();
                    file.row(cells.iter().map(String::as_str));
                    count += 1;
                }
                if file.len() > largest {
                    return Ok(Err(too_large()));
                }
            }
        }
        if file.len() > largest {
            return Ok(Err(too_large()));
        }
        Ok(Ok((count, file.finish(), access)))
    }

    /// Every record a list contract answers for a filter, each id with its
    /// version, in the list's order. Offset pages are not a snapshot: a
    /// record moved, added or dropped while they are read can shift another
    /// past a page boundary unseen. So the list is read in full until two
    /// reads in a row agree, at most [`READS`] times; a list that keeps
    /// changing is refused in words rather than read with a gap.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn read_list(
        &self,
        execution: &Execution,
        client: &tokio_postgres::Client,
        registered: &Registered,
        list: &str,
        filter: &Value,
        max: i64,
        started: std::time::Instant,
    ) -> Result<std::result::Result<Listed, Failed>> {
        let id_property = registered.definition.import.id;
        let mut previous: Option<Listed> = None;
        for _ in 0..READS {
            let mut read: Listed = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for page in 1_i64.. {
                if started.elapsed() > BUDGET {
                    return Ok(Err(Failed(
                        "The list took too long to read in one go. Narrow the filters and try again.".into(),
                    )));
                }
                let mut input = filter.clone();
                input["page"] = json!(page);
                input["page_size"] = json!(PAGE);
                let listed = match self.jobs.read(client, execution, list, &input).await {
                    Ok(listed) => listed,
                    Err(bilibi_agent_api::Error::Backend(error))
                        if !matches!(
                            error,
                            Error::Forbidden | Error::Unauthorized | Error::Invalid
                        ) =>
                    {
                        return Err(error)
                    }
                    Err(_) => {
                        return Ok(Err(Failed(
                            "The list could not be read with these filters.".into(),
                        )))
                    }
                };
                let records = listed["result"].as_array().ok_or(Error::Internal)?;
                for record in records {
                    let id = record.get(id_property).ok_or(Error::Internal)?;
                    if seen.insert(id.to_string()) {
                        read.push((id.clone(), record["version"].as_i64()));
                    }
                }
                if read.len() as i64 > max {
                    return Ok(Err(Failed(format!(
                        "The list has more than {max} records, the most one run takes. Narrow the filters and try again."
                    ))));
                }
                if (records.len() as i64) < PAGE {
                    break;
                }
            }
            if previous.as_ref() == Some(&read) {
                return Ok(Ok(read));
            }
            previous = Some(read);
        }
        Ok(Err(Failed(
            "The list kept changing while it was read, so it could not be read without gaps. Try again when it is quieter.".into(),
        )))
    }

    /// Records how the export ended and tells its person; false when it
    /// had already ended, so nothing was recorded.
    async fn finish_export(
        &self,
        execution: &Execution,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        id: i64,
        result: std::result::Result<Made, Failed>,
    ) -> Result<bool> {
        let mut tx = execution
            .begin(client, "base.bulk_data.export.finish")
            .await?;
        let old = load(tx.sql(), scope, id, true).await?;
        if old.state != ExportState::Running {
            return Ok(false);
        }
        let tenant = scope.tenant();
        let label = self.label(&old.record_type).to_owned();
        let (state, rows, artifact, access, expires, error) = match result {
            Ok(made) => (
                "completed",
                Some(made.rows),
                Some(made.artifact),
                Some(made.access),
                made.expires_at,
                None,
            ),
            Err(Failed(why)) => ("failed", None, None, None, None, Some(why)),
        };
        let run = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    // The filters were only needed to read the list; a search
                    // can quote a person's name or email, so they are not kept.
                    let row = sql.query_one(&format!("UPDATE base_bulk_data_exports SET state=$3,row_count=$4,artifact=$5,error=$6,access=$7,expires_at=$8::text::timestamptz,filter='{{}}'::jsonb,finished_at=clock_timestamp(),version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id, &state, &rows, &artifact, &error, &access, &expires]).await?;
                    let run = ExportRun::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.export".into(), id, old: old.evidence(), new: run.evidence() }]))
                })
            })
            .await?;
        let notice = match run.state {
            ExportState::Completed => {
                Notice::new(format!("Export of {label} is ready")).body(format!(
                    "{} rows. The file is kept for a limited time.",
                    run.row_count.unwrap_or(0)
                ))
            }
            _ => Notice::new(format!("Export of {label} did not finish"))
                .body(run.error.clone().unwrap_or_default()),
        }
        .kind("bulk_data.export")
        .url(format!("/exports/{id}"));
        self.notifications
            .notify(&mut tx, scope, &[scope.user()], &notice)
            .await?;
        tx.commit().await?;
        self.wake(tenant, scope.user());
        Ok(true)
    }

    /// The person's newest exports, marked as their pages show them.
    pub async fn exports(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        limit: i64,
    ) -> Result<Vec<ExportRun>> {
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        let mut runs = client
            .query(
                &format!("SELECT {COLUMNS} FROM base_bulk_data_exports WHERE tenant_id=$1 AND started_by=$2 ORDER BY id DESC LIMIT $3"),
                &[&scope.tenant(), &scope.user(), &limit.clamp(1, 100)],
            )
            .await?
            .iter()
            .map(ExportRun::from_row)
            .collect::<Result<Vec<_>>>()?;
        self.mark(client, scope, &mut runs).await?;
        Ok(runs)
    }
}

/// Each column's label, from the installed catalog, and whether the person
/// may see it (a masked column is seen, partly). `Err` names a column the
/// list does not have.
fn shown(
    catalog: &bilibi_grid::Catalog,
    table: &str,
    specs: &[String],
) -> std::result::Result<Vec<(String, bool)>, String> {
    let installed = bilibi_grid::installed().map_err(|_| String::new())?;
    let all = installed.table(table).ok_or_else(String::new)?;
    let seen = catalog.table(table);
    specs
        .iter()
        .map(|spec| {
            let column = bilibi_grid::column::resolve(spec, all, &|id| installed.table(id))
                .map_err(|_| spec.clone())?;
            let visible = seen.is_some_and(|root| catalog.resolve(root, spec).is_ok());
            Ok((column.label, visible))
        })
        .collect()
}

/// A projected cell as CSV text: blank when restricted or empty.
fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(object)) if object.contains_key("restricted") => String::new(),
        Some(other) => other.to_string(),
    }
}

pub(crate) struct ExportJob(pub Arc<BulkData>);

impl Handler for ExportJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(id) = args["export"].as_i64() else {
                return JobOutcome::Cancel("worker_args_refused");
            };
            let outcome = match self.0.work_export(execution, id).await {
                Ok(()) => JobOutcome::Completed,
                Err(Error::Unauthorized | Error::Forbidden) => JobOutcome::Cancel("actor_refused"),
                Err(Error::NotFound) => JobOutcome::Cancel("export_missing"),
                Err(Error::Conflict) => JobOutcome::Retry("lease_lost"),
                Err(_) => JobOutcome::Retry("export_unavailable"),
            };
            // Queue retries no more after the last attempt: end the export
            // in words now rather than leave it running with its filters.
            if matches!(outcome, JobOutcome::Retry(_))
                && execution.attempt >= execution.max_attempts
            {
                let _ = self.0.give_up_export(execution, id).await;
            }
            outcome
        })
    }
}

/// Queue's argument check: an export id, nothing else.
pub(crate) fn arguments(args: Value) -> Result<Value> {
    let object = args.as_object().ok_or(Error::Invalid)?;
    if object.len() == 1
        && object
            .get("export")
            .and_then(Value::as_i64)
            .is_some_and(|id| id > 0)
    {
        Ok(args)
    } else {
        Err(Error::Invalid)
    }
}
