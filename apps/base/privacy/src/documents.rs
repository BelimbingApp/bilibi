//! The documents a request ends with (ADR 05), stored through `base/artifacts`
//! under the request's company: the access export, an expiring JSON bundle
//! read only through `Artifacts::read` so every download is audited, and the
//! erasure certificate, a retained PDF listing every store's answer.
use crate::{
    requests::{RequestView, BOUND},
    Privacy, REQUEST_APPROVE, REQUEST_VIEW,
};
use bilibi_artifacts::{
    pdf::{self, Block},
    Operation, Owner, Reference, Retention,
};
use bilibi_authz::authorize_proof;
use bilibi_database::{Error, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime};
use tokio_postgres::Client;

/// The stable Artifacts owner ID of request documents.
pub const ARTIFACT_OWNER: &str = "base.privacy";
/// Tenant setting: days an access export stays downloadable.
pub const EXPORT_DAYS: &str = "privacy.export_days";
pub(crate) const EXPORT_DEFAULT: i64 = 30;
/// Tenant setting: years an erasure certificate is kept.
pub const CERTIFICATE_YEARS: &str = "privacy.certificate_years";
pub(crate) const CERTIFICATE_DEFAULT: i64 = 7;

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
/// actor's tenant filed under the same company.
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

/// Bundles every store's staged export into one expiring JSON artifact.
pub(crate) async fn export(
    privacy: &Privacy,
    client: &mut Client,
    scope: &Scope,
    request: &RequestView,
) -> Result<String> {
    let rows = client
        .query(
            "SELECT store,records,files FROM base_privacy_request_exports WHERE tenant_id=$1 AND request_id=$2 ORDER BY store",
            &[&scope.tenant(), &request.id],
        )
        .await?;
    let stores: Vec<Value> = rows
        .iter()
        .map(|row| {
            let store: String = row.get(0);
            let label = privacy
                .stores
                .by_key
                .get(store.as_str())
                .map_or_else(|| store.clone(), |(_, data)| data.label.to_owned());
            json!({"store": store, "label": label, "records": row.get::<_, Value>(1), "files": row.get::<_, Value>(2)})
        })
        .collect();
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
    let bytes = serde_json::to_vec_pretty(&bundle).map_err(|_| Error::Internal)?;
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
            "application/json",
            days,
        )
        .await
        .map_err(artifact_error)?;
    Ok(metadata.id)
}

/// Text the built-in PDF font shows: printable ASCII, others become `?`.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .collect()
}
fn count(result: &Value, action: &str) -> i64 {
    result
        .get("actions")
        .and_then(Value::as_array)
        .map_or(0, |actions| {
            actions
                .iter()
                .filter(|a| a.get("action").and_then(Value::as_str) == Some(action))
                .filter_map(|a| a.get("count").and_then(Value::as_i64))
                .sum()
        })
}

/// Renders and retains the erasure certificate: each store, its action and
/// counts, every record kept and why, deferrals with dates, and the approver.
pub(crate) async fn certificate(
    privacy: &Privacy,
    client: &mut Client,
    scope: &Scope,
    request: &RequestView,
) -> Result<String> {
    let mut stores = Vec::new();
    let mut kept = Vec::new();
    for store in &request.stores {
        let result = store.result.clone().unwrap_or(Value::Null);
        let kept_here = result
            .get("kept")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let deferral = result
            .get("deferral")
            .filter(|d| !d.is_null())
            .map(|d| {
                format!(
                    "{} (by {})",
                    d.get("reason").and_then(Value::as_str).unwrap_or(""),
                    d.get("complete_by").and_then(Value::as_str).unwrap_or("")
                )
            })
            .unwrap_or_else(|| "-".into());
        stores.push(vec![
            plain(&store.label),
            store.state.clone(),
            count(&result, "anonymised").to_string(),
            count(&result, "deleted").to_string(),
            kept_here.len().to_string(),
            plain(&deferral),
        ]);
        for record in kept_here {
            kept.push(vec![
                plain(&store.label),
                plain(record.get("kind").and_then(Value::as_str).unwrap_or("")),
                plain(record.get("id").and_then(Value::as_str).unwrap_or("")),
                record
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .replace('_', " "),
                record
                    .get("until")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
                    .to_owned(),
            ]);
        }
    }
    let holds = client
        .query(
            "SELECT id,reason FROM base_privacy_holds WHERE tenant_id=$1 AND released_at IS NULL \
             AND (person_id=$2 OR record_kind IS NOT NULL) ORDER BY id",
            &[&scope.tenant(), &request.person.get()],
        )
        .await?;
    let mut blocks = vec![
        Block::Text(format!(
            "Request {} for person {}.",
            request.id,
            request.person.get()
        )),
        Block::Text(format!(
            "Received {} by user {}. Approved {} by user {}.",
            request.received_at,
            request.received_by,
            request.approved_at.as_deref().unwrap_or("-"),
            request
                .approved_by
                .map_or_else(|| "-".into(), |id| id.to_string())
        )),
        Block::Text(format!(
            "Identity verified: {}",
            plain(request.verification.as_deref().unwrap_or("-"))
        )),
        Block::Text("Every store mounted on this server answered:".into()),
        Block::Table {
            headers: [
                "Store",
                "State",
                "Anonymised",
                "Deleted",
                "Kept",
                "Deferred",
            ]
            .map(String::from)
            .to_vec(),
            rows: stores,
        },
    ];
    if !kept.is_empty() {
        blocks.push(Block::Text("Records kept, and why:".into()));
        blocks.push(Block::Table {
            headers: ["Store", "Record kind", "ID", "Reason", "Until"]
                .map(String::from)
                .to_vec(),
            rows: kept,
        });
    }
    if !holds.is_empty() {
        blocks.push(Block::Text("Legal holds in force:".into()));
        blocks.push(Block::Table {
            headers: ["Hold", "Reason"].map(String::from).to_vec(),
            rows: holds
                .iter()
                .map(|row| {
                    vec![
                        row.get::<_, i64>(0).to_string(),
                        plain(&row.get::<_, String>(1)),
                    ]
                })
                .collect(),
        });
    }
    let bytes = pdf::render(&pdf::Document {
        title: "Erasure completion certificate".into(),
        metadata: pdf::Metadata {
            author: None,
            subject: Some(format!("Request {}", request.id)),
            keywords: None,
        },
        blocks,
    })
    .map_err(artifact_error)?;
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
    let metadata = privacy
        .artifacts
        .for_owner(scope, request.company, &DOCUMENTS)
        .put_retained(
            client,
            Reference {
                subject: format!("request:{}", request.id),
                kind: "certificate".into(),
            },
            &bytes,
            "application/pdf",
            Retention {
                retain_until,
                legal_hold: false,
                reason: Some("Data-subject erasure certificate".into()),
            },
        )
        .await
        .map_err(artifact_error)?;
    Ok(metadata.id)
}

impl Privacy {
    /// The request's export bundle or certificate, read through Artifacts so the
    /// download is audited. Returns the content type and bytes.
    pub async fn request_document(
        &self,
        client: &mut Client,
        scope: &Scope,
        id: i64,
    ) -> Result<(String, Vec<u8>, String)> {
        let request = self.request(&*client, scope, id).await?;
        let (artifact, name) = match (&request.export_artifact, &request.certificate_artifact) {
            (Some(artifact), _) => (
                artifact.clone(),
                format!("privacy-request-{id}-export.json"),
            ),
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
