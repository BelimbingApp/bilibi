//! Person's personal-data store (ADR 05, `base/personal_data`): the person
//! record, the person's own attachments (`core.person` Media subject) and
//! documents (`core.person` Artifacts owner). Erasure anonymises the person to
//! a stub that keeps its ID, kind and links, so affiliations, accounts and
//! other records still point at it, and marks it erased. Files are read and
//! removed only through the grant-checked paths of Media and Artifacts, so a
//! retired person's documents and those filed under another company are
//! reached too, and nothing else gains that reach. Other people sharing an
//! email or phone are counted for the operator, never touched. No Bilimbi
//! counterpart.
use crate::{
    files::{ARTIFACT_OWNER, ATTACHMENT_SUBJECT, FILES},
    model::{Person, COLUMNS},
};
use bilibi_artifacts::Artifacts;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_media::{AttachmentSubject, Media, ASSET_KIND, ATTACHMENT_KIND};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::OrganisationId;
use bilibi_personal_data::{
    Action, Disclosure, Erased, ErasedRecord, Erasure, Exported, File, FileRef, Handler,
    HandlerFuture, Holds, Identifier, Kept, KeptReason, KindCount, Located, PersonalData, Progress,
    RecordKind, Subject, ERASED, PERSONAL_DATA, RECORD_KINDS,
};
use bilibi_tenancy::Scope;
use serde_json::json;
use std::sync::{Arc, OnceLock};
use tokio_postgres::{Client, GenericClient};

/// The audit record kind person changes use.
const PERSON_KIND: &str = "person";
const FIELDS: [&str; 3] = ["name", "email", "phone"];
/// The kind a kept document is listed under.
const DOCUMENT_KIND: &str = "core.person document";

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
fn media_error(error: bilibi_media::Error) -> Error {
    match error {
        bilibi_media::Error::Database(error) => error,
        bilibi_media::Error::NotFound => Error::NotFound,
        _ => Error::Internal,
    }
}
fn artifact_error(error: bilibi_artifacts::Error) -> Error {
    match error {
        bilibi_artifacts::Error::Database(error) => error,
        bilibi_artifacts::Error::NotFound => Error::NotFound,
        _ => Error::Internal,
    }
}

fn identifiers(subject: &Subject) -> (Vec<String>, Vec<String>) {
    let (mut emails, mut phones) = (Vec::new(), Vec::new());
    for identifier in &subject.identifiers {
        match identifier {
            Identifier::Email(value) => emails.push(value.to_lowercase()),
            // Digits only, compared with the stored phone's digits, so
            // `+60 12-345 6789` and `60123456789` match.
            Identifier::Phone(value) => {
                let digits: String = value.chars().filter(char::is_ascii_digit).collect();
                if !digits.is_empty() {
                    phones.push(digits);
                }
            }
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
fn files_of(subject: &Subject) -> String {
    subject.person.get().to_string()
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
                    "SELECT count(*) FILTER (WHERE id=$2), count(*) FILTER (WHERE id<>$2 AND (lower(email)=ANY($3) OR regexp_replace(phone,'[^0-9]','','g')=ANY($4))) \
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
            let id = files_of(subject);
            // Files are counted only when this reader may see them.
            if let Ok(count) = files
                .media
                .count_attachments(
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
                    kind: ATTACHMENT_KIND.into(),
                    by_person: count,
                    by_identifier: 0,
                });
            }
            if let Ok(documents) = files
                .artifacts
                .subject_documents(client, scope, &FILES, &id)
                .await
            {
                kinds.push(KindCount {
                    kind: DOCUMENT_KIND.into(),
                    by_person: documents.len() as i64,
                    by_identifier: 0,
                });
            }
            Ok(Located {
                kinds,
                ..Located::default()
            })
        })
    }
    fn export<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
        grant: &'a Disclosure,
    ) -> HandlerFuture<'a, Exported> {
        Box::pin(async move {
            grant.check_subject(scope, subject)?;
            let Some(person) = person(client, scope, subject, false).await? else {
                return Ok(Exported::default());
            };
            let files = services()?;
            let id = files_of(subject);
            let attachments = files
                .media
                .disclosed_attachments(
                    client,
                    scope,
                    grant,
                    AttachmentSubject {
                        kind: ATTACHMENT_SUBJECT,
                        id: &id,
                    },
                )
                .await
                .map_err(media_error)?;
            let documents = files
                .artifacts
                .disclosed_documents(client, scope, grant, &FILES, &id)
                .await
                .map_err(artifact_error)?;
            let mut refs: Vec<FileRef> = attachments
                .iter()
                .map(|a| FileRef {
                    reference: format!("media:{}", a.public_id),
                    name: a
                        .original_filename
                        .clone()
                        .unwrap_or_else(|| a.public_id.clone()),
                    content_type: a
                        .mime_type
                        .clone()
                        .unwrap_or_else(|| "application/octet-stream".into()),
                })
                .collect();
            refs.extend(documents.iter().map(|(company, d)| FileRef {
                reference: format!("artifact:{}:{}", company.get(), d.id),
                name: format!("{}-{}", d.reference.kind, d.id),
                content_type: d.content_type.clone(),
            }));
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
    fn file<'a>(
        &'a self,
        client: &'a mut Client,
        scope: &'a Scope,
        grant: &'a Disclosure,
        reference: &'a str,
    ) -> HandlerFuture<'a, File> {
        Box::pin(async move {
            grant.check(scope)?;
            let files = services()?;
            let id = grant.person().get().to_string();
            if let Some(public_id) = reference.strip_prefix("media:") {
                let file = files
                    .media
                    .disclosed_attachment(
                        &*client,
                        scope,
                        grant,
                        AttachmentSubject {
                            kind: ATTACHMENT_SUBJECT,
                            id: &id,
                        },
                        public_id,
                    )
                    .await
                    .map_err(media_error)?;
                return Ok(File {
                    content_type: file
                        .mime_type
                        .unwrap_or_else(|| "application/octet-stream".into()),
                    bytes: file.bytes,
                });
            }
            let (company, document) = reference
                .strip_prefix("artifact:")
                .and_then(|rest| rest.split_once(':'))
                .ok_or(Error::NotFound)?;
            let company = company
                .parse::<i64>()
                .ok()
                .and_then(|c| OrganisationId::new(c).ok())
                .ok_or(Error::NotFound)?;
            let read = files
                .artifacts
                .for_disclosure(scope, company, &FILES, grant, &id)
                .read(client, document)
                .await
                .map_err(artifact_error)?;
            Ok(File {
                content_type: read.metadata.content_type,
                bytes: read.bytes,
            })
        })
    }
    fn erase<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        grant: &'a Erasure,
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            grant.check_subject(scope, subject)?;
            let Some(old) = person(tx.sql(), scope, subject, true).await? else {
                return Ok(Erased::default());
            };
            if old.system_key.is_some() {
                return Err(Error::Forbidden);
            }
            let id = old.id.get();
            let mut erased = Erased::default();
            match holds.record(PERSON_KIND, id).await? {
                Some(hold) => erased.kept.push(Kept::held(PERSON_KIND, id, hold)),
                None => {
                    erased.count(PERSON_KIND, Action::Anonymised, 1);
                    // Current values decide, not an earlier erasure's mark: the
                    // stub may have been edited since. A clean stub is still
                    // written while its history holds values, since the
                    // database clears evidence only of a record this
                    // transaction changed; otherwise it is not a write.
                    let live = old.name != ERASED || old.email.is_some() || old.phone.is_some();
                    if live || bilibi_audit::holds_values(tx, PERSON_KIND, id).await? {
                        let tenant = scope.tenant();
                        let before = old.json();
                        tx.mutate(move |sql| {
                        Box::pin(async move {
                            let row = sql
                                .query_one(
                                    &format!("UPDATE persons SET name=$3,email=NULL,phone=NULL,erased_at=coalesce(erased_at,now()),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"),
                                    &[&tenant, &id, &ERASED],
                                )
                                .await?;
                            // `[erased]` where a value was: erasure, not absence.
                            let mut after = Person::from_row(&row)?.json();
                            for field in FIELDS {
                                if !before[field].is_null() {
                                    after[field] = json!(ERASED);
                                }
                            }
                            Ok(((), vec![Change { kind: PERSON_KIND.into(), id, old: before, new: after }]))
                        })
                    })
                    .await?;
                        erased.records.push(ErasedRecord {
                            kind: PERSON_KIND.into(),
                            id,
                            person: None,
                        });
                    }
                }
            }
            let files = services()?
                .media
                .erase_attachments(
                    tx,
                    scope,
                    grant,
                    AttachmentSubject {
                        kind: ATTACHMENT_SUBJECT,
                        id: &id.to_string(),
                    },
                    holds,
                )
                .await
                .map_err(media_error)?;
            for (attachment, hold) in files.held {
                erased
                    .kept
                    .push(Kept::held(ATTACHMENT_KIND, attachment, hold));
            }
            erased.count(ATTACHMENT_KIND, Action::Deleted, files.erased.len() as i64);
            for file in files.erased {
                erased.records.push(ErasedRecord {
                    kind: ATTACHMENT_KIND.into(),
                    id: file.id,
                    person: None,
                });
                erased.records.push(ErasedRecord {
                    kind: ASSET_KIND.into(),
                    id: file.asset_id,
                    person: None,
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
        grant: &'a Erasure,
        holds: &'a Holds,
        progress: &'a dyn Progress,
    ) -> HandlerFuture<'a, ()> {
        Box::pin(async move {
            grant.check_subject(scope, subject)?;
            let files = services()?;
            let id = files_of(subject);
            loop {
                let removal = files
                    .media
                    .remove_erased_bytes(
                        client,
                        scope,
                        grant,
                        AttachmentSubject {
                            kind: ATTACHMENT_SUBJECT,
                            id: &id,
                        },
                    )
                    .await
                    .map_err(media_error)?;
                if !removal.more {
                    break;
                }
                if removal.removed == 0 {
                    // Another attempt is removing them now: try again later.
                    return Err(Error::Conflict);
                }
            }
            let documents = files
                .artifacts
                .erasable_documents(client, scope, grant, &FILES, &id)
                .await
                .map_err(artifact_error)?;
            let person_hold = holds.person_hold().await?;
            for (company, document) in documents {
                let kept = |reason, until, hold| Kept {
                    kind: DOCUMENT_KIND.into(),
                    id: document.id.clone(),
                    reason,
                    until,
                    hold,
                };
                let mut step = Erased::default();
                if let Some(hold) = person_hold {
                    step.kept
                        .push(kept(KeptReason::LegalHold, None, Some(hold)));
                    progress.record(step).await?;
                    continue;
                }
                match files
                    .artifacts
                    .for_erasure(scope, company, &FILES, grant, &id)
                    .delete(client, &document.id)
                    .await
                {
                    Ok(()) => step.count(DOCUMENT_KIND, Action::Deleted, 1),
                    Err(bilibi_artifacts::Error::LegalHold) => {
                        step.kept.push(kept(KeptReason::LegalHold, None, None))
                    }
                    Err(bilibi_artifacts::Error::Retained) => step.kept.push(kept(
                        KeptReason::RetainedUntil,
                        document.retain_until.clone(),
                        None,
                    )),
                    Err(error) => return Err(artifact_error(error)),
                }
                // Saved now: a failure on the next document keeps this one counted.
                progress.record(step).await?;
            }
            Ok(())
        })
    }
}

// Media checks a grant against the attachment subject kind, Artifacts against
// the owner ID. Both are "core.person" today, so the second entry changes
// nothing now; each is listed under its own constant so that renaming one does
// not silently stop erasure and disclosure of the other's files.
inventory::submit! { Contribution::new(PERSONAL_DATA, "core/person.people",
Provenance { module: "core/person", source: file!(), line: line!() },
&PersonalData { label: "People (contacts)", kinds: &[PERSON_KIND, ATTACHMENT_KIND, ASSET_KIND], person_subjects: &[ATTACHMENT_SUBJECT, ARTIFACT_OWNER], handler: &PEOPLE }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/person.person",
Provenance { module: "core/person", source: file!(), line: line!() },
&RecordKind { kind: PERSON_KIND, fields: &FIELDS, subject_identifier: true, person: true }) }
