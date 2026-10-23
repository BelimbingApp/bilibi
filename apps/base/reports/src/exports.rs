//! Prepared exports: a result too large to download at once runs on the
//! Queue as the person who asked (`base/reports.export`, re-proved by Queue
//! on every attempt), is stored as a private artifact of their company that
//! expires after `reports.export_days`, and the bell says when it is ready.
//! Only the requester reads it. Each export is audited like a download.
//! No Bilimbi counterpart.
use crate::{authorize, contributions, export::Format, store, ReportError, Reports, EXPORT};
use bilibi_artifacts::{Metadata, Operation, Owner, Reference};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_notifications::Notice;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Outcome as JobOutcome};
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

/// The queue worker that prepares one export.
pub const WORKER: &str = "base/reports-export";
const KIND: &str = "base_report_export";
/// Exports one person's list shows at most.
const LISTED: i64 = 20;

/// One prepared export as its requester sees it.
#[derive(Clone, Debug, Serialize)]
pub struct Prepared {
    pub id: i64,
    pub report_id: i64,
    pub report_name: String,
    pub format: &'static str,
    /// `queued`, `ready`, `failed` or `expired`.
    pub state: &'static str,
    pub filename: Option<String>,
    pub rows: Option<i32>,
    pub message: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    /// Where a ready export downloads, for its requester only.
    pub path: Option<String>,
}

pub fn path(id: i64) -> String {
    format!("/report-exports/{id}")
}

fn prepared(row: &tokio_postgres::Row) -> Prepared {
    let id: i64 = row.get(0);
    let state: String = row.get(4);
    let expired: bool = row.get(10);
    let state = match (state.as_str(), expired) {
        ("ready", true) => "expired",
        ("ready", false) => "ready",
        ("failed", _) => "failed",
        _ => "queued",
    };
    let format: String = row.get(3);
    Prepared {
        id,
        report_id: row.get(1),
        report_name: row.get(2),
        format: if format == "pdf" { "pdf" } else { "csv" },
        state,
        filename: row.get(5),
        rows: row.get(6),
        message: row.get(7),
        created_at: row.get(8),
        expires_at: row.get(9),
        path: (state == "ready").then(|| path(id)),
    }
}
const COLUMNS: &str = "id, report_id, report_name, format, state, filename, rows, message, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), COALESCE(expires_at <= now(), false)";

/// The artifact owner: an export's bytes belong to its requester, in the
/// company the export was asked in.
pub(crate) struct ExportOwner;
fn subject(id: i64) -> String {
    format!("report-export-{id}")
}
impl Owner for ExportOwner {
    fn owner_id(&self) -> &'static str {
        "base/reports.exports"
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        operation: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            if scope.actor().organisation() != Some(company) {
                return Err(Error::Forbidden);
            }
            let state = match operation {
                Operation::Create => "queued",
                Operation::Read | Operation::Delete => "ready",
                // Expired exports of the company are removed by whoever asks
                // for a new one: they are past use to everyone.
                Operation::Purge => return Ok(()),
                Operation::Retain => return Err(Error::Forbidden),
            };
            let id = reference
                .and_then(|r| r.subject.strip_prefix("report-export-"))
                .and_then(|id| id.parse::<i64>().ok())
                .ok_or(Error::Forbidden)?;
            let found = client
                .query_opt(
                    "SELECT 1 FROM base_report_exports WHERE tenant_id=$1 AND id=$2 AND requester_id=$3 AND company_id=$4 AND state=$5",
                    &[&scope.tenant(), &id, &scope.user(), &company.get(), &state],
                )
                .await?;
            found.map(|_| ()).ok_or(Error::Forbidden)
        })
    }
}

impl Reports {
    /// Asks for a prepared export of a report the reader may run, inside the
    /// caller's audited transaction: the request row and its queue job
    /// commit together. Needs `reports.export` and a company to keep it in.
    pub async fn prepare_export(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        answers: &BTreeMap<usize, Value>,
        format: Format,
    ) -> Result<i64, ReportError> {
        authorize(tx.sql(), scope, EXPORT).await?;
        let company = scope.actor().organisation().ok_or_else(|| {
            ReportError::Refused("Prepared exports are kept with your company, and your account is not linked to one.".into())
        })?;
        let report = match store::read(tx.sql(), scope.tenant(), scope.user(), id, false).await {
            Err(Error::NotFound) => return Err(ReportError::NotFound),
            other => other?,
        };
        let answers: BTreeMap<String, Value> = answers
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let answers = json!(answers);
        let (tenant, user, name) = (scope.tenant(), scope.user(), report.name.clone());
        let format_key = format.key();
        let export = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let export: i64 = sql
                        .query_one(
                            "INSERT INTO base_report_exports(tenant_id,requester_id,company_id,report_id,report_name,format,answers) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id",
                            &[&tenant, &user, &company.get(), &id, &name, &format_key, &answers],
                        )
                        .await?
                        .get(0);
                    let new = json!({"report_id": id, "format": format_key, "state": "queued"});
                    Ok((export, vec![Change { kind: KIND.into(), id: export, old: Value::Null, new }]))
                })
            })
            .await?;
        self.queue
            .enqueue_for(tx, scope, WORKER, json!({"export": export}))
            .await?;
        Ok(export)
    }

    /// The reader's own recent exports, newest first; of one report when
    /// `report` is given. Needs `reports.export`.
    pub async fn exports(
        &self,
        scope: &Scope,
        report: Option<i64>,
    ) -> Result<Vec<Prepared>, ReportError> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, EXPORT).await?;
        let rows = client
            .query(
                &format!("SELECT {COLUMNS} FROM base_report_exports WHERE tenant_id=$1 AND requester_id=$2 AND ($3::bigint IS NULL OR report_id=$3) ORDER BY created_at DESC, id DESC LIMIT {LISTED}"),
                &[&scope.tenant(), &scope.user(), &report],
            )
            .await
            .map_err(Error::from)?;
        Ok(rows.iter().map(prepared).collect())
    }

    /// A ready export's file, for its requester only, until it expires.
    pub async fn exported(&self, scope: &Scope, export: i64) -> Result<crate::File, ReportError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, EXPORT).await?;
        let row = client
            .query_opt(
                "SELECT artifact_id, filename, format, rows, company_id FROM base_report_exports WHERE tenant_id=$1 AND id=$2 AND requester_id=$3 AND state='ready'",
                &[&scope.tenant(), &export, &scope.user()],
            )
            .await
            .map_err(Error::from)?
            .ok_or(ReportError::NotFound)?;
        let (artifact, filename, format, rows): (String, String, String, Option<i32>) =
            (row.get(0), row.get(1), row.get(2), row.get(3));
        let company = OrganisationId::new(row.get(4)).map_err(|_| Error::Internal)?;
        let document = self
            .artifacts
            .for_owner(scope, company, &ExportOwner)
            .read(&mut client, &artifact)
            .await
            .map_err(|_| ReportError::NotFound)?;
        Ok(crate::File {
            filename,
            content_type: Format::from_key(&format)
                .unwrap_or(Format::Csv)
                .content_type(),
            bytes: document.bytes,
            rows: rows.unwrap_or_default().max(0) as usize,
        })
    }

    /// One attempt at a prepared export, as its requester. A refusal the
    /// reader can act on (too many rows, a hidden filter) finishes the
    /// request as failed with that sentence; a transient fault retries.
    /// `quiet` (a scheduled delivery) neither needs `reports.export` nor
    /// rings the bell for a refusal: the delivery records why instead.
    pub(crate) async fn prepare(
        &self,
        execution: &Execution,
        export: i64,
        quiet: bool,
    ) -> JobOutcome {
        let Some(scope) = execution.scope() else {
            return JobOutcome::Cancel("not_a_user_job");
        };
        let Ok(mut client) = self.db.get().await else {
            return JobOutcome::Retry("database_unavailable");
        };
        let row = client
            .query_opt(
                "SELECT report_id, format, answers, company_id, state FROM base_report_exports WHERE tenant_id=$1 AND id=$2 AND requester_id=$3",
                &[&scope.tenant(), &export, &scope.user()],
            )
            .await;
        let row = match row {
            Ok(Some(row)) => row,
            Ok(None) => return JobOutcome::Cancel("export_missing"),
            Err(_) => return JobOutcome::Retry("database_unavailable"),
        };
        // Already finished by an earlier attempt: nothing to do twice.
        if row.get::<_, String>(4) != "queued" {
            return JobOutcome::Completed;
        }
        let (report, format, answers): (i64, String, Value) = (row.get(0), row.get(1), row.get(2));
        let Ok(company) = OrganisationId::new(row.get(3)) else {
            return JobOutcome::Cancel("export_invalid");
        };
        let format = Format::from_key(&format).unwrap_or(Format::Csv);
        let answers: BTreeMap<usize, Value> = answers
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| Some((k.parse().ok()?, v.clone())))
                    .collect()
            })
            .unwrap_or_default();
        let owned = self.artifacts.for_owner(scope, company, &ExportOwner);
        // Expired exports of the company go first; a failure here is not
        // this export's concern.
        let _ = owned.purge_expired(&mut client).await;
        let cap = match self.queued_rows(scope).await {
            Ok(cap) => cap,
            Err(_) => return JobOutcome::Retry("settings_unavailable"),
        };
        let file = match self
            .export_within(scope, report, &answers, format, cap, !quiet)
            .await
        {
            Ok(file) => file,
            Err(ReportError::Database(_)) => return JobOutcome::Retry("report_unavailable"),
            Err(refused) => {
                let message = refused.to_string();
                return match self
                    .finish(execution, scope, export, Finished::Failed(&message), quiet)
                    .await
                {
                    Ok(()) => JobOutcome::Completed,
                    Err(_) => JobOutcome::Retry("finish_failed"),
                };
            }
        };
        let days = {
            let Ok(client) = self.db.get().await else {
                return JobOutcome::Retry("database_unavailable");
            };
            match self
                .setting(&**client, scope, contributions::EXPORT_DAYS_KEY)
                .await
            {
                Ok(days) => days as i64,
                Err(_) => return JobOutcome::Retry("settings_unavailable"),
            }
        };
        let reference = Reference {
            subject: subject(export),
            kind: "report-export".into(),
        };
        let stored = owned
            .put_expiring(&mut client, reference, &file.bytes, file.content_type, days)
            .await;
        let metadata = match stored {
            Ok(metadata) => metadata,
            Err(_) => return JobOutcome::Retry("store_failed"),
        };
        match self
            .finish(
                execution,
                scope,
                export,
                Finished::Ready(&metadata, &file),
                false,
            )
            .await
        {
            Ok(()) => JobOutcome::Completed,
            Err(_) => JobOutcome::Retry("finish_failed"),
        }
    }

    /// Records the outcome and rings the requester's bell, in one audited
    /// transaction fenced by the job's lease.
    async fn finish(
        &self,
        execution: &Execution,
        scope: &Scope,
        export: i64,
        outcome: Finished<'_>,
        quiet: bool,
    ) -> Result<(), Error> {
        let mut client = self.db.get().await?;
        let mut tx = execution
            .begin(&mut client, "reports.export.finish")
            .await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let (state, artifact, filename, rows, message, expires) = match &outcome {
            Finished::Ready(metadata, file) => (
                "ready",
                Some(metadata.id.clone()),
                Some(file.filename.clone()),
                Some(file.rows as i32),
                None,
                metadata.expires_at.clone(),
            ),
            Finished::Failed(message) => (
                "failed",
                None,
                None,
                None,
                Some(message.chars().take(500).collect::<String>()),
                None,
            ),
        };
        let new = json!({"state": state, "rows": rows});
        let name: String = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_opt(
                            "UPDATE base_report_exports SET state=$4, artifact_id=$5, filename=$6, rows=$7, message=$8, expires_at=$9::text::timestamptz, finished_at=now() WHERE tenant_id=$1 AND id=$2 AND requester_id=$3 AND state='queued' RETURNING report_name",
                            &[&tenant, &export, &user, &state, &artifact, &filename, &rows, &message, &expires],
                        )
                        .await?
                        .ok_or(Error::Conflict)?;
                    Ok((row.get(0), vec![Change { kind: KIND.into(), id: export, old: json!({"state": "queued"}), new }]))
                })
            })
            .await?;
        if quiet && matches!(outcome, Finished::Failed(_)) {
            return tx.commit().await;
        }
        let notice = match outcome {
            Finished::Ready(..) => Notice::new("Your export is ready")
                .body(format!("{name}: download it before it expires."))
                .url(path(export))
                .icon("export"),
            Finished::Failed(message) => Notice::new("Your export could not be prepared")
                .body(format!("{name}: {message}"))
                .url("/reports")
                .icon("error"),
        };
        self.notifications
            .notify(&mut tx, scope, &[user], &notice)
            .await?;
        tx.commit().await
    }
}

enum Finished<'a> {
    Ready(&'a Metadata, &'a crate::File),
    Failed(&'a str),
}

/// The queue handler bound to [`WORKER`].
pub(crate) struct Preparer(pub Arc<Reports>);
impl Handler for Preparer {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            match args["export"].as_i64() {
                Some(export) if export > 0 => self.0.prepare(execution, export, false).await,
                _ => JobOutcome::Cancel("invalid_arguments"),
            }
        })
    }
}

/// The worker's arguments: exactly `{"export": <id>}`.
pub(crate) fn validate(args: Value) -> bilibi_database::Result<Value> {
    match args.as_object() {
        Some(object)
            if object.len() == 1
                && object
                    .get("export")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| id > 0) =>
        {
            Ok(args)
        }
        _ => Err(Error::Invalid),
    }
}
