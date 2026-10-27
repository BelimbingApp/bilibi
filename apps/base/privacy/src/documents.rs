//! The documents a request ends with (ADR 05), stored through `base/artifacts`
//! under the request's company: the access export, an expiring ZIP of every
//! store's records and the files they reference, read only through
//! `Artifacts::read` so every download is audited; and the erasure
//! certificate, a retained A4 PDF listing every store's answer and every
//! record kept, with why and until when. No trail stores an erased value.
use crate::{
    holds,
    requests::{RequestView, BOUND},
    runner::Answer,
    Privacy, REQUEST_APPROVE, REQUEST_VIEW,
};
use askama::Template;
use bilibi_artifacts::{HtmlFuture, HtmlOwner, Operation, Owner, PdfOptions, Reference, Retention};
use bilibi_authz::authorize_proof;
use bilibi_database::{Error, Result};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_personal_data::{Erasure, FileRef, KeptReason};
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{Cursor, Write},
    time::{Duration, SystemTime},
};
use tokio_postgres::Client;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

/// The stable Artifacts owner ID of request documents.
pub const ARTIFACT_OWNER: &str = "base.privacy";
/// Tenant setting: days an access export stays downloadable.
pub const EXPORT_DAYS: &str = "privacy.export_days";
pub(crate) const EXPORT_DEFAULT: i64 = 30;
/// Tenant setting: years an erasure certificate is kept.
pub const CERTIFICATE_YEARS: &str = "privacy.certificate_years";
pub(crate) const CERTIFICATE_DEFAULT: i64 = 7;
/// Bump when the certificate's layout changes; recorded as provenance.
const CERTIFICATE_TEMPLATE: &str = "privacy-certificate-v2";

fn request_id(reference: &Reference) -> Result<i64> {
    reference
        .subject
        .strip_prefix("request:")
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or(Error::NotFound)
}

/// Request documents follow the request: reading needs `privacy.request.view`,
/// anything else `privacy.request.approve`, and the request must be one of the
/// actor's tenant filed under the same company. Purging expired exports is the
/// scheduler's maintenance.
pub struct RequestDocuments;
pub static DOCUMENTS: RequestDocuments = RequestDocuments;
impl Owner for RequestDocuments {
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
            let parties = &BOUND.get().ok_or(Error::Forbidden)?.parties;
            if operation == Operation::Purge
                && reference.is_none()
                && scope.principal() == Some(bilibi_schedule::PRINCIPAL)
            {
                return parties
                    .organisation(client, scope, filed_under)
                    .await
                    .map(|_| ());
            }
            if operation == Operation::Read {
                parties
                    .organisation_name(client, scope, filed_under)
                    .await?
                    .ok_or(Error::NotFound)?;
            } else {
                parties.organisation(client, scope, filed_under).await?;
            }
            let capability = if operation == Operation::Read {
                REQUEST_VIEW
            } else {
                REQUEST_APPROVE
            };
            authorize_proof(client, scope, capability).await?;
            let Some(reference) = reference else {
                return match operation {
                    Operation::Purge => Ok(()),
                    _ => Err(Error::Invalid),
                };
            };
            let id = request_id(reference)?;
            client
                .query_opt(
                    "SELECT 1 FROM base_privacy_requests WHERE tenant_id=$1 AND id=$2 AND company_id=$3",
                    &[&scope.tenant(), &id, &filed_under.get()],
                )
                .await?
                .ok_or(Error::NotFound)?;
            Ok(())
        })
    }
}

/// The certificate's facts. Counts, kinds, IDs, reasons and dates only.
#[derive(Debug, Default, Deserialize, Serialize, Template)]
#[template(path = "privacy_certificate.html")]
struct Certificate {
    request: i64,
    person: i64,
    received_at: String,
    received_by: i64,
    verified_by: String,
    approved_at: String,
    approved_by: String,
    stores: Vec<StoreLine>,
    kept: Vec<KeptLine>,
    holds: Vec<HoldLine>,
}
#[derive(Debug, Default, Deserialize, Serialize)]
struct StoreLine {
    label: String,
    state: String,
    anonymised: i64,
    deleted: i64,
    kept: usize,
    deferred: String,
}
#[derive(Debug, Default, Deserialize, Serialize)]
struct KeptLine {
    store: String,
    kind: String,
    id: String,
    reason: String,
    until: String,
}
#[derive(Debug, Default, Deserialize, Serialize)]
struct HoldLine {
    id: i64,
    reason: String,
}
impl HtmlOwner for RequestDocuments {
    fn render_html<'a>(
        &'a self,
        _scope: &'a Scope,
        _company: OrganisationId,
        _reference: &'a Reference,
        data: &'a Value,
    ) -> HtmlFuture<'a> {
        Box::pin(async move {
            let certificate: Certificate = serde_json::from_value(data.clone())
                .map_err(|_| bilibi_artifacts::Error::Invalid)?;
            certificate
                .render()
                .map_err(|_| bilibi_artifacts::Error::Invalid)
        })
    }
}

fn artifact_error(error: bilibi_artifacts::Error) -> Error {
    match error {
        bilibi_artifacts::Error::Database(error) => error,
        bilibi_artifacts::Error::NotFound => Error::NotFound,
        bilibi_artifacts::Error::Invalid => Error::Invalid,
        _ => Error::Internal,
    }
}
async fn setting(
    privacy: &Privacy,
    client: &Client,
    scope: &Scope,
    key: &str,
    default: i64,
    max: i64,
) -> Result<i64> {
    Ok(privacy
        .settings
        .get(client, scope, bilibi_settings::Target::Tenant, key)
        .await?
        .value
        .as_i64()
        .filter(|value| (1..=max).contains(value))
        .unwrap_or(default))
}

/// A file name safe inside the bundle: no path, no control characters.
fn bundle_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':') {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect();
    let clean = clean.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if clean.is_empty() {
        "file".into()
    } else {
        clean.to_owned()
    }
}

/// Bundles every store's staged export, and the bytes of every file a store
/// listed (read through that store with the request's grant), into one
/// expiring ZIP. A bundle larger than `artifacts.max_bytes` is `Invalid`
/// (`export_too_large` on the request), never a shorter export.
pub(crate) async fn export(
    privacy: &Privacy,
    client: &mut Client,
    scope: &Scope,
    request: &RequestView,
) -> Result<String> {
    let grant = privacy
        .issuer
        .disclosure(scope.tenant(), request.id, request.person);
    let limit = privacy
        .settings
        .get_global(&*client, "artifacts.max_bytes")
        .await?
        .as_u64()
        .ok_or(Error::Internal)? as usize;
    let rows = client
        .query(
            "SELECT store,records,files FROM base_privacy_request_exports WHERE tenant_id=$1 AND request_id=$2 ORDER BY store",
            &[&scope.tenant(), &request.id],
        )
        .await?;
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let (mut stores, mut size) = (Vec::with_capacity(rows.len()), 0_usize);
    for row in rows {
        let store: String = row.get(0);
        let (key, data) = privacy.stores.get(&store).ok_or(Error::Internal)?;
        let refs: Vec<FileRef> = serde_json::from_value(row.get(2)).map_err(|_| Error::Internal)?;
        let folder = key.replace(['/', '.'], "-");
        let mut files = Vec::with_capacity(refs.len());
        for (index, file) in refs.iter().enumerate() {
            let read = data
                .handler
                .file(client, scope, &grant, &file.reference)
                .await?;
            size += read.bytes.len();
            if size > limit {
                return Err(Error::Invalid);
            }
            let path = format!("files/{folder}/{}-{}", index + 1, bundle_name(&file.name));
            zip.start_file(path.as_str(), options)
                .map_err(|_| Error::Internal)?;
            zip.write_all(&read.bytes).map_err(|_| Error::Internal)?;
            files.push(json!({"name": file.name, "path": path, "content_type": read.content_type}));
        }
        stores.push(json!({"store": key, "label": data.label, "records": row.get::<_, Value>(1), "files": files}));
    }
    let generated: String = client
        .query_one(
            "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
            &[],
        )
        .await?
        .get(0);
    let bundle = json!({
        "request": request.id, "kind": request.kind.as_str(), "person": request.person.get(),
        "generated_at": generated, "stores": stores,
    });
    let json = serde_json::to_vec_pretty(&bundle).map_err(|_| Error::Internal)?;
    size += json.len();
    if size > limit {
        return Err(Error::Invalid);
    }
    zip.start_file("export.json", options)
        .map_err(|_| Error::Internal)?;
    zip.write_all(&json).map_err(|_| Error::Internal)?;
    let bytes = zip.finish().map_err(|_| Error::Internal)?.into_inner();
    if bytes.len() > limit {
        return Err(Error::Invalid);
    }
    let days = setting(privacy, client, scope, EXPORT_DAYS, EXPORT_DEFAULT, 365).await?;
    let metadata = privacy
        .artifacts
        .for_owner(scope, request.company, &DOCUMENTS)
        .put_expiring(
            client,
            Reference {
                subject: format!("request:{}", request.id),
                kind: "export".into(),
            },
            &bytes,
            "application/zip",
            days,
        )
        .await
        .map_err(artifact_error)?;
    Ok(metadata.id)
}

fn reason_text(reason: KeptReason) -> &'static str {
    match reason {
        KeptReason::LegalHold => "Legal hold",
        KeptReason::RetainedUntil => "Retained record",
        KeptReason::LegalMinimum => "Legal minimum period",
        KeptReason::Shared => "Shared with another party",
    }
}

/// Renders (A4) and retains the erasure certificate: each store, its action
/// and counts, every record kept with its reason and end date, the legal
/// holds that kept them, and who received and approved the request. It never
/// shows how the identity was checked.
pub(crate) async fn certificate(
    privacy: &Privacy,
    client: &mut Client,
    scope: &Scope,
    request: &RequestView,
) -> Result<String> {
    // Printed in UTC, named: the certificate outlives any reader's zone.
    let utc = |at: &str| {
        at.parse::<jiff::Timestamp>().map_or_else(
            |_| at.to_owned(),
            |at| {
                bilibi_datetime::DisplayContext {
                    mode: bilibi_datetime::Mode::Utc,
                    timezone: "UTC".into(),
                    locale: bilibi_locale::FALLBACK,
                }
                .render(
                    at,
                    bilibi_datetime::Format::DateTime,
                    bilibi_datetime::Precision::Minute,
                )
                .utc
            },
        )
    };
    let mut certificate = Certificate {
        request: request.id,
        person: request.person.get(),
        received_at: utc(&request.received_at),
        received_by: request.received_by,
        verified_by: request
            .verified_by
            .map_or_else(|| "-".into(), |id| format!("user {id}")),
        approved_at: request
            .approved_at
            .as_deref()
            .map_or_else(|| "-".into(), utc),
        approved_by: request
            .approved_by
            .map_or_else(|| "-".into(), |id| format!("user {id}")),
        ..Certificate::default()
    };
    let mut hold_ids = Vec::new();
    for store in &request.stores {
        let answer: Answer = store
            .result
            .clone()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::Internal)?
            .unwrap_or_default();
        let count = |action| {
            answer
                .actions
                .iter()
                .filter(|a| a.action == action)
                .map(|a| a.count)
                .sum()
        };
        certificate.stores.push(StoreLine {
            label: store.label.clone(),
            state: store.state.clone(),
            anonymised: count(bilibi_personal_data::Action::Anonymised),
            deleted: count(bilibi_personal_data::Action::Deleted),
            kept: answer.kept.len(),
            deferred: answer.deferral.as_ref().map_or_else(
                || "-".into(),
                |d| format!("{} (by {})", d.reason, d.complete_by),
            ),
        });
        for kept in &answer.kept {
            hold_ids.extend(kept.hold);
            certificate.kept.push(KeptLine {
                store: store.label.clone(),
                kind: kept.kind.clone(),
                id: kept.id.clone(),
                reason: reason_text(kept.reason).into(),
                until: kept.until.as_deref().map_or_else(|| "-".into(), utc),
            });
        }
    }
    hold_ids.sort_unstable();
    hold_ids.dedup();
    certificate.holds = holds::reasons(&*client, scope.tenant(), &hold_ids)
        .await?
        .into_iter()
        .map(|(id, reason)| HoldLine { id, reason })
        .collect();
    let years = setting(
        privacy,
        client,
        scope,
        CERTIFICATE_YEARS,
        CERTIFICATE_DEFAULT,
        100,
    )
    .await?;
    let retain_until = SystemTime::now() + Duration::from_secs(years as u64 * 365 * 86_400);
    let data = serde_json::to_value(&certificate).map_err(|_| Error::Internal)?;
    let options = PdfOptions::default()
        .template_version(CERTIFICATE_TEMPLATE)
        .retained(Retention {
            retain_until,
            legal_hold: false,
            reason: Some("Data-subject erasure certificate".into()),
        });
    let rendered = privacy
        .artifacts
        .for_owner(scope, request.company, &DOCUMENTS)
        .render_inline_with(
            client,
            Reference {
                subject: format!("request:{}", request.id),
                kind: "certificate".into(),
            },
            &data,
            &options,
        )
        .await
        .map_err(artifact_error)?;
    Ok(rendered.document.id)
}

/// Deletes an export built for `request` that was never published, because
/// an erasure of the person overtook it.
pub(crate) async fn discard_export(
    privacy: &Privacy,
    client: &mut Client,
    scope: &Scope,
    request: &RequestView,
    artifact: &str,
) -> Result<()> {
    match privacy
        .artifacts
        .for_owner(scope, request.company, &DOCUMENTS)
        .delete(client, artifact)
        .await
    {
        Ok(()) | Err(bilibi_artifacts::Error::NotFound) => Ok(()),
        Err(error) => Err(artifact_error(error)),
    }
}

/// Deletes the export of every request about `person`, under the erasure's
/// grant (an earlier access request may be filed under another company).
/// Deleting one already deleted is a no-op.
pub(crate) async fn delete_exports(
    privacy: &Privacy,
    client: &mut Client,
    scope: &Scope,
    grant: &Erasure,
    person: PersonId,
) -> Result<()> {
    let rows = client
        .query(
            "SELECT id,company_id,export_artifact FROM base_privacy_requests WHERE tenant_id=$1 AND person_id=$2 AND export_artifact IS NOT NULL ORDER BY id",
            &[&scope.tenant(), &person.get()],
        )
        .await?;
    for row in rows {
        let (id, artifact): (i64, String) = (row.get(0), row.get(2));
        let company = OrganisationId::new(row.get(1)).map_err(|_| Error::Internal)?;
        let subject = format!("request:{id}");
        match privacy
            .artifacts
            .for_erasure(scope, company, &DOCUMENTS, grant, &subject)
            .delete(client, &artifact)
            .await
        {
            Ok(()) | Err(bilibi_artifacts::Error::NotFound) => {}
            Err(error) => return Err(artifact_error(error)),
        }
    }
    Ok(())
}

/// Removes the tenant's expired exports, company by company, as the
/// scheduler; a document that will not go stays for the next run.
pub(crate) async fn purge_exports(
    privacy: &Privacy,
    client: &mut Client,
    scope: &Scope,
) -> Result<bool> {
    let companies: Vec<i64> = client
        .query(
            "SELECT DISTINCT company_id FROM base_privacy_requests WHERE tenant_id=$1 AND export_artifact IS NOT NULL ORDER BY company_id",
            &[&scope.tenant()],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let mut failed = false;
    for company in companies {
        let company = OrganisationId::new(company).map_err(|_| Error::Internal)?;
        match privacy
            .artifacts
            .for_owner(scope, company, &DOCUMENTS)
            .purge_expired(client)
            .await
        {
            Ok(report) => failed |= !report.failed.is_empty(),
            Err(_) => failed = true,
        }
    }
    Ok(!failed)
}

impl Privacy {
    /// The request's export bundle or certificate, read through Artifacts so the
    /// download is audited. Returns the content type, bytes and file name. A
    /// reader who sees any personal field masked or withheld must give a reason
    /// and hold `privacy.field.reveal` to download an export, which is
    /// recorded like a reveal.
    pub async fn request_document(
        &self,
        client: &mut Client,
        scope: &Scope,
        id: i64,
        reason: Option<&str>,
    ) -> Result<(String, Vec<u8>, String)> {
        let request = self.request(&*client, scope, id).await?;
        let (artifact, name) = match (&request.export_artifact, &request.certificate_artifact) {
            (Some(artifact), _) => {
                self.reveal_export(&*client, scope, id, reason).await?;
                (artifact.clone(), format!("privacy-request-{id}-export.zip"))
            }
            (None, Some(artifact)) => (
                artifact.clone(),
                format!("privacy-request-{id}-certificate.pdf"),
            ),
            (None, None) => return Err(Error::NotFound),
        };
        let document = self
            .artifacts
            .for_owner(scope, request.company, &DOCUMENTS)
            .read(client, &artifact)
            .await
            .map_err(artifact_error)?;
        Ok((document.metadata.content_type, document.bytes, name))
    }
}
