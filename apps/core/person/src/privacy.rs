//! Person's personal-data store (ADR 05, `base/privacy`): the person record,
//! the person's own attachments (`core.person` Media subject) and documents
//! (`core.person` Artifacts owner). Erasure anonymises the person to a stub
//! that keeps its ID, kind and links, so affiliations, accounts and other
//! records still point at it. Other people sharing an email or phone are
//! counted for the operator, never touched. No Bilimbi counterpart.
use crate::{
    files::{ARTIFACT_OWNER, ATTACHMENT_SUBJECT, FILES},
    model::{Person, COLUMNS},
};
use bilibi_artifacts::Artifacts;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_media::{AttachmentSubject, Media};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_privacy::{
    Action, Erased, ErasedRecord, Exported, FileRef, Handler, HandlerFuture, Holds, Identifier,
    Kept, KeptReason, KindAction, KindCount, Located, PersonalData, Subject, ERASED, PERSONAL_DATA,
};
use bilibi_tenancy::Scope;
use serde_json::json;
use std::sync::{Arc, OnceLock};
use tokio_postgres::{Client, GenericClient};

/// The audit record kind person changes use.
const PERSON_KIND: &str = "person";
const FIELDS: [&str; 3] = ["name", "email", "phone"];

struct Files {
    media: Arc<Media>,
    artifacts: Arc<Artifacts>,
}
static SERVICES: OnceLock<Files> = OnceLock::new();
/// Bound once by the Person service factory.
pub(crate) fn bind(
    media: Arc<Media>,
    artifacts: Arc<Artifacts>,
) -> bilibi_module_registry::Result<()> {
    SERVICES.set(Files { media, artifacts }).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate person privacy binding".into())
    })
}
fn services() -> Result<&'static Files> {
    SERVICES.get().ok_or(Error::Internal)
}

fn identifiers(subject: &Subject) -> (Vec<String>, Vec<String>) {
    let (mut emails, mut phones) = (Vec::new(), Vec::new());
    for identifier in &subject.identifiers {
        match identifier {
            Identifier::Email(value) => emails.push(value.to_lowercase()),
            Identifier::Phone(value) => phones.push(value.clone()),
        }
    }
    (emails, phones)
}
async fn person(
    client: &impl GenericClient,
    scope: &Scope,
    subject: &Subject,
    lock: bool,
) -> Result<Option<Person>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM persons WHERE tenant_id=$1 AND id=$2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    client
        .query_opt(&sql, &[&scope.tenant(), &subject.person.get()])
        .await?
        .map(|row| Person::from_row(&row))
        .transpose()
}

struct People;
static PEOPLE: People = People;
impl Handler for People {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located> {
        Box::pin(async move {
            let (emails, phones) = identifiers(subject);
            let row = client
                .query_one(
                    "SELECT count(*) FILTER (WHERE id=$2), count(*) FILTER (WHERE id<>$2 AND (lower(email)=ANY($3) OR phone=ANY($4))) \
                     FROM persons WHERE tenant_id=$1",
                    &[&scope.tenant(), &subject.person.get(), &emails, &phones],
                )
                .await?;
            let mut kinds = vec![KindCount {
                kind: PERSON_KIND.into(),
                by_person: row.get(0),
                by_identifier: row.get(1),
            }];
            let files = services()?;
            let id = subject.person.get().to_string();
            // Files are counted only when this reader may see them.
            if let Ok(attachments) = files
                .media
                .subject_attachments(
                    client,
                    scope,
                    AttachmentSubject {
                        kind: ATTACHMENT_SUBJECT,
                        id: &id,
                    },
                )
                .await
            {
                kinds.push(KindCount {
                    kind: "attachment".into(),
                    by_person: attachments.len() as i64,
                    by_identifier: 0,
                });
            }
            if let Ok(documents) = files
                .artifacts
                .subject_documents(client, scope, &FILES, &id)
                .await
            {
                kinds.push(KindCount {
                    kind: "document".into(),
                    by_person: documents.len() as i64,
                    by_identifier: 0,
                });
            }
            Ok(Located { kinds })
        })
    }
    fn export<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Exported> {
        Box::pin(async move {
            let Some(person) = person(client, scope, subject, false).await? else {
                return Ok(Exported::default());
            };
            let files = services()?;
            let id = subject.person.get().to_string();
            let attachments = files
                .media
                .subject_attachments(
                    client,
                    scope,
                    AttachmentSubject {
                        kind: ATTACHMENT_SUBJECT,
                        id: &id,
                    },
                )
                .await
                .map_err(|_| Error::Forbidden)?;
            let documents = files
                .artifacts
                .subject_documents(client, scope, &FILES, &id)
                .await
                .map_err(|_| Error::Forbidden)?;
            let mut refs: Vec<FileRef> = attachments
                .iter()
                .map(|a| FileRef::Media(a.public_id.clone()))
                .collect();
            refs.extend(
                documents
                    .iter()
                    .map(|(_, d)| FileRef::Artifact(d.id.clone())),
            );
            Ok(Exported {
                records: json!({
                    "person": person.json(),
                    "attachments": attachments.iter().map(|a| json!({
                        "public_id": a.public_id, "filename": a.original_filename, "mime_type": a.mime_type,
                        "size": a.file_size, "state": a.state.as_str(),
                    })).collect::<Vec<_>>(),
                    "documents": documents.iter().map(|(company, d)| json!({
                        "id": d.id, "company_id": company.get(), "kind": d.reference.kind, "content_type": d.content_type,
                    })).collect::<Vec<_>>(),
                }),
                files: refs,
            })
        })
    }
    fn erase<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            let Some(old) = person(tx.sql(), scope, subject, true).await? else {
                return Ok(Erased::default());
            };
            if old.system_key.is_some() {
                return Err(Error::Forbidden);
            }
            let id = old.id.get();
            if holds.record_held(PERSON_KIND, id) {
                return Ok(Erased {
                    kept: vec![Kept {
                        kind: PERSON_KIND.into(),
                        id: id.to_string(),
                        reason: KeptReason::LegalHold,
                        until: None,
                    }],
                    ..Erased::default()
                });
            }
            let mut erased = Erased::default();
            if old.name != ERASED || old.email.is_some() || old.phone.is_some() {
                let tenant = scope.tenant();
                let before = old.json();
                tx.mutate(move |sql| {
                    Box::pin(async move {
                        let row = sql
                            .query_one(
                                &format!("UPDATE persons SET name=$3,email=NULL,phone=NULL,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"),
                                &[&tenant, &id, &ERASED],
                            )
                            .await?;
                        let after = Person::from_row(&row)?;
                        Ok(((), vec![Change { kind: PERSON_KIND.into(), id, old: before, new: after.json() }]))
                    })
                })
                .await?;
            }
            erased.actions.push(KindAction {
                kind: PERSON_KIND.into(),
                action: Action::Anonymised,
                count: 1,
            });
            erased.records.push(ErasedRecord {
                kind: PERSON_KIND.into(),
                id,
                fields: FIELDS.map(String::from).to_vec(),
            });
            let attachments = services()?
                .media
                .erase_attachments(
                    tx,
                    scope,
                    AttachmentSubject {
                        kind: ATTACHMENT_SUBJECT,
                        id: &id.to_string(),
                    },
                )
                .await
                .map_err(|_| Error::Forbidden)?;
            if !attachments.is_empty() {
                erased.actions.push(KindAction {
                    kind: "attachment".into(),
                    action: Action::Deleted,
                    count: attachments.len() as i64,
                });
            }
            Ok(erased)
        })
    }
    fn after_erase<'a>(
        &'a self,
        client: &'a mut Client,
        scope: &'a Scope,
        subject: &'a Subject,
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            let files = services()?;
            files
                .media
                .remove_erased_bytes(client, scope)
                .await
                .map_err(|_| Error::Internal)?;
            let mut erased = Erased::default();
            if holds.record_held(PERSON_KIND, subject.person.get()) {
                return Ok(erased);
            }
            let id = subject.person.get().to_string();
            let documents = files
                .artifacts
                .subject_documents(client, scope, &FILES, &id)
                .await
                .map_err(|_| Error::Forbidden)?;
            let mut deleted = 0;
            for (company, document) in documents {
                let kept = |reason, until| Kept {
                    kind: format!("{ARTIFACT_OWNER} document"),
                    id: document.id.clone(),
                    reason,
                    until,
                };
                match files
                    .artifacts
                    .for_owner(scope, company, &FILES)
                    .delete(client, &document.id)
                    .await
                {
                    Ok(()) => deleted += 1,
                    Err(bilibi_artifacts::Error::LegalHold) => {
                        erased.kept.push(kept(KeptReason::LegalHold, None))
                    }
                    Err(bilibi_artifacts::Error::Retained) => erased.kept.push(kept(
                        KeptReason::RetainedUntil,
                        document.retain_until.clone(),
                    )),
                    Err(bilibi_artifacts::Error::Database(error)) => return Err(error),
                    Err(_) => return Err(Error::Internal),
                }
            }
            if deleted > 0 {
                erased.actions.push(KindAction {
                    kind: "document".into(),
                    action: Action::Deleted,
                    count: deleted,
                });
            }
            Ok(erased)
        })
    }
}

inventory::submit! { Contribution::new(PERSONAL_DATA, "core/person.people",
Provenance { module: "core/person", source: file!(), line: line!() },
&PersonalData { label: "People (contacts)", record_kinds: &[PERSON_KIND], handler: &PEOPLE }) }
