//! Private-file owner adapters for companies (K15): the Media attachment
//! subject `core.company` and the Artifacts owner `core.company`. Base asks
//! `FILES` on the caller's sealed connection before every operation; the
//! adapter answers from Company's own rules and never trusts an id.
//!
//! One rule serves both, the same the company pages follow: a read needs
//! `admin.company.view` now and a company live in the actor's own tenant
//! (the detail page's rule); a write needs `admin.company.update` (purge,
//! `.delete`), the actor's company-administration reach (current company, or any with
//! `admin.company.tenant-wide.manage`) and an unarchived company. Missing and
//! cross-tenant companies are `NotFound`; a denied or archived one is
//! `Forbidden`. A company's documents are filed under the company itself: an
//! artifact reference naming another subject is refused.
use crate::{
    contributions::{DELETE, UPDATE, VIEW},
    organisation::Directory,
};
use bilibi_artifacts::{Operation, Owner, Reference};
use bilibi_authz::authorize_proof;
use bilibi_database::{Error, Result};
use bilibi_media::{Subject, SubjectAction, SubjectOwner, SUBJECTS};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{OrganisationDirectory, ProofClient, ProofFuture, Scope};

/// The Media subject kind whose ids are company ids.
pub const ATTACHMENT_SUBJECT: &str = "core.company";
/// The stable Artifacts owner id; Base also binds the adapter type.
pub const ARTIFACT_OWNER: &str = "core.company";

pub struct CompanyFiles;
/// The one adapter instance Base and owner code share.
pub static FILES: CompanyFiles = CompanyFiles;

pub(crate) fn company_id(subject: &str) -> Result<OrganisationId> {
    subject
        .parse::<i64>()
        .ok()
        .and_then(|id| OrganisationId::new(id).ok())
        .ok_or(Error::NotFound)
}

pub(crate) async fn authorize(
    client: &dyn ProofClient,
    scope: &Scope,
    id: OrganisationId,
    capability: &str,
    write: bool,
) -> Result<()> {
    if !write {
        authorize_proof(client, scope, capability).await?;
        return Directory
            .lookup(client, scope, id)
            .await?
            .map(|_| ())
            .ok_or(Error::NotFound);
    }
    let company = Directory.target(client, scope, id, capability).await?;
    if company.archived {
        return Err(Error::Forbidden);
    }
    Ok(())
}

impl SubjectOwner for CompanyFiles {
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        action: SubjectAction,
        subject_id: &'a str,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let id = company_id(subject_id)?;
            match action {
                SubjectAction::Download => authorize(client, scope, id, VIEW, false).await,
                SubjectAction::Upload
                | SubjectAction::Submit
                | SubjectAction::Retain
                | SubjectAction::Discard => authorize(client, scope, id, UPDATE, true).await,
            }
        })
    }
}

impl Owner for CompanyFiles {
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
            if let Some(reference) = reference {
                if company_id(&reference.subject)? != company {
                    return Err(Error::NotFound);
                }
            }
            match operation {
                Operation::Read => authorize(client, scope, company, VIEW, false).await,
                Operation::Create | Operation::Delete | Operation::Retain => {
                    authorize(client, scope, company, UPDATE, true).await
                }
                // Purge is company maintenance: the delete authority, on a
                // company that may still be written.
                Operation::Purge => authorize(client, scope, company, DELETE, true).await,
            }
        })
    }
}

inventory::submit! {
    Contribution::new(SUBJECTS, ATTACHMENT_SUBJECT,
        Provenance { module: "core/company", source: file!(), line: line!() },
        &Subject { owner: &FILES })
}
