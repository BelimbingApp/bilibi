//! A run's files (issue 133): the uploaded CSV is a private artifact owned by
//! the run (Artifacts owner `base.bulk_data`), and the file of failed rows is
//! written from it on every download, through a `Download` contribution that
//! reads the artifact each time. An export's file is a private artifact owned
//! by the export (kind `export`, subject `export:{id}`), served the same way.
//! Only the person who started the run or export reaches its files, in the
//! company it was filed under. Both kinds expire after `bulk_data.file_days`;
//! each upload and each export first purges the company's expired ones
//! (Artifacts' bounded batch, as Reports' prepared exports do), and an hourly
//! scheduled sweep purges the rest in every tenant, so expired bytes go even
//! from a company where nobody uploads or exports again.
use crate::{BulkData, Pass, CREATE, EXPORTED, VIEW};
use bilibi_artifacts::{Operation, Owner, Reference};
use bilibi_authz::authorize_proof;
use bilibi_database::Error;
use bilibi_party_ids::OrganisationId;
use bilibi_personal_data::Erasure;
use bilibi_tenancy::{PartyProofs, ProofClient, ProofFuture, Scope};
use bilibi_ui::download::{Download, DownloadFuture, DownloadRequest, FileBody};
use std::sync::{Arc, OnceLock};

/// The stable Artifacts owner id; Base also binds the adapter type.
pub const ARTIFACT_OWNER: &str = "base.bulk_data";
/// The artifact kind of an uploaded file.
pub const SOURCE: &str = "import-source";

pub struct RunFiles;
/// The one adapter instance Base and the facade share.
pub static FILES: RunFiles = RunFiles;
static PARTIES: OnceLock<Arc<PartyProofs>> = OnceLock::new();

pub(crate) fn bind(parties: Arc<PartyProofs>) -> bilibi_module_registry::Result<()> {
    PARTIES.set(parties).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate import files binding".into())
    })
}

impl Owner for RunFiles {
    fn owner_id(&self) -> &'static str {
        ARTIFACT_OWNER
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
            let parties = PARTIES.get().ok_or(Error::Forbidden)?;
            match operation {
                Operation::Create | Operation::Delete => {
                    parties.organisation(client, scope, company).await?;
                }
                _ => {
                    parties
                        .organisation_name(client, scope, company)
                        .await?
                        .ok_or(Error::NotFound)?;
                }
            }
            let Some(reference) = reference else {
                // Purge of expired or deleted run files: the person who may
                // use the Imports pages in this tenant, as an export's own
                // person may hold no import capability.
                return authorize_proof(client, scope, VIEW).await;
            };
            let (table, id, capability) = match reference.kind.as_str() {
                SOURCE => (
                    "base_bulk_data_runs",
                    reference.subject.parse::<i64>(),
                    if operation == Operation::Read {
                        VIEW
                    } else {
                        CREATE
                    },
                ),
                // An export's file is made by its job as the person, who
                // reads the list: the Imports pages are its only capability
                // here, and the type's own export capability is checked by
                // the export before the file is made.
                EXPORTED => (
                    "base_bulk_data_exports",
                    reference
                        .subject
                        .strip_prefix("export:")
                        .ok_or(Error::NotFound)?
                        .parse::<i64>(),
                    VIEW,
                ),
                _ => return Err(Error::NotFound),
            };
            authorize_proof(client, scope, capability).await?;
            let id = id.map_err(|_| Error::NotFound)?;
            client
                .query_opt(
                    &format!("SELECT 1 FROM {table} WHERE tenant_id=$1 AND id=$2 AND started_by=$3 AND company_id=$4{}",
                        if reference.kind == EXPORTED && operation == Operation::Read { " AND cardinality(erased_for)=0" } else { "" }),
                    &[&scope.tenant(), &id, &scope.user(), &company.get()],
                )
                .await?
                .ok_or(Error::NotFound)?;
            Ok(())
        })
    }
    fn erasure_subject<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        grant: &'a Erasure,
        subject: &'a str,
    ) -> ProofFuture<'a, bool> {
        Box::pin(async move {
            let Some(id) = subject
                .strip_prefix("export:")
                .and_then(|id| id.parse::<i64>().ok())
            else {
                return Ok(false);
            };
            let Some(person) = grant.person() else {
                return Ok(false);
            };
            Ok(client.query_opt(
                "SELECT 1 FROM base_bulk_data_exports WHERE tenant_id=$1 AND id=$2 AND $3=ANY(erased_for)",
                &[&grant.tenant(), &id, &person.get()],
            ).await?.is_some())
        })
    }
}

/// `/imports/{id}/failed-rows`: the rows of a pass that did not go in, with
/// the file's own columns and an Error column, ready to fix and upload again.
/// `pass=preview` gives the dry run's.
pub(crate) struct FailedRows(pub Arc<BulkData>);

impl Download<Scope, Error> for FailedRows {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let id = request
                .path
                .get("id")
                .and_then(|id| id.parse::<i64>().ok())
                .ok_or(Error::NotFound)?;
            let pass = match request.query.get("pass").map(String::as_str) {
                Some("preview") => Pass::Preview,
                None | Some("run") => Pass::Run,
                Some(_) => return Err(Error::NotFound),
            };
            let (name, bytes) = self.0.failed_rows(request.scope, id, pass).await?;
            Ok(FileBody::attachment(bytes, "text/csv", &name))
        })
    }
}

/// `/exports/{id}/file`: a finished export's CSV, read through Artifacts on
/// every download, so each one is audited.
pub(crate) struct ExportFile(pub Arc<BulkData>);

impl Download<Scope, Error> for ExportFile {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let id = request
                .path
                .get("id")
                .and_then(|id| id.parse::<i64>().ok())
                .ok_or(Error::NotFound)?;
            let (name, bytes) = self.0.export_file(request.scope, id).await?;
            Ok(FileBody::attachment(bytes, "text/csv", &name))
        })
    }
}

impl BulkData {
    /// Purges the company's expired run files: Artifacts' bounded batch, so
    /// a backlog drains over the next uploads and exports. A file that
    /// cannot be purged is logged by its content-free code, and Artifacts
    /// records the attempt and holds it for an operator after its limit.
    pub(crate) async fn purge(
        &self,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        company: OrganisationId,
    ) {
        match self
            .artifacts
            .for_owner(scope, company, &FILES)
            .purge_expired(client)
            .await
        {
            Ok(report) => {
                for (_, error) in &report.failed {
                    eprintln!(
                        "warning: an expired bulk-data file could not be purged: {}",
                        error.code()
                    );
                }
            }
            Err(error) => eprintln!(
                "warning: expired bulk-data files could not be purged: {}",
                error.code()
            ),
        }
    }
}

pub(crate) const SWEEP: &str = "base/bulk_data.file-sweep";
const SWEEP_WORKER: &str = "base/bulk_data-file-sweep";

fn no_arguments(args: serde_json::Value) -> bilibi_database::Result<serde_json::Value> {
    match args.as_object() {
        Some(object) if object.is_empty() => Ok(args),
        _ => Err(Error::Invalid),
    }
}
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_queue::WORKERS, SWEEP_WORKER, crate::provenance!(), &bilibi_queue::Worker { max_attempts: 3, validate: |args| bilibi_schedule::worker::validate(args, no_arguments) }) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_queue::HANDLERS, SWEEP_WORKER, crate::provenance!(), &bilibi_queue::HandlerBinding { service: SWEEP, capability: bilibi_schedule::EXECUTE }) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_schedule::DEFINITIONS, "base/bulk_data-file-sweep", crate::provenance!(), &bilibi_schedule::Definition {
    name: "Purge expired import and export files", expression: "41 * * * *", timezone: "Etc/UTC",
    task_name: "Base Bulk Data file sweep", worker: SWEEP_WORKER, args: || serde_json::json!({}),
    overlap: bilibi_schedule::Overlap::Forbid, misfire: bilibi_schedule::Misfire::Coalesce, reach: bilibi_schedule::Reach::Installation, owner_route: None,
}) }

/// The hourly sweep: one bounded batch of expired run files in every tenant,
/// purged by Artifacts as its sweeper. A file that cannot be purged is logged
/// by its content-free code; Artifacts records the attempt and holds it for
/// an operator after its limit, as for any purge.
pub(crate) struct Sweep {
    pub schedule: Arc<bilibi_schedule::Schedule>,
    pub artifacts: Arc<bilibi_artifacts::Artifacts>,
    pub db: bilibi_database::Db,
}
impl bilibi_queue::Handler for Sweep {
    fn perform<'a>(
        &'a self,
        execution: &'a bilibi_queue::Execution,
        args: serde_json::Value,
    ) -> bilibi_queue::HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            let swept = async {
                let mut object = self
                    .db
                    .get()
                    .await
                    .map_err(|_| bilibi_artifacts::Error::Invalid)?;
                let client: &mut tokio_postgres::Client = &mut object;
                self.artifacts.sweep_expired(client, &FILES).await
            }
            .await;
            match swept {
                Ok(report) => {
                    for (_, error) in &report.failed {
                        eprintln!(
                            "warning: an expired bulk-data file could not be purged: {}",
                            error.code()
                        );
                    }
                    bilibi_queue::Outcome::Completed
                }
                Err(error) => {
                    eprintln!(
                        "warning: expired bulk-data files could not be swept: {}",
                        error.code()
                    );
                    bilibi_queue::Outcome::Retry("files_unavailable")
                }
            }
        }))
    }
}
