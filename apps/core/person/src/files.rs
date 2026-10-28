//! Private-file owner adapters for persons (K15): the Media attachment
//! subject `core.person` (Bilimbi `AttachmentSubjectAuthorizer`) and the
//! Artifacts owner `core.person` (Bilimbi `Artifacts.Owner`). Base asks
//! `FILES` on the caller's sealed connection before every operation.
//!
//! A person's files follow Person's own rules: the operation capability now
//! (`admin.person.view` to download or read, `.update` to upload, submit,
//! retain, discard, create or delete, `.retire` to purge) and a person of the
//! actor's own tenant; a write needs an unretired person. The company a
//! document is filed under is reproved through Tenancy: a live company for
//! a read, an unarchived one for a write. Missing and cross-tenant ids are
//! `NotFound`; a denied, retired or archived target is `Forbidden`.
use crate::contributions::{RETIRE, UPDATE, VIEW};
use bilibi_artifacts::{Operation, Owner, Reference};
use bilibi_authz::authorize_proof;
use bilibi_database::{Error, Result};
use bilibi_media::{Subject, SubjectAction, SubjectOwner, SUBJECTS};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_tenancy::{PartyProofs, ProofClient, ProofFuture, Scope};
use std::sync::{Arc, OnceLock};

/// The Media subject kind whose ids are person ids.
pub const ATTACHMENT_SUBJECT: &str = "core.person";
/// The stable Artifacts owner id; Base also binds the adapter type.
pub const ARTIFACT_OWNER: &str = "core.person";

pub struct PersonFiles;
/// The one adapter instance Base and owner code share.
pub static FILES: PersonFiles = PersonFiles;
static PARTIES: OnceLock<Arc<PartyProofs>> = OnceLock::new();

/// Bound once by the Person service factory; company proofs come from here.
pub(crate) fn bind(parties: Arc<PartyProofs>) -> bilibi_module_registry::Result<()> {
    PARTIES.set(parties).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate person files binding".into())
    })
}

pub(crate) fn person_id(subject: &str) -> Result<PersonId> {
    subject
        .parse::<i64>()
        .ok()
        .and_then(|id| PersonId::new(id).ok())
        .ok_or(Error::NotFound)
}

pub(crate) async fn authorize(
    client: &dyn ProofClient,
    scope: &Scope,
    id: PersonId,
    capability: &str,
    write: bool,
) -> Result<()> {
    authorize_proof(client, scope, capability).await?;
    let row = client
        .query_opt(
            "SELECT retired_at IS NULL FROM persons WHERE tenant_id=$1 AND id=$2",
            &[&scope.tenant(), &id.get()],
        )
        .await?
        .ok_or(Error::NotFound)?;
    if write && !row.get::<_, bool>(0) {
        return Err(Error::Forbidden);
    }
    Ok(())
}

async fn company(
    client: &dyn ProofClient,
    scope: &Scope,
    id: OrganisationId,
    write: bool,
) -> Result<()> {
    let parties = PARTIES.get().ok_or(Error::Forbidden)?;
    if write {
        parties.organisation(client, scope, id).await?;
    } else {
        parties
            .organisation_name(client, scope, id)
            .await?
            .ok_or(Error::NotFound)?;
    }
    Ok(())
}

impl SubjectOwner for PersonFiles {
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        action: SubjectAction,
        subject_id: &'a str,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let id = person_id(subject_id)?;
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

impl Owner for PersonFiles {
    fn owner_id(&self) -> &'static str {
        ARTIFACT_OWNER
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        filed_under: OrganisationId,
        operation: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let write = operation != Operation::Read;
            company(client, scope, filed_under, write).await?;
            // Purge has no reference: company maintenance authority only.
            let Some(reference) = reference else {
                return match operation {
                    Operation::Purge => authorize_proof(client, scope, RETIRE).await,
                    _ => Err(Error::Invalid),
                };
            };
            let id = person_id(&reference.subject)?;
            match operation {
                Operation::Read => authorize(client, scope, id, VIEW, false).await,
                Operation::Create | Operation::Delete | Operation::Retain => {
                    authorize(client, scope, id, UPDATE, true).await
                }
                Operation::Purge => authorize(client, scope, id, RETIRE, true).await,
            }
        })
    }
}

inventory::submit! {
    Contribution::new(SUBJECTS, ATTACHMENT_SUBJECT,
        Provenance { module: "core/person", source: file!(), line: line!() },
        &Subject { owner: &FILES })
}
