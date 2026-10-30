//! A run's files (issue 133): the uploaded CSV is a private artifact owned by
//! the run (Artifacts owner `base.bulk_data`), and the file of failed rows is
//! written from it on every download, through a `Download` contribution that
//! reads the artifact each time. An export's file is a private artifact owned
//! by the export (kind `export`, subject `export:{id}`), served the same way.
//! Only the person who started the run or export reaches its files, in the
//! company it was filed under.
use crate::{BulkData, Pass, CREATE, EXPORTED, VIEW};
use bilibi_artifacts::{Operation, Owner, Reference};
use bilibi_authz::authorize_proof;
use bilibi_database::Error;
use bilibi_party_ids::OrganisationId;
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
                // Purge: the person who may start imports in this tenant.
                return authorize_proof(client, scope, CREATE).await;
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
                    &format!("SELECT 1 FROM {table} WHERE tenant_id=$1 AND id=$2 AND started_by=$3 AND company_id=$4"),
                    &[&scope.tenant(), &id, &scope.user(), &company.get()],
                )
                .await?
                .ok_or(Error::NotFound)?;
            Ok(())
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
